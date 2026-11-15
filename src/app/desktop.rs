//! Blocking persistence stays on the controller thread, including initialization.
//! The GUI reads only this bounded nonsecret snapshot and never opens SQLite.
use super::{
    controller::AppController, destination_manager::FileDestinationRepository, error::AppError,
    events::*,
};
use crate::{
    app::types::{Artifact, ArtifactKind},
    persistence::{
        artifacts::ArtifactStore,
        history::{History, UploadJob},
        settings::Settings,
    },
    platform::CredentialStore,
    vectr::{
        client::VectrClient,
        queue::{QueueCommand, QueueController, QueueEffect},
        uploader::{ClientFactory, Uploader},
    },
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
pub struct DesktopView {
    pub capture_acquiring: Option<uuid::Uuid>,
    pub capture_surface: Option<Arc<Mutex<super::controller::CaptureSurface>>>,
    pub pins: Vec<super::controller::PinImage>,
    pub settings: Option<Settings>,
    pub window: Option<Window>,
    pub focus_revision: u64,
    pub lifecycle: Lifecycle,
    pub exit_requested: bool,
    pub jobs: Vec<UploadJob>,
    pub history_retry_report: Option<(usize, usize)>,
    pub history_query: crate::persistence::history::browser::HistoryQuery,
    pub history_page: crate::persistence::history::browser::HistoryPage,
    pub batch_summary: Vec<super::controller::BatchSummaryRow>,
    pub transfers: HashMap<crate::app::types::JobId, (u64, Option<f64>)>,
    pub queue_paused: bool,
    pub error: Option<AppError>,
    pub saved: u64,
    pub recorded_shortcut: Option<(uuid::Uuid, super::shortcuts::KeyChord)>,
    pub picker_requests: u64,
    pub picker_selection: super::destinations::DestinationSelection,
    pub shortcut_statuses: Vec<(
        super::shortcuts::ShortcutAction,
        super::shortcuts::ShortcutStatus,
    )>,
    pub save_result: Option<(uuid::Uuid, Result<(), AppError>)>,
    pub accepted_workflows: usize,
    pub notices: Vec<super::controller::NoticeDisplay>,
    pub notifications_enabled: bool,
    /// Files rejected by the current/most recent batch's staging, bounded;
    /// cleared when a new native-picker batch is accepted.
    pub failed_uploads: Vec<(String, AppError)>,
    pub connection: Option<(
        uuid::Uuid,
        Result<crate::vectr::uploader_info::UploaderInfoSupport, AppError>,
    )>,
}
impl Default for DesktopView {
    fn default() -> Self {
        Self {
            capture_acquiring: None,
            capture_surface: None,
            pins: vec![],
            settings: None,
            window: None,
            focus_revision: 0,
            lifecycle: Lifecycle::Running,
            exit_requested: false,
            jobs: vec![],
            batch_summary: vec![],
            history_page: Default::default(),
            history_query: Default::default(),
            history_retry_report: None,
            transfers: HashMap::new(),
            queue_paused: false,
            error: None,
            saved: 0,
            recorded_shortcut: None,
            picker_requests: 0,
            picker_selection: Default::default(),
            shortcut_statuses: vec![],
            save_result: None,
            accepted_workflows: 0,
            notices: vec![],
            notifications_enabled: true,
            failed_uploads: vec![],
            connection: None,
        }
    }
}
#[derive(Clone)]
pub struct DesktopClient {
    tx: mpsc::Sender<AppCommand>,
    stop: CancellationToken,
    view: Arc<Mutex<DesktopView>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl DesktopClient {
    pub(crate) fn wake(&self) {
        (self.wake)();
    }
    pub fn snapshot(&self) -> DesktopView {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn submit(&self, command: AppCommand) -> Result<(), AppError> {
        if matches!(command, AppCommand::Quit) {
            if self.tx.is_closed() {
                let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
                view.exit_requested = true;
                view.lifecycle = Lifecycle::Exited;
                drop(view);
                (self.wake)();
                return Ok(());
            }
            // Explicit Quit bypasses the bounded command queue: a congested or
            // slow-dispatching controller must never drop a user's shutdown
            // request to ordinary backpressure. The serve loop already treats
            // this cancellation as a priority Quit dispatch.
            self.stop.cancel();
            return Ok(());
        }
        self.tx
            .try_send(command)
            .map_err(|_| AppError::InUse("controller busy or stopped; try again"))
    }
    pub fn report_error(&self, error: AppError) {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).error = Some(error);
        (self.wake)();
    }
}
pub struct DesktopRuntime {
    pub client: DesktopClient,
    stop: CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl DesktopRuntime {
    pub fn spawn(
        root: PathBuf,
        store: Arc<dyn CredentialStore>,
        factory: Arc<dyn ClientFactory>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, AppError> {
        let (tx, rx) = mpsc::channel(32);
        let view = Arc::new(Mutex::new(DesktopView::default()));
        let stop = CancellationToken::new();
        let client = DesktopClient {
            tx,
            stop: stop.clone(),
            view: view.clone(),
            wake: wake.clone(),
        };
        let shutdown = stop.clone();
        let thread = std::thread::Builder::new()
            .name("desktop-controller".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| AppError::Persistence("controller runtime unavailable"))
                    .and_then(|runtime| {
                        runtime.block_on(serve(
                            root,
                            store,
                            factory,
                            rx,
                            view.clone(),
                            wake.clone(),
                            shutdown,
                        ))
                    });
                if let Err(error) = result {
                    let mut state = view.lock().unwrap_or_else(|e| e.into_inner());
                    state.error = Some(error);
                    state.lifecycle = Lifecycle::Exited;
                    state.window = Some(Window::Settings);
                    state.focus_revision += 1;
                    drop(state);
                    wake();
                }
            })
            .map_err(|_| AppError::Persistence("controller thread unavailable"))?;
        Ok(Self {
            client,
            stop,
            thread: Some(thread),
        })
    }
}

type ConnectionTask = tokio::task::JoinHandle<(
    uuid::Uuid,
    Result<crate::vectr::uploader_info::UploaderInfoSupport, AppError>,
)>;
/// `index` is the file's position in its original picker selection.
type StagingTask = (uuid::Uuid, usize, String, Result<Artifact, AppError>);
/// How many `stage_file_cancellable` calls (and therefore open source-file
/// handles) may be in flight at once, regardless of how many files were
/// accepted across however many concurrent batches.
const MAX_CONCURRENT_STAGING: usize = 4;
/// How many staging requests may sit admitted-but-not-yet-running at once.
/// Beyond this, a newly accepted file is bounced with an explicit busy
/// outcome instead of growing the backlog without limit.
const MAX_PENDING_STAGING: usize = 64;
struct Effects {
    captures: tokio::task::JoinSet<(
        uuid::Uuid,
        Result<crate::capture::request::CaptureSnapshot, AppError>,
    )>,
    capture_cancel: Option<CancellationToken>,
    queue: Option<mpsc::Sender<QueueCommand>>,
    metadata: Option<ConnectionTask>,
    state: DesktopView,
    local_actions: tokio::task::JoinSet<Result<(), AppError>>,
    local_cancel: CancellationToken,
    artifacts: Arc<ArtifactStore>,
    staging: tokio::task::JoinSet<StagingTask>,
    staging_limit: Arc<tokio::sync::Semaphore>,
    /// Batch/index/display-name for each outstanding `staging` task, so a
    /// panicked or aborted task can still resolve its exact batch slot
    /// instead of leaving that batch permanently pending.
    staging_meta: HashMap<tokio::task::Id, (uuid::Uuid, usize, String)>,
}
impl Effects {
    async fn apply(&mut self, effects: Vec<AppEffect>, history: &History) -> Result<(), AppError> {
        for effect in effects {
            match effect {
                AppEffect::ArchiveMaintenance(task) => {
                    let cancel = self.local_cancel.clone();
                    self.local_actions.spawn_blocking(move || task.run(cancel));
                }
                AppEffect::AcquireCapture { session, request } => {
                    let cancel = CancellationToken::new();
                    self.capture_cancel = Some(cancel.clone());
                    self.state.capture_acquiring = Some(session);
                    let busy = self.captures.len() >= 2;
                    self.captures.spawn(async move {
                        // The current-thread runtime cannot start acquisition until
                        // this effect batch publishes capture suppression to the UI.
                        let result = tokio::task::spawn_blocking(move || {
                            if busy {
                                Err(AppError::InUse(
                                    "capture acquisition is still stopping; try again",
                                ))
                            } else {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    crate::platform::windows::capture::acquire_screenshot(
                                        request, cancel,
                                    )
                                }))
                                .unwrap_or(Err(
                                    AppError::Capture("capture acquisition worker failed"),
                                ))
                            }
                        })
                        .await
                        .unwrap_or(Err(AppError::Capture("capture acquisition worker failed")));
                        (session, result)
                    });
                }
                AppEffect::CancelCaptureAcquisition => {
                    if let Some(cancel) = self.capture_cancel.take() {
                        cancel.cancel();
                    }
                    self.state.capture_acquiring = None;
                }
                AppEffect::CaptureSurface(surface) => {
                    self.state.capture_acquiring = None;
                    self.state.capture_surface = surface;
                }
                AppEffect::PinImage(pin) => self.state.pins.push(pin),
                AppEffect::ClosePin(id) => self.state.pins.retain(|pin| pin.id != id),
                AppEffect::ShortcutRecorded { request, chord } => {
                    self.state.recorded_shortcut = Some((request, chord))
                }
                AppEffect::ShortcutStatuses(statuses) => self.state.shortcut_statuses = statuses,
                AppEffect::OpenFilePicker(selection) => {
                    self.state.picker_selection = selection;
                    self.state.picker_requests += 1;
                }
                AppEffect::LocalHistoryAction(task) => {
                    if self.local_actions.len() >= 4 {
                        self.state.error =
                            Some(AppError::InUse("local actions are busy; try again"));
                    } else {
                        let cancel = self.local_cancel.clone();
                        self.local_actions.spawn_blocking(move || task.run(cancel));
                    }
                }
                AppEffect::HistoryRetryReport { queued, skipped } => {
                    self.state.history_retry_report = Some((queued, skipped))
                }
                AppEffect::Window(window) => {
                    self.state.window = window;
                    self.state.focus_revision += 1;
                }
                AppEffect::SettingsSaved(settings) => {
                    self.state.settings = Some(settings);
                    self.state.saved += 1;
                }
                AppEffect::ConnectionSaved { request, result } => {
                    self.state.save_result =
                        Some((request, result.as_ref().map(|_| ()).map_err(Clone::clone)));
                    match result {
                        Ok(settings) => {
                            self.state.settings = Some(settings);
                            self.state.saved += 1;
                            if let Some(task) = self.metadata.take() {
                                task.abort();
                            }
                            self.state.connection = None;
                        }
                        Err(error) => self.state.error = Some(error),
                    }
                }
                AppEffect::Queue(command) => {
                    if let Some(queue) = &self.queue {
                        queue
                            .send(command)
                            .await
                            .map_err(|_| AppError::Persistence("upload driver stopped"))?;
                    }
                }
                AppEffect::TestConnection {
                    request,
                    origin,
                    token,
                } => {
                    if let Some(task) = self.metadata.take() {
                        task.abort();
                    }
                    self.metadata = Some(tokio::spawn(async move {
                        let result = match VectrClient::new(origin, token) {
                            Ok(client) => tokio::time::timeout(
                                std::time::Duration::from_secs(20),
                                client.uploader_info(),
                            )
                            .await
                            .unwrap_or(Err(AppError::Network)),
                            Err(error) => Err(error),
                        };
                        (request, result)
                    }));
                }
                AppEffect::ConnectionTest { request, result } => {
                    self.state.connection = Some((request, result))
                }
                AppEffect::CancelConnectionTest => {
                    if let Some(task) = self.metadata.take() {
                        task.abort();
                    }
                    self.state.connection = None;
                }
                AppEffect::Error(error) => self.state.error = Some(error),
                AppEffect::StageFiles { batch, paths } => {
                    self.state.failed_uploads.clear();
                    for (index, path) in paths.into_iter().enumerate() {
                        let display_name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "selected file".to_owned());
                        // Bounded pending backlog: a request beyond the cap
                        // is bounced with an explicit busy outcome rather
                        // than growing the queue (and the open-handle count
                        // behind it) without limit.
                        if self.staging.len() >= MAX_PENDING_STAGING {
                            let display_name = display_name.clone();
                            self.staging.spawn(async move {
                                (
                                    batch,
                                    index,
                                    display_name,
                                    Err(AppError::InUse(
                                        "the staging queue is busy; try again shortly",
                                    )),
                                )
                            });
                            continue;
                        }
                        let artifacts = self.artifacts.clone();
                        let cancel = self.local_cancel.clone();
                        let limit = self.staging_limit.clone();
                        let task_display_name = display_name.clone();
                        let handle = self.staging.spawn(async move {
                            if cancel.is_cancelled() {
                                return (batch, index, task_display_name, Err(AppError::Cancelled));
                            }
                            // Bounds concurrent `stage_file_cancellable` calls
                            // (and therefore concurrently open source-file
                            // handles) to a small fixed number, independent
                            // of how many files/batches were accepted.
                            let Ok(_permit) = limit.acquire_owned().await else {
                                return (
                                    batch,
                                    index,
                                    task_display_name,
                                    Err(AppError::Persistence("staging worker pool closed")),
                                );
                            };
                            if cancel.is_cancelled() {
                                return (batch, index, task_display_name, Err(AppError::Cancelled));
                            }
                            let outcome = tokio::task::spawn_blocking(move || {
                                artifacts.stage_file_cancellable(&path, ArtifactKind::File, &cancel)
                            })
                            .await
                            .unwrap_or(Err(AppError::Persistence("file staging worker failed")));
                            (batch, index, task_display_name, outcome)
                        });
                        self.staging_meta
                            .insert(handle.id(), (batch, index, display_name));
                    }
                }
                AppEffect::FileRejected {
                    display_name,
                    error,
                } => {
                    if self.state.failed_uploads.len() < 100 {
                        self.state.failed_uploads.push((display_name, error));
                    }
                }
                AppEffect::DrainWorker => {
                    if let Some(cancel) = self.capture_cancel.take() {
                        cancel.cancel();
                    }
                    self.local_cancel.cancel();
                    self.state.exit_requested = true;
                    self.state.lifecycle = Lifecycle::Draining;
                    // Closing the sole command sender invokes the approved driver's
                    // durable PauseAll and settlement path; never abort its request.
                    self.queue.take();
                    if let Some(task) = self.metadata.take() {
                        task.abort();
                    }
                }
                AppEffect::Exit => {
                    self.state.lifecycle = Lifecycle::Exited;
                    if !self.state.exit_requested {
                        self.state.window = Some(Window::Settings);
                        self.state.focus_revision += 1;
                        self.state
                            .error
                            .get_or_insert(AppError::Persistence("upload driver stopped"));
                    }
                }
                AppEffect::RefreshQueue
                | AppEffect::JobAccepted(_)
                | AppEffect::WorkflowAccepted(_) => {}
            }
        }
        self.state.jobs = history.shell_jobs()?;
        self.state
            .transfers
            .retain(|id, _| self.state.jobs.iter().any(|job| job.id == *id));
        self.state.queue_paused = history.queue_paused()?;
        Ok(())
    }
    fn publish(&self, view: &Mutex<DesktopView>, wake: &dyn Fn()) {
        *view.lock().unwrap_or_else(|e| e.into_inner()) = self.state.clone();
        wake();
    }
}
async fn serve(
    root: PathBuf,
    store: Arc<dyn CredentialStore>,
    factory: Arc<dyn ClientFactory>,
    mut commands: mpsc::Receiver<AppCommand>,
    view: Arc<Mutex<DesktopView>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: CancellationToken,
) -> Result<(), AppError> {
    let history = Arc::new(History::open(&root.join("history.db"))?);
    let artifacts = Arc::new(ArtifactStore::new(root.join("spool"))?);
    let mut controller = AppController::open(
        Box::new(FileDestinationRepository::open(
            &root.join("settings.json"),
        )?),
        history.clone(),
        store,
        artifacts.clone(),
        Arc::new(crate::platform::windows::shell_actions::WindowsShellActions),
        Arc::new(crate::platform::windows::clipboard::WindowsClipboard),
    )?;
    let queue = QueueController::new(history.clone())?;
    let uploader = Arc::new(Uploader::new(history.clone(), artifacts.clone(), factory));
    let (queue_tx, queue_rx) = crate::vectr::queue::command_channel();
    let (effect_tx, mut effect_rx) = mpsc::channel(32);
    let mut effects = Effects {
        captures: tokio::task::JoinSet::new(),
        capture_cancel: None,
        local_actions: tokio::task::JoinSet::new(),
        local_cancel: CancellationToken::new(),
        artifacts: artifacts.clone(),
        staging: tokio::task::JoinSet::new(),
        staging_limit: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_STAGING)),
        staging_meta: HashMap::new(),
        queue: Some(queue_tx),
        metadata: None,
        state: DesktopView {
            settings: Some(controller.settings().clone()),
            notifications_enabled: controller.notifications_enabled(),
            window: controller.window,
            focus_revision: 1,
            ..Default::default()
        },
    };
    effects
        .apply(controller.startup_effects(), &history)
        .await?;
    let mut driver = tokio::spawn(queue.serve(uploader, queue_rx, effect_tx));
    effects.publish(&view, wake.as_ref());
    let mut effect_open = true;
    let mut driver_consumed = false;
    let outcome = loop {
        let results = tokio::select! {
            result = effects.captures.join_next(), if !effects.captures.is_empty() => {
                match result {
                    Some(Ok((session, result))) => controller.on_event(AppEvent::CaptureAcquired { session, result }),
                    _ => effects.state.capture_acquiring.map_or_else(Vec::new, |session| controller.on_event(AppEvent::CaptureAcquired { session, result: Err(AppError::Capture("capture acquisition worker failed")) })),
                }
            }
            result = effects.local_actions.join_next(), if !effects.local_actions.is_empty() => {
                match result {Some(Ok(Ok(())))=>vec![],Some(Ok(Err(AppError::Cancelled)))=>vec![],Some(Ok(Err(error)))=>vec![AppEffect::Error(error)],_=>vec![AppEffect::Error(AppError::Persistence("local action worker failed"))]}
            }
            // No lifecycle guard: a file already accepted into a batch must
            // still become a durable job even if Quit arrives mid-staging.
            result = effects.staging.join_next_with_id(), if !effects.staging.is_empty() => {
                match result {
                    Some(Ok((id, (batch, index, display_name, outcome)))) => {
                        effects.staging_meta.remove(&id);
                        controller.on_event(AppEvent::FileStaged { batch, index, display_name, outcome })
                    }
                    // A panicked/aborted staging task still resolves its
                    // exact batch slot via the metadata recorded at spawn
                    // time, instead of leaving that batch permanently
                    // pending with no `FileStaged` ever arriving for it.
                    Some(Err(join_error)) => match effects.staging_meta.remove(&join_error.id()) {
                        Some((batch, index, display_name)) => controller.on_event(AppEvent::FileStaged {
                            batch, index, display_name,
                            outcome: Err(AppError::Persistence("file staging worker failed")),
                        }),
                        None => vec![AppEffect::Error(AppError::Persistence("file staging worker failed"))],
                    },
                    None => vec![],
                }
            }
            _ = stop.cancelled(), if controller.lifecycle == Lifecycle::Running => controller.dispatch(AppCommand::Quit),
            command = commands.recv(), if controller.lifecycle == Lifecycle::Running => {
                effects.state.error = None;
                controller.dispatch(command.unwrap_or(AppCommand::Quit))
            }
            effect = effect_rx.recv(), if effect_open => {
                match effect {
                    Some(effect) => {
                        if let QueueEffect::Progress { job, acknowledged, speed } = &effect { effects.state.transfers.insert(*job, (*acknowledged, *speed)); }
                        controller.on_event(AppEvent::Queue(effect))
                    }
                    None => { effect_open = false; vec![] }
                }
            }
            result = &mut driver => {
                driver_consumed = true;
                let result = result.unwrap_or(Err(AppError::Persistence("upload driver failed")));
                controller.on_event(AppEvent::WorkerStopped(result))
            }
            result = async { match effects.metadata.as_mut() { Some(task) => task.await, None => std::future::pending().await } } => {
                effects.metadata = None;
                match result {
                    Ok((request, result)) => controller.on_event(AppEvent::ConnectionTest { request, result }),
                    Err(_) => vec![AppEffect::Error(AppError::Network)],
                }
            }
        };
        effects.state.accepted_workflows = controller.active_workflow_count();
        effects.state.batch_summary = match controller.batch_summary() {
            Ok(rows) => rows,
            Err(error) => break Err(error),
        };
        if controller.window == Some(Window::History) {
            effects.state.history_query = controller.history_query().clone();
            effects.state.history_page = match controller.history_page() {
                Ok(page) => page,
                Err(error) => break Err(error),
            };
        }
        effects.state.notices = controller.visible_notices();
        effects.state.notifications_enabled = controller.notifications_enabled();
        if let Err(error) = effects.apply(results, &history).await {
            break Err(error);
        }
        effects.publish(&view, wake.as_ref());
        if controller.lifecycle == Lifecycle::Exited {
            break Ok(());
        }
    };
    // Even a snapshot/read failure must close and drain, not detach the driver
    // and let runtime teardown abort a request with an unrecorded outcome.
    effects.local_cancel.cancel();
    if let Some(cancel) = effects.capture_cancel.take() {
        cancel.cancel();
    }
    while effects.captures.join_next().await.is_some() {}
    while effects.local_actions.join_next().await.is_some() {}
    // A file already accepted into a batch durably becomes a job the moment
    // `on_event` runs, even though nothing reads the published view anymore;
    // draining here (instead of dropping the JoinSet) is what prevents an
    // in-flight staged file from being orphaned without a job record. Every
    // resulting effect is applied (not just the success path, and not just
    // discarded) so an `AppEffect::Error` from a failed drain-time commit
    // still lands in `state.error` instead of being silently swallowed.
    while let Some(result) = effects.staging.join_next_with_id().await {
        let event_effects = match result {
            Ok((id, (batch, index, display_name, outcome))) => {
                effects.staging_meta.remove(&id);
                controller.on_event(AppEvent::FileStaged {
                    batch,
                    index,
                    display_name,
                    outcome,
                })
            }
            Err(join_error) => match effects.staging_meta.remove(&join_error.id()) {
                Some((batch, index, display_name)) => controller.on_event(AppEvent::FileStaged {
                    batch,
                    index,
                    display_name,
                    outcome: Err(AppError::Persistence("file staging worker failed")),
                }),
                None => vec![AppEffect::Error(AppError::Persistence(
                    "file staging worker failed",
                ))],
            },
        };
        if let Err(error) = effects.apply(event_effects, &history).await {
            effects.state.error = Some(error);
        }
    }
    effects.publish(&view, wake.as_ref());
    effects.queue.take();
    if let Some(task) = effects.metadata.take() {
        task.abort();
        let _ = task.await;
    }
    if !driver_consumed {
        let _ = driver.await;
    }
    outcome
}
impl Drop for DesktopRuntime {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
