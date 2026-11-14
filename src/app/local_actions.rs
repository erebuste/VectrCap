//! Cancellable shell operations run off the controller's Tokio thread.
use super::{controller::AppController, error::AppError, events::HistoryAction};
use crate::{
    persistence::history::{
        History, JobStatus,
        browser::{open_local_file, verified_local_cancellable},
    },
    platform::windows::shell_actions::ShellActions,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
pub struct LocalHistoryAction {
    pub(super) id: uuid::Uuid,
    pub(super) action: HistoryAction,
    pub(super) history: Arc<History>,
    pub(super) shell: Arc<dyn ShellActions>,
}
impl LocalHistoryAction {
    pub fn run(self, cancel: CancellationToken) -> Result<(), AppError> {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let row = self
            .history
            .history_record(self.id)?
            .ok_or(AppError::InvalidInput("history item no longer available"))?;
        match self.action {
            HistoryAction::OpenLocal => {
                let extension = AppController::viewer_extension(&row.artifact).ok_or(
                    AppError::InvalidInput("this file can only be revealed in its folder"),
                )?;
                let _guard = verified_local_cancellable(&row.artifact, u64::MAX, &cancel)?;
                let current = self
                    .history
                    .history_record(self.id)?
                    .ok_or(AppError::InvalidInput("history item no longer available"))?;
                if current.artifact != row.artifact {
                    return Err(AppError::ArtifactChanged);
                }
                if cancel.is_cancelled() {
                    return Err(AppError::Cancelled);
                }
                self.shell.open_document(&row.artifact.path, extension)?;
            }
            HistoryAction::ShowFolder => {
                let _guard = open_local_file(&row.artifact.path)?;
                if cancel.is_cancelled() {
                    return Err(AppError::Cancelled);
                }
                self.shell.reveal_path(&row.artifact.path)?;
            }
            HistoryAction::OpenLink => {
                let Some(job) = row.job else {
                    return Err(AppError::InvalidInput("no confirmed link"));
                };
                let JobStatus::Completed(receipt) = job.status else {
                    return Err(AppError::InvalidInput("no confirmed link"));
                };
                self.shell.open_url(receipt.url())?;
            }
            _ => return Err(AppError::InvalidInput("unsupported local history action")),
        }
        Ok(())
    }
}
