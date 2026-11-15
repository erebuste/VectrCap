//! Recoverable persistence for one explicit finish. Only prepared encoded bytes
//! reach disk; Copy/Pin consumers receive the exact decoded encoded result.
use super::{error::AppError, types::Artifact};
use crate::{
    persistence::{
        archive::{ArchiveEntry, ArchiveStore},
        artifacts::ArtifactStore,
        filename::{FilenamePreset, random_suffix, validate_file_name},
        history::History,
        settings::ArchivePolicy,
        winfs,
    },
    ui::{capture_overlay::OverlayAction, image_export::ImageFinish},
};
use chrono::{DateTime, Local};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ArchiveDecision {
    #[default]
    Retry,
    ContinueWithoutLocalCopy,
}

/// Startup recovery runs in the desktop's bounded worker set. Its classified
/// failures are surfaced through the same error path as other local actions.
pub struct ArchiveMaintenance {
    history: Arc<History>,
    policy: ArchivePolicy,
    filename: FilenamePreset,
}
impl ArchiveMaintenance {
    pub fn new(history: Arc<History>, policy: ArchivePolicy, filename: FilenamePreset) -> Self {
        Self {
            history,
            policy,
            filename,
        }
    }
    pub fn run(self, cancel: tokio_util::sync::CancellationToken) -> Result<(), AppError> {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        self.policy.validate()?;
        let archive = ArchiveStore::new(self.history, self.policy.resolved_root()?, self.filename);
        archive.recover_partials()?;
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        archive.reconcile_uploaded(&self.policy)?;
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        archive.cleanup(&self.policy, Local::now())?;
        Ok(())
    }
}

pub struct ImageFinishTransaction {
    finish: ImageFinish,
    captured_at: DateTime<Local>,
    artifact: Option<Artifact>,
    archive: Option<ArchiveEntry>,
    saved_path: Option<PathBuf>,
    archive_failed: bool,
    continued: bool,
    archive_required: Option<bool>,
}
impl ImageFinishTransaction {
    pub fn new(finish: ImageFinish) -> Result<Self, AppError> {
        if !matches!(
            finish.action,
            OverlayAction::Upload | OverlayAction::Copy | OverlayAction::Save | OverlayAction::Pin
        ) {
            return Err(AppError::InvalidInput("choose an image finish action"));
        }
        Ok(Self {
            finish,
            captured_at: Local::now(),
            artifact: None,
            archive: None,
            saved_path: None,
            archive_failed: false,
            continued: false,
            archive_required: None,
        })
    }
    pub fn finish(&self) -> &ImageFinish {
        &self.finish
    }
    pub fn artifact(&self) -> Option<&Artifact> {
        self.artifact.as_ref()
    }
    pub fn archive(&self) -> Option<&ArchiveEntry> {
        self.archive.as_ref()
    }
    pub fn archive_failed(&self) -> bool {
        self.archive_failed
    }
    pub fn saved_path(&self) -> Option<&Path> {
        self.saved_path.as_deref()
    }

    /// Retry may use an explicitly changed policy/folder. Continue is accepted
    /// only after this transaction actually encountered an automatic archive error.
    pub fn persist(
        &mut self,
        artifacts: &ArtifactStore,
        history: Arc<History>,
        policy: &ArchivePolicy,
        filename: &FilenamePreset,
        save_path: Option<&Path>,
        decision: ArchiveDecision,
    ) -> Result<(), AppError> {
        policy.validate()?;
        if decision == ArchiveDecision::ContinueWithoutLocalCopy {
            if !self.archive_failed || self.finish.action == OverlayAction::Save {
                return Err(AppError::InvalidInput(
                    "continue requires a failed automatic archive",
                ));
            }
            self.continued = true;
            self.archive_failed = false;
        }
        if self.finish.action == OverlayAction::Save {
            if self.saved_path.is_none() {
                let path = save_path.ok_or(AppError::InvalidInput("choose a Save destination"))?;
                save_image_as(&self.finish.prepared, path)?;
                self.saved_path = Some(path.to_owned());
            }
            return Ok(());
        }
        let upload = self.finish.action == OverlayAction::Upload;
        let archive = *self
            .archive_required
            .get_or_insert_with(|| policy.should_auto_archive(upload))
            && !self.continued;
        if (upload || archive) && self.artifact.is_none() {
            let name = filename.render(
                self.captured_at,
                &random_suffix(),
                self.finish.prepared.format().output_format(),
            )?;
            match artifacts.stage_image(&self.finish.prepared, name) {
                Ok(artifact) => self.artifact = Some(artifact),
                Err(error) => {
                    // Copy/Pin need this spool only to create the archive. A
                    // failed local-copy prerequisite may be explicitly skipped;
                    // an upload's immutable source may never be skipped.
                    self.archive_failed = !upload;
                    return Err(error);
                }
            }
        }
        if archive && self.archive.is_none() {
            let result = policy.resolved_root().and_then(|root| {
                ArchiveStore::new(history, root, filename.clone()).save_finalized(
                    self.artifact.as_ref().expect("archive source staged"),
                    self.captured_at,
                )
            });
            match result {
                Ok(entry) => {
                    self.archive = Some(entry);
                    self.archive_failed = false;
                }
                Err(error) => {
                    self.archive_failed = true;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Call only while no job references this spool. Non-upload finishes and
    /// cancellation release transient staging, leaving any recorded archive intact.
    pub fn discard_unadmitted(&mut self, artifacts: &ArtifactStore) -> Result<(), AppError> {
        if let Some(artifact) = &self.artifact {
            artifacts.discard_staged(artifact)?;
            self.artifact = None;
        }
        Ok(())
    }
}

/// Save exactly the selected path, atomically and without overwrite. The parent
/// is bound by handle; publication and rollback operate on the created object.
fn save_image_as(
    prepared: &crate::editor::export::PreparedImageExport,
    path: &Path,
) -> Result<(), AppError> {
    if !path.is_absolute() {
        return Err(AppError::InvalidInput(
            "Save requires an absolute destination",
        ));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(AppError::InvalidInput("Save name must be Unicode"))?;
    validate_file_name(name)?;
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_none_or(|e| !e.eq_ignore_ascii_case(prepared.extension()))
    {
        return Err(AppError::InvalidInput(
            "Save extension must match the selected image format",
        ));
    }
    let parent = path
        .parent()
        .ok_or(AppError::InvalidInput("Save parent is missing"))?;
    const STAGE: &str = "saving the finalized image";
    let root = winfs::open_root(parent).map_err(|e| AppError::io(STAGE, &e))?;
    crate::persistence::ensure_free_space(parent, prepared.encoded_size() as u64)?;
    let partial = format!(".vectrcap-partial-{}", uuid::Uuid::new_v4().simple());
    let file = winfs::create_child_file(&root, &partial).map_err(|e| AppError::io(STAGE, &e))?;
    let result = (|| {
        (&file)
            .write_all(prepared.bytes())
            .map_err(|e| AppError::io(STAGE, &e))?;
        file.sync_all().map_err(|e| AppError::io(STAGE, &e))?;
        winfs::rename_in_place(&file, name).map_err(|e| AppError::io(STAGE, &e))
    })();
    if result.is_err() {
        let _ = winfs::delete_by_handle(&file);
    }
    result
}
