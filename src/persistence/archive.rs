use std::fs;
use std::io;
use std::iter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Local, TimeDelta};
use uuid::Uuid;

use crate::app::error::AppError;
use crate::app::types::{Artifact, ArtifactId, ArtifactKind};
use crate::persistence::filename::{persist_new, random_suffix, validate_file_name};
use crate::persistence::history::{History, ReleaseRule};
use crate::persistence::settings::{ArchivePolicy, StorageMode};
use crate::persistence::{
    PARTIAL_PREFIX, copy_hashing, ensure_free_space, fingerprint, local_from_ms, now_ms,
    open_no_follow, remove_owned_file,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArchiveEntryId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveOrigin {
    /// Created by the storage policy; eligible for policy cleanup when unchanged.
    Automatic,
    /// Explicit user Save; never removed automatically.
    Manual,
}

/// Ownership record for one archive file the app created.
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
    pub kept_modified: usize,
    pub kept_missing: usize,
    pub kept_other_root: usize,
}

/// Rows fetched per release query; bounds memory and connection-lock time.
const RELEASE_PAGE: usize = 64;

/// User-visible dated archive (`<root>/YYYY-MM/<name>`). Independent of the
/// upload spool: editing, moving, or deleting archive files never affects
/// queued uploads, and cleanup only touches unchanged files it recorded.
pub struct ArchiveStore {
    history: Arc<History>,
    root: PathBuf,
}

impl ArchiveStore {
    pub fn new(history: Arc<History>, root: PathBuf) -> Self {
        ArchiveStore { history, root }
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

    /// Walks eligible entries in bounded `(created_ms, id)` keyset pages. A
    /// page only nominates candidates; eligibility is rechecked at commit.
    fn release(&self, root: &Path, rule: ReleaseRule) -> Result<CleanupReport, AppError> {
        let mut report = CleanupReport::default();
        let root_text = root.to_str().ok_or(AppError::InvalidInput(
            "archive path must be valid Unicode",
        ))?;
        let mut after = None;
        loop {
            let page = self
                .history
                .archive_page(root_text, rule, after, RELEASE_PAGE)?;
            let Some(last) = page.last() else {
                return Ok(report);
            };
            after = Some((last.created_ms, last.entry.id));
            for candidate in &page {
                self.remove_if_unchanged(&candidate.entry, root, rule, &mut report)?;
            }
        }
    }

    fn save(
        &self,
        source: &Artifact,
        captured_at: DateTime<Local>,
        origin: ArchiveOrigin,
    ) -> Result<ArchiveEntry, AppError> {
        const STAGE: &str = "writing the archive copy";
        if source.kind == ArtifactKind::File {
            return Err(AppError::InvalidInput(
                "only finalized captures and recordings are archived",
            ));
        }
        if !self.root.is_absolute() {
            return Err(AppError::InvalidInput(
                "archive folder must be an absolute path",
            ));
        }
        validate_file_name(&source.display_name)?;
        let month_dir = self.root.join(captured_at.format("%Y-%m").to_string());
        fs::create_dir_all(&month_dir)
            .map_err(|e| AppError::io("creating the archive folder", &e))?;
        ensure_free_space(&month_dir, source.size)?;

        let mut src = open_no_follow(&source.path)
            .map_err(|e| AppError::io("opening the staged file", &e))?;
        let mut tmp = tempfile::Builder::new()
            .prefix(PARTIAL_PREFIX)
            .tempfile_in(&month_dir)
            .map_err(|e| AppError::io(STAGE, &e))?;
        let (size, sha256) = copy_hashing(&mut src, tmp.as_file_mut(), source.size)
            .map_err(|e| AppError::io(STAGE, &e))?;
        if size != source.size || sha256 != source.sha256 {
            return Err(AppError::ArtifactChanged);
        }
        tmp.as_file()
            .sync_all()
            .map_err(|e| AppError::io(STAGE, &e))?;

        let name = source.display_name.as_str();
        let (stem, ext) = name
            .rsplit_once('.')
            .map_or((name, None), |(s, e)| (s, Some(e)));
        let mut candidates = iter::once(Ok(name.to_owned())).chain(iter::repeat_with(|| {
            Ok(match ext {
                Some(ext) => format!("{stem}_{}.{ext}", random_suffix()),
                None => format!("{stem}_{}", random_suffix()),
            })
        }));
        let (_, path) = persist_new(tmp, &month_dir, &mut candidates)?;

        let created_ms = now_ms();
        let entry = ArchiveEntry {
            id: ArchiveEntryId(Uuid::new_v4()),
            artifact_id: source.id,
            root: self.root.clone(),
            path,
            origin,
            created_at: local_from_ms(created_ms)?,
            size,
            sha256,
        };
        if let Err(e) = self.history.record_archive(source, &entry, created_ms) {
            // Never leave a file whose ownership was not recorded.
            let _ = remove_owned_file(&entry.path);
            return Err(e);
        }
        Ok(entry)
    }

    fn remove_if_unchanged(
        &self,
        entry: &ArchiveEntry,
        root: &Path,
        rule: ReleaseRule,
        report: &mut CleanupReport,
    ) -> Result<(), AppError> {
        let inside_root = entry.path.parent().and_then(Path::parent) == Some(root);
        if entry.root != root || !inside_root {
            report.kept_other_root += 1;
            return Ok(());
        }
        // ponytail: check-then-delete window; a handle-based delete would close it.
        match fingerprint(&entry.path) {
            Ok((size, hash)) if size == entry.size && hash == entry.sha256 => {}
            Ok(_) => {
                report.kept_modified += 1;
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                report.kept_missing += 1;
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                report.kept_modified += 1;
                return Ok(());
            }
            Err(e) => return Err(AppError::io("checking an archive file", &e)),
        }
        crate::persistence::hook("cleanup.verified");
        let removed = self
            .history
            .commit_archive_removal(entry.id, rule.require_uploaded, || {
                remove_owned_file(&entry.path)
                    .map_err(|e| AppError::io("removing an archive file", &e))
            })?;
        if removed {
            report.removed += 1;
        } else {
            report.kept_active += 1;
        }
        Ok(())
    }
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
    use crate::persistence::artifacts::ArtifactStore;
    use crate::persistence::settings::ServerOrigin;
    use crate::persistence::test_support::{clear_hook, set_hook};

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
            Fx { dir, store, history, root, n: Default::default() }
        }

        fn archive(&self) -> ArchiveStore {
            ArchiveStore::new(self.history.clone(), self.root.clone())
        }

        fn screenshot(&self) -> Artifact {
            self.n.set(self.n.get() + 1);
            let name = format!("VectrCap_2026-11-14_08-03-09_0000000{}.png", self.n.get());
            let work = self.dir.path().join(&name);
            fs::write(&work, format!("png {}", self.n.get())).unwrap();
            self.store.stage_file(&work, ArtifactKind::Screenshot).unwrap()
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
        Local.with_ymd_and_hms(2026, 11, 14, 8, 3, 9).single().unwrap()
    }

    fn later() -> DateTime<Local> {
        Local::now() + TimeDelta::days(10)
    }

    fn snapshot() -> DestinationSnapshot {
        let mut d = Destinations::default();
        let id = d
            .add("Phone", ServerOrigin::parse("https://vectr.example").unwrap(), None)
            .unwrap();
        d.bind_credential(id, &HashSet::new()).unwrap();
        resolve_destination(DestinationSelection::Inherit, DestinationSelection::Inherit, &d)
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
        assert!(exists(&entry.path), "a source needed by an unfinished job is kept");
        assert_eq!((report.removed, report.kept_active), (0, 1));
    }
}
