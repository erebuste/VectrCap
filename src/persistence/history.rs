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
use crate::persistence::winfs::FileIdentity;
use crate::persistence::{PARTIAL_PREFIX, local_from_ms, now_ms};

const SCHEMA_VERSION: i64 = 2;

const SCHEMA_V2: &str = r"
CREATE TABLE upload_retry (
    job_id TEXT NOT NULL REFERENCES jobs(id),
    operation TEXT NOT NULL,
    failures INTEGER NOT NULL CHECK (failures BETWEEN 0 AND 5),
    not_before_ms INTEGER NOT NULL CHECK (not_before_ms >= 0),
    PRIMARY KEY (job_id,operation)
) STRICT;
";

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
    -- Number of times the job entered 'running'; results name the attempt that produced them.
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX jobs_by_status ON jobs(status);
-- A server session has exactly one owning job per normalized server origin.
CREATE UNIQUE INDEX jobs_session_owner ON jobs(dest_origin, session_id) WHERE session_id IS NOT NULL;
CREATE INDEX jobs_by_artifact ON jobs(artifact_id);

CREATE TABLE archive_entries (
    id TEXT PRIMARY KEY NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('automatic', 'manual')),
    size INTEGER NOT NULL CHECK (size >= 0),
    sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
    -- Volume serial + file ID of the bound root directory and of the published file.
    root_identity BLOB NOT NULL CHECK (length(root_identity) = 24),
    file_identity BLOB NOT NULL CHECK (length(file_identity) = 24),
    created_ms INTEGER NOT NULL,
    removed_ms INTEGER
) STRICT;

-- Durable write intent for an archive copy in progress: recorded before the
-- partial file `.vectrcap-partial-<id>` is created, removed atomically with the
-- ownership record. Startup recovery acts only on files these rows prove ours.
CREATE TABLE archive_intents (
    id TEXT PRIMARY KEY NOT NULL,
    root TEXT NOT NULL,
    root_identity BLOB NOT NULL CHECK (length(root_identity) = 24),
    month TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
    file_identity BLOB CHECK (file_identity IS NULL OR length(file_identity) = 24),
    final_name TEXT,
    created_ms INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX archive_live_path ON archive_entries(path) WHERE removed_ms IS NULL;
-- Release passes page live automatic entries per root in (created_ms, id) order.
CREATE INDEX archive_live_auto ON archive_entries(root, created_ms, id)
    WHERE removed_ms IS NULL AND origin = 'automatic';

CREATE TABLE meta (key TEXT PRIMARY KEY NOT NULL, value INTEGER NOT NULL) STRICT;
INSERT INTO meta (key, value) VALUES ('queue_paused', 0);
";

const JOB_SELECT: &str = "SELECT j.id, j.session_id, j.status, j.failure_code, j.receipt_url, j.receipt_name, \
     j.paused, j.dest_id, j.dest_label, j.dest_origin, j.dest_credential_generation, j.dest_link_domain, \
     a.id, a.path, a.display_name, a.kind, a.size, a.sha256, j.attempt \
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
/// receipt is never downgraded and a cancelled job never restarts. Entering
/// `completed` is not a transition: it goes through `History::record_receipt`,
/// which binds the result to the job's session and attempt.
fn transition_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "running" | "cancelled")
            | (
                "running",
                "pending" | "interrupted" | "uncertain" | "failed" | "cancelled"
            )
            | ("interrupted", "running" | "failed" | "cancelled")
            | ("uncertain", "pending" | "running" | "failed" | "cancelled")
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
    pub attempt: u32,
}

/// SQLite-backed durable jobs and local history. All SQL is parameterized.
pub struct History {
    conn: Mutex<Connection>,
}

impl History {
    /// A safe operation's retry budget and deadline survive pause and restart.
    pub fn upload_retry(&self, id: JobId, operation: &str) -> Result<Option<(u32, i64)>, AppError> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT failures, not_before_ms FROM upload_retry WHERE job_id=?1 AND operation=?2",
                params![id.0.to_string(), operation],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
    pub fn set_upload_retry(
        &self,
        id: JobId,
        operation: &str,
        failures: u32,
        not_before_ms: i64,
    ) -> Result<(), AppError> {
        validate_failure_code(operation)?;
        if !(1..=5).contains(&failures) || not_before_ms < 0 {
            return Err(AppError::InvalidInput("invalid upload retry state"));
        }
        if operation != "create"
            && operation != "empty"
            && operation != "status"
            && operation != "complete"
            && !operation
                .strip_prefix("piece.")
                .is_some_and(|s| s.parse::<u32>().is_ok_and(|n| n < 100_000))
        {
            return Err(AppError::InvalidInput("invalid retry operation"));
        }
        self.lock()?.execute("INSERT INTO upload_retry(job_id,operation,failures,not_before_ms) VALUES (?1,?2,?3,?4) ON CONFLICT(job_id,operation) DO UPDATE SET failures=excluded.failures, not_before_ms=excluded.not_before_ms",params![id.0.to_string(),operation,failures,not_before_ms])?;
        Ok(())
    }
    pub fn clear_operation_retry(&self, id: JobId, operation: &str) -> Result<(), AppError> {
        self.lock()?.execute(
            "DELETE FROM upload_retry WHERE job_id=?1 AND operation=?2",
            params![id.0.to_string(), operation],
        )?;
        Ok(())
    }
    /// Explicit Retry resets the attempt budget while retaining server cooldowns.
    pub fn reset_upload_retry(&self, id: JobId) -> Result<(), AppError> {
        self.lock()?.execute(
            "UPDATE upload_retry SET failures=0 WHERE job_id=?1",
            params![id.0.to_string()],
        )?;
        Ok(())
    }
    pub fn upload_retry_deadline(&self, id: JobId) -> Result<Option<i64>, AppError> {
        Ok(self.lock()?.query_row(
            "SELECT max(not_before_ms) FROM upload_retry WHERE job_id=?1",
            params![id.0.to_string()],
            |r| r.get(0),
        )?)
    }
    pub fn clear_upload_retry(&self, id: JobId) -> Result<(), AppError> {
        self.lock()?.execute(
            "DELETE FROM upload_retry WHERE job_id=?1",
            params![id.0.to_string()],
        )?;
        Ok(())
    }
    /// Explicit restart creates a fresh sessionless job with the same source
    /// and destination. Replacement publication and retirement are atomic.
    pub fn restart_upload(&self, id: JobId) -> Result<UploadJob, AppError> {
        let new_id = JobId::new_random();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = tx
            .query_row(
                &format!("{JOB_SELECT} WHERE j.id=?1"),
                params![id.0.to_string()],
                RawJob::read,
            )
            .optional()?
            .ok_or(AppError::InvalidInput("unknown upload job"))?
            .into_job()?;
        if !matches!(
            old.status,
            JobStatus::Failed { .. }
                | JobStatus::Uncertain
                | JobStatus::Interrupted
                | JobStatus::Cancelled
        ) {
            return Err(AppError::InvalidInput(
                "upload cannot be restarted in this state",
            ));
        }
        tx.execute("INSERT INTO jobs(id,artifact_id,dest_id,dest_label,dest_origin,dest_credential_generation,dest_link_domain,status,paused,created_ms,updated_ms) SELECT ?2,artifact_id,dest_id,dest_label,dest_origin,dest_credential_generation,dest_link_domain,'pending',paused,?3,?3 FROM jobs WHERE id=?1",params![id.0.to_string(),new_id.0.to_string(),now_ms()])?;
        tx.execute(
            "UPDATE jobs SET status='cancelled',updated_ms=?2 WHERE id=?1",
            params![id.0.to_string(), now_ms()],
        )?;
        tx.commit()?;
        drop(conn);
        self.job(new_id)?
            .ok_or(AppError::Persistence("replacement upload disappeared"))
    }
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
            attempt: 0,
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

    /// Applies one validated status change atomically. Entering `Running`
    /// starts a new attempt. `Completed` is refused here; use `record_receipt`.
    pub fn transition(&self, id: JobId, to: JobStatus) -> Result<(), AppError> {
        let code = match &to {
            JobStatus::Failed { code } => {
                validate_failure_code(code)?;
                Some(code.as_str())
            }
            JobStatus::Completed(_) => {
                return Err(AppError::InvalidInput(
                    "a confirmed result is recorded with its session and attempt",
                ));
            }
            _ => None,
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
            "UPDATE jobs SET status = ?2, failure_code = ?3, updated_ms = ?4, \
             attempt = attempt + (?2 = 'running') WHERE id = ?1",
            params![id.0.to_string(), to.tag(), code, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a confirmed Vectr result for one started attempt of a job, even
    /// if the job was cancelled, failed, or interrupted while the request was in
    /// flight (remote success is authoritative; no work restarts). The result
    /// must name the job's recorded session (`None` for a sessionless empty-file
    /// upload, enforced against stored artifact size) and an attempt that
    /// actually started. Nonempty artifacts require a matching recorded session.
    /// Recording the same receipt
    /// again is a no-op; a different receipt for a completed job is refused.
    pub fn record_receipt(
        &self,
        id: JobId,
        attempt: u32,
        session: Option<Uuid>,
        receipt: &UploadReceipt,
    ) -> Result<(), AppError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        type ReceiptRow = (
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
        );
        let row: Option<ReceiptRow> = tx
            .query_row(
                "SELECT j.status, j.attempt, j.session_id, j.receipt_url, j.receipt_name, a.size \
                 FROM jobs j JOIN artifacts a ON a.id = j.artifact_id WHERE j.id = ?1",
                params![id.0.to_string()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        let (status, started, stored_session, url, name, size) =
            row.ok_or(AppError::InvalidInput("unknown upload job"))?;
        if attempt == 0 || i64::from(attempt) > started {
            return Err(AppError::InvalidInput(
                "result does not belong to a started upload attempt",
            ));
        }
        if size != 0 && session.is_none() {
            return Err(AppError::InvalidInput(
                "nonempty upload results require a recorded session",
            ));
        }
        if stored_session != session.map(|s| s.to_string()) {
            return Err(AppError::InvalidInput(
                "result belongs to a different upload session",
            ));
        }
        if status == "completed" {
            return if url.as_deref() == Some(receipt.url())
                && name.as_deref() == Some(receipt.name())
            {
                Ok(())
            } else {
                Err(AppError::InvalidInput(
                    "a different result is already recorded for this upload",
                ))
            };
        }
        tx.execute(
            "UPDATE jobs SET status = 'completed', failure_code = NULL, receipt_url = ?2, \
             receipt_name = ?3, updated_ms = ?4 WHERE id = ?1",
            params![id.0.to_string(), receipt.url(), receipt.name(), now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records the server session for a running job, exactly once. A session
    /// belongs to one job per server origin (enforced by a unique index, so
    /// concurrent claims from other connections cannot both succeed);
    /// re-recording the same session on its owner is idempotent.
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
        )
        .map_err(|e| match e.sqlite_error_code() {
            Some(rusqlite::ErrorCode::ConstraintViolation) => {
                AppError::InvalidInput("this upload session belongs to another job")
            }
            _ => e.into(),
        })?;
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

    /// Records ownership of a published archive file (with the identities of
    /// its bound root and of the file itself) and retires its write intent,
    /// in one transaction.
    pub(crate) fn record_archive(
        &self,
        artifact: &Artifact,
        entry: &ArchiveEntry,
        created_ms: i64,
        intent: &ArchiveIntent,
        file_identity: FileIdentity,
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
            "INSERT INTO archive_entries (id, artifact_id, root, path, origin, size, sha256, \
             root_identity, file_identity, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.id.0.to_string(),
                entry.artifact_id.0.to_string(),
                root,
                path,
                origin,
                size,
                entry.sha256.as_slice(),
                intent.root_identity.to_bytes().as_slice(),
                file_identity.to_bytes().as_slice(),
                created_ms,
            ],
        )?;
        tx.execute(
            "DELETE FROM archive_intents WHERE id = ?1",
            params![intent.id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Inserts or updates a write intent (durable before the next file step).
    pub(crate) fn save_archive_intent(&self, intent: &ArchiveIntent) -> Result<(), AppError> {
        let size = i64::try_from(intent.size)
            .map_err(|_| AppError::InvalidInput("archive file is too large"))?;
        self.lock()?.execute(
            "INSERT OR REPLACE INTO archive_intents (id, root, root_identity, month, size, sha256, \
             file_identity, final_name, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                intent.id.to_string(),
                intent.root,
                intent.root_identity.to_bytes().as_slice(),
                intent.month,
                size,
                intent.sha256.as_slice(),
                intent.file_identity.map(|i| i.to_bytes().to_vec()),
                intent.final_name,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn drop_archive_intent(&self, id: Uuid) -> Result<(), AppError> {
        self.lock()?.execute(
            "DELETE FROM archive_intents WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// Recovery pages nominate work only. Recheck the complete live snapshot and
    /// exclude every registered archive (manual/active eligibility belongs to
    /// policy cleanup, never crash recovery), under the same write transaction
    /// that orders registration/intent retirement. `delete` holds the validated
    /// file open and must delete by that handle.
    pub(crate) fn commit_recovered_archive_removal(
        &self,
        intent: &ArchiveIntent,
        name: &str,
        delete: impl FnOnce() -> Result<(), AppError>,
    ) -> Result<bool, AppError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let path = Path::new(&intent.root).join(&intent.month).join(name);
        let path = path
            .to_str()
            .ok_or(AppError::InvalidInput("archive path must be valid Unicode"))?;
        let eligible: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM archive_intents i WHERE i.id = ?1 \
             AND i.root = ?2 AND i.root_identity = ?3 AND i.month = ?4 \
             AND i.size = ?5 AND i.sha256 = ?6 AND i.file_identity IS ?7 AND i.final_name IS ?8 \
             AND NOT EXISTS (SELECT 1 FROM archive_entries e WHERE e.removed_ms IS NULL \
                 AND (e.path = ?9 OR (e.root_identity = i.root_identity AND e.file_identity = i.file_identity))))",
            params![intent.id.to_string(), intent.root, intent.root_identity.to_bytes().as_slice(),
                intent.month, i64::try_from(intent.size).map_err(|_| corrupt())?, intent.sha256.as_slice(),
                intent.file_identity.map(|i| i.to_bytes().to_vec()), intent.final_name, path],
            |r| r.get(0),
        )?;
        if !eligible {
            return Ok(false);
        }
        delete()?;
        tx.execute(
            "DELETE FROM archive_intents WHERE id = ?1",
            params![intent.id.to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Retire only the fetched snapshot; never discard a writer's newer proof.
    pub(crate) fn retire_archive_intent(&self, intent: &ArchiveIntent) -> Result<(), AppError> {
        self.lock()?.execute(
            "DELETE FROM archive_intents WHERE id = ?1 AND root = ?2 AND root_identity = ?3 \
             AND month = ?4 AND size = ?5 AND sha256 = ?6 AND file_identity IS ?7 AND final_name IS ?8",
            params![intent.id.to_string(), intent.root, intent.root_identity.to_bytes().as_slice(),
                intent.month, i64::try_from(intent.size).map_err(|_| corrupt())?, intent.sha256.as_slice(),
                intent.file_identity.map(|i| i.to_bytes().to_vec()), intent.final_name],
        )?;
        Ok(())
    }

    /// A bounded page of outstanding write intents after `after` (by ID).
    pub(crate) fn archive_intents_page(
        &self,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ArchiveIntent>, AppError> {
        type Row = (
            String,
            String,
            Vec<u8>,
            String,
            i64,
            Vec<u8>,
            Option<Vec<u8>>,
            Option<String>,
        );
        let rows: Vec<Row> = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare_cached(
                "SELECT id, root, root_identity, month, size, sha256, file_identity, final_name \
                 FROM archive_intents WHERE id > ?1 ORDER BY id LIMIT ?2",
            )?;
            stmt.query_map(
                params![
                    after.map(|a| a.to_string()).unwrap_or_default(),
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )?
            .collect::<Result<_, _>>()?
        };
        rows.into_iter()
            .map(
                |(id, root, root_id, month, size, sha, file_id, final_name)| {
                    Ok(ArchiveIntent {
                        id: parse_uuid(&id)?,
                        root,
                        root_identity: identity_col(root_id)?,
                        month,
                        size: u64::try_from(size).map_err(|_| corrupt())?,
                        sha256: sha.try_into().map_err(|_| corrupt())?,
                        file_identity: file_id.map(identity_col).transpose()?,
                        final_name,
                    })
                },
            )
            .collect()
    }

    /// One bounded page of live automatic entries under `root` that `rule`
    /// currently nominates, after the `(created_ms, id)` keyset `after`.
    pub(crate) fn archive_page(
        &self,
        root: &str,
        rule: ReleaseRule,
        after: Option<(i64, ArchiveEntryId)>,
        limit: usize,
    ) -> Result<Vec<ArchiveCandidate>, AppError> {
        let (after_ms, after_id) =
            after.map_or((i64::MIN, String::new()), |(ms, id)| (ms, id.0.to_string()));
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare_cached(ARCHIVE_PAGE_SQL)?;
            stmt.query_map(
                params![
                    root,
                    rule.created_at_or_before_ms,
                    rule.require_uploaded,
                    after_ms,
                    after_id,
                    limit
                ],
                |r| {
                    Ok((
                        (
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                        ),
                        r.get::<_, i64>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, Vec<u8>>(7)?,
                        r.get::<_, Vec<u8>>(8)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        rows.into_iter()
            .map(
                |((id, artifact, root, path), size, sha, created, root_id, file_id)| {
                    Ok(ArchiveCandidate {
                        created_ms: created,
                        root_identity: identity_col(root_id)?,
                        file_identity: identity_col(file_id)?,
                        entry: ArchiveEntry {
                            id: ArchiveEntryId(parse_uuid(&id)?),
                            artifact_id: ArtifactId(parse_uuid(&artifact)?),
                            root: PathBuf::from(root),
                            path: PathBuf::from(path),
                            origin: ArchiveOrigin::Automatic,
                            created_at: local_from_ms(created)?,
                            size: u64::try_from(size).map_err(|_| corrupt())?,
                            sha256: sha.try_into().map_err(|_| corrupt())?,
                        },
                    })
                },
            )
            .collect()
    }

    /// Destructive commit for one archive entry. Under the database write
    /// lock it rechecks that the entry is still live, automatic, unreferenced
    /// by any unfinished job (and uploaded, if required); only then runs
    /// `delete` and records the removal in the same transaction. A job created
    /// concurrently is ordered either before (entry kept, `Ok(false)`) or after
    /// the commit. `delete` must act on an already-validated file handle.
    pub(crate) fn commit_archive_removal(
        &self,
        id: ArchiveEntryId,
        require_uploaded: bool,
        delete: impl FnOnce() -> Result<(), AppError>,
    ) -> Result<bool, AppError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let eligible: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM archive_entries e \
             WHERE e.id = ?1 AND e.removed_ms IS NULL AND e.origin = 'automatic' \
             AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.artifact_id = e.artifact_id \
                 AND j.status NOT IN ('completed', 'cancelled')) \
             AND (?2 = 0 OR EXISTS (SELECT 1 FROM jobs j WHERE j.artifact_id = e.artifact_id \
                 AND j.status = 'completed')))",
            params![id.0.to_string(), require_uploaded],
            |r| r.get(0),
        )?;
        if !eligible {
            return Ok(false);
        }
        delete()?;
        tx.execute(
            "UPDATE archive_entries SET removed_ms = ?2 WHERE id = ?1",
            params![id.0.to_string(), now_ms()],
        )?;
        tx.commit()?;
        Ok(true)
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

/// Which live automatic entries a release pass may consider.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReleaseRule {
    pub created_at_or_before_ms: i64,
    /// Only entries whose artifact has a confirmed upload.
    pub require_uploaded: bool,
}

/// Params: ?1 root, ?2 created_ms cutoff, ?3 require uploaded, ?4/?5 keyset
/// after `(created_ms, id)`, ?6 limit. Served by `archive_live_auto` in
/// `(root, created_ms, id)` order, so no sort or full scan is needed.
pub(crate) const ARCHIVE_PAGE_SQL: &str = "SELECT e.id, e.artifact_id, e.root, e.path, e.size, e.sha256, e.created_ms, \
     e.root_identity, e.file_identity \
     FROM archive_entries e \
     WHERE e.removed_ms IS NULL AND e.origin = 'automatic' AND e.root = ?1 AND e.created_ms <= ?2 \
     AND (e.created_ms, e.id) > (?4, ?5) \
     AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.artifact_id = e.artifact_id \
         AND j.status NOT IN ('completed', 'cancelled')) \
     AND (?3 = 0 OR EXISTS (SELECT 1 FROM jobs j WHERE j.artifact_id = e.artifact_id \
         AND j.status = 'completed')) \
     ORDER BY e.created_ms, e.id LIMIT ?6";

/// A live automatic entry nominated by a page query (not yet authorized).
pub(crate) struct ArchiveCandidate {
    pub created_ms: i64,
    pub entry: ArchiveEntry,
    pub root_identity: FileIdentity,
    pub file_identity: FileIdentity,
}

/// One archive copy in progress (see the `archive_intents` table).
#[derive(Debug, Clone)]
pub(crate) struct ArchiveIntent {
    pub id: Uuid,
    pub root: String,
    pub root_identity: FileIdentity,
    pub month: String,
    pub size: u64,
    pub sha256: [u8; 32],
    /// Identity of the partial file once it exists.
    pub file_identity: Option<FileIdentity>,
    /// Name being published; set before each no-clobber rename attempt.
    pub final_name: Option<String>,
}

impl ArchiveIntent {
    pub(crate) fn partial_name(&self) -> String {
        format!("{PARTIAL_PREFIX}{}", self.id.simple())
    }
}

fn identity_col(bytes: Vec<u8>) -> Result<FileIdentity, AppError> {
    FileIdentity::from_bytes(&bytes).ok_or_else(corrupt)
}

fn migrate(conn: &mut Connection) -> Result<(), AppError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    match version {
        0 => {
            tx.execute_batch(SCHEMA_V1)?;
            tx.execute_batch(SCHEMA_V2)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            Ok(())
        }
        1 => {
            tx.execute_batch(SCHEMA_V2)?;
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
    attempt: i64,
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
            attempt: r.get(18)?,
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
            attempt: u32::try_from(self.attempt).map_err(|_| corrupt())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Raw rows: artifact + archive entry (+ optional job) in the test database.
    struct Rows {
        conn: Connection,
        n: u32,
    }

    impl Rows {
        fn entry(&mut self, root: &str, origin: &str, created_ms: i64) -> String {
            self.n += 1;
            let artifact = Uuid::new_v4().to_string();
            let id = Uuid::new_v4().to_string();
            self.conn
                .execute(
                    "INSERT INTO artifacts (id, path, display_name, kind, size, sha256, created_ms) \
                     VALUES (?1, ?2, 'x.png', 'screenshot', 1, zeroblob(32), 0)",
                    params![artifact, format!(r"C:\spool\{}", self.n)],
                )
                .unwrap();
            self.conn
                .execute(
                    "INSERT INTO archive_entries (id, artifact_id, root, path, origin, size, sha256, \
                     root_identity, file_identity, created_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, zeroblob(32), zeroblob(24), zeroblob(24), ?6)",
                    params![id, artifact, root, format!(r"{root}\2026-11\{}.png", self.n), origin, created_ms],
                )
                .unwrap();
            id
        }

        fn job(&self, entry: &str, status: &str) {
            self.conn
                .execute(
                    "INSERT INTO jobs (id, artifact_id, dest_id, dest_label, dest_origin, \
                     dest_credential_generation, status, created_ms, updated_ms) \
                     SELECT ?1, artifact_id, ?2, 'Phone', 'https://vectr.example', 1, ?3, 0, 0 \
                     FROM archive_entries WHERE id = ?4",
                    params![
                        Uuid::new_v4().to_string(),
                        Uuid::new_v4().to_string(),
                        status,
                        entry
                    ],
                )
                .unwrap();
        }
    }

    fn all_pages(h: &History, rule: ReleaseRule, limit: usize) -> Vec<(i64, String)> {
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let page = h.archive_page(r"C:\R", rule, after, limit).unwrap();
            assert!(
                page.len() <= limit,
                "page of {} exceeds limit {limit}",
                page.len()
            );
            let Some(last) = page.last() else { break };
            after = Some((last.created_ms, last.entry.id));
            seen.extend(
                page.iter()
                    .map(|c| (c.created_ms, c.entry.id.0.to_string())),
            );
        }
        seen
    }

    #[test]
    fn archive_pages_are_bounded_filtered_and_tie_stable() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("history.sqlite3");
        let history = History::open(&db).unwrap();
        let mut rows = Rows {
            conn: Connection::open(&db).unwrap(),
            n: 0,
        };

        let mut eligible: Vec<(i64, String)> = [10, 10, 10, 20, 20, 30, 40]
            .into_iter()
            .map(|ms| (ms, rows.entry(r"C:\R", "automatic", ms)))
            .collect();
        eligible.sort();
        rows.entry(r"C:\R", "manual", 5);
        rows.entry(r"C:\S", "automatic", 5);
        rows.entry(r"C:\R", "automatic", 50); // newer than the cutoff
        let removed = rows.entry(r"C:\R", "automatic", 5);
        rows.conn
            .execute(
                "UPDATE archive_entries SET removed_ms = 1 WHERE id = ?1",
                params![removed],
            )
            .unwrap();
        let active = rows.entry(r"C:\R", "automatic", 5);
        rows.job(&active, "failed");

        let age = ReleaseRule {
            created_at_or_before_ms: 45,
            require_uploaded: false,
        };
        for limit in [1, 2, 3, 64] {
            assert_eq!(all_pages(&history, age, limit), eligible, "limit {limit}");
        }

        // Upload reconciliation additionally needs a confirmed upload.
        rows.job(&eligible[1].1, "completed");
        rows.job(&eligible[4].1, "completed");
        let uploaded = ReleaseRule {
            created_at_or_before_ms: i64::MAX,
            require_uploaded: true,
        };
        assert_eq!(
            all_pages(&history, uploaded, 1),
            vec![eligible[1].clone(), eligible[4].clone()]
        );

        // The scan is served by the partial live/automatic index, not a table scan + sort.
        let plan: Vec<String> = rows
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {ARCHIVE_PAGE_SQL}"))
            .unwrap()
            .query_map(params![r"C:\R", 45, false, i64::MIN, "", 2], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let plan = plan.join(" | ");
        assert!(plan.contains("archive_live_auto"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}
