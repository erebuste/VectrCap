use crate::{
    app::{
        destinations::DestinationSelection, error::AppError, shortcuts::ShortcutAction,
        types::ArtifactKind,
    },
    persistence::settings::{ArchivePolicy, Settings, StorageMode},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CaptureTask {
    QuickMenu,
    Review,
    SaveFinal,
    SaveAs,
    CopyImage,
    CopyFile,
    CopyFilePath,
    CopyFolderPath,
    Reveal,
    Pin,
    BeforeUploadReview,
    Upload,
}
impl CaptureTask {
    pub const ALL: [Self; 12] = [
        Self::QuickMenu,
        Self::Review,
        Self::SaveFinal,
        Self::SaveAs,
        Self::CopyImage,
        Self::CopyFile,
        Self::CopyFilePath,
        Self::CopyFolderPath,
        Self::Reveal,
        Self::Pin,
        Self::BeforeUploadReview,
        Self::Upload,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::QuickMenu => "Show quick task menu",
            Self::Review => "Show review/editor",
            Self::SaveFinal => "Save final output",
            Self::SaveAs => "Save as",
            Self::CopyImage => "Copy image",
            Self::CopyFile => "Copy file",
            Self::CopyFilePath => "Copy file path",
            Self::CopyFolderPath => "Copy folder path",
            Self::Reveal => "Show in Explorer",
            Self::Pin => "Pin",
            Self::BeforeUploadReview => "Show before-upload review",
            Self::Upload => "Upload to Vectr",
        }
    }
    pub fn unavailable_reason(self, kind: ArtifactKind) -> Option<&'static str> {
        match (self, kind) {
            (
                Self::CopyImage | Self::Pin,
                ArtifactKind::Gif | ArtifactKind::Mp4 | ArtifactKind::File,
            ) => Some("This action requires a still image"),
            (
                Self::QuickMenu | Self::Review | Self::SaveFinal | Self::SaveAs,
                ArtifactKind::File,
            ) => Some("Existing files bypass capture and editing tasks"),
            _ => None,
        }
    }
    pub fn needs_durable(self) -> bool {
        matches!(
            self,
            Self::CopyFile | Self::CopyFilePath | Self::CopyFolderPath | Self::Reveal
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UploadTask {
    CopyUrl,
    OpenUrl,
    ShowResult,
}
impl UploadTask {
    pub const ALL: [Self; 3] = [Self::CopyUrl, Self::OpenUrl, Self::ShowResult];
    pub fn label(self) -> &'static str {
        match self {
            Self::CopyUrl => "Copy URL",
            Self::OpenUrl => "Open URL",
            Self::ShowResult => "Show result window",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileScope {
    #[default]
    Captures,
    Files,
    All,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskProfile {
    pub capture: BTreeSet<CaptureTask>,
    pub after_upload: BTreeSet<UploadTask>,
    pub before_upload_review: bool,
    pub scope: ProfileScope,
    pub destination: DestinationSelection,
}
impl Default for TaskProfile {
    fn default() -> Self {
        Self {
            capture: BTreeSet::from([CaptureTask::Review]),
            after_upload: BTreeSet::from([UploadTask::CopyUrl]),
            before_upload_review: false,
            scope: ProfileScope::Captures,
            destination: DestinationSelection::Inherit,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskProfiles {
    pub global: TaskProfile,
    pub overrides: BTreeMap<ShortcutAction, TaskProfile>,
}
impl TaskProfiles {
    pub fn save_final(&self, archive: &ArchivePolicy) -> bool {
        archive.storage_mode == StorageMode::AlwaysSave
    }
    pub fn set_save_final(&mut self, archive: &mut ArchivePolicy, enabled: bool) {
        archive.storage_mode = if enabled {
            StorageMode::AlwaysSave
        } else {
            StorageMode::NeverAutomatic
        };
    }
    pub fn validate(&self, settings: &Settings) -> Result<(), AppError> {
        for p in std::iter::once(&self.global).chain(self.overrides.values()) {
            if p.capture.contains(&CaptureTask::SaveFinal) {
                return Err(AppError::InvalidInput(
                    "Save final output uses the archive storage policy",
                ));
            }
            if let DestinationSelection::Specific(id) = p.destination
                && settings.destinations.get(id).is_none()
            {
                return Err(AppError::InvalidInput(
                    "repair the task profile destination reference",
                ));
            }
        }
        if self.overrides.keys().any(|a| !a.supports_task_profile()) {
            return Err(AppError::InvalidInput(
                "task profile requires a capture or upload action",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveProfile {
    pub profile: TaskProfile,
    pub archive: ArchivePolicy,
}
impl EffectiveProfile {
    pub fn snapshot(settings: &Settings, action: Option<ShortcutAction>) -> Self {
        Self {
            profile: action
                .and_then(|a| settings.tasks.overrides.get(&a))
                .unwrap_or(&settings.tasks.global)
                .clone(),
            archive: settings.archive.clone(),
        }
    }
}
