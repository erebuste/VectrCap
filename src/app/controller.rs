//! Single owner for workflow/job acceptance and settings/credential transactions.
//! There is no await point between reference inspection and durable commit.
use super::{
    destination_manager::{
        DestinationEdit, DestinationManager, DestinationReferences, DestinationRepository,
    },
    destinations::{DestinationId, DestinationSelection, DestinationSnapshot, resolve_destination},
    error::AppError,
    events::*,
};
use crate::{
    persistence::{
        artifacts::ArtifactStore,
        history::History,
        settings::{ServerOrigin, Settings},
    },
    platform::CredentialStore,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Default)]
struct ReferenceState {
    active: HashSet<DestinationId>,
    // No hotkey/profile schema in 3B. Future configured-reference writers must
    // join this owner and the persisted settings transaction, never mutate here.
    configured: HashMap<String, DestinationId>,
}
#[derive(Default)]
struct References(Mutex<ReferenceState>);
impl DestinationReferences for References {
    fn active_workflows(&self) -> Result<HashSet<DestinationId>, AppError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| AppError::Persistence("workflow references unavailable"))?
            .active
            .clone())
    }
    fn validate_configured(&self, candidate: &Settings) -> Result<(), AppError> {
        let refs = self
            .0
            .lock()
            .map_err(|_| AppError::Persistence("configured references unavailable"))?;
        if refs
            .configured
            .values()
            .any(|id| candidate.destinations.get(*id).is_none())
        {
            return Err(AppError::InvalidInput(
                "repair configured destination references before removal",
            ));
        }
        Ok(())
    }
}
pub struct AppController {
    manager: DestinationManager,
    history: Arc<History>,
    store: Arc<dyn CredentialStore>,
    artifacts: Arc<ArtifactStore>,
    references: Arc<References>,
    workflows: HashMap<Uuid, DestinationSnapshot>,
    pub lifecycle: Lifecycle,
    pub window: Option<Window>,
}
impl AppController {
    pub fn open(
        repository: Box<dyn DestinationRepository>,
        history: Arc<History>,
        store: Arc<dyn CredentialStore>,
        artifacts: Arc<ArtifactStore>,
    ) -> Result<Self, AppError> {
        let references = Arc::new(References::default());
        let manager = DestinationManager::open(
            repository,
            store.clone(),
            history.clone(),
            references.clone(),
        )?;
        let window = manager
            .settings()
            .destinations
            .is_empty()
            .then_some(Window::Settings);
        Ok(Self {
            manager,
            history,
            store,
            artifacts,
            references,
            workflows: HashMap::new(),
            lifecycle: Lifecycle::Running,
            window,
        })
    }
    pub fn settings(&self) -> &Settings {
        self.manager.settings()
    }
    pub fn active_workflow_count(&self) -> usize {
        self.workflows.len()
    }
    pub fn dispatch(&mut self, command: AppCommand) -> Vec<AppEffect> {
        if matches!(command, AppCommand::Quit) && self.lifecycle != Lifecycle::Running {
            return vec![];
        }
        if self.lifecycle != Lifecycle::Running {
            return vec![AppEffect::Error(AppError::InUse(
                "application is shutting down",
            ))];
        }
        let connection_request = match &command {
            AppCommand::TestConnection { request, .. }
            | AppCommand::TestSavedConnection { request, .. } => Some(*request),
            _ => None,
        };
        self.apply(command)
            .unwrap_or_else(|error| match connection_request {
                Some(request) => vec![AppEffect::ConnectionTest {
                    request,
                    result: Err(error),
                }],
                None => vec![AppEffect::Error(error)],
            })
    }
    fn update_active(&self) -> Result<(), AppError> {
        self.references
            .0
            .lock()
            .map_err(|_| AppError::Persistence("workflow references unavailable"))?
            .active = self.workflows.values().map(|s| s.id).collect();
        Ok(())
    }
    fn apply(&mut self, command: AppCommand) -> Result<Vec<AppEffect>, AppError> {
        Ok(match command {
            AppCommand::Show(window) => {
                self.window = Some(window);
                let mut effects = vec![AppEffect::Window(self.window)];
                if window != Window::Settings {
                    effects.push(AppEffect::CancelConnectionTest);
                }
                effects
            }
            AppCommand::CloseWindow => {
                self.window = None;
                vec![AppEffect::Window(None), AppEffect::CancelConnectionTest]
            }
            AppCommand::ApplyDestinations { edit, revision } => {
                self.manager.apply(edit, revision)?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::SaveConnection {
                request,
                edit,
                revision,
            } => {
                let result = self
                    .manager
                    .apply(edit, revision)
                    .map(|_| self.settings().clone());
                vec![AppEffect::ConnectionSaved { request, result }]
            }
            AppCommand::SetDefault { id, revision } => {
                let mut destinations = self.settings().destinations.clone();
                destinations.set_default(id)?;
                self.manager.apply(
                    DestinationEdit {
                        destinations,
                        tokens: vec![],
                    },
                    revision,
                )?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::BeginWorkflow { id, selection } => {
                if id.is_nil() || self.workflows.contains_key(&id) || self.workflows.len() >= 32 {
                    return Err(AppError::InvalidInput(
                        "invalid, duplicate or excess workflow",
                    ));
                }
                self.manager.ensure_revision(self.settings().revision)?;
                let snapshot = resolve_destination(
                    selection,
                    DestinationSelection::Inherit,
                    &self.settings().destinations,
                )?;
                self.store.read(&snapshot.credential)?;
                self.workflows.insert(id, snapshot);
                self.update_active()?;
                vec![AppEffect::WorkflowAccepted(id)]
            }
            AppCommand::AcceptWorkflow { id, artifact } => {
                let snapshot = self
                    .workflows
                    .get(&id)
                    .ok_or(AppError::InvalidInput("unknown accepted workflow"))?;
                self.manager.ensure_revision(self.settings().revision)?;
                self.artifacts.verify(&artifact)?;
                // Reference stays live until the durable job owns it.
                let job = self.history.create_job(&artifact, snapshot)?;
                self.workflows.remove(&id);
                self.update_active()?;
                vec![
                    AppEffect::JobAccepted(job.id),
                    AppEffect::Queue(crate::vectr::queue::QueueCommand::Admit(job.id)),
                ]
            }
            AppCommand::ReleaseWorkflow(id) => {
                self.workflows.remove(&id);
                self.update_active()?;
                self.manager.cleanup()?;
                vec![]
            }
            AppCommand::Cleanup => {
                self.manager.cleanup()?;
                vec![]
            }
            AppCommand::Queue(action) => vec![AppEffect::Queue(action.into())],
            AppCommand::TestConnection {
                request,
                origin,
                token,
            } => {
                crate::platform::windows::credentials::validate_token(&token)?;
                vec![AppEffect::TestConnection {
                    request,
                    origin: ServerOrigin::parse(&origin)?,
                    token,
                }]
            }
            AppCommand::TestSavedConnection {
                request,
                id,
                revision,
            } => {
                self.manager.ensure_revision(revision)?;
                let destination = resolve_destination(
                    DestinationSelection::Specific(id),
                    DestinationSelection::Inherit,
                    &self.settings().destinations,
                )?;
                let token = self.store.read(&destination.credential)?;
                vec![AppEffect::TestConnection {
                    request,
                    origin: destination.origin,
                    token,
                }]
            }
            AppCommand::CancelConnectionTest => vec![AppEffect::CancelConnectionTest],
            AppCommand::Quit => {
                self.lifecycle = Lifecycle::Draining;
                self.workflows.clear();
                self.update_active()?;
                vec![AppEffect::DrainWorker]
            }
        })
    }
    pub fn on_event(&mut self, event: AppEvent) -> Vec<AppEffect> {
        match event {
            AppEvent::WorkerStopped(result) => {
                self.lifecycle = Lifecycle::Exited;
                let mut effects = vec![];
                if let Err(error) = result {
                    effects.push(AppEffect::Error(error));
                }
                effects.push(AppEffect::Exit);
                effects
            }
            AppEvent::Queue(crate::vectr::queue::QueueEffect::Error(error)) => {
                vec![AppEffect::Error(error), AppEffect::RefreshQueue]
            }
            AppEvent::Queue(_) => vec![AppEffect::RefreshQueue],
            AppEvent::ConnectionTest { request, result }
                if self.lifecycle == Lifecycle::Running =>
            {
                vec![AppEffect::ConnectionTest { request, result }]
            }
            AppEvent::ConnectionTest { .. } => vec![],
        }
    }
}
