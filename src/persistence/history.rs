use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use crate::app::destinations::{CredentialKey, DestinationId, DestinationSnapshot};
use crate::app::error::AppError;
use crate::app::types::{Artifact, ArtifactId, ArtifactKind, JobId, UploadReceipt};
use crate::persistence::archive::{ArchiveEntry, ArchiveEntryId, ArchiveOrigin};
use crate::persistence::settings::{LinkOrigin, ServerOrigin};
use crate::persistence::{local_from_ms, now_ms};

const SCHEMA_VERSION: i64 = 1;

/// Schema v1. Pause intents and favorites start false; runtime state lives here,
/// never in exported settings.
const SCHEMA_V1: &str = r"
CREATE TABLE artifacts (
    id TEXT PRIMARY KEY NOT NULL,
    path TEXT NOT NULL,
    display_name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('screenshot', 'gif', 'mp4', 'file')),
    size INTEGER NOT NULL CHECK (size >= 0),
    sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
    created_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE history_items (
    id TEXT PRIMARY KEY NOT NULL,
    artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(id),
    favorite INTEGER NOT NULL DEFAULT 0 CHECK (favorite IN (0, 1)),
    created_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    dest_id TEXT NOT NULL,
    dest_label TEXT NOT NULL,
    dest_origin TEXT NOT NULL,
    dest_credential_generation INTEGER NOT NULL CHECK (dest_credential_generation > 0),
    dest_link_domain TEXT,
    session_id TEXT,
    status TEXT NOT NULL CHECK (status IN
        ('pending', 'running', 'interrupted', 'uncertain', 'failed', 'completed', 'cancelled')),
    failure_code TEXT,
    receipt_url TEXT,
    receipt_name TEXT,
    paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX jobs_by_status ON jobs(status);
CREATE INDEX jobs_by_artifact ON jobs(artifact_id);

CREATE TABLE archive_entries (
    id TEXT PRIMARY KEY NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('automatic', 'manual')),
    size INTEGER NOT NULL CHECK (size >= 0),
    sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
    created_ms INTEGER NOT NULL,
    removed_ms INTEGER
) STRICT;
CREATE UNIQUE INDEX archive_live_path ON archive_entries(path) WHERE removed_ms IS NULL;

CREATE TABLE meta (key TEXT PRIMARY KEY NOT NULL, value INTEGER NOT NULL) STRICT;
INSERT INTO meta (key, value) VALUES ('queue_paused', 0);
";

const JOB_SELECT: &str = "SELECT j.id, j.session_id, j.status, j.failure_code, j.receipt_url, j.receipt_name, \
     j.paused, j.dest_id, j.dest_label, j.dest_origin, j.dest_credential_generation, j.dest_link_domain, \
     a.id, a.path, a.display_name, a.kind, a.size, a.sha256 \
     FROM jobs j JOIN artifacts a ON a.id = j.artifact_id";

const UNFINISHED: &str = "status NOT IN ('completed', 'cancelled')";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    /// Was running with a known session at shutdown; resume by querying it.
    Interrupted,
    /// Creation/empty upload may have reached the server; needs explicit resolution.
    Uncertain,
    /// `code` is a short typed identifier (`[a-z0-9_.-]{1,64}`), never raw error text.
    Failed {
        code: String,
    },
    Completed(UploadReceipt),
    Cancelled,
}

impl JobStatus {
    fn tag(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Interrupted => "interrupted",
            JobStatus::Uncertain => "uncertain",
            JobStatus::Failed { .. } => "failed",
            JobStatus::Completed(_) => "completed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, JobStatus::Completed(_) | JobStatus::Cancelled)
    }
}

/// Allowed status changes. Completed/cancelled are terminal: a confirmed
/// receipt is never downgraded and a cancelled job never restarts.
fn transition_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "running" | "cancelled")
            | (
                "running",
                "pending" | "interrupted" | "uncertain" | "failed" | "completed" | "cancelled"
            )
            | ("interrupted", "running" | "failed" | "cancelled")
            | (
                "uncertain",
                "pending" | "running" | "failed" | "completed" | "cancelled"
            )
            | ("failed", "pending" | "cancelled")
    )
}

/// Durable upload job. Never contains a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadJob {
    pub id: JobId,
    pub artifact: Artifact,
    pub destination: DestinationSnapshot,
    pub session: Option<Uuid>,
    pub status: JobStatus,
    pub paused: bool,
}

/// SQLite-backed durable jobs and local history. All SQL is parameterized.
pub struct History {
    conn: Mutex<Connection>,
}

impl History {
    pub fn open(path: &Path) -> Result<History, AppError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(|e| AppError::io("creating the history folder", &e))?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        migrate(&mut conn)?;
        Ok(History {
            conn: Mutex::new(conn),
        })
    }

    /// Persists the artifact, its history item, and a pending job in one transaction.
    pub fn create_job(
        &self,
        artifact: &Artifact,
        destination: &DestinationSnapshot,
    ) -> Result<UploadJob, AppError> {
        if destination.credential.destination() != destination.id
            || destination.credential.origin() != &destination.origin
        {
            return Err(AppError::InvalidInput(
                "destination snapshot does not match its credential binding",
            ));
        }
        let job = UploadJob {
            id: JobId::new_random(),
            artifact: artifact.clone(),
            destination: destination.clone(),
            session: None,
            status: JobStatus::Pending,
            paused: false,
        };
        let now = now_ms();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_artifact(&tx, artifact, now)?;
        tx.execute(
            "INSERT INTO jobs (id, artifact_id, dest_id, dest_label, dest_origin, dest_credential_generation, \
             dest_link_domain, status, paused, created_ms, updated_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', 0, ?8, ?8)",
            params![
                job.id.0.to_string(),
                artifact.id.0.to_string(),
                destination.id.0.to_string(),
                destination.label,
                destination.origin.as_str(),
                destination.credential.generation(),
                destination.link_domain.as_ref().map(LinkOrigin::as_str),
                now,
            ],
        )?;
        tx.commit()?;
        Ok(job)
    }

    pub fn job(&self, id: JobId) -> Result<Option<UploadJob>, AppError> {
        let conn = self.lock()?;
        let raw = conn
            .query_row(
                &format!("{JOB_SELECT} WHERE j.id = ?1"),
                params![id.0.to_string()],
                RawJob::read,
            )
            .optional()?;
        raw.map(RawJob::into_job).transpose()
    }

    /// Applies one validated status change atomically.
    pub fn transition(&self, id: JobId, to: JobStatus) -> Result<(), AppError> {
        if let JobStatus::Failed { code } = &to {
            validate_failure_code(code)?;
        }
        let (code, url, name) = match &to {
            JobStatus::Failed { code } => (Some(code.as_str()), None, None),
            JobStatus::Completed(receipt) => (None, Some(receipt.url()), Some(receipt.name())),
            _ => (None, None, None),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: String = tx
            .query_row(
                "SELECT status FROM jobs WHERE id = ?1",
                params![id.0.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(AppError::InvalidInput("unknown upload job"))?;
        if !transition_allowed(&current, to.tag()) {
            return Err(AppError::InvalidInput(
                "this upload state change is not allowed",
            ));
        }
        tx.execute(
            "UPDATE jobs SET status = ?2, failure_code = ?3, receipt_url = ?4, receipt_name = ?5, updated_ms = ?6 \
             WHERE id = ?1",
            params![id.0.to_string(), to.tag(), code, url, name, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records the server session for a running job, exactly once.
    pub fn set_session(&self, id: JobId, session: Uuid) -> Result<(), AppError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT status, session_id FROM jobs WHERE id = ?1",
                params![id.0.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (status, existing) = row.ok_or(AppError::InvalidInput("unknown upload job"))?;
        let session_text = session.to_string();
        if status != "running" || existing.is_some_and(|s| s != session_text) {
            return Err(AppError::InvalidInput(
                "an upload session can only be recorded once, while running",
            ));
        }
        tx.execute(
            "UPDATE jobs SET session_id = ?2, updated_ms = ?3 WHERE id = ?1",
            params![id.0.to_string(), session_text, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Persists a per-job pause intent (independent of status and queue pause).
    pub fn set_job_paused(&self, id: JobId, paused: bool) -> Result<(), AppError> {
        let changed = self.lock()?.execute(
            &format!("UPDATE jobs SET paused = ?2, updated_ms = ?3 WHERE id = ?1 AND {UNFINISHED}"),
            params![id.0.to_string(), paused, now_ms()],
        )?;
        if changed == 0 {
            return Err(AppError::InvalidInput("unknown or finished upload job"));
        }
        Ok(())
    }

    pub fn set_queue_paused(&self, paused: bool) -> Result<(), AppError> {
        self.lock()?.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'queue_paused'",
            params![paused],
        )?;
        Ok(())
    }

    pub fn queue_paused(&self) -> Result<bool, AppError> {
        let value: i64 = self.lock()?.query_row(
            "SELECT value FROM meta WHERE key = 'queue_paused'",
            [],
            |r| r.get(0),
        )?;
        Ok(value != 0)
    }

    /// Startup recovery. Running jobs with a known session become Interrupted
    /// (resume by status query); without one, creation may have reached the
    /// server, so they become Uncertain. Returns every unfinished job.
    pub fn recover_jobs(&self) -> Result<Vec<UploadJob>, AppError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        tx.execute(
            "UPDATE jobs SET status = 'interrupted', updated_ms = ?1 WHERE status = 'running' AND session_id IS NOT NULL",
            params![now],
        )?;
        tx.execute(
            "UPDATE jobs SET status = 'uncertain', updated_ms = ?1 WHERE status = 'running' AND session_id IS NULL",
            params![now],
        )?;
        let raws = {
            let mut stmt = tx.prepare(&format!(
                "{JOB_SELECT} WHERE j.{UNFINISHED} ORDER BY j.created_ms, j.id"
            ))?;
            stmt.query_map([], RawJob::read)?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        raws.into_iter().map(RawJob::into_job).collect()
    }

    /// Destinations referenced by unfinished jobs; their identity is frozen.
    pub fn active_destination_ids(&self) -> Result<HashSet<DestinationId>, AppError> {
        self.uuid_set(&format!(
            "SELECT DISTINCT dest_id FROM jobs WHERE {UNFINISHED}"
        ))
        .map(|ids| ids.into_iter().map(DestinationId).collect())
    }

    /// Artifacts whose spool copy an unfinished job still needs.
    pub fn artifacts_needing_spool(&self) -> Result<HashSet<ArtifactId>, AppError> {
        self.uuid_set(&format!(
            "SELECT DISTINCT artifact_id FROM jobs WHERE {UNFINISHED}"
        ))
        .map(|ids| ids.into_iter().map(ArtifactId).collect())
    }

    pub(crate) fn record_archive(
        &self,
        artifact: &Artifact,
        entry: &ArchiveEntry,
        created_ms: i64,
    ) -> Result<(), AppError> {
        let utf8 = |p: &Path| {
            p.to_str()
                .map(str::to_owned)
                .ok_or(AppError::InvalidInput("archive path must be valid Unicode"))
        };
        let (root, path) = (utf8(&entry.root)?, utf8(&entry.path)?);
        let size = i64::try_from(entry.size)
            .map_err(|_| AppError::InvalidInput("archive file is too large"))?;
        let origin = match entry.origin {
            ArchiveOrigin::Automatic => "automatic",
            ArchiveOrigin::Manual => "manual",
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_artifact(&tx, artifact, created_ms)?;
        tx.execute(
            "INSERT INTO archive_entries (id, artifact_id, root, path, origin, size, sha256, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.id.0.to_string(),
                entry.artifact_id.0.to_string(),
                root,
                path,
                origin,
                size,
                entry.sha256.as_slice(),
                created_ms,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Live archive entries created at or before `created_before_ms`, with
    /// whether their artifact has unfinished or completed jobs.
    // ponytail: loads all matching rows; page by created_ms if archives reach ~10^5 entries.
    pub(crate) fn archive_candidates(
        &self,
        created_before_ms: i64,
    ) -> Result<Vec<ArchiveCandidate>, AppError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT e.id, e.artifact_id, e.root, e.path, e.origin, e.size, e.sha256, e.created_ms, \
             EXISTS (SELECT 1 FROM jobs j WHERE j.artifact_id = e.artifact_id AND j.{UNFINISHED}), \
             EXISTS (SELECT 1 FROM jobs j WHERE j.artifact_id = e.artifact_id AND j.status = 'completed') \
             FROM archive_entries e WHERE e.removed_ms IS NULL AND e.created_ms <= ?1 ORDER BY e.created_ms, e.id"
        ))?;
        let rows = stmt
            .query_map(params![created_before_ms], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, Vec<u8>>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, bool>(8)?,
                    r.get::<_, bool>(9)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(id, artifact, root, path, origin, size, sha, created, active, uploaded)| {
                    let origin = match origin.as_str() {
                        "automatic" => ArchiveOrigin::Automatic,
                        "manual" => ArchiveOrigin::Manual,
                        _ => return Err(corrupt()),
                    };
                    let entry = ArchiveEntry {
                        id: ArchiveEntryId(parse_uuid(&id)?),
                        artifact_id: ArtifactId(parse_uuid(&artifact)?),
                        root: PathBuf::from(root),
                        path: PathBuf::from(path),
                        origin,
                        created_at: local_from_ms(created)?,
                        size: u64::try_from(size).map_err(|_| corrupt())?,
                        sha256: sha.try_into().map_err(|_| corrupt())?,
                    };
                    Ok(ArchiveCandidate {
                        entry,
                        active,
                        uploaded,
                    })
                },
            )
            .collect()
    }

    pub(crate) fn mark_archive_removed(&self, id: ArchiveEntryId) -> Result<(), AppError> {
        self.lock()?.execute(
            "UPDATE archive_entries SET removed_ms = ?2 WHERE id = ?1 AND removed_ms IS NULL",
            params![id.0.to_string(), now_ms()],
        )?;
        Ok(())
    }

    fn uuid_set(&self, sql: &str) -> Result<HashSet<Uuid>, AppError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(sql)?;
        let texts = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        texts.iter().map(|t| parse_uuid(t)).collect()
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, AppError> {
        self.conn
            .lock()
            .map_err(|_| AppError::Persistence("history lock poisoned"))
    }
}

pub(crate) struct ArchiveCandidate {
    pub entry: ArchiveEntry,
    /// The artifact has an unfinished job that may still need its files.
    pub active: bool,
    /// The artifact has at least one confirmed upload.
    pub uploaded: bool,
}

fn migrate(conn: &mut Connection) -> Result<(), AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    match version {
        0 => {
            tx.execute_batch(SCHEMA_V1)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            Ok(())
        }
        SCHEMA_VERSION => Ok(()),
        _ => Err(AppError::Persistence(
            "history was created by a newer VectrCap version",
        )),
    }
}

/// Inserts the artifact and its single history item if they are new; an
/// existing record must describe the same bytes.
fn insert_artifact(tx: &Transaction<'_>, a: &Artifact, now: i64) -> Result<(), AppError> {
    let path = a.path.to_str().ok_or(AppError::InvalidInput(
        "artifact path must be valid Unicode",
    ))?;
    let size =
        i64::try_from(a.size).map_err(|_| AppError::InvalidInput("artifact is too large"))?;
    let id = a.id.0.to_string();
    let existing: Option<(String, i64, Vec<u8>)> = tx
        .query_row(
            "SELECT path, size, sha256 FROM artifacts WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match existing {
        Some((p, s, h)) if p == path && s == size && h == a.sha256 => {}
        Some(_) => {
            return Err(AppError::Persistence(
                "artifact record conflicts with stored metadata",
            ));
        }
        None => {
            tx.execute(
                "INSERT INTO artifacts (id, path, display_name, kind, size, sha256, created_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    path,
                    a.display_name,
                    kind_tag(a.kind),
                    size,
                    a.sha256.as_slice(),
                    now
                ],
            )?;
            tx.execute(
                "INSERT INTO history_items (id, artifact_id, created_ms) VALUES (?1, ?2, ?3)",
                params![Uuid::new_v4().to_string(), id, now],
            )?;
        }
    }
    Ok(())
}

fn validate_failure_code(code: &str) -> Result<(), AppError> {
    let ok = (1..=64).contains(&code.len())
        && code.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        });
    if !ok {
        return Err(AppError::InvalidInput(
            "failure code must be a short lowercase identifier",
        ));
    }
    Ok(())
}

fn kind_tag(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Screenshot => "screenshot",
        ArtifactKind::Gif => "gif",
        ArtifactKind::Mp4 => "mp4",
        ArtifactKind::File => "file",
    }
}

fn corrupt() -> AppError {
    AppError::Persistence("history record is corrupt")
}

fn parse_uuid(text: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(text).map_err(|_| corrupt())
}

/// Column values of one job row; converted (and validated) outside the
/// rusqlite callback so corruption maps to a typed error.
struct RawJob {
    id: String,
    session: Option<String>,
    status: String,
    failure_code: Option<String>,
    receipt_url: Option<String>,
    receipt_name: Option<String>,
    paused: bool,
    dest_id: String,
    dest_label: String,
    dest_origin: String,
    dest_generation: i64,
    dest_link: Option<String>,
    artifact_id: String,
    artifact_path: String,
    display_name: String,
    kind: String,
    size: i64,
    sha256: Vec<u8>,
}

impl RawJob {
    fn read(r: &Row<'_>) -> rusqlite::Result<RawJob> {
        Ok(RawJob {
            id: r.get(0)?,
            session: r.get(1)?,
            status: r.get(2)?,
            failure_code: r.get(3)?,
            receipt_url: r.get(4)?,
            receipt_name: r.get(5)?,
            paused: r.get(6)?,
            dest_id: r.get(7)?,
            dest_label: r.get(8)?,
            dest_origin: r.get(9)?,
            dest_generation: r.get(10)?,
            dest_link: r.get(11)?,
            artifact_id: r.get(12)?,
            artifact_path: r.get(13)?,
            display_name: r.get(14)?,
            kind: r.get(15)?,
            size: r.get(16)?,
            sha256: r.get(17)?,
        })
    }

    fn into_job(self) -> Result<UploadJob, AppError> {
        let status = match self.status.as_str() {
            "pending" => JobStatus::Pending,
            "running" => JobStatus::Running,
            "interrupted" => JobStatus::Interrupted,
            "uncertain" => JobStatus::Uncertain,
            "cancelled" => JobStatus::Cancelled,
            "failed" => {
                let code = self.failure_code.ok_or_else(corrupt)?;
                validate_failure_code(&code).map_err(|_| corrupt())?;
                JobStatus::Failed { code }
            }
            "completed" => {
                let (url, name) = self
                    .receipt_url
                    .zip(self.receipt_name)
                    .ok_or_else(corrupt)?;
                JobStatus::Completed(UploadReceipt::new(&url, &name).map_err(|_| corrupt())?)
            }
            _ => return Err(corrupt()),
        };
        let kind = match self.kind.as_str() {
            "screenshot" => ArtifactKind::Screenshot,
            "gif" => ArtifactKind::Gif,
            "mp4" => ArtifactKind::Mp4,
            "file" => ArtifactKind::File,
            _ => return Err(corrupt()),
        };
        let dest_id = DestinationId(parse_uuid(&self.dest_id)?);
        let origin = ServerOrigin::parse(&self.dest_origin).map_err(|_| corrupt())?;
        let generation = u32::try_from(self.dest_generation).map_err(|_| corrupt())?;
        let link_domain = self
            .dest_link
            .map(|l| LinkOrigin::parse(&l))
            .transpose()
            .map_err(|_| corrupt())?;
        Ok(UploadJob {
            id: JobId(parse_uuid(&self.id)?),
            artifact: Artifact {
                id: ArtifactId(parse_uuid(&self.artifact_id)?),
                path: PathBuf::from(self.artifact_path),
                display_name: self.display_name,
                kind,
                size: u64::try_from(self.size).map_err(|_| corrupt())?,
                sha256: self.sha256.try_into().map_err(|_| corrupt())?,
            },
            destination: DestinationSnapshot {
                id: dest_id,
                label: self.dest_label,
                origin: origin.clone(),
                credential: CredentialKey::new(dest_id, origin, generation),
                link_domain,
            },
            session: self.session.map(|s| parse_uuid(&s)).transpose()?,
            status,
            paused: self.paused,
        })
    }
}
