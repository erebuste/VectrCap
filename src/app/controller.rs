//! Single owner for workflow/job acceptance and settings/credential transactions.
//! There is no await point between reference inspection and durable commit.
mod capture;
pub mod ingress;
mod recording;
mod scrolling;
mod task_chains;
mod utility;
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
    shortcuts::{ShortcutEvent, ShortcutFocus, ShortcutManager, ShortcutOutcome},
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
pub use capture::CaptureSurface;
pub use recording::{RecordingSelection, RecordingView};
pub use scrolling::ScrollingView;
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
        if candidate.shortcuts.bindings.iter().any(|binding| matches!(binding.destination, DestinationSelection::Specific(id) if candidate.destinations.get(id).is_none())) {
            return Err(AppError::InvalidInput("repair shortcut destination references before removal"));
        }
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
    fn set_configured_shortcuts(
        &self,
        bindings: &[super::shortcuts::ShortcutBinding],
    ) -> Result<(), AppError> {
        let mut refs = self
            .0
            .lock()
            .map_err(|_| AppError::Persistence("configured references unavailable"))?;
        refs.configured
            .retain(|key, _| !key.starts_with("shortcut:"));
        for binding in bindings {
            if let DestinationSelection::Specific(id) = binding.destination {
                refs.configured
                    .insert(format!("shortcut:{:?}", binding.action), id);
            }
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
/// Tracks one accepted native-file-picker batch from acceptance through its
/// last upload settlement, so every file's completion can be grouped into a
/// single notice instead of one toast per file.
struct FileBatch {
    results: super::batches::UploadBatch,
    task_context: crate::workflow::runner::WorkflowContext,
    /// Paths not yet staged (succeeded or failed). The destination snapshot
    /// in `workflows` is released once this reaches zero, independent of
    /// whether any created jobs have finished uploading yet.
    staging_remaining: usize,
    staging_done: Vec<bool>,
    /// Created jobs not yet in a terminal state for notification purposes.
    jobs_remaining: usize,
    /// One slot per originally selected path, in picker order; filled in as
    /// each job resolves (truthfully, never fabricated) regardless of the
    /// order staging/settlement actually complete in. A rejected file (no
    /// job was ever created) keeps its slot `None` forever; it is still
    /// surfaced to the user via `failed_uploads`, just not folded into this
    /// per-job grouped notice.
    completions: Vec<Option<JobCompletion>>,
}
struct DeferredImport {
    preview: crate::persistence::settings_transfer::ImportPreview,
    selection: crate::persistence::settings_transfer::ImportSelection,
    reply: std::sync::mpsc::SyncSender<Result<(), AppError>>,
}
pub struct AppController {
    diagnostics: crate::diagnostics::checks::DiagnosticService,
    deferred_import: Option<DeferredImport>,
    ingress: super::ingress::Ingress,
    install: Option<(Uuid, super::destination_install::DestinationInstallSession)>,
    task_runners: HashMap<Uuid, crate::workflow::runner::TaskRunner>,
    task_images: HashMap<Uuid, crate::ui::image_export::ImageFinish>,
    task_batches: HashMap<Uuid, (Uuid, usize)>,
    accepted_task_contexts: HashMap<Uuid, crate::workflow::runner::WorkflowContext>,

    // Worker ownership outlives a canceled selection/surface until acquisition
    // actually settles. A canceled worker must not overlap a replacement tool.
    pending_acquisitions: HashSet<Uuid>,
    utility: Option<utility::UtilityWorkflow>,
    capture: Option<capture::CaptureWorkflow>,
    scrolling: Option<scrolling::ScrollingWorkflow>,
    recording: Option<recording::RecordingWorkflow>,
    recording_origin: std::time::Instant,
    pins: crate::pin::PinManager,
    pin_load: Option<Uuid>,
    batch_jobs: Vec<super::types::JobId>,
    history_query: crate::persistence::history::browser::HistoryQuery,
    manager: DestinationManager,
    history: Arc<History>,
    store: Arc<dyn CredentialStore>,
    artifacts: Arc<ArtifactStore>,
    references: Arc<References>,
    workflows: HashMap<Uuid, DestinationSnapshot>,
    batches: HashMap<Uuid, FileBatch>,
    /// Each batch member's batch id and its original picker-order index.
    job_batch: HashMap<super::types::JobId, (Uuid, usize)>,
    notifications: NotificationCenter,
    shell: Arc<dyn ShellActions>,
    clipboard: Arc<dyn ClipboardWriter>,
    /// `None` until `AttachHotkeyWindow` arrives: native registration needs
    /// the tray's real window handle, which does not exist until the UI
    /// thread creates it (just after this controller starts). The window
    /// between app start and that attach is a few milliseconds; every
    /// shortcut-dependent command degrades to a harmless no-op/busy error
    /// during it rather than panicking or guessing a handle.
    shortcuts: Option<ShortcutManager>,
    shortcut_entry_active: Option<Uuid>,
    pub lifecycle: Lifecycle,
    pub window: Option<Window>,
    /// What History should bring into view; cleared when another page is shown.
    pub history_focus: Option<super::events::HistoryFocus>,
}
impl AppController {
    /// Opens History with `focus` in view (replaces the old Uploads, Task
    /// results and Batch summary pages).
    pub(super) fn focus_history(&mut self, focus: super::events::HistoryFocus) -> AppEffect {
        self.window = Some(Window::History);
        self.history_focus = Some(focus);
        AppEffect::Window(self.window)
    }
    pub fn open(
        repository: Box<dyn DestinationRepository>,
        history: Arc<History>,
        store: Arc<dyn CredentialStore>,
        artifacts: Arc<ArtifactStore>,
        shell: Arc<dyn ShellActions>,
        clipboard: Arc<dyn ClipboardWriter>,
    ) -> Result<Self, AppError> {
        let references = Arc::new(References::default());
        let ingress = super::ingress::Ingress::open(
            &artifacts
                .root()
                .parent()
                .ok_or(AppError::Persistence("ingress root missing"))?
                .join("ingress.db"),
        )?;
        references
            .0
            .lock()
            .map_err(|_| AppError::Persistence("references unavailable"))?
            .active = ingress
            .pending()?
            .into_iter()
            .filter_map(|p| p.destination.map(|d| d.id))
            .collect();
        let manager = DestinationManager::open(
            repository,
            store.clone(),
            history.clone(),
            references.clone(),
        )?;
        let recovered = history.task_workflows()?;
        let window = manager
            .settings()
            .destinations
            .is_empty()
            .then_some(Window::Settings);
        let mut notifications = NotificationCenter::new();
        notifications.set_notifications_enabled(manager.settings().notifications_enabled);
        Ok(Self {
            deferred_import: None,
            ingress,
            install: None,
            task_runners: recovered.into_iter().map(|r| (r.context().id, r)).collect(),
            task_images: HashMap::new(),
            task_batches: HashMap::new(),
            accepted_task_contexts: HashMap::new(),
            pending_acquisitions: HashSet::new(),
            utility: None,
            capture: None,
            scrolling: None,
            recording: None,
            recording_origin: std::time::Instant::now(),
            pins: crate::pin::PinManager::with_defaults(manager.settings().pin.clone())?,
            pin_load: None,
            manager,
            diagnostics: Default::default(),
            history,
            store,
            artifacts,
            references,
            workflows: HashMap::new(),
            batches: HashMap::new(),
            job_batch: HashMap::new(),
            batch_jobs: vec![],
            history_query: Default::default(),
            notifications,
            shell,
            clipboard,
            shortcuts: None,
            shortcut_entry_active: None,
            lifecycle: Lifecycle::Running,
            window,
            history_focus: None,
        })
    }
    pub fn startup_effects(&self) -> Vec<AppEffect> {
        vec![AppEffect::ArchiveMaintenance(
            super::image_finish::ArchiveMaintenance::new(
                self.history.clone(),
                self.settings().archive.clone(),
                self.settings().filename.clone(),
            ),
        )]
    }
    /// Bootstrap through an injected registrar, shared by native startup and tests.
    pub fn initialize_shortcuts(
        &mut self,
        registrar: Box<dyn super::shortcuts::ShortcutRegistrar>,
    ) -> Vec<AppEffect> {
        if self.shortcuts.is_some() {
            return vec![];
        }
        let (manager, _) = ShortcutManager::new(registrar, self.settings().shortcuts.effective());
        let statuses = super::shortcuts::ShortcutAction::all()
            .iter()
            .map(|&action| (action, manager.status(action)))
            .collect();
        self.shortcuts = Some(manager);
        vec![AppEffect::ShortcutStatuses(statuses)]
    }
    fn finish_deferred_import(&mut self) -> Vec<AppEffect> {
        if self.deferred_import.is_none() {
            return vec![];
        }
        if self.lifecycle == Lifecycle::Running
            && (self.capture.is_some()
                || self.recording.is_some()
                || self.scrolling.is_some()
                || self.utility.is_some()
                || !self.pending_acquisitions.is_empty())
        {
            return vec![];
        }
        let pending = self.deferred_import.take().expect("checked pending import");
        let result = if self.lifecycle != Lifecycle::Running {
            Err(AppError::Cancelled)
        } else {
            match &mut self.shortcuts {
                Some(shortcuts) => crate::persistence::settings_transfer::SettingsTransfer::new(
                    &mut self.manager,
                    shortcuts,
                    &crate::persistence::settings_transfer::NativeImportAudioResolver,
                )
                .apply_import(pending.preview, pending.selection),
                None => Err(AppError::InUse(
                    "hotkeys are still initializing; refresh and try again",
                )),
            }
        };
        // A closed feedback channel must never turn a committed import into failure.
        let _ = pending.reply.try_send(result.clone());
        match result {
            Ok(()) => {
                self.notifications
                    .set_notifications_enabled(self.settings().notifications_enabled);
                self.pins.set_defaults(self.settings().pin.clone());
                let statuses = super::shortcuts::ShortcutAction::all()
                    .iter()
                    .map(|&action| {
                        (
                            action,
                            self.shortcuts.as_ref().expect("initialized").status(action),
                        )
                    })
                    .collect();
                vec![
                    AppEffect::ShortcutStatuses(statuses),
                    AppEffect::SettingsSaved(self.settings().clone()),
                ]
            }
            Err(error) => vec![AppEffect::Error(error)],
        }
    }
    pub fn shortcuts(&self) -> Option<&ShortcutManager> {
        self.shortcuts.as_ref()
    }
    pub fn pins(&self) -> &crate::pin::PinManager {
        &self.pins
    }
    fn load_pin(
        &mut self,
        source: crate::pin::source::PinLoad,
    ) -> Result<Vec<AppEffect>, AppError> {
        if self.pin_load.is_some() {
            return Err(AppError::InUse("a pin image is still loading"));
        }
        if self.pins.snapshots().len() >= crate::pin::manager::MAX_PINS {
            return Err(AppError::InUse(
                "at most 16 pin windows may be open; close a pin first",
            ));
        }
        let request = Uuid::new_v4();
        self.pin_load = Some(request);
        Ok(vec![
            AppEffect::PinSourceChooser(false),
            AppEffect::LoadPin {
                request,
                source,
                clipboard: self.clipboard.clone(),
            },
        ])
    }
    fn pin_effects(&self, effects: Vec<crate::pin::PinEffect>) -> Result<Vec<AppEffect>, AppError> {
        let mut output = vec![];
        for effect in effects {
            match effect {
                crate::pin::PinEffect::CopyImage(pixels) => self.clipboard.write_image(&pixels)?,
                effect => output.push(AppEffect::Pin(effect)),
            }
        }
        Ok(output)
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
                let local_available = job.as_ref().is_some_and(|j| {
                    crate::persistence::history::browser::local_available_path(&j.artifact.path)
                });
                Ok(BatchSummaryRow {
                    id: *id,
                    job,
                    local_available,
                })
            })
            .collect()
    }
    fn copy_batch_links(&self) -> Result<Vec<AppEffect>, AppError> {
        let links: Vec<String> = self
            .batch_summary()?
            .into_iter()
            .filter_map(|row| match row.job?.status {
                JobStatus::Completed(receipt) => Some(receipt.url().to_string()),
                _ => None,
            })
            .collect();
        if links.is_empty() {
            return Err(AppError::InvalidInput("no confirmed links in this batch"));
        }
        self.clipboard.write_text(&links.join("\n"))?;
        Ok(vec![])
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
        let first = jobs[0];
        self.batch_jobs = jobs;
        Ok(vec![
            self.focus_history(super::events::HistoryFocus::Batch(first)),
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
                let extension = Self::viewer_extension(&job.artifact).ok_or(
                    AppError::InvalidInput("this file can only be revealed in its folder"),
                )?;
                self.shell.open_document(&job.artifact.path, extension)?;
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
    /// Accepts a confirmed native-picker selection (1 to 100 paths). Resolves
    /// the default destination exactly once for the whole batch, reads its
    /// credential eagerly so a missing/broken setup fails here with no file
    /// touched, then hands every path to the shell for bounded off-thread
    /// staging. `MAX_BATCH_ITEMS` also bounds `CompletionNotice::Batch`.
    fn accept_file_selection(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        selection: DestinationSelection,
    ) -> Result<Vec<AppEffect>, AppError> {
        let request = super::cli::LaunchRequest::upload(
            paths.into_iter().map(|p| p.into_os_string()).collect(),
        )?;
        self.accept_launch_for(
            request,
            selection,
            Some(super::shortcuts::ShortcutAction::UploadFile),
        )
        .map(|(_, effects)| effects)
    }
    /// Creates the durable job for one successfully staged batch file,
    /// reusing the batch's single pre-resolved destination snapshot. The
    /// artifact arrives verified by the staging worker. A configured review
    /// can delay admission, so the shared upload effect revalidates immutable
    /// source integrity at the final admission boundary.
    fn accept_batch_artifact(
        &mut self,
        batch: Uuid,
        index: usize,
        artifact: &Artifact,
    ) -> Result<Vec<AppEffect>, AppError> {
        self.manager.ensure_not_corrupted()?;
        let mut context = self
            .batches
            .get(&batch)
            .ok_or(AppError::InvalidInput("unknown accepted batch"))?
            .task_context
            .clone();
        context.id = ingress::member_id(batch, index);
        context.batch_id = Some(batch);
        context.kind = artifact.kind;
        context.intent = crate::workflow::runner::TaskIntent::Direct(
            crate::workflow::profile::CaptureTask::Upload,
        );
        let id = context.id;
        let mut runner = crate::workflow::runner::TaskRunner::new(context);
        runner.accept_finalized(Arc::new(artifact.clone()), None);
        self.task_batches.insert(id, (batch, index));
        if let Some(state) = self.batches.get_mut(&batch) {
            state.jobs_remaining += 1;
        }
        match self.launch_tasks(runner, None) {
            Ok(mut effects) => {
                if self
                    .task_runners
                    .get(&id)
                    .is_some_and(|r| r.context().upload_job.is_none())
                {
                    effects.push(self.focus_history(super::events::HistoryFocus::Workflow(id)));
                }
                Ok(effects)
            }
            Err(e) => {
                self.task_batches.remove(&id);
                if let Some(state) = self.batches.get_mut(&batch) {
                    state.jobs_remaining = state.jobs_remaining.saturating_sub(1);
                }
                Err(e)
            }
        }
    }
    /// One path from an accepted batch finished staging, successfully or not.
    fn file_staged(
        &mut self,
        batch: Uuid,
        index: usize,
        display_name: String,
        outcome: Result<Artifact, AppError>,
    ) -> Vec<AppEffect> {
        let valid = self
            .batches
            .get(&batch)
            .and_then(|state| state.staging_done.get(index))
            .is_some_and(|done| !*done);
        if !valid {
            let mut effects = vec![AppEffect::Error(AppError::InvalidInput(
                "unknown or duplicate staging result",
            ))];
            if let Ok(artifact) = outcome
                && let Err(error) = self.rollback_staged(&artifact)
            {
                effects.push(AppEffect::Error(error));
            }
            return effects;
        }
        let mut effects = vec![];
        let mut rejected = false;
        match outcome {
            Ok(artifact) => match self.accept_batch_artifact(batch, index, &artifact) {
                Ok(out) => effects.extend(out),
                Err(error) => {
                    rejected = true;
                    effects.push(AppEffect::FileRejected {
                        display_name,
                        error,
                    });
                    if let Err(error) = self.rollback_staged(&artifact) {
                        effects.push(AppEffect::Error(error));
                    }
                }
            },
            Err(error) => {
                rejected = true;
                effects.push(AppEffect::FileRejected {
                    display_name,
                    error,
                });
            }
        }
        if rejected {
            if let Err(error) = self.ingress.reject_slot(batch, index) {
                effects.push(AppEffect::Error(error));
                return effects;
            }
            if let Some(state) = self.batches.get_mut(&batch)
                && let Err(error) = state
                    .results
                    .resolve(index, super::batches::BatchOutcome::Failed)
            {
                effects.push(AppEffect::Error(error));
            }
        }
        if let Some(state) = self.batches.get_mut(&batch) {
            state.staging_done[index] = true;
            state.staging_remaining = state.staging_remaining.saturating_sub(1);
            if state.staging_remaining == 0 {
                self.workflows.remove(&batch);
                if let Err(error) = self.update_active() {
                    effects.push(AppEffect::Error(error));
                }
                if let Err(error) = self.manager.cleanup() {
                    effects.push(AppEffect::Error(error));
                }
            }
        }
        self.maybe_flush_batch(batch, &mut effects);
        effects
    }
    fn rollback_staged(&self, artifact: &Artifact) -> Result<(), AppError> {
        if !self.history.artifact_is_recorded(artifact.id)?
            && !self
                .history
                .artifacts_needing_spool()?
                .contains(&artifact.id)
        {
            self.artifacts.discard_staged(artifact)?;
        }
        Ok(())
    }
    /// Removes `job` from its batch's pending count (if any) and, once
    /// staging and every created job have resolved, emits one grouped
    /// completion notice for the whole batch instead of one per job.
    fn resolve_batch_job(
        &mut self,
        job: super::types::JobId,
        completion: Option<JobCompletion>,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        let Some(&(batch, index)) = self.job_batch.get(&job) else {
            return false;
        };
        let outcome = match self.history.job(job) {
            Ok(Some(record)) => match record.status {
                JobStatus::Completed(receipt) => {
                    super::batches::BatchOutcome::Success(receipt.url().into())
                }
                JobStatus::Uncertain => super::batches::BatchOutcome::Uncertain,
                JobStatus::Cancelled => super::batches::BatchOutcome::Cancelled,
                JobStatus::Failed { .. } => super::batches::BatchOutcome::Failed,
                _ => return true,
            },
            Ok(None) => {
                effects.push(AppEffect::Error(AppError::Persistence(
                    "batch job is missing",
                )));
                return true;
            }
            Err(error) => {
                effects.push(AppEffect::Error(error));
                return true;
            }
        };
        if let Some(state) = self.batches.get_mut(&batch) {
            if let Err(error) = state.results.resolve(index, outcome) {
                effects.push(AppEffect::Error(error));
                return true;
            }
            state.jobs_remaining = state.jobs_remaining.saturating_sub(1);
            if let Some(completion) = completion
                && let Some(slot) = state.completions.get_mut(index)
            {
                *slot = Some(completion);
            }
        }
        self.job_batch.remove(&job);
        self.maybe_flush_batch(batch, effects);
        true
    }
    /// Truthfully closes one batch member's slot when it reaches a state
    /// that will never produce a receipt on its own (cancelled, uncertain,
    /// or superseded by an explicit restart) — never a fabricated success,
    /// and never an automatic restart. Without this, one stuck slot would
    /// hold every successful sibling's notice forever (`jobs_remaining`
    /// never reaches zero). A standalone (non-batch) job in these states
    /// still shows nothing: this only ever touches batch bookkeeping.
    fn resolve_batch_slot(
        &mut self,
        job: super::types::JobId,
        status: CompletionStatus,
        effects: &mut Vec<AppEffect>,
    ) {
        if !self.job_batch.contains_key(&job) {
            return;
        }
        let completion = match self.history.job(job) {
            Ok(Some(record)) => {
                let local_available = record.artifact.path.exists();
                let safe_to_open = local_available && Self::is_safe_to_open(&record.artifact);
                match JobCompletion::try_new(
                    job,
                    &record.artifact.display_name,
                    local_available,
                    safe_to_open,
                    None,
                    status,
                ) {
                    Ok(completion) => Some(completion),
                    Err(error) => {
                        effects.push(AppEffect::Error(error));
                        None
                    }
                }
            }
            Ok(None) => None,
            Err(error) => {
                effects.push(AppEffect::Error(error));
                None
            }
        };
        self.resolve_batch_job(job, completion, effects);
    }
    fn maybe_flush_batch(&mut self, batch: Uuid, effects: &mut Vec<AppEffect>) {
        let done = self
            .batches
            .get(&batch)
            .is_some_and(|state| state.staging_remaining == 0 && state.jobs_remaining == 0);
        if !done {
            return;
        }
        // Commit the once-only batch settlement before external clipboard work.
        // A clipboard failure keeps links in History for explicit retry.
        if let Err(error) = self.ingress.complete(batch) {
            effects.push(AppEffect::Error(error));
            return;
        }
        let Some(mut state) = self.batches.remove(&batch) else {
            return;
        };
        self.workflows.remove(&batch);
        if let Err(error) = self.update_active() {
            effects.push(AppEffect::Error(error));
        }
        if let Some(text) = state.results.take_automatic_copy()
            && let Err(error) = self.clipboard.write_text(&text)
        {
            effects.push(AppEffect::Error(error));
        }
        // The slot count is the ORIGINAL picker-selection size; a selection
        // of exactly one file gets an ordinary single-job notice instead of
        // a one-item "batch", regardless of how many siblings were rejected
        // during staging (there are none when the slot count is 1).
        let single_selection = state.completions.len() == 1;
        let completions: Vec<JobCompletion> = state.completions.into_iter().flatten().collect();
        let notice = match (single_selection, completions.len()) {
            (_, 0) => return,
            (true, 1) => CompletionNotice::Job(completions.into_iter().next().unwrap()),
            _ => CompletionNotice::Batch(completions),
        };
        match self.notifications.record(notice) {
            Ok(_) => {}
            // An already-visible duplicate is benign; see the single-job path below.
            Err(AppError::InvalidInput(_)) => {}
            Err(error) => effects.push(AppEffect::Error(error)),
        }
    }
    pub fn dispatch(&mut self, command: AppCommand) -> Vec<AppEffect> {
        let mut effects = self.dispatch_inner(command);
        effects.extend(self.finish_deferred_import());
        effects
    }
    fn dispatch_inner(&mut self, command: AppCommand) -> Vec<AppEffect> {
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
        let pin_source = matches!(
            &command,
            AppCommand::PinSource(_)
                | AppCommand::LoadPinFile(_)
                | AppCommand::HistoryAction {
                    action: HistoryAction::Pin,
                    ..
                }
        );
        self.apply(command)
            .unwrap_or_else(|error| match connection_request {
                Some(request) => vec![AppEffect::ConnectionTest {
                    request,
                    result: Err(error),
                }],
                None if pin_source => {
                    vec![AppEffect::PinSourceChooser(true), AppEffect::Error(error)]
                }
                None => vec![AppEffect::Error(error)],
            })
    }
    fn update_active(&self) -> Result<(), AppError> {
        self.references
            .0
            .lock()
            .map_err(|_| AppError::Persistence("workflow references unavailable"))?
            .active = self
            .workflows
            .values()
            .map(|s| s.id)
            .chain(
                self.ingress
                    .pending()?
                    .into_iter()
                    .filter_map(|p| p.destination.map(|d| d.id)),
            )
            .chain(
                self.task_runners
                    .values()
                    .filter(|r| !r.complete())
                    .filter_map(|r| r.context().destination.as_ref().map(|d| d.id)),
            )
            .collect();
        Ok(())
    }
    fn history_action(
        &mut self,
        ids: Vec<Uuid>,
        action: HistoryAction,
    ) -> Result<Vec<AppEffect>, AppError> {
        use crate::persistence::history::browser::validate_selection;
        validate_selection(&ids)?;
        let rows = ids
            .iter()
            .map(|id| {
                self.history
                    .history_record(*id)?
                    .ok_or(AppError::InvalidInput("history item no longer available"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        match action {
            HistoryAction::Pin => {
                if rows.len() != 1 {
                    return Err(AppError::InvalidInput("select one image to pin"));
                }
                if !crate::pin::source::is_raster_name(&rows[0].name) {
                    return Err(AppError::InvalidInput(
                        "only still raster images can be pinned",
                    ));
                }
                return self.load_pin(crate::pin::source::PinLoad::History(
                    rows[0].artifact.clone(),
                ));
            }
            HistoryAction::Remove => self.history.remove_history(&ids)?,
            HistoryAction::RetryFailed => {
                let jobs = self.history.retry_history_jobs(&ids)?;
                if jobs.is_empty() {
                    return Err(AppError::InvalidInput(
                        "no eligible failed uploads selected",
                    ));
                }
                let queued = jobs.len();
                let skipped = ids.len() - queued;
                return Ok(vec![
                    AppEffect::Queue(crate::vectr::queue::QueueCommand::RetryJobs(jobs)),
                    AppEffect::HistoryRetryReport { queued, skipped },
                ]);
            }
            HistoryAction::CopyLinks => {
                let links: Vec<_> = rows
                    .iter()
                    .filter_map(|r| r.job.as_ref())
                    .filter_map(|j| {
                        if let JobStatus::Completed(receipt) = &j.status {
                            Some(receipt.url())
                        } else {
                            None
                        }
                    })
                    .collect();
                if links.is_empty() {
                    return Err(AppError::InvalidInput("no confirmed links selected"));
                }
                self.clipboard.write_text(&links.join("\n"))?;
            }
            _ => {
                if rows.len() != 1 {
                    return Err(AppError::InvalidInput("select one item for this action"));
                }
                return Ok(vec![AppEffect::LocalHistoryAction(
                    super::local_actions::LocalHistoryAction {
                        id: ids[0],
                        action,
                        history: self.history.clone(),
                        shell: self.shell.clone(),
                    },
                )]);
            }
        }
        Ok(vec![])
    }
    pub fn viewer_extension(artifact: &Artifact) -> Option<&'static str> {
        match artifact.kind {
            ArtifactKind::Screenshot => {
                let extension = std::path::Path::new(&artifact.display_name)
                    .extension()?
                    .to_str()?;
                ["png", "jpg", "jpeg", "webp"]
                    .into_iter()
                    .find(|known| known.eq_ignore_ascii_case(extension))
            }
            ArtifactKind::Gif => Some("gif"),
            ArtifactKind::Mp4 => Some("mp4"),
            ArtifactKind::File => {
                let extension = std::path::Path::new(&artifact.display_name)
                    .extension()?
                    .to_str()?;
                INERT_EXTENSIONS
                    .iter()
                    .copied()
                    .find(|known| known.eq_ignore_ascii_case(extension))
            }
        }
    }
    fn apply(&mut self, command: AppCommand) -> Result<Vec<AppEffect>, AppError> {
        Ok(match command {
            AppCommand::RefreshDiagnostics { reply } => {
                self.diagnostics.refresh(
                    self.history.clone(),
                    crate::diagnostics::checks::StorageRoots {
                        spool: self.artifacts.root().to_owned(),
                        archive: self.settings().archive.resolved_root().ok(),
                    },
                    self.settings().destinations.len() as u32,
                    reply,
                );
                vec![]
            }
            AppCommand::TestDiagnosticConnection {
                id,
                revision,
                cancel,
                reply,
            } => {
                let settings = self.settings();
                match self.manager.ensure_revision(revision).and_then(|()| {
                    settings
                        .destinations
                        .get(id)
                        .ok_or(AppError::NeedsSetup("select a destination"))
                }) {
                    Ok(d) => {
                        let origin = d.origin.clone();
                        let destination = resolve_destination(
                            DestinationSelection::Specific(id),
                            DestinationSelection::Inherit,
                            &settings.destinations,
                        );
                        self.diagnostics.connection(
                            origin,
                            destination,
                            Arc::new(crate::platform::CredentialClientFactory::new(
                                self.store.clone(),
                            )),
                            cancel,
                            reply,
                        );
                    }
                    Err(error) => {
                        let _ = reply.try_send(crate::diagnostics::report::ConnectionCheck {
                            uploader: crate::diagnostics::checks::uploader_result(Err(error)),
                            ..Default::default()
                        });
                    }
                }
                vec![]
            }
            AppCommand::ApplySettingsImport {
                preview,
                selection,
                reply,
            } => {
                let result = self.manager.ensure_revision(preview.base_revision());
                if let Err(error) = result {
                    let _ = reply.try_send(Err(error.clone()));
                    return Err(error);
                }
                if self.deferred_import.is_some() {
                    let error = AppError::InUse(
                        "a preferences import is already waiting for the active session",
                    );
                    let _ = reply.try_send(Err(error.clone()));
                    return Err(error);
                }
                self.deferred_import = Some(DeferredImport {
                    preview: *preview,
                    selection,
                    reply,
                });
                vec![]
            }
            AppCommand::StartUtility(kind) => self.start_utility(kind)?,
            AppCommand::Utility { session, input } => self.utility_input(session, input),
            AppCommand::InterruptUtility => {
                let mut effects = self.close_utility();
                if !effects.is_empty() {
                    effects.push(AppEffect::Error(AppError::Capture(
                        "utility closed after a display or session interruption",
                    )));
                }
                effects
            }
            AppCommand::PinFromScreenFor(selection) => {
                let mut effects = self.start_capture_profile(
                    crate::capture::request::CaptureMode::Region,
                    selection,
                    Some(super::shortcuts::ShortcutAction::PinFromScreen),
                )?;
                if let Some(capture) = &mut self.capture {
                    capture.intended_pin = true;
                }
                effects.push(AppEffect::PinSourceChooser(false));
                effects
            }
            AppCommand::SetTaskProfiles {
                tasks,
                archive,
                revision,
            } => {
                self.manager.ensure_revision(revision)?;
                self.manager.set_task_profiles(tasks, archive)?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::Task { id, command } => self.task_command(id, command)?,
            AppCommand::RunRecordingTasks => self.run_recording_tasks()?,
            AppCommand::ToggleScrolling => self.toggle_scrolling(DestinationSelection::Inherit)?,
            AppCommand::ToggleScrollingFor(selection) => self.toggle_scrolling(selection)?,
            AppCommand::ScrollingSelection { session, rect } => {
                match self.select_scrolling(session, rect) {
                    Ok(effects) => effects,
                    Err(error) => {
                        if let Some(surface) =
                            self.scrolling.as_ref().and_then(|w| w.selection.as_ref())
                            && let Ok(mut surface) = surface.lock()
                        {
                            surface.overlay.finish_failed(error.clone());
                        }
                        vec![AppEffect::Error(error)]
                    }
                }
            }
            AppCommand::ScrollingSelectionClosed(id) => self.scrolling_selection_closed(id),
            AppCommand::ScrollingControl(control) => {
                if matches!(control, crate::scrolling::runtime::ScrollControl::Cancel) {
                    self.cancel_scrolling()
                } else if let Some(w) = &self.scrolling {
                    if w.selection.is_some() {
                        self.cancel_scrolling()
                    } else {
                        vec![AppEffect::ScrollingControl(control)]
                    }
                } else {
                    vec![]
                }
            }
            AppCommand::StartAudioTest(options) => {
                options.validate()?;
                if self
                    .recording
                    .as_ref()
                    .is_some_and(|w| w.session.acquiring())
                {
                    return Err(AppError::InUse(
                        "audio tests are unavailable during recording",
                    ));
                }
                vec![AppEffect::StartAudioTest(options)]
            }
            AppCommand::StopAudioTest => vec![AppEffect::StopAudioTest],
            AppCommand::RefreshAudioEndpoints => vec![AppEffect::RefreshAudioEndpoints],
            AppCommand::CloseRecordingReview => self.close_recording_review()?,
            AppCommand::OpenRecordingPlayer => {
                if let Some(view) = self.recording_view()
                    && view.mp4
                    && let Some(path) = view.playback_path
                {
                    self.shell.open_document(&path, "mp4")?;
                }
                vec![]
            }
            AppCommand::ToggleRecordingFor { mp4, selection } => {
                self.toggle_recording(mp4, selection)?
            }
            AppCommand::ToggleRecording { mp4 } => {
                self.toggle_recording(mp4, DestinationSelection::Inherit)?
            }
            AppCommand::Recording(input) => self.recording_input(input)?,
            AppCommand::RecordingSelectionClosed(id) => {
                self.recording_selection_closed(id);
                vec![]
            }
            AppCommand::RecordingSelection { session, rect } => {
                match self.select_recording(session, rect) {
                    Ok(e) => e,
                    Err(error) => {
                        if let Some(w) = &self.recording
                            && let Some(surface) = &w.selection
                            && let Ok(mut s) = surface.lock()
                        {
                            s.overlay.finish_failed(error.clone());
                        }
                        return Err(error);
                    }
                }
            }
            AppCommand::RetryRecordingArchive { continue_without } => {
                self.retry_recording_archive(continue_without)?
            }
            AppCommand::SetRecordingPreferences {
                preferences,
                limits,
                revision,
            } => {
                self.manager.ensure_revision(revision)?;
                self.manager
                    .set_recording_preferences(preferences, limits)?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }

            AppCommand::StartCapture { mode, selection } => self.start_capture(mode, selection)?,
            AppCommand::FinishCapture {
                session,
                finish,
                save_path,
            } => self.finish_capture(session, Some(finish), save_path, Default::default()),
            AppCommand::RetryCaptureFinish { session, decision } => {
                self.finish_capture(session, None, None, decision)
            }
            AppCommand::CancelCapture(session) => self.cancel_capture(session),
            AppCommand::CancelActiveCapture => {
                if let Some(capture) = &self.capture {
                    self.cancel_capture(capture.id)
                } else {
                    vec![]
                }
            }
            AppCommand::ResumeCapture(session) => self.resume_capture(session)?,
            AppCommand::Pin { id, input } => {
                let effects = self.pins.handle(id, input)?;
                self.pin_effects(effects)?
            }
            AppCommand::CloseAllPins => {
                self.pin_load = None;
                self.pins
                    .close_all()
                    .into_iter()
                    .map(AppEffect::Pin)
                    .collect()
            }
            AppCommand::RefreshPinWorkAreas => {
                if self.pins.snapshots().is_empty() {
                    vec![]
                } else {
                    vec![AppEffect::RefreshPinWorkAreas]
                }
            }
            AppCommand::LoadPinFile(path) => {
                self.load_pin(crate::pin::source::PinLoad::File(path))?
            }
            AppCommand::PinSource(source) => {
                use crate::pin::source::{PinLoad, PinSource};
                match source {
                    PinSource::Chooser => vec![AppEffect::PinSourceChooser(true)],
                    PinSource::Clipboard => self.load_pin(PinLoad::Clipboard)?,
                    PinSource::File => {
                        vec![AppEffect::PinSourceChooser(false), AppEffect::PickPinFile]
                    }
                    PinSource::Screen => {
                        self.apply(AppCommand::PinFromScreenFor(DestinationSelection::Inherit))?
                    }
                }
            }
            AppCommand::DismissPinSourceChooser => vec![AppEffect::PinSourceChooser(false)],
            AppCommand::SetHistoryPreferences(preferences) => {
                self.manager.set_history_preferences(preferences)?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::HistoryAction { ids, action } => self.history_action(ids, action)?,
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
            AppCommand::CopyBatchLinks => self.copy_batch_links()?,
            AppCommand::Show(window) => {
                if self
                    .capture
                    .as_ref()
                    .is_some_and(|capture| capture.surface.is_none())
                {
                    return Err(AppError::InUse(
                        "wait for capture acquisition or cancel it first",
                    ));
                }
                self.shortcut_entry_active = None;
                self.window = Some(window);
                self.history_focus = None;
                let mut effects = vec![AppEffect::Window(self.window)];
                if window != Window::Settings {
                    effects.push(AppEffect::CancelConnectionTest);
                    effects.push(AppEffect::StopAudioTest);
                }
                effects
            }
            AppCommand::FocusHistory(focus) => vec![self.focus_history(focus)],
            AppCommand::ClearHistoryFocus => {
                self.history_focus = None;
                vec![]
            }
            AppCommand::CloseWindow => {
                let mut effects = self.close_utility();
                self.shortcut_entry_active = None;
                self.window = None;
                effects.extend([
                    AppEffect::Window(None),
                    AppEffect::CancelConnectionTest,
                    AppEffect::StopAudioTest,
                ]);
                effects
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
                let mut context = self.task_context(ArtifactKind::File, None, selection);
                context.id = id;
                let snapshot = context.destination.clone().ok_or(AppError::NeedsSetup(
                    "selected workflow destination needs setup",
                ))?;
                self.store.read(&snapshot.credential)?;
                self.accepted_task_contexts.insert(id, context);
                self.workflows.insert(id, snapshot);
                self.update_active()?;
                vec![AppEffect::WorkflowAccepted(id)]
            }
            AppCommand::AcceptWorkflow { id, artifact } => {
                self.manager.ensure_not_corrupted()?;
                self.artifacts.verify(&artifact)?;
                if !self.workflows.contains_key(&id) {
                    return Err(AppError::InvalidInput("unknown accepted workflow"));
                }
                let mut context = self
                    .accepted_task_contexts
                    .get(&id)
                    .cloned()
                    .ok_or(AppError::InvalidInput("workflow profile is missing"))?;
                context.kind = artifact.kind;
                context.intent = crate::workflow::runner::TaskIntent::Direct(
                    crate::workflow::profile::CaptureTask::Upload,
                );
                let mut runner = crate::workflow::runner::TaskRunner::new(context);
                runner.accept_finalized(Arc::new(artifact), None);
                let mut effects = self.launch_tasks(runner, None)?;
                self.accepted_task_contexts.remove(&id);
                self.workflows.remove(&id);
                if let Err(e) = self.update_active() {
                    effects.push(AppEffect::Error(e));
                }
                effects
            }
            AppCommand::ReleaseWorkflow(id) => {
                self.accepted_task_contexts.remove(&id);
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
            AppCommand::Launch { request, reply } => match self.accept_launch(request) {
                Ok((ack, effects)) => {
                    if let Some(reply) = reply {
                        let _ = reply.send(Ok(ack.request_id));
                    }
                    effects
                }
                Err(error) => {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err(error.clone()));
                    }
                    return Err(error);
                }
            },
            AppCommand::Install { request, command } => self.install_command(request, command)?,
            AppCommand::UploadSource(source) => self.accept_upload_source(source)?,
            AppCommand::UploadClipboard => vec![AppEffect::ReadUploadClipboard {
                selection: DestinationSelection::Inherit,
                action: None,
            }],
            AppCommand::UploadClipboardFor(selection) => vec![AppEffect::ReadUploadClipboard {
                selection,
                action: Some(super::shortcuts::ShortcutAction::UploadFromClipboard),
            }],
            AppCommand::UploadSourceFor {
                source,
                selection,
                action,
            } => self.accept_upload_source_for(source, selection, action)?,
            AppCommand::ResumeIngress => self.resume_ingress()?,
            AppCommand::CancelPendingIngress(id) => self.cancel_pending_ingress(id)?,
            AppCommand::AcceptFileSelection(paths) => {
                self.accept_file_selection(paths, DestinationSelection::Inherit)?
            }
            AppCommand::AcceptFileSelectionFor { paths, selection } => {
                self.accept_file_selection(paths, selection)?
            }
            AppCommand::AttachHotkeyWindow(hwnd) => self.initialize_shortcuts(Box::new(
                crate::platform::windows::hotkeys::Win32HotkeyBackend::new(hwnd),
            )),
            AppCommand::NativeHotkeyFired(id) => {
                if self.capture.is_some() || self.utility.is_some() {
                    return Ok(vec![]);
                }
                if let Some(request) = self.shortcut_entry_active
                    && let Some(shortcuts) = &self.shortcuts
                    && let Some(action) = shortcuts.action_for_id(id)
                    && let Some(chord) = shortcuts
                        .bindings()
                        .iter()
                        .find(|b| b.action == action)
                        .and_then(|b| b.chord)
                {
                    return Ok(vec![AppEffect::ShortcutRecorded { request, chord }]);
                }
                let outcome = self.shortcuts.as_ref().and_then(|shortcuts| {
                    let action = shortcuts.action_for_id(id)?;
                    let focus = if self.shortcut_entry_active.is_some() {
                        ShortcutFocus::ShortcutEntry
                    } else {
                        ShortcutFocus::Normal
                    };
                    shortcuts.handle(
                        ShortcutEvent {
                            action,
                            repeat: false,
                        },
                        focus,
                    )
                });
                match outcome {
                    Some(ShortcutOutcome::Command(cmd)) => self.apply(cmd)?,
                    Some(ShortcutOutcome::OpenFilePicker(selection)) => {
                        vec![AppEffect::OpenFilePicker(selection)]
                    }
                    None => vec![],
                }
            }
            AppCommand::ApplyShortcuts { bindings, revision } => {
                self.manager.ensure_revision(revision)?;
                let Some(shortcuts) = &mut self.shortcuts else {
                    return Err(AppError::InUse("hotkeys are still initializing; try again"));
                };
                let manager = &mut self.manager;
                shortcuts.apply(bindings, |candidate| manager.save_shortcuts(candidate))?;
                let statuses = super::shortcuts::ShortcutAction::all()
                    .iter()
                    .map(|&action| (action, shortcuts.status(action)))
                    .collect();
                return Ok(vec![
                    AppEffect::ShortcutStatuses(statuses),
                    AppEffect::SettingsSaved(self.settings().clone()),
                ]);
            }
            AppCommand::ResetShortcuts { actions, revision } => {
                self.manager.ensure_revision(revision)?;
                let Some(shortcuts) = &mut self.shortcuts else {
                    return Err(AppError::InUse("hotkeys are still initializing; try again"));
                };
                let candidate = shortcuts.reset_candidate(actions.as_deref());
                let manager = &mut self.manager;
                shortcuts.apply(candidate, |c| manager.save_shortcuts(c))?;
                let statuses = super::shortcuts::ShortcutAction::all()
                    .iter()
                    .map(|&action| (action, shortcuts.status(action)))
                    .collect();
                vec![
                    AppEffect::ShortcutStatuses(statuses),
                    AppEffect::SettingsSaved(self.settings().clone()),
                ]
            }
            AppCommand::SetShortcutEntryActive(active) => {
                self.shortcut_entry_active = active;
                vec![]
            }
            AppCommand::SetAppearance {
                appearance,
                revision,
            } => {
                self.manager.ensure_revision(revision)?;
                self.manager.set_appearance(appearance)?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::SetRegionPreferences {
                preferences,
                revision,
            } => {
                self.manager.ensure_revision(revision)?;
                self.manager.set_region_preferences(preferences)?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::SetStartup { startup, revision } => {
                self.manager.ensure_revision(revision)?;
                crate::platform::windows::startup::apply(
                    &crate::platform::windows::startup::NativeStartup,
                    self.manager.settings().startup.launch_at_sign_in,
                    startup.launch_at_sign_in,
                    || self.manager.set_startup(startup),
                )?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::ChooseArchiveFolder
            | AppCommand::ChooseTaskSave { .. }
            | AppCommand::ChooseUploadFile => vec![],
            AppCommand::SetArchivePolicy { policy, revision } => {
                self.manager.ensure_revision(revision)?;
                self.manager.set_archive_policy(policy)?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::Quit => {
                let scrolling_effects = if self.scrolling.is_some() {
                    self.cancel_scrolling()
                } else {
                    vec![]
                };
                if self
                    .recording
                    .as_ref()
                    .is_some_and(|w| w.session.acquiring())
                {
                    return self.quit_recording();
                }
                // Release the utility's exclusive reservation before the
                // fallible quit_recording call below. Otherwise a finalized
                // recording review still awaiting Save/Discard returns Err
                // via `?` on every Quit dispatch (including the cancelled-
                // signal retry loop), permanently blocking the utility
                // overlay's release and every later capture/recording/
                // utility acquisition behind it.
                let utility_effects = self.close_utility();
                self.quit_recording()?;
                let mut capture_effects = if let Some(capture) = &self.capture {
                    self.cancel_capture(capture.id)
                } else {
                    vec![]
                };
                capture_effects.extend(scrolling_effects);
                capture_effects.extend(utility_effects);
                self.shortcuts = None;
                self.pin_load = None;
                capture_effects.extend(self.pins.close_all().into_iter().map(AppEffect::Pin));
                self.lifecycle = Lifecycle::Draining;
                // Workflows are deliberately NOT cleared here: a batch file
                // already accepted (its destination reserved in `workflows`)
                // may still be mid-staging when Quit arrives, and a later
                // successful `FileStaged` for it must still find that
                // reservation and durably commit — see `file_staged`. No
                // command but `Quit` itself reaches `apply` once Draining
                // starts (checked in `dispatch`), so nothing new can be
                // accepted into `workflows` from here on regardless.
                capture_effects.push(AppEffect::StopAudioTest);
                capture_effects.push(AppEffect::DrainWorker);
                capture_effects
            }
        })
    }
    pub fn on_event(&mut self, event: AppEvent) -> Vec<AppEffect> {
        let mut effects = self.on_event_inner(event);
        effects.extend(self.finish_deferred_import());
        effects
    }
    fn on_event_inner(&mut self, event: AppEvent) -> Vec<AppEffect> {
        match event {
            AppEvent::UtilityAcquired { session, result } => self.utility_acquired(session, result),
            AppEvent::ScrollingAcquired { session, result } => {
                self.pending_acquisitions.remove(&session);
                self.scrolling_acquired(session, result)
            }
            AppEvent::Scrolling(event) => self.scrolling_event(event),
            AppEvent::RecordingAcquired { session, result } => {
                self.pending_acquisitions.remove(&session);
                self.recording_acquired(session, result)
            }
            AppEvent::RecordingWorker { session, input } => {
                if self.recording.as_ref().is_some_and(|w| w.id == session) {
                    self.recording_input(input)
                        .unwrap_or_else(|e| vec![AppEffect::Error(e)])
                } else {
                    vec![]
                }
            }
            AppEvent::PinWorkAreas { result } => {
                if self.lifecycle != Lifecycle::Running || self.pins.snapshots().is_empty() {
                    return vec![];
                }
                result
                    .and_then(|areas| self.pins.update_work_areas(areas.work_areas, areas.pointer))
                    .map(|effects| effects.into_iter().map(AppEffect::Pin).collect())
                    .unwrap_or_else(|error| vec![AppEffect::Error(error)])
            }
            AppEvent::PinLoaded { request, result } => {
                if self.pin_load != Some(request) || self.lifecycle != Lifecycle::Running {
                    return vec![];
                }
                self.pin_load = None;
                let result = result.and_then(|loaded| {
                    let mut effects = self
                        .pins
                        .update_work_areas(loaded.work_areas, loaded.pointer)?;
                    let id = self.pins.open(loaded.image, None)?;
                    effects.push(crate::pin::PinEffect::UpdateViewport(
                        self.pins.get(id).expect("just opened pin").clone(),
                    ));
                    Ok(effects.into_iter().map(AppEffect::Pin).collect())
                });
                result.unwrap_or_else(|error| {
                    vec![AppEffect::PinSourceChooser(true), AppEffect::Error(error)]
                })
            }
            AppEvent::CaptureAcquired { session, result } => {
                self.pending_acquisitions.remove(&session);
                self.capture_acquired(session, result)
            }
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
                effects.extend(self.task_upload_changed(id));
                if let Ok(Some(job)) = self.history.job(id) {
                    if matches!(job.status, JobStatus::Completed(_))
                        && self.settings().archive.storage_mode
                            == crate::persistence::settings::StorageMode::PendingOrFailed
                    {
                        let result = self.settings().archive.resolved_root().and_then(|root| {
                            crate::persistence::archive::ArchiveStore::new(
                                self.history.clone(),
                                root,
                                self.settings().filename.clone(),
                            )
                            .reconcile_uploaded(&self.settings().archive)
                        });
                        if let Err(error) = result {
                            effects.push(AppEffect::Error(error));
                        }
                    }
                    match self.completion_for(&job) {
                        Ok(Some(CompletionNotice::Job(completion))) => {
                            // A batch member's completion is collected into
                            // one grouped notice instead of its own toast.
                            if !self.resolve_batch_job(id, Some(completion.clone()), &mut effects) {
                                match self.notifications.record(CompletionNotice::Job(completion)) {
                                    Ok(_) => {}
                                    // An already-visible duplicate for this
                                    // job is expected and benign; any other
                                    // failure is real.
                                    Err(AppError::InvalidInput(_)) => {}
                                    Err(error) => effects.push(AppEffect::Error(error)),
                                }
                            }
                        }
                        Ok(Some(CompletionNotice::Batch(_))) => {}
                        Ok(None) => {
                            // Cancelled and Uncertain never produce a receipt
                            // on their own; a batch member stuck in either
                            // must still free its slot (truthfully, not
                            // silently) or sibling successes could be
                            // withheld forever. A standalone job in these
                            // states still shows nothing (`resolve_batch_slot`
                            // is a no-op outside a batch).
                            match job.status {
                                JobStatus::Cancelled => self.resolve_batch_slot(
                                    id,
                                    CompletionStatus::Cancelled,
                                    &mut effects,
                                ),
                                JobStatus::Uncertain => self.resolve_batch_slot(
                                    id,
                                    CompletionStatus::Uncertain,
                                    &mut effects,
                                ),
                                _ => {}
                            }
                        }
                        Err(error) => effects.push(AppEffect::Error(error)),
                    }
                }
                effects.push(AppEffect::RefreshQueue);
                effects
            }
            AppEvent::Queue(crate::vectr::queue::QueueEffect::Restarted {
                previous,
                replacement,
            }) => {
                // An explicit restart (never automatic) retires the OLD job
                // as cancelled and admits a brand-new replacement job that
                // is not part of any batch; if `previous` was a batch
                // member, close its slot truthfully instead of leaving it
                // stuck (`job_batch` has no entry for the replacement, so a
                // later `Changed` for it is just an ordinary standalone job).
                let mut effects = vec![];
                effects.extend(self.task_upload_replaced(previous, replacement));
                self.resolve_batch_slot(previous, CompletionStatus::Cancelled, &mut effects);
                effects.push(AppEffect::RefreshQueue);
                effects
            }
            AppEvent::FileStaged {
                batch,
                index,
                display_name,
                outcome,
            } => self.file_staged(batch, index, display_name, outcome),
            AppEvent::Queue(_) => vec![AppEffect::RefreshQueue],
            AppEvent::ConnectionTest { request, result }
                if self.install.as_ref().is_some_and(|(id, s)| {
                    *id == request
                        && s.state() == super::destination_install::InstallState::Verifying
                }) =>
            {
                match self
                    .install
                    .as_mut()
                    .expect("checked install")
                    .1
                    .verification_result(result)
                {
                    Ok(()) => vec![],
                    Err(e) => vec![AppEffect::Error(e)],
                }
            }
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
