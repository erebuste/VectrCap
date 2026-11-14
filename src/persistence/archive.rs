use std::fs::{self, File};
use std::io;
use std::iter;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Local, TimeDelta};
use uuid::Uuid;

use crate::app::error::AppError;
use crate::app::types::{Artifact, ArtifactId, ArtifactKind};
use crate::persistence::filename::{
    FilenamePreset, MAX_NAME_ATTEMPTS, OutputFormat, random_suffix, validate_file_name,
};
use crate::persistence::history::{ArchiveCandidate, ArchiveIntent, History, ReleaseRule};
use crate::persistence::settings::{ArchivePolicy, StorageMode};
use crate::persistence::winfs::{self, ERROR_SHARING_VIOLATION, FileIdentity};
use crate::persistence::{copy_hashing, ensure_free_space, hook, local_from_ms, now_ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArchiveEntryId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveOrigin {
    /// Created by the storage policy; eligible for policy cleanup when unchanged.
    Automatic,
    /// Explicit user Save; never removed automatically.
    Manual,
}

/// Ownership record for one archive file the app created. The database also
/// records the file's identity (volume + file ID) and that of the bound root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub id: ArchiveEntryId,
    pub artifact_id: ArtifactId,
    pub root: PathBuf,
    pub path: PathBuf,
    pub origin: ArchiveOrigin,
    pub created_at: DateTime<Local>,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// Outcome of one release pass. Manual, other-root, still-referenced, and
/// too-recent entries are filtered out by the indexed query and never
/// examined; the `kept_*` counts describe examined candidates only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed: usize,
    /// Became referenced by an unfinished job before the delete committed.
    pub kept_active: usize,
    /// Different bytes, a different (replacement) file, or not a regular file.
    pub kept_modified: usize,
    pub kept_missing: usize,
    /// The configured root path now names a different folder.
    pub kept_other_root: usize,
    /// Open in another program without delete sharing; retried next pass.
    pub kept_in_use: usize,
    /// Filesystem/identity queries cannot establish supported persistent ownership.
    pub kept_unproven_identity: usize,
}

/// Rows fetched per query; bounds memory and connection-lock time.
const PAGE: usize = 64;
const SAVING: &str = "writing the archive copy";

/// User-visible dated archive (`<root>/YYYY-MM/<name>`). Independent of the
/// upload spool: editing, moving, or deleting archive files never affects
/// queued uploads, and cleanup only touches unchanged files it recorded.
///
/// Every operation binds the configured root once by path (a redirected
/// Pictures folder is a legitimate root) and reaches the month folder and
/// files only relative to that handle, refusing descendant reparse points.
/// Files are published, verified, and deleted through one handle that
/// denies other writers, renames, and deletes for as long as it is held.
pub struct ArchiveStore {
    history: Arc<History>,
    root: PathBuf,
    /// Regenerates a bounded name (fresh `{id}`) when the bound name collides.
    preset: FilenamePreset,
}

/// What startup recovery concluded about one write intent.
enum Recovery {
    Removed,
    /// Nothing (left) to do; the intent can be retired without touching files.
    Settled,
    /// Possibly still in use or temporarily unreachable; keep the intent.
    Retry,
}

impl ArchiveStore {
    pub fn new(history: Arc<History>, root: PathBuf, preset: FilenamePreset) -> Self {
        ArchiveStore {
            history,
            root,
            preset,
        }
    }

    /// Automatic (policy) copy of a finalized screenshot/recording.
    pub fn save_finalized(
        &self,
        source: &Artifact,
        captured_at: DateTime<Local>,
    ) -> Result<ArchiveEntry, AppError> {
        self.save(source, captured_at, ArchiveOrigin::Automatic)
    }

    /// Explicit Save: works under every storage mode and is never auto-removed.
    pub fn save_manual(
        &self,
        source: &Artifact,
        captured_at: DateTime<Local>,
    ) -> Result<ArchiveEntry, AppError> {
        self.save(source, captured_at, ArchiveOrigin::Manual)
    }

    /// Age-based cleanup (disabled unless `cleanup_age_days` is set).
    pub fn cleanup(
        &self,
        policy: &ArchivePolicy,
        now: DateTime<Local>,
    ) -> Result<CleanupReport, AppError> {
        policy.validate()?;
        let Some(days) = policy.cleanup_age_days else {
            return Ok(CleanupReport::default());
        };
        let cutoff = now - TimeDelta::days(i64::from(days));
        self.release(
            &policy.resolved_root()?,
            ReleaseRule {
                created_at_or_before_ms: cutoff.timestamp_millis(),
                require_uploaded: false,
            },
        )
    }

    /// Keep-pending/failed mode: releases unchanged automatic copies whose
    /// uploads are confirmed and that no unfinished job still references.
    pub fn reconcile_uploaded(&self, policy: &ArchivePolicy) -> Result<CleanupReport, AppError> {
        if policy.storage_mode != StorageMode::PendingOrFailed {
            return Ok(CleanupReport::default());
        }
        self.release(
            &policy.resolved_root()?,
            ReleaseRule {
                created_at_or_before_ms: i64::MAX,
                require_uploaded: true,
            },
        )
    }

    /// Startup recovery for interrupted archive writes (bounded pages of
    /// durable write intents). Deletes only the intent's own partial, or its
    /// published-but-unrecorded file when identity and bytes still match, via
    /// the verified handle. Foreign, replaced, or still-open files are kept.
    /// Returns the number of files removed.
    pub fn recover_partials(&self) -> Result<usize, AppError> {
        let mut removed = 0;
        let mut after = None;
        loop {
            let page = self.history.archive_intents_page(after, PAGE)?;
            let Some(last) = page.last() else {
                return Ok(removed);
            };
            after = Some(last.id);
            hook("recovery.page_fetched");
            for intent in &page {
                match recover_intent(&self.history, intent)? {
                    Recovery::Removed => {
                        removed += 1;
                    }
                    Recovery::Settled => self.history.retire_archive_intent(intent)?,
                    Recovery::Retry => {}
                }
            }
        }
    }

    fn save(
        &self,
        source: &Artifact,
        captured_at: DateTime<Local>,
        origin: ArchiveOrigin,
    ) -> Result<ArchiveEntry, AppError> {
        if source.kind == ArtifactKind::File {
            return Err(AppError::InvalidInput(
                "only finalized captures and recordings are archived",
            ));
        }
        let root_text = self
            .root
            .to_str()
            .filter(|_| self.root.is_absolute())
            .ok_or(AppError::InvalidInput(
                "archive folder must be an absolute Unicode path",
            ))?;
        validate_file_name(&source.display_name)?;
        let format = output_format(&source.display_name)?;
        let month = captured_at.format("%Y-%m").to_string();

        fs::create_dir_all(&self.root)
            .map_err(|e| AppError::io("creating the archive folder", &e))?;
        let root = winfs::open_root(&self.root)
            .map_err(|e| AppError::io("opening the archive folder", &e))?;
        let month_dir = winfs::open_child_dir(&root, &month, true)
            .map_err(|e| dir_error("opening the archive month folder", &e))?;
        ensure_free_space(&self.root, source.size)?;
        let mut src = winfs::open_read_exclusive(&source.path)
            .map_err(|e| AppError::io("opening the staged file", &e))?;

        let mut intent = ArchiveIntent {
            id: Uuid::new_v4(),
            root: root_text.to_owned(),
            root_identity: winfs::identity(&root).map_err(|e| AppError::io(SAVING, &e))?,
            month,
            size: source.size,
            sha256: source.sha256,
            file_identity: None,
            final_name: None,
        };
        self.history.save_archive_intent(&intent)?;
        let file = match winfs::create_child_file(&month_dir, &intent.partial_name()) {
            Ok(file) => file,
            Err(e) => {
                let _ = self.history.drop_archive_intent(intent.id);
                return Err(AppError::io(SAVING, &e));
            }
        };

        hook("archive.before_identity_recorded");
        let result = (|| {
            let file_identity = winfs::identity(&file).map_err(|e| AppError::io(SAVING, &e))?;
            intent.file_identity = Some(file_identity);
            self.history.save_archive_intent(&intent)?;
            hook("archive.partial_created");

            let (size, sha256) = copy_hashing(&mut src, &mut &file, source.size)
                .map_err(|e| AppError::io(SAVING, &e))?;
            if size != source.size || sha256 != source.sha256 {
                return Err(AppError::ArtifactChanged);
            }
            file.sync_all().map_err(|e| AppError::io(SAVING, &e))?;

            // The bound name first; on collision a fresh, equally bounded
            // name from the preset (never a suffix appended past the cap).
            let names =
                iter::once(Ok(source.display_name.clone())).chain(iter::repeat_with(|| {
                    self.preset.render(captured_at, &random_suffix(), format)
                }));
            let mut published = None;
            for name in names.take(MAX_NAME_ATTEMPTS) {
                let name = name?;
                validate_file_name(&name)?;
                intent.final_name = Some(name.clone());
                self.history.save_archive_intent(&intent)?;
                match winfs::rename_in_place(&file, &name) {
                    Ok(()) => {
                        published = Some(name);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(AppError::io(SAVING, &e)),
                }
            }
            let name = published.ok_or(AppError::Io {
                stage: "choosing a unique file name",
                kind: io::ErrorKind::AlreadyExists,
            })?;
            hook("archive.published");

            let created_ms = now_ms();
            let entry = ArchiveEntry {
                id: ArchiveEntryId(Uuid::new_v4()),
                artifact_id: source.id,
                root: self.root.clone(),
                path: self.root.join(&intent.month).join(name),
                origin,
                created_at: local_from_ms(created_ms)?,
                size,
                sha256,
            };
            self.history
                .record_archive(source, &entry, created_ms, &intent, file_identity)?;
            Ok(entry)
        })();

        if result.is_err() {
            // Never leave an unrecorded file: delete exactly the object this
            // handle created (a swapped-in file cannot be reached). If that
            // fails, the intent stays for startup recovery.
            let deleted = winfs::delete_by_handle(&file).is_ok();
            drop(file);
            if deleted {
                let _ = self.history.drop_archive_intent(intent.id);
            }
        }
        result
    }

    /// Walks eligible entries in bounded `(created_ms, id)` keyset pages. A
    /// page only nominates candidates; eligibility is rechecked at commit.
    fn release(&self, root_path: &Path, rule: ReleaseRule) -> Result<CleanupReport, AppError> {
        let mut report = CleanupReport::default();
        let root_text = root_path
            .to_str()
            .ok_or(AppError::InvalidInput("archive path must be valid Unicode"))?;
        let root = match winfs::open_root(root_path) {
            Ok(root) => root,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(AppError::io("opening the archive folder", &e)),
        };
        let root_identity =
            winfs::identity(&root).map_err(|e| AppError::io("opening the archive folder", &e))?;
        let mut after = None;
        loop {
            let page = self.history.archive_page(root_text, rule, after, PAGE)?;
            let Some(last) = page.last() else {
                return Ok(report);
            };
            after = Some((last.created_ms, last.entry.id));
            for candidate in &page {
                self.release_one(
                    candidate,
                    root_path,
                    &root,
                    root_identity,
                    rule,
                    &mut report,
                )?;
            }
        }
    }

    fn release_one(
        &self,
        candidate: &ArchiveCandidate,
        root_path: &Path,
        root: &File,
        root_identity: FileIdentity,
        rule: ReleaseRule,
        report: &mut CleanupReport,
    ) -> Result<(), AppError> {
        const CHECKING: &str = "checking an archive file";
        let entry = &candidate.entry;
        let Some((month, name)) = month_and_name(&entry.path, root_path) else {
            report.kept_other_root += 1;
            return Ok(());
        };
        if !candidate.root_identity.is_proven() || !root_identity.is_proven() {
            report.kept_unproven_identity += 1;
            return Ok(());
        }
        if !candidate.root_identity.same_object(root_identity) {
            report.kept_other_root += 1;
            return Ok(());
        }
        let month_dir = match winfs::open_child_dir(root, month, false) {
            Ok(dir) => dir,
            Err(e) => return keep(e, report, CHECKING),
        };
        let file = match winfs::open_child_file(&month_dir, name) {
            Ok(file) => file,
            Err(e) => return keep(e, report, CHECKING),
        };
        // Same object (not just same bytes) and still the recorded bytes.
        let identity = winfs::identity(&file).map_err(|e| AppError::io(CHECKING, &e))?;
        if !identity.is_proven() || !candidate.file_identity.is_proven() {
            report.kept_unproven_identity += 1;
            return Ok(());
        }
        let same_object = identity.same_object(candidate.file_identity);
        let unchanged = same_object && {
            let (read, hash) = copy_hashing(&mut &file, &mut io::sink(), entry.size)
                .map_err(|e| AppError::io(CHECKING, &e))?;
            read == entry.size && hash == entry.sha256
        };
        if !unchanged {
            report.kept_modified += 1;
            return Ok(());
        }
        hook("cleanup.verified");
        let removed =
            self.history
                .commit_archive_removal(entry.id, rule.require_uploaded, || {
                    winfs::delete_by_handle(&file)
                        .map_err(|e| AppError::io("removing an archive file", &e))
                })?;
        drop(file);
        if removed {
            report.removed += 1;
        } else {
            report.kept_active += 1;
        }
        Ok(())
    }
}

/// Outcome of an explicit, user-confirmed local-file deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalDeleteOutcome {
    Deleted,
    /// Nothing app-owned exists for this item (already gone, or it was
    /// never archived — e.g. an existing-file upload, which this action
    /// never touches).
    NoOwnedCopy,
    /// An unfinished job still references this artifact.
    Active,
}

/// Explicit, user-confirmed deletion of the one app-owned archived local
/// copy for `artifact_id` (automatic or manual), using the exact same
/// handle-bound identity verification as policy cleanup's `release_one`:
/// the recorded root/file identity must still match before anything is
/// opened for delete, so this can never become a path-based arbitrary
/// delete. Never touches an original imported user file (those are never
/// archived) or a remote/server-side copy. History's favorite flag and any
/// confirmed upload link are untouched: only the `archive_entries` row for
/// this local copy is marked removed.
pub fn delete_local(
    history: &History,
    artifact_id: crate::app::types::ArtifactId,
) -> Result<LocalDeleteOutcome, AppError> {
    const CHECKING: &str = "checking the archived file";
    let Some(candidate) = history.archive_entry_for_artifact(artifact_id)? else {
        return Ok(LocalDeleteOutcome::NoOwnedCopy);
    };
    let entry = &candidate.entry;
    let root = match winfs::open_root(&entry.root) {
        Ok(root) => root,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(LocalDeleteOutcome::NoOwnedCopy);
        }
        Err(e) => return Err(AppError::io("opening the archive folder", &e)),
    };
    let root_identity =
        winfs::identity(&root).map_err(|e| AppError::io("opening the archive folder", &e))?;
    let Some((month, name)) = month_and_name(&entry.path, &entry.root) else {
        return Ok(LocalDeleteOutcome::NoOwnedCopy);
    };
    if !candidate.root_identity.is_proven()
        || !root_identity.is_proven()
        || !candidate.root_identity.same_object(root_identity)
    {
        return Ok(LocalDeleteOutcome::NoOwnedCopy);
    }
    let month_dir = match winfs::open_child_dir(&root, month, false) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(LocalDeleteOutcome::NoOwnedCopy);
        }
        Err(e) => return Err(dir_error(CHECKING, &e)),
    };
    let file = match winfs::open_child_file(&month_dir, name) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(LocalDeleteOutcome::NoOwnedCopy);
        }
        Err(e) => return Err(AppError::io(CHECKING, &e)),
    };
    let identity = winfs::identity(&file).map_err(|e| AppError::io(CHECKING, &e))?;
    if !identity.is_proven()
        || !candidate.file_identity.is_proven()
        || !identity.same_object(candidate.file_identity)
    {
        return Ok(LocalDeleteOutcome::NoOwnedCopy);
    }
    let removed = history.commit_explicit_archive_removal(entry.id, || {
        winfs::delete_by_handle(&file).map_err(|e| AppError::io("deleting the local file", &e))
    })?;
    drop(file);
    Ok(if removed {
        LocalDeleteOutcome::Deleted
    } else {
        LocalDeleteOutcome::Active
    })
}

/// Classifies a failed open of an archive descendant as a reason to keep it.
fn keep(e: io::Error, report: &mut CleanupReport, stage: &'static str) -> Result<(), AppError> {
    match e.kind() {
        io::ErrorKind::NotFound => report.kept_missing += 1,
        // A reparse point or non-regular object where ours used to be.
        io::ErrorKind::InvalidInput => report.kept_modified += 1,
        _ if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => report.kept_in_use += 1,
        _ => return Err(AppError::io(stage, &e)),
    }
    Ok(())
}

fn dir_error(stage: &'static str, e: &io::Error) -> AppError {
    if e.kind() == io::ErrorKind::InvalidInput {
        AppError::InvalidInput("the archive month folder is a link to another location")
    } else {
        AppError::io(stage, e)
    }
}

/// `<root>/<YYYY-MM>/<name>` → (`YYYY-MM`, `name`); anything else is not ours.
fn month_and_name<'a>(path: &'a Path, root: &Path) -> Option<(&'a str, &'a str)> {
    let mut parts = path.strip_prefix(root).ok()?.components();
    let (Some(Component::Normal(month)), Some(Component::Normal(name)), None) =
        (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    Some((month.to_str()?, name.to_str()?))
}

/// The encoder-owned format of a finalized capture, from its bound name.
fn output_format(name: &str) -> Result<OutputFormat, AppError> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    Ok(match ext.as_deref() {
        Some("png") => OutputFormat::Png,
        Some("jpg") => OutputFormat::Jpeg,
        Some("webp") => OutputFormat::WebP,
        Some("gif") => OutputFormat::Gif,
        Some("mp4") => OutputFormat::Mp4,
        _ => {
            return Err(AppError::InvalidInput(
                "archived captures need an encoder-owned extension",
            ));
        }
    })
}

fn recover_intent(history: &History, intent: &ArchiveIntent) -> Result<Recovery, AppError> {
    const RECOVERING: &str = "recovering an interrupted archive copy";
    let io_err = |e: io::Error| AppError::io(RECOVERING, &e);
    let in_use = |e: &io::Error| e.raw_os_error() == Some(ERROR_SHARING_VIOLATION);
    let gone = |e: &io::Error| {
        matches!(
            e.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
        )
    };

    let root = match winfs::open_root(Path::new(&intent.root)) {
        Ok(root) => root,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Recovery::Settled),
        Err(_) => return Ok(Recovery::Retry), // e.g. an offline drive
    };
    if !winfs::identity(&root)
        .map_err(io_err)?
        .same_object(intent.root_identity)
    {
        // Changed or unproven root binding: never act inside it.
        return Ok(Recovery::Settled);
    }
    let month = match winfs::open_child_dir(&root, &intent.month, false) {
        Ok(dir) => dir,
        Err(e) if gone(&e) => return Ok(Recovery::Settled),
        Err(e) => return Err(io_err(e)),
    };

    // 1. The partial itself (still under its unique intent-derived name).
    match winfs::open_child_file(&month, &intent.partial_name()) {
        Ok(file) => {
            let ours = match intent.file_identity {
                Some(id) => winfs::identity(&file).map_err(io_err)?.same_object(id),
                // Neither the unique name nor a plausible length proves ownership.
                None => false,
            };
            if !ours {
                return Ok(Recovery::Settled);
            }
            return remove_recovered(history, intent, &intent.partial_name(), &file);
        }
        Err(e) if in_use(&e) => return Ok(Recovery::Retry), // an active writer
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) if gone(&e) => return Ok(Recovery::Settled),
        Err(e) => return Err(io_err(e)),
    }

    // 2. Renamed to its final name, but the ownership record never committed.
    let (Some(id), Some(name)) = (intent.file_identity, intent.final_name.as_deref()) else {
        return Ok(Recovery::Settled);
    };
    match winfs::open_child_file(&month, name) {
        Ok(file) => {
            if !winfs::identity(&file).map_err(io_err)?.same_object(id) {
                return Ok(Recovery::Settled); // someone else's file at that name
            }
            let (read, hash) =
                copy_hashing(&mut &file, &mut io::sink(), intent.size).map_err(io_err)?;
            if read != intent.size || hash != intent.sha256 {
                return Ok(Recovery::Settled); // changed since: treat as the user's
            }
            remove_recovered(history, intent, name, &file)
        }
        Err(e) if in_use(&e) => Ok(Recovery::Retry),
        Err(e) if gone(&e) => Ok(Recovery::Settled),
        Err(e) => Err(io_err(e)),
    }
}

fn remove_recovered(
    history: &History,
    intent: &ArchiveIntent,
    name: &str,
    file: &File,
) -> Result<Recovery, AppError> {
    let removed = history.commit_recovered_archive_removal(intent, name, || {
        winfs::delete_by_handle(file)
            .map_err(|e| AppError::io("recovering an interrupted archive copy", &e))
    })?;
    Ok(if removed {
        Recovery::Removed
    } else {
        Recovery::Settled
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;
    use crate::app::destinations::{
        DestinationSelection, DestinationSnapshot, Destinations, resolve_destination,
    };
    use crate::persistence::PARTIAL_PREFIX;
    use crate::persistence::artifacts::ArtifactStore;
    use crate::persistence::settings::ServerOrigin;
    use crate::persistence::test_support::{
        assert_sharing_violations, clear_hook, mutation_attempts, set_hook,
    };

    struct Fx {
        dir: TempDir,
        store: ArtifactStore,
        history: Arc<History>,
        root: PathBuf,
        n: std::cell::Cell<u32>,
    }

    impl Fx {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let store = ArtifactStore::new(dir.path().join("spool")).unwrap();
            let history = Arc::new(History::open(&dir.path().join("history.sqlite3")).unwrap());
            let root = dir.path().join("Pictures").join("VectrCap");
            Fx {
                dir,
                store,
                history,
                root,
                n: Default::default(),
            }
        }

        fn archive(&self) -> ArchiveStore {
            ArchiveStore::new(
                self.history.clone(),
                self.root.clone(),
                FilenamePreset::default(),
            )
        }

        fn month(&self) -> PathBuf {
            self.root.join("2026-11")
        }

        fn names_in_month(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.month())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }

        fn intents(&self) -> i64 {
            rusqlite::Connection::open(self.dir.path().join("history.sqlite3"))
                .unwrap()
                .query_row("SELECT count(*) FROM archive_intents", [], |r| r.get(0))
                .unwrap()
        }

        fn screenshot(&self) -> Artifact {
            self.n.set(self.n.get() + 1);
            let name = format!("VectrCap_2026-11-14_08-03-09_0000000{}.png", self.n.get());
            let work = self.dir.path().join(&name);
            fs::write(&work, format!("png {}", self.n.get())).unwrap();
            self.store
                .stage_file(&work, ArtifactKind::Screenshot)
                .unwrap()
        }

        fn policy(&self) -> ArchivePolicy {
            ArchivePolicy {
                storage_mode: StorageMode::AlwaysSave,
                archive_root: Some(self.root.clone()),
                cleanup_age_days: Some(1),
            }
        }
    }

    fn captured() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 11, 14, 8, 3, 9)
            .single()
            .unwrap()
    }

    fn later() -> DateTime<Local> {
        Local::now() + TimeDelta::days(10)
    }

    fn snapshot() -> DestinationSnapshot {
        let mut d = Destinations::default();
        let id = d
            .add(
                "Phone",
                ServerOrigin::parse("https://vectr.example").unwrap(),
                None,
            )
            .unwrap();
        d.bind_credential(id, &HashSet::new()).unwrap();
        resolve_destination(
            DestinationSelection::Inherit,
            DestinationSelection::Inherit,
            &d,
        )
        .unwrap()
    }

    fn exists(p: &Path) -> bool {
        fs::symlink_metadata(p).is_ok()
    }

    #[test]
    fn new_active_reference_at_commit_keeps_the_file() {
        let fx = Fx::new();
        let archive = fx.archive();
        let art = fx.screenshot();
        let entry = archive.save_finalized(&art, captured()).unwrap();
        let (history, queued) = (fx.history.clone(), art.clone());
        set_hook(move |point| {
            if point == "cleanup.verified" {
                // A new upload of this capture is accepted between check and delete.
                history.create_job(&queued, &snapshot()).unwrap();
            }
        });
        let report = archive.cleanup(&fx.policy(), later());
        clear_hook();
        let report = report.unwrap();
        assert!(
            exists(&entry.path),
            "a source needed by an unfinished job is kept"
        );
        assert_eq!((report.removed, report.kept_active), (0, 1));
    }

    /// Runs `save_finalized` with a hook that "crashes" the process at `point`
    /// (unwinding drops handles exactly like process exit; nothing else runs).
    fn crash_at(fx: &Fx, archive: &ArchiveStore, art: &Artifact, point: &'static str) {
        set_hook(move |p| {
            if p == point {
                panic!("simulated crash at {p}");
            }
        });
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            archive.save_finalized(art, captured())
        }));
        clear_hook();
        assert!(outcome.is_err(), "expected the simulated crash");
        assert_eq!(
            fx.intents(),
            1,
            "the write intent was durable before the crash"
        );
    }

    #[test]
    fn verified_file_cannot_be_swapped_before_its_delete() {
        let fx = Fx::new();
        let archive = fx.archive();
        let entry = archive
            .save_finalized(&fx.screenshot(), captured())
            .unwrap();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (s, p) = (seen.clone(), entry.path.clone());
        set_hook(move |point| {
            if point == "cleanup.verified" {
                *s.borrow_mut() = mutation_attempts(&p);
            }
        });
        let report = archive.cleanup(&fx.policy(), later());
        clear_hook();
        assert_sharing_violations(&seen.borrow());
        assert_eq!(report.unwrap().removed, 1);
        assert!(!exists(&entry.path));
        assert!(!exists(&entry.path.with_extension("moved")));
    }

    #[test]
    fn record_failure_rolls_back_only_the_published_object() {
        let fx = Fx::new();
        let archive = fx.archive();
        let art = fx.screenshot();
        let published = fx.month().join(&art.display_name);
        let db = fx.dir.path().join("history.sqlite3");
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (s, p, id) = (seen.clone(), published.clone(), art.id);
        set_hook(move |point| {
            if point == "archive.published" {
                // Make the ownership record fail: a conflicting artifact row.
                rusqlite::Connection::open(&db)
                    .unwrap()
                    .execute(
                        "INSERT INTO artifacts (id, path, display_name, kind, size, sha256, created_ms) \
                         VALUES (?1, 'conflict', 'x.png', 'screenshot', 1, zeroblob(32), 0)",
                        rusqlite::params![id.0.to_string()],
                    )
                    .unwrap();
                *s.borrow_mut() = mutation_attempts(&p);
            }
        });
        let result = archive.save_finalized(&art, captured());
        clear_hook();
        assert!(
            matches!(result, Err(AppError::Persistence(_))),
            "{result:?}"
        );
        assert_sharing_violations(&seen.borrow());
        assert!(fx.names_in_month().is_empty(), "{:?}", fx.names_in_month());
        assert_eq!(fx.intents(), 0);
    }

    #[test]
    fn interrupted_partial_is_recovered_at_startup() {
        let fx = Fx::new();
        let archive = fx.archive();
        crash_at(&fx, &archive, &fx.screenshot(), "archive.partial_created");
        let names = fx.names_in_month();
        assert!(
            names.len() == 1 && names[0].starts_with(PARTIAL_PREFIX),
            "{names:?}"
        );
        fs::write(fx.month().join(".vectrcap-partial-foreign"), b"not ours").unwrap();
        fs::write(fx.month().join("notes.png"), b"user file").unwrap();

        assert_eq!(archive.recover_partials().unwrap(), 1);
        assert_eq!(
            fx.names_in_month(),
            vec![
                ".vectrcap-partial-foreign".to_string(),
                "notes.png".to_string()
            ],
            "only the proven app-owned partial is removed"
        );
        assert_eq!(fx.intents(), 0);
        assert_eq!(archive.recover_partials().unwrap(), 0);
    }

    #[test]
    fn crash_after_publish_before_record_is_recovered() {
        let fx = Fx::new();
        let archive = fx.archive();
        let art = fx.screenshot();
        crash_at(&fx, &archive, &art, "archive.published");
        assert_eq!(fx.names_in_month(), vec![art.display_name.clone()]);
        fs::write(fx.month().join("keep.png"), b"user file").unwrap();

        assert_eq!(archive.recover_partials().unwrap(), 1);
        assert_eq!(fx.names_in_month(), vec!["keep.png".to_string()]);
        assert_eq!(fx.intents(), 0);
    }

    #[test]
    fn published_file_replaced_before_recovery_is_kept() {
        let fx = Fx::new();
        let archive = fx.archive();
        let art = fx.screenshot();
        crash_at(&fx, &archive, &art, "archive.published");
        let published = fx.month().join(&art.display_name);
        let bytes = fs::read(&published).unwrap();
        fs::remove_file(&published).unwrap();
        fs::write(&published, &bytes).unwrap(); // the user's own file now

        assert_eq!(archive.recover_partials().unwrap(), 0);
        assert_eq!(fs::read(&published).unwrap(), bytes);
        assert_eq!(fx.intents(), 0);
    }

    #[test]
    fn recovery_never_touches_an_active_write() {
        let fx = Fx::new();
        let archive = Arc::new(fx.archive());
        let removed = std::rc::Rc::new(std::cell::Cell::new(None));
        let (r, a) = (removed.clone(), archive.clone());
        set_hook(move |point| {
            if point == "archive.partial_created" {
                r.set(Some(a.recover_partials().unwrap()));
            }
        });
        let entry = archive.save_finalized(&fx.screenshot(), captured());
        clear_hook();
        assert_eq!(removed.get(), Some(0), "an open partial is never swept");
        assert!(exists(&entry.unwrap().path));
        assert_eq!(fx.intents(), 0);
    }
    #[test]
    fn stale_recovery_page_preserves_committed_manual_archive() {
        stale_recovery_page_preserves_committed_archive(true);
    }

    #[test]
    fn stale_recovery_page_preserves_committed_active_archive() {
        stale_recovery_page_preserves_committed_archive(false);
    }

    fn stale_recovery_page_preserves_committed_archive(manual: bool) {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;
        let fx = Fx::new();
        let art = fx.screenshot();
        if !manual {
            fx.history.create_job(&art, &snapshot()).unwrap();
        }
        // Separate connection models the writer/recovery lifecycle across processes.
        let writer_history =
            Arc::new(History::open(&fx.dir.path().join("history.sqlite3")).unwrap());
        let writer = ArchiveStore::new(writer_history, fx.root.clone(), FilenamePreset::default());
        let (ready_tx, ready_rx) = sync_channel(1);
        let (resume_tx, resume_rx) = sync_channel(1);
        let (done_tx, done_rx) = sync_channel(1);
        let worker = std::thread::spawn(move || {
            set_hook(move |point| {
                if point == "archive.published" {
                    ready_tx.send(()).unwrap();
                    resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                }
            });
            let result = if manual {
                writer.save_manual(&art, captured())
            } else {
                writer.save_finalized(&art, captured())
            };
            clear_hook();
            // save returned: registration/intent retirement committed, file closed.
            done_tx.send(result.unwrap()).unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let saved = std::rc::Rc::new(std::cell::RefCell::new(None));
        let result = saved.clone();
        set_hook(move |point| {
            if point == "recovery.page_fetched" && result.borrow().is_none() {
                resume_tx.send(()).unwrap();
                *result.borrow_mut() = Some(done_rx.recv_timeout(Duration::from_secs(10)).unwrap());
            }
        });
        let recovery = fx.archive().recover_partials();
        clear_hook();
        worker.join().unwrap();
        let entry = saved.borrow().clone().expect("post-fetch barrier reached");
        assert_eq!(
            recovery.unwrap(),
            0,
            "a stale intent must not remove a committed archive"
        );
        assert_eq!(fs::read(&entry.path).unwrap(), b"png 1");
        assert_eq!(fx.intents(), 0);
        let conn = rusqlite::Connection::open(fx.dir.path().join("history.sqlite3")).unwrap();
        let live: i64 = conn
            .query_row(
                "SELECT count(*) FROM archive_entries WHERE id = ?1 AND removed_ms IS NULL",
                rusqlite::params![entry.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, 1);
        if !manual {
            assert_eq!(fx.history.artifacts_needing_spool().unwrap().len(), 1);
        }
    }

    #[test]
    fn no_identity_intent_preserves_same_name_foreign_sentinel() {
        let fx = Fx::new();
        let archive = fx.archive();
        crash_at(
            &fx,
            &archive,
            &fx.screenshot(),
            "archive.before_identity_recorded",
        );
        let intent = fx
            .history
            .archive_intents_page(None, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert!(intent.file_identity.is_none());
        let path = fx.month().join(intent.partial_name());
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"user").unwrap(); // shorter than the intended five-byte copy
        assert_eq!(
            archive.recover_partials().unwrap(),
            0,
            "no durable identity is no deletion authority"
        );
        assert_eq!(fs::read(&path).unwrap(), b"user");
        assert_eq!(fx.intents(), 0);
    }

    #[test]
    fn legacy_reused_ids_never_authorize_cleanup_of_same_bytes_replacement() {
        for filesystem in ["FAT", "FAT32", "exFAT", "ReFS", "NTFS", "unknown"] {
            winfs::identity_queries::with(filesystem, false, || {
                let fx = Fx::new();
                let archive = fx.archive();
                let art = fx.screenshot();
                let entry = archive.save_finalized(&art, captured()).unwrap();
                let original = entry.path.with_extension("original");
                fs::rename(&entry.path, &original).unwrap();
                fs::write(&entry.path, b"png 1").unwrap();
                let result = archive.cleanup(&fx.policy(), later()).unwrap();
                assert_eq!(
                    result.removed, 0,
                    "{filesystem}: reusable legacy IDs cannot authorize deletion"
                );
                assert_eq!(fs::read(&entry.path).unwrap(), b"png 1");
                assert_eq!(fs::read(&original).unwrap(), b"png 1");
                assert_eq!(fs::read(&art.path).unwrap(), b"png 1");
            });
        }
    }

    #[test]
    fn legacy_reused_ids_never_authorize_crash_recovery_deletion() {
        for filesystem in ["FAT", "ReFS", "NTFS"] {
            winfs::identity_queries::with(filesystem, false, || {
                let fx = Fx::new();
                let archive = fx.archive();
                let art = fx.screenshot();
                crash_at(&fx, &archive, &art, "archive.published");
                let path = fx.month().join(&art.display_name);
                fs::rename(&path, path.with_extension("original")).unwrap();
                fs::write(&path, b"png 1").unwrap();
                assert_eq!(
                    archive.recover_partials().unwrap(),
                    0,
                    "{filesystem}: same name/bytes/reused ID is not durable ownership proof"
                );
                assert_eq!(fs::read(&path).unwrap(), b"png 1");
            });
        }
    }

    #[test]
    fn unknown_filesystem_modern_id_does_not_authorize_cleanup() {
        winfs::identity_queries::with("unknown", true, || {
            let fx = Fx::new();
            let archive = fx.archive();
            let entry = archive
                .save_finalized(&fx.screenshot(), captured())
                .unwrap();
            assert_eq!(archive.cleanup(&fx.policy(), later()).unwrap().removed, 0);
            assert_eq!(fs::read(&entry.path).unwrap(), b"png 1");
        });
    }

    #[test]
    fn explicit_delete_removes_owned_copy_and_keeps_favorite_and_link() {
        let fx = Fx::new();
        let archive = fx.archive();
        let art = fx.screenshot();
        let entry = archive.save_manual(&art, captured()).unwrap();
        let job = fx.history.create_job(&art, &snapshot()).unwrap();
        let receipt =
            crate::app::types::UploadReceipt::new("https://vectr.example/x", "x.png").unwrap();
        fx.history
            .transition(job.id, crate::persistence::history::JobStatus::Running)
            .unwrap();
        let session = uuid::Uuid::new_v4();
        fx.history.set_session(job.id, session).unwrap();
        fx.history
            .record_receipt(job.id, 1, Some(session), &receipt)
            .unwrap();
        let history_id = fx.history.browse(&Default::default()).unwrap().records[0].id;
        fx.history.set_favorite(history_id, true).unwrap();

        assert_eq!(
            delete_local(&fx.history, art.id).unwrap(),
            LocalDeleteOutcome::Deleted
        );
        assert!(!exists(&entry.path), "the local copy must be gone");
        let record = fx.history.history_record(history_id).unwrap().unwrap();
        assert!(record.favorite, "favorite is preserved after local removal");
        assert!(
            matches!(
                record.job.unwrap().status,
                crate::persistence::history::JobStatus::Completed(_)
            ),
            "the confirmed upload link is preserved after local removal"
        );
    }

    #[test]
    fn explicit_delete_is_blocked_by_an_active_job() {
        let fx = Fx::new();
        let archive = fx.archive();
        let art = fx.screenshot();
        let entry = archive.save_finalized(&art, captured()).unwrap();
        fx.history.create_job(&art, &snapshot()).unwrap();

        assert_eq!(
            delete_local(&fx.history, art.id).unwrap(),
            LocalDeleteOutcome::Active
        );
        assert!(
            exists(&entry.path),
            "a source needed by an unfinished job is kept"
        );
    }

    #[test]
    fn explicit_delete_never_touches_a_file_that_was_never_archived() {
        let fx = Fx::new();
        let art = fx.screenshot(); // staged, but never archived
        assert_eq!(
            delete_local(&fx.history, art.id).unwrap(),
            LocalDeleteOutcome::NoOwnedCopy
        );
        assert!(exists(&art.path), "the staged spool copy is untouched");
    }

    #[test]
    fn explicit_delete_is_safe_when_the_recorded_object_was_swapped() {
        for filesystem in ["FAT", "ReFS", "NTFS"] {
            winfs::identity_queries::with(filesystem, false, || {
                let fx = Fx::new();
                let archive = fx.archive();
                let art = fx.screenshot();
                let entry = archive.save_finalized(&art, captured()).unwrap();
                let bytes = fs::read(&entry.path).unwrap();
                fs::remove_file(&entry.path).unwrap();
                fs::write(&entry.path, &bytes).unwrap(); // same name/bytes, a different file object

                assert_eq!(
                    delete_local(&fx.history, art.id).unwrap(),
                    LocalDeleteOutcome::NoOwnedCopy,
                    "{filesystem}: an unproven/foreign object is never deleted"
                );
                assert!(exists(&entry.path));
            });
        }
    }
}
