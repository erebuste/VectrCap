//! Single owner for workflow/job acceptance and settings/credential transactions.
//! There is no await point between reference inspection and durable commit.
use super::{
    destination_manager::{
        DestinationEdit, DestinationManager, DestinationReferences, DestinationRepository,
    },
    destinations::{DestinationId, DestinationSelection, DestinationSnapshot, resolve_destination},
    error::AppError,
    events::*,
    notifications::{
        CompletionNotice, CompletionStatus, JobCompletion, NoticeView, NotificationCenter,
        RequestedCompletionAction,
    },
    types::{Artifact, ArtifactKind},
};
use crate::{
    persistence::{
        artifacts::ArtifactStore,
        history::{History, JobStatus, UploadJob},
        settings::{ServerOrigin, Settings},
    },
    platform::{
        CredentialStore,
        windows::{clipboard::ClipboardWriter, shell_actions::ShellActions},
    },
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
/// One visible completion notice paired with its currently offered actions.
#[derive(Clone)]
pub struct NoticeDisplay {
    pub view: NoticeView,
    pub actions: Vec<RequestedCompletionAction>,
}
/// Fixed, non-exhaustive allowlist of inert, viewer-handled extensions; an
/// existing user file only offers Open when its (normalized) extension is in
/// this list. Everything else — including an unrecognized or ambiguous name —
/// defaults to reveal-only, never open/execute. A denylist of dangerous
/// extensions can never be exhaustive (new installer/script/shortcut types
/// keep appearing); default-deny is the only safe posture here.
const INERT_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "mp4", "webm", "mov", "mkv", "avi",
    "pdf", "txt", "md", "csv", "log", "json", "xml", "rtf", "wav", "mp3", "zip",
];
/// Retained only as a redundant second check behind the allowlist above.
#[allow(dead_code)]
const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "exe",
    "bat",
    "cmd",
    "com",
    "scr",
    "msi",
    "ps1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "msc",
    "jar",
    "reg",
    "lnk",
    "pif",
    "gadget",
    "application",
    "hta",
];
#[derive(Clone)]
pub struct BatchSummaryRow {
    pub id: super::types::JobId,
    pub job: Option<UploadJob>,
    pub local_available: bool,
}
pub struct AppController {
    batch_jobs: Vec<super::types::JobId>,
    history_query: crate::persistence::history::browser::HistoryQuery,
    manager: DestinationManager,
    history: Arc<History>,
    store: Arc<dyn CredentialStore>,
    artifacts: Arc<ArtifactStore>,
    references: Arc<References>,
    workflows: HashMap<Uuid, DestinationSnapshot>,
    notifications: NotificationCenter,
    shell: Arc<dyn ShellActions>,
    clipboard: Arc<dyn ClipboardWriter>,
    pub lifecycle: Lifecycle,
    pub window: Option<Window>,
}
impl AppController {
    pub fn open(
        repository: Box<dyn DestinationRepository>,
        history: Arc<History>,
        store: Arc<dyn CredentialStore>,
        artifacts: Arc<ArtifactStore>,
        shell: Arc<dyn ShellActions>,
        clipboard: Arc<dyn ClipboardWriter>,
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
        let mut notifications = NotificationCenter::new();
        notifications.set_notifications_enabled(manager.settings().notifications_enabled);
        Ok(Self {
            manager,
            history,
            store,
            artifacts,
            references,
            workflows: HashMap::new(),
            batch_jobs: vec![],
            history_query: Default::default(),
            notifications,
            shell,
            clipboard,
            lifecycle: Lifecycle::Running,
            window,
        })
    }
    pub fn history_page(
        &self,
    ) -> Result<crate::persistence::history::browser::HistoryPage, AppError> {
        self.history.browse(&self.history_query)
    }
    pub fn history_query(&self) -> &crate::persistence::history::browser::HistoryQuery {
        &self.history_query
    }
    pub fn settings(&self) -> &Settings {
        self.manager.settings()
    }
    /// Resolve exactly the selected IDs, never the bounded queue snapshot.
    pub fn batch_summary(&self) -> Result<Vec<BatchSummaryRow>, AppError> {
        self.batch_jobs
            .iter()
            .map(|id| {
                let job = self.history.job(*id)?;
                let local_available = job.as_ref().is_some_and(|j| j.artifact.path.is_file());
                Ok(BatchSummaryRow {
                    id: *id,
                    job,
                    local_available,
                })
            })
            .collect()
    }
    fn show_batch_summary(
        &mut self,
        jobs: Vec<super::types::JobId>,
    ) -> Result<Vec<AppEffect>, AppError> {
        let mut seen = HashSet::new();
        if jobs.is_empty()
            || jobs.len() > super::notifications::MAX_BATCH_ITEMS
            || jobs.iter().any(|id| id.0.is_nil() || !seen.insert(*id))
        {
            return Err(AppError::InvalidInput(
                "batch selection must contain 1 to 100 distinct job IDs",
            ));
        }
        // Validate reads before changing the active selection. Missing records
        // retain their place and are explicitly rendered as unavailable.
        for id in &jobs {
            self.history.job(*id)?;
        }
        self.batch_jobs = jobs;
        self.window = Some(Window::BatchSummary);
        Ok(vec![
            AppEffect::Window(self.window),
            AppEffect::CancelConnectionTest,
        ])
    }
    pub fn active_workflow_count(&self) -> usize {
        self.workflows.len()
    }
    pub fn visible_notices(&self) -> Vec<NoticeDisplay> {
        self.notifications
            .visible()
            .into_iter()
            .map(|view| {
                let actions = self.notifications.actions(view.id()).unwrap_or_default();
                NoticeDisplay { view, actions }
            })
            .collect()
    }
    pub fn notifications_enabled(&self) -> bool {
        self.notifications.notifications_enabled()
    }
    /// Non-`ArtifactKind::File` artifacts are the app's own encoder output and
    /// always safe; an existing user-selected file is reveal-only when its
    /// name has an executable-shaped extension.
    fn is_safe_to_open(artifact: &Artifact) -> bool {
        if artifact.kind != ArtifactKind::File {
            return true;
        }
        // Windows strips trailing dots/spaces from a filename before
        // deciding how to launch it; Path::extension() does not, so an
        // unnormalized name could hide a dangerous extension behind what
        // Path sees as "no extension" (which this function must not treat
        // as safe). Reject embedded separators/colons outright: this is a
        // display name, never a path component.
        let trimmed = artifact.display_name.trim_end_matches(['.', ' ']);
        if trimmed.is_empty() || trimmed.contains(['/', '\\', ':']) {
            return false;
        }
        let Some(extension) = std::path::Path::new(trimmed)
            .extension()
            .and_then(|e| e.to_str())
        else {
            return false;
        };
        INERT_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(extension))
    }
    /// Re-reads the job fresh from History and confirms its current state
    /// still supports `action`, independent of whatever the notice snapshot
    /// said when it was recorded.
    fn resolve_for_action(
        &self,
        action: &RequestedCompletionAction,
        id: crate::app::types::JobId,
    ) -> Result<UploadJob, AppError> {
        let job = self.history.job(id)?.ok_or(AppError::InvalidInput(
            "completion action is no longer available",
        ))?;
        let ok = match action {
            RequestedCompletionAction::OpenLocal(_) | RequestedCompletionAction::ShowFolder(_) => {
                job.artifact.path.exists()
            }
            RequestedCompletionAction::OpenLink(_)
            | RequestedCompletionAction::CopyLink(_)
            | RequestedCompletionAction::CopyAgain(_) => {
                matches!(job.status, JobStatus::Completed(_))
            }
            RequestedCompletionAction::OpenBatchSummary { .. }
            | RequestedCompletionAction::CopySuccessfulLinks { .. } => true,
        };
        if !ok {
            return Err(AppError::InvalidInput(
                "completion action is no longer available",
            ));
        }
        Ok(job)
    }
    fn execute_completion_action(
        &mut self,
        notice: crate::app::notifications::NoticeId,
        action: RequestedCompletionAction,
    ) -> Result<Vec<AppEffect>, AppError> {
        match &action {
            RequestedCompletionAction::OpenLocal(id) => {
                let job = self.resolve_for_action(&action, *id)?;
                self.shell.open_path(&job.artifact.path)?;
            }
            RequestedCompletionAction::ShowFolder(id) => {
                let job = self.resolve_for_action(&action, *id)?;
                self.shell.reveal_path(&job.artifact.path)?;
            }
            RequestedCompletionAction::OpenLink(id) => {
                let job = self.resolve_for_action(&action, *id)?;
                let JobStatus::Completed(receipt) = &job.status else {
                    return Err(AppError::InvalidInput(
                        "completion action is no longer available",
                    ));
                };
                self.shell.open_url(receipt.url())?;
            }
            RequestedCompletionAction::CopyLink(id) | RequestedCompletionAction::CopyAgain(id) => {
                let job = self.resolve_for_action(&action, *id)?;
                let JobStatus::Completed(receipt) = &job.status else {
                    return Err(AppError::InvalidInput(
                        "completion action is no longer available",
                    ));
                };
                self.clipboard.write_text(receipt.url()).inspect_err(|_| {
                    // The receipt itself is still valid; only the clipboard
                    // write failed, so this must offer a retry, not vanish.
                    let _ = self.notifications.note_copy_failed(notice, *id);
                })?;
            }
            RequestedCompletionAction::OpenBatchSummary { jobs } => {
                return self.show_batch_summary(jobs.clone());
            }
            RequestedCompletionAction::CopySuccessfulLinks { jobs } => {
                let mut links = Vec::new();
                for id in jobs {
                    let job = self.resolve_for_action(&action, *id)?;
                    if let JobStatus::Completed(receipt) = &job.status {
                        links.push(receipt.url().to_owned());
                    }
                }
                self.clipboard.write_text(&links.join("\n"))?;
            }
        }
        Ok(vec![])
    }
    fn completion_for(&self, job: &UploadJob) -> Result<Option<CompletionNotice>, AppError> {
        let local_available = job.artifact.path.exists();
        let safe_to_open = local_available && Self::is_safe_to_open(&job.artifact);
        let notice = match &job.status {
            JobStatus::Completed(receipt) => Some(CompletionNotice::Job(JobCompletion::try_new(
                job.id,
                &job.artifact.display_name,
                local_available,
                safe_to_open,
                Some(receipt.clone()),
                CompletionStatus::uploaded(),
            )?)),
            JobStatus::Failed { code } => Some(CompletionNotice::Job(JobCompletion::try_new(
                job.id,
                &job.artifact.display_name,
                local_available,
                safe_to_open,
                None,
                CompletionStatus::failed(code)?,
            )?)),
            _ => None,
        };
        Ok(notice)
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
            AppCommand::QueryHistory(query) => {
                self.history.browse(&query)?;
                self.history_query = query;
                vec![]
            }
            AppCommand::SetHistoryFavorite { id, favorite } => {
                self.history.set_favorite(id, favorite)?;
                vec![]
            }
            AppCommand::ShowBatchSummary(jobs) => self.show_batch_summary(jobs)?,
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
                self.manager.ensure_not_corrupted()?;
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
                self.manager.ensure_not_corrupted()?;
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
            AppCommand::DismissNotice(id) => {
                self.notifications.dismiss(id)?;
                vec![]
            }
            AppCommand::SetNotificationsEnabled(enabled) => {
                self.manager.set_notifications_enabled(enabled)?;
                self.notifications.set_notifications_enabled(enabled);
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::ExecuteCompletionAction { notice, action } => {
                self.notifications.request(notice, &action)?;
                self.execute_completion_action(notice, action)?
            }
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
            AppEvent::Queue(crate::vectr::queue::QueueEffect::Changed(id)) => {
                let mut effects = vec![];
                if let Ok(Some(job)) = self.history.job(id) {
                    match self.completion_for(&job) {
                        Ok(Some(notice)) => match self.notifications.record(notice) {
                            Ok(_) => {}
                            // An already-visible duplicate for this job is
                            // expected and benign; any other failure is real.
                            Err(AppError::InvalidInput(_)) => {}
                            Err(error) => effects.push(AppEffect::Error(error)),
                        },
                        Ok(None) => {}
                        Err(error) => effects.push(AppEffect::Error(error)),
                    }
                }
                effects.push(AppEffect::RefreshQueue);
                effects
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
#[cfg(test)]
mod tests {
    use super::*;
    fn file_artifact(display_name: &str) -> Artifact {
        Artifact {
            id: crate::app::types::ArtifactId::new_random(),
            path: std::path::PathBuf::from("spool/synthetic"),
            display_name: display_name.into(),
            kind: ArtifactKind::File,
            size: 9,
            sha256: [0u8; 32],
        }
    }
    #[test]
    fn unrecognized_extension_defaults_to_reveal_only() {
        // A denylist can never enumerate every dangerous extension (.scf,
        // .cpl, .iso, .msix, ...); the classifier must default-deny.
        assert!(!AppController::is_safe_to_open(&file_artifact("evil.scf")));
    }
    #[test]
    fn trailing_dot_disguised_executable_is_reveal_only() {
        // Windows strips trailing dots/spaces from a filename before
        // treating it as executable; Path::extension() does not, so a name
        // ending "exe." must still classify as unsafe, not as "no extension".
        assert!(!AppController::is_safe_to_open(&file_artifact("evil.exe.")));
        assert!(!AppController::is_safe_to_open(&file_artifact("evil.exe ")));
    }
    #[test]
    fn known_inert_extension_is_openable() {
        assert!(AppController::is_safe_to_open(&file_artifact("photo.png")));
        assert!(AppController::is_safe_to_open(&file_artifact("report.pdf")));
    }
    #[test]
    fn non_file_kind_is_always_safe() {
        let mut artifact = file_artifact("capture.png");
        artifact.kind = ArtifactKind::Screenshot;
        assert!(AppController::is_safe_to_open(&artifact));
    }
}
