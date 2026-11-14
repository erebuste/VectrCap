//! Blocking persistence stays on the controller thread, including initialization.
//! The GUI reads only this bounded nonsecret snapshot and never opens SQLite.
use super::{
    controller::AppController, destination_manager::FileDestinationRepository, error::AppError,
    events::*,
};
use crate::{
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
    pub settings: Option<Settings>,
    pub window: Option<Window>,
    pub focus_revision: u64,
    pub lifecycle: Lifecycle,
    pub exit_requested: bool,
    pub jobs: Vec<UploadJob>,
    pub transfers: HashMap<crate::app::types::JobId, (u64, Option<f64>)>,
    pub queue_paused: bool,
    pub error: Option<AppError>,
    pub saved: u64,
    pub save_result: Option<(uuid::Uuid, Result<(), AppError>)>,
    pub accepted_workflows: usize,
    pub notices: Vec<super::controller::NoticeDisplay>,
    pub notifications_enabled: bool,
    pub connection: Option<(
        uuid::Uuid,
        Result<crate::vectr::uploader_info::UploaderInfoSupport, AppError>,
    )>,
}
impl Default for DesktopView {
    fn default() -> Self {
        Self {
            settings: None,
            window: None,
            focus_revision: 0,
            lifecycle: Lifecycle::Running,
            exit_requested: false,
            jobs: vec![],
            transfers: HashMap::new(),
            queue_paused: false,
            error: None,
            saved: 0,
            save_result: None,
            accepted_workflows: 0,
            notices: vec![],
            notifications_enabled: true,
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
struct Effects {
    queue: Option<mpsc::Sender<QueueCommand>>,
    metadata: Option<ConnectionTask>,
    state: DesktopView,
}
impl Effects {
    async fn apply(&mut self, effects: Vec<AppEffect>, history: &History) -> Result<(), AppError> {
        for effect in effects {
            match effect {
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
                AppEffect::DrainWorker => {
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
    let uploader = Arc::new(Uploader::new(history.clone(), artifacts, factory));
    let (queue_tx, queue_rx) = crate::vectr::queue::command_channel();
    let (effect_tx, mut effect_rx) = mpsc::channel(32);
    let mut effects = Effects {
        queue: Some(queue_tx),
        metadata: None,
        state: DesktopView {
            settings: Some(controller.settings().clone()),
            window: controller.window,
            focus_revision: 1,
            ..Default::default()
        },
    };
    effects.apply(vec![], &history).await?;
    let mut driver = tokio::spawn(queue.serve(uploader, queue_rx, effect_tx));
    effects.publish(&view, wake.as_ref());
    let mut effect_open = true;
    let mut driver_consumed = false;
    let outcome = loop {
        let results = tokio::select! {
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
