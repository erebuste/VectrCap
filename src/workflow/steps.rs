use super::profile::{CaptureTask, UploadTask};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStep {
    Finalize,
    Capture(CaptureTask),
    AfterUpload(UploadTask),
}
impl TaskStep {
    pub fn label(self) -> &'static str {
        match self {
            Self::Finalize => "Finalize output",
            Self::Capture(t) => t.label(),
            Self::AfterUpload(t) => t.label(),
        }
    }
    pub fn can_skip(self) -> bool {
        !matches!(
            self,
            Self::Finalize
                | Self::Capture(
                    CaptureTask::Review | CaptureTask::BeforeUploadReview | CaptureTask::QuickMenu
                )
        )
    }

    pub fn interactive(self) -> bool {
        matches!(
            self,
            Self::Capture(
                CaptureTask::QuickMenu
                    | CaptureTask::Review
                    | CaptureTask::SaveAs
                    | CaptureTask::BeforeUploadReview
                    | CaptureTask::Pin
            ) | Self::AfterUpload(UploadTask::OpenUrl | UploadTask::ShowResult)
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepOutcome {
    Pending,
    Running,
    Succeeded,
    Failed(String),
    Skipped(String),
    NeedsUserAction(String),
}
impl StepOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Succeeded => "Succeeded",
            Self::Failed(_) => "Failed",
            Self::Skipped(_) => "Skipped",
            Self::NeedsUserAction(_) => "Needs your action",
        }
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Failed(s) | Self::Skipped(s) | Self::NeedsUserAction(s) => Some(s),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepState {
    pub step: TaskStep,
    pub outcome: StepOutcome,
}
