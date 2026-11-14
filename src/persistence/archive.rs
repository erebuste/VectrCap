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
use crate::persistence::history::History;
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed: usize,
    pub kept_active: usize,
    pub kept_manual: usize,
    pub kept_modified: usize,
    pub kept_missing: usize,
    pub kept_other_root: usize,
}

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
        let mut report = CleanupReport::default();
        let Some(days) = policy.cleanup_age_days else {
            return Ok(report);
        };
        let root = policy.resolved_root()?;
        let cutoff = now - TimeDelta::days(i64::from(days));
        for candidate in self.history.archive_candidates(cutoff.timestamp_millis())? {
            if candidate.entry.origin == ArchiveOrigin::Manual {
                report.kept_manual += 1;
            } else if candidate.active {
                report.kept_active += 1;
            } else {
                self.remove_if_unchanged(&candidate.entry, &root, &mut report)?;
            }
        }
        Ok(report)
    }

    /// Keep-pending/failed mode: releases unchanged automatic copies whose
    /// uploads are confirmed and that no unfinished job still references.
    pub fn reconcile_uploaded(&self, policy: &ArchivePolicy) -> Result<CleanupReport, AppError> {
        let mut report = CleanupReport::default();
        if policy.storage_mode != StorageMode::PendingOrFailed {
            return Ok(report);
        }
        let root = policy.resolved_root()?;
        for candidate in self.history.archive_candidates(i64::MAX)? {
            if candidate.entry.origin == ArchiveOrigin::Manual {
                report.kept_manual += 1;
            } else if candidate.active || !candidate.uploaded {
                report.kept_active += 1;
            } else {
                self.remove_if_unchanged(&candidate.entry, &root, &mut report)?;
            }
        }
        Ok(report)
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
        remove_owned_file(&entry.path).map_err(|e| AppError::io("removing an archive file", &e))?;
        self.history.mark_archive_removed(entry.id)?;
        report.removed += 1;
        Ok(())
    }
}
