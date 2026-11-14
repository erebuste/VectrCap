//! Typed shell boundary. Secret-bearing commands deliberately implement neither
//! Debug nor serialization. OS adapters cannot submit queue settlement events.
use crate::{
    app::{
        destination_manager::DestinationEdit,
        destinations::{DestinationId, DestinationSelection},
        error::AppError,
        types::Artifact,
    },
    persistence::settings::Settings,
    vectr::{
        client::UploadToken,
        queue::{QueueCommand, QueueEffect},
        uploader_info::UploaderInfoSupport,
    },
};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Settings,
    Uploads,
    BatchSummary,
    History,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Running,
    Draining,
    Exited,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HistoryAction {
    OpenLocal,
    ShowFolder,
    OpenLink,
    CopyLinks,
    RetryFailed,
    Remove,
}
pub enum AppCommand {
    SetHistoryPreferences(crate::persistence::settings::HistoryPreferences),
    HistoryAction {
        ids: Vec<Uuid>,
        action: HistoryAction,
    },
    Show(Window),
    CloseWindow,
    QueryHistory(crate::persistence::history::browser::HistoryQuery),
    SetHistoryFavorite {
        id: Uuid,
        favorite: bool,
    },
    ShowBatchSummary(Vec<crate::app::types::JobId>),
    ApplyDestinations {
        edit: DestinationEdit,
        revision: u64,
    },
    SaveConnection {
        request: Uuid,
        edit: DestinationEdit,
        revision: u64,
    },
    SetDefault {
        id: DestinationId,
        revision: u64,
    },
    BeginWorkflow {
        id: Uuid,
        selection: DestinationSelection,
    },
    AcceptWorkflow {
        id: Uuid,
        artifact: Artifact,
    },
    ReleaseWorkflow(Uuid),
    Cleanup,
    Queue(QueueAction),
    TestConnection {
        request: Uuid,
        origin: String,
        token: UploadToken,
    },
    TestSavedConnection {
        request: Uuid,
        id: DestinationId,
        revision: u64,
    },
    CancelConnectionTest,
    DismissNotice(crate::app::notifications::NoticeId),
    SetNotificationsEnabled(bool),
    ExecuteCompletionAction {
        notice: crate::app::notifications::NoticeId,
        action: crate::app::notifications::RequestedCompletionAction,
    },
    /// A confirmed native file-picker selection (1 to 100 paths). The
    /// destination default is resolved exactly once, here, before any file
    /// is staged, and reused for every file in the batch regardless of how
    /// long staging takes or whether the default changes meanwhile.
    AcceptFileSelection(Vec<PathBuf>),
    Quit,
}
#[derive(Debug, Clone, Copy)]
pub enum QueueAction {
    PauseAll,
    ResumeAll,
    RetryEligible,
    Pause(crate::app::types::JobId),
    Resume(crate::app::types::JobId),
    Cancel(crate::app::types::JobId),
}
impl From<QueueAction> for QueueCommand {
    fn from(action: QueueAction) -> Self {
        match action {
            QueueAction::PauseAll => Self::PauseAll,
            QueueAction::ResumeAll => Self::ResumeAll,
            QueueAction::RetryEligible => Self::RetryEligible,
            QueueAction::Pause(id) => Self::PauseJob(id),
            QueueAction::Resume(id) => Self::ResumeJob(id),
            QueueAction::Cancel(id) => Self::Cancel(id),
        }
    }
}
pub enum AppEvent {
    Queue(QueueEffect),
    WorkerStopped(Result<(), AppError>),
    ConnectionTest {
        request: Uuid,
        result: Result<UploaderInfoSupport, AppError>,
    },
    /// One file from an accepted batch finished staging (or failed to).
    FileStaged {
        batch: Uuid,
        display_name: String,
        outcome: Result<Artifact, AppError>,
    },
}
pub enum AppEffect {
    LocalHistoryAction(super::local_actions::LocalHistoryAction),
    HistoryRetryReport {
        queued: usize,
        skipped: usize,
    },
    Window(Option<Window>),
    SettingsSaved(Settings),
    ConnectionSaved {
        request: Uuid,
        result: Result<Settings, AppError>,
    },
    WorkflowAccepted(Uuid),
    JobAccepted(crate::app::types::JobId),
    Queue(QueueCommand),
    TestConnection {
        request: Uuid,
        origin: crate::persistence::settings::ServerOrigin,
        token: UploadToken,
    },
    ConnectionTest {
        request: Uuid,
        result: Result<UploaderInfoSupport, AppError>,
    },
    RefreshQueue,
    DrainWorker,
    CancelConnectionTest,
    Exit,
    Error(AppError),
    /// Begin bounded off-thread staging for every path in an accepted batch.
    StageFiles {
        batch: Uuid,
        paths: Vec<PathBuf>,
    },
    /// One file in a batch could not be staged; its sibling files are
    /// unaffected. No job was created and no original file was touched.
    FileRejected {
        display_name: String,
        error: AppError,
    },
}
