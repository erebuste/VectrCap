//! Typed shell boundary. Secret-bearing commands deliberately implement neither
//! Debug nor serialization. OS adapters cannot submit queue settlement events.
use crate::{
    app::{
        destination_manager::DestinationEdit,
        destinations::{DestinationId, DestinationSelection},
        error::AppError,
        shortcuts::{ShortcutAction, ShortcutBinding},
        types::Artifact,
    },
    persistence::settings::{AppearancePreferences, ArchivePolicy, Settings, StartupPreference},
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
    Tasks,
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
    Pin,
    OpenLocal,
    ShowFolder,
    OpenLink,
    CopyLinks,
    RetryFailed,
    Remove,
    /// Deletes only the app-owned local archive copy (see `archive::delete_local`).
    /// Distinct from `Remove`: the history record, favorite, and any
    /// confirmed upload link are untouched.
    DeleteLocalFile,
}
pub enum AppCommand {
    StartUtility(crate::tools::session::UtilityKind),
    Utility {
        session: Uuid,
        input: crate::tools::session::UtilityInput,
    },
    InterruptUtility,

    Launch {
        request: super::cli::LaunchRequest,
        reply: Option<std::sync::mpsc::SyncSender<Result<Uuid, AppError>>>,
    },
    Install {
        request: Uuid,
        command: super::controller::ingress::InstallCommand,
    },
    ResumeIngress,
    CancelPendingIngress(Uuid),
    UploadSource(super::upload_sources::UploadSource),
    UploadClipboard,
    UploadClipboardFor(DestinationSelection),
    UploadSourceFor {
        source: super::upload_sources::UploadSource,
        selection: DestinationSelection,
        action: Option<ShortcutAction>,
    },
    PinFromScreenFor(DestinationSelection),
    SetTaskProfiles {
        tasks: crate::workflow::profile::TaskProfiles,
        archive: ArchivePolicy,
        revision: u64,
    },
    Task {
        id: Uuid,
        command: crate::workflow::runner::TaskCommand,
    },
    RunRecordingTasks,

    ToggleScrolling,
    ToggleScrollingFor(DestinationSelection),
    ScrollingSelection {
        session: Uuid,
        rect: crate::capture::geometry::PhysicalRect,
    },
    ScrollingSelectionClosed(Uuid),
    ScrollingControl(crate::scrolling::runtime::ScrollControl),

    StartAudioTest(crate::recording::options::AudioOptions),
    StopAudioTest,
    RefreshAudioEndpoints,
    OpenRecordingPlayer,
    CloseRecordingReview,
    ToggleRecordingFor {
        mp4: bool,
        selection: DestinationSelection,
    },
    ToggleRecording {
        mp4: bool,
    },
    RecordingSelectionClosed(Uuid),
    RecordingSelection {
        session: Uuid,
        rect: crate::capture::geometry::PhysicalRect,
    },
    RetryRecordingArchive {
        continue_without: bool,
    },
    SetRecordingPreferences {
        preferences: crate::recording::preferences::RecordingPreferences,
        limits: crate::persistence::settings::RecordingLimits,
        revision: u64,
    },
    Recording(crate::recording::session::RecordingInput),
    StartCapture {
        mode: crate::capture::request::CaptureMode,
        selection: DestinationSelection,
    },
    FinishCapture {
        session: Uuid,
        finish: crate::ui::image_export::ImageFinish,
        save_path: Option<PathBuf>,
    },
    RetryCaptureFinish {
        session: Uuid,
        decision: super::image_finish::ArchiveDecision,
    },
    CancelCapture(Uuid),
    CancelActiveCapture,
    ResumeCapture(Uuid),
    PinSource(crate::pin::source::PinSource),
    DismissPinSourceChooser,
    LoadPinFile(PathBuf),
    Pin {
        id: crate::pin::PinId,
        input: crate::pin::PinInput,
    },
    CloseAllPins,
    RefreshPinWorkAreas,
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
    AcceptFileSelectionFor {
        paths: Vec<PathBuf>,
        selection: DestinationSelection,
    },
    /// The tray's hidden window now exists; native hotkey registration can
    /// begin. Carries its raw `HWND` value (`isize`), never a type that
    /// implies cross-thread ownership of the window itself.
    AttachHotkeyWindow(isize),
    /// One native hotkey fired with this id. Resolved to an action (if any
    /// current registration still owns it), then dispatched through the
    /// same suppression/repeat policy as every other shortcut event.
    NativeHotkeyFired(i32),
    ApplyShortcuts {
        bindings: Vec<ShortcutBinding>,
        revision: u64,
    },
    ResetShortcuts {
        actions: Option<Vec<ShortcutAction>>,
        revision: u64,
    },
    /// Set by the Hotkeys settings page while its press-to-set field is
    /// focused, so a matching active global hotkey cannot also dispatch.
    SetShortcutEntryActive(Option<Uuid>),
    SetAppearance {
        appearance: AppearancePreferences,
        revision: u64,
    },
    SetRegionPreferences {
        preferences: crate::capture::preferences::RegionCapturePreferences,
        revision: u64,
    },
    SetStartup {
        startup: StartupPreference,
        revision: u64,
    },
    ChooseArchiveFolder,
    SetArchivePolicy {
        policy: ArchivePolicy,
        revision: u64,
    },
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
    UtilityAcquired {
        session: Uuid,
        result: Result<crate::tools::UtilitySnapshot, AppError>,
    },

    ScrollingAcquired {
        session: Uuid,
        result: Result<crate::platform::windows::capture::ScrollingSelectionSnapshot, AppError>,
    },
    Scrolling(crate::scrolling::runtime::RuntimeEvent),
    RecordingAcquired {
        session: Uuid,
        result: Result<crate::capture::request::CaptureSnapshot, AppError>,
    },
    RecordingWorker {
        session: Uuid,
        input: crate::recording::session::RecordingInput,
    },
    PinWorkAreas {
        result: Result<crate::pin::source::PinWorkAreas, AppError>,
    },
    PinLoaded {
        request: Uuid,
        result: Result<crate::pin::source::LoadedPin, AppError>,
    },
    CaptureAcquired {
        session: Uuid,
        result: Result<crate::capture::request::CaptureSnapshot, AppError>,
    },
    Queue(QueueEffect),
    WorkerStopped(Result<(), AppError>),
    ConnectionTest {
        request: Uuid,
        result: Result<UploaderInfoSupport, AppError>,
    },
    /// One file from an accepted batch finished staging (or failed to).
    /// `index` is the file's position in the original picker selection,
    /// independent of the (possibly different) order staging settles in.
    FileStaged {
        batch: Uuid,
        index: usize,
        display_name: String,
        outcome: Result<Artifact, AppError>,
    },
}
pub enum AppEffect {
    AcquireUtility {
        session: Uuid,
    },
    CancelUtilityAcquisition,
    UtilitySurface(
        Option<std::sync::Arc<std::sync::Mutex<crate::ui::screen_sampler::UtilitySurface>>>,
    ),

    CancelInstallVerify,
    VerifyInstall {
        request: Uuid,
        client: crate::vectr::client::VectrClient,
    },
    ReadUploadClipboard {
        selection: DestinationSelection,
        action: Option<ShortcutAction>,
    },
    StageIngressFile {
        batch: Uuid,
        index: usize,
        path: PathBuf,
    },
    AcquireScrolling(Uuid),
    CancelScrolling,
    StartScrolling {
        session: Uuid,
        target: crate::platform::windows::windows::ScrollingTarget,
    },
    ScrollingControl(crate::scrolling::runtime::ScrollControl),
    StartAudioTest(crate::recording::options::AudioOptions),
    RefreshAudioEndpoints,
    StopAudioTest,
    AcquireRecording {
        session: Uuid,
        request: crate::capture::request::CaptureRequest,
    },
    CancelRecordingAcquisition,
    StartRecordingWorker {
        session: Uuid,
        options: crate::recording::options::RecordingOptions,
        sources: Vec<crate::recording::session::ResolvedSource>,
    },
    RecordingControl(crate::recording::worker::RecordingControl),
    RefreshPinWorkAreas,
    ArchiveMaintenance(super::image_finish::ArchiveMaintenance),
    AcquireCapture {
        session: Uuid,
        request: crate::capture::request::CaptureRequest,
    },
    CancelCaptureAcquisition,
    CaptureSurface(Option<std::sync::Arc<std::sync::Mutex<super::controller::CaptureSurface>>>),
    Pin(crate::pin::PinEffect),
    LoadPin {
        request: Uuid,
        source: crate::pin::source::PinLoad,
        clipboard: std::sync::Arc<dyn crate::platform::windows::clipboard::ClipboardWriter>,
    },
    PinSourceChooser(bool),
    PickPinFile,
    ShortcutRecorded {
        request: Uuid,
        chord: super::shortcuts::KeyChord,
    },
    ShortcutStatuses(
        Vec<(
            super::shortcuts::ShortcutAction,
            super::shortcuts::ShortcutStatus,
        )>,
    ),
    LocalHistoryAction(super::local_actions::LocalHistoryAction),
    HistoryRetryReport {
        queued: usize,
        skipped: usize,
    },
    /// The Upload-file hotkey fired: the UI thread owns the native picker's
    /// single-flight gate and must open it, exactly like the tray menu item.
    OpenFilePicker(DestinationSelection),
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
