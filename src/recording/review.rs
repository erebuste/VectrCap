//! Finalized recording ownership and the shared capture archive policy.
use super::{EncodedRecording, options::*};
use crate::{
    app::{
        error::AppError,
        types::{Artifact, ArtifactId, ArtifactKind},
    },
    persistence::{
        archive::{ArchiveEntry, ArchiveStore, LocalDeleteOutcome, delete_local},
        artifacts::ArtifactStore,
        filename::{FilenamePreset, OutputFormat, random_suffix},
        history::History,
        settings::ArchivePolicy,
    },
};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingMetadata {
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub active_millis: u64,
    pub size: u64,
    pub audio: AudioMode,
}
impl RecordingMetadata {
    pub fn validate(&self) -> Result<(), AppError> {
        if ![10, 15, 20, 30, 60].contains(&self.fps)
            || self.active_millis == 0
            || self.active_millis > 3_600_000
            || self.size == 0
        {
            return Err(AppError::InvalidInput(
                "invalid finalized recording metadata",
            ));
        }
        crate::capture::geometry::PhysicalRect::new(0, 0, self.width, self.height)?.rgba_len()?;
        Ok(())
    }
}
pub struct RecordingReview {
    output: EncodedRecording,
    artifact: Option<Artifact>,
    archive: Option<ArchiveEntry>,
    created: chrono::DateTime<chrono::Local>,
    continued: bool,
    archive_failed: bool,
    history_record_failed: bool,
    admitted: bool,
    metadata: RecordingMetadata,
}
impl RecordingReview {
    pub fn new(output: EncodedRecording) -> Self {
        let size = std::fs::metadata(&output.path).map_or(0, |m| m.len());
        let metadata = RecordingMetadata {
            fps: output.options.fps(),
            width: output.width,
            height: output.height,
            active_millis: output
                .active_duration
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            size,
            audio: output.options.audio().map_or(AudioMode::Off, |a| a.mode),
        };
        Self {
            output,
            artifact: None,
            archive: None,
            created: chrono::Local::now(),
            continued: false,
            archive_failed: false,
            history_record_failed: false,
            admitted: false,
            metadata,
        }
    }
    pub fn source_path(&self) -> &Path {
        &self.output.path
    }
    pub fn archive_path(&self) -> Option<&Path> {
        self.archive.as_ref().map(|a| a.path.as_path())
    }
    pub fn artifact(&self) -> Option<&Artifact> {
        self.artifact.as_ref()
    }
    pub fn metadata(&self) -> &RecordingMetadata {
        &self.metadata
    }
    pub fn archive_failed(&self) -> bool {
        self.archive_failed
    }
    /// True if the archive file is saved but its History/metadata row is not
    /// (so it may not surface in History browsing or recording-metadata lookups).
    pub fn history_record_failed(&self) -> bool {
        self.history_record_failed
    }
    pub fn admitted(&self) -> bool {
        self.admitted
    }
    pub fn mark_admitted(&mut self) {
        self.admitted = true;
    }
    pub fn continue_without_archive(&mut self) -> Result<(), AppError> {
        if !self.archive_failed {
            return Err(AppError::InvalidInput(
                "continue requires a failed automatic archive",
            ));
        }
        self.continued = true;
        self.archive_failed = false;
        Ok(())
    }
    pub fn persist(
        &mut self,
        artifacts: &ArtifactStore,
        history: Arc<History>,
        policy: &ArchivePolicy,
        filename: &FilenamePreset,
        upload: bool,
        manual: bool,
    ) -> Result<(), AppError> {
        policy.validate()?;
        self.metadata.validate()?;
        if self.artifact.is_none() {
            let kind = if matches!(self.output.options.format, FormatOptions::Gif(_)) {
                ArtifactKind::Gif
            } else {
                ArtifactKind::Mp4
            };
            let mut a = artifacts.stage_file(&self.output.path, kind)?;
            let format = if kind == ArtifactKind::Gif {
                OutputFormat::Gif
            } else {
                OutputFormat::Mp4
            };
            a.display_name = filename.render(self.created, &random_suffix(), format)?;
            self.artifact = Some(a);
        }
        let a = self.artifact.as_ref().expect("staged recording");
        if self.archive.is_none()
            && (manual || (!self.continued && policy.should_auto_archive(upload)))
        {
            let result = policy.resolved_root().and_then(|root| {
                let store = ArchiveStore::new(history.clone(), root, filename.clone());
                if manual {
                    store.save_manual(a, self.created)
                } else {
                    store.save_finalized(a, self.created)
                }
            });
            match result {
                Ok(entry) => {
                    self.archive = Some(entry);
                    self.archive_failed = false;
                }
                Err(e) => {
                    self.archive_failed = !manual;
                    return Err(e);
                }
            }
        }
        // protect_manual_archive flips the archive row's origin to Manual, which is
        // what excludes it from automatic age-based cleanup and upload reconciliation
        // (see ARCHIVE_PAGE_SQL's origin='automatic' filter). A failure here must keep
        // propagating: swallowing it would report "saved" for a file whose row is
        // still 'automatic' and so remains eligible for deletion by a later cleanup
        // pass, despite the user's explicit Save never actually having protected it.
        if manual && let Some(entry) = &mut self.archive {
            history.protect_manual_archive(entry.id)?;
            entry.origin = crate::persistence::archive::ArchiveOrigin::Manual;
        }
        // record_recording only writes the History/metadata row, not the archive
        // origin; a failure here does not expose the file to cleanup, so it does not
        // need to block an already-successful (and already-protected, if applicable)
        // save. It is still surfaced via history_record_failed rather than silently
        // dropped, so the caller does not present the save as fully successful.
        self.history_record_failed = history
            .record_recording(a, &self.metadata, self.created.timestamp_millis())
            .inspect_err(|error| {
                eprintln!("recording history bookkeeping after successful save failed: {error}")
            })
            .is_err();
        Ok(())
    }
    pub fn discard(
        &mut self,
        artifacts: &ArtifactStore,
        history: &History,
    ) -> Result<(), AppError> {
        if let Some(a) = &self.artifact {
            if history.artifacts_needing_spool()?.contains(&a.id) {
                return Err(AppError::InUse(
                    "cancel the recording upload before discarding its source",
                ));
            }
            if delete_local(history, a.id)? == LocalDeleteOutcome::Active {
                return Err(AppError::InUse("recording is required by an upload"));
            }
            if a.path.exists() {
                artifacts.discard_staged(a)?;
            }
            self.archive = None;
            self.artifact = None;
        }
        Ok(())
    }
    pub fn id(&self) -> Option<ArtifactId> {
        self.artifact.as_ref().map(|a| a.id)
    }
}
