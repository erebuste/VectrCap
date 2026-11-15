//! Single owner for workflow/job acceptance and settings/credential transactions.
//! There is no await point between reference inspection and durable commit.
mod capture;
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
pub use capture::{CaptureSurface, PinImage};
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
pub struct AppController {
    capture: Option<capture::CaptureWorkflow>,
    pins: HashSet<Uuid>,
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
            capture: None,
            pins: HashSet::new(),
            manager,
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
    pub fn shortcuts(&self) -> Option<&ShortcutManager> {
        self.shortcuts.as_ref()
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
        if paths.is_empty() || paths.len() > super::notifications::MAX_BATCH_ITEMS {
            return Err(AppError::InvalidInput("select 1 to 100 files to upload"));
        }
        if self.workflows.len() >= 32 || self.batches.len() >= 32 {
            return Err(AppError::InvalidInput(
                "too many concurrent uploads accepted",
            ));
        }
        self.manager.ensure_not_corrupted()?;
        let snapshot = resolve_destination(
            DestinationSelection::Inherit,
            selection,
            &self.settings().destinations,
        )?;
        self.store.read(&snapshot.credential)?;
        let batch = Uuid::new_v4();
        self.workflows.insert(batch, snapshot);
        self.batches.insert(
            batch,
            FileBatch {
                staging_remaining: paths.len(),
                staging_done: vec![false; paths.len()],
                jobs_remaining: 0,
                completions: vec![None; paths.len()],
            },
        );
        self.update_active()?;
        Ok(vec![
            AppEffect::WorkflowAccepted(batch),
            AppEffect::StageFiles { batch, paths },
        ])
    }
    /// Creates the durable job for one successfully staged batch file,
    /// reusing the batch's single pre-resolved destination snapshot. The
    /// artifact arrives already verified by the staging task that produced
    /// it (off this thread); re-hashing it again here would re-block the
    /// controller on exactly the large-file cost the staging handoff exists
    /// to avoid, so this only does cheap, in-memory checks.
    fn accept_batch_artifact(
        &mut self,
        batch: Uuid,
        index: usize,
        artifact: &Artifact,
    ) -> Result<Vec<AppEffect>, AppError> {
        let snapshot = self
            .workflows
            .get(&batch)
            .ok_or(AppError::InvalidInput("unknown accepted workflow"))?;
        self.manager.ensure_not_corrupted()?;
        let job = self.history.create_job(artifact, snapshot)?;
        self.job_batch.insert(job.id, (batch, index));
        if let Some(state) = self.batches.get_mut(&batch) {
            state.jobs_remaining += 1;
        }
        Ok(vec![
            AppEffect::JobAccepted(job.id),
            AppEffect::Queue(crate::vectr::queue::QueueCommand::Admit(job.id)),
        ])
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
            .get_mut(&batch)
            .and_then(|state| state.staging_done.get_mut(index))
            .is_some_and(|done| {
                if *done {
                    false
                } else {
                    *done = true;
                    true
                }
            });
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
        let mut effects = Vec::new();
        match outcome {
            Ok(artifact) => match self.accept_batch_artifact(batch, index, &artifact) {
                Ok(job_effects) => effects.extend(job_effects),
                Err(error) => {
                    effects.push(AppEffect::FileRejected {
                        display_name,
                        error,
                    });
                    if let Err(error) = self.rollback_staged(&artifact) {
                        effects.push(AppEffect::Error(error));
                    }
                }
            },
            Err(error) => effects.push(AppEffect::FileRejected {
                display_name,
                error,
            }),
        }
        if let Some(state) = self.batches.get_mut(&batch) {
            state.staging_remaining = state.staging_remaining.saturating_sub(1);
            if state.staging_remaining == 0 {
                // Every path has either become a durable job or been
                // explicitly rejected; the destination reservation is no
                // longer needed even if some jobs are still uploading.
                self.workflows.remove(&batch);
                let _ = self.update_active();
                let _ = self.manager.cleanup();
            }
        }
        self.maybe_flush_batch(batch, &mut effects);
        effects
    }
    fn rollback_staged(&self, artifact: &Artifact) -> Result<(), AppError> {
        if !self.history.artifact_is_recorded(artifact.id)? {
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
        let Some((batch, index)) = self.job_batch.remove(&job) else {
            return false;
        };
        if let Some(state) = self.batches.get_mut(&batch) {
            state.jobs_remaining = state.jobs_remaining.saturating_sub(1);
            if let Some(completion) = completion
                && let Some(slot) = state.completions.get_mut(index)
            {
                *slot = Some(completion);
            }
        }
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
        let Some(state) = self.batches.remove(&batch) else {
            return;
        };
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
            AppCommand::ResumeCapture(session) => {
                if let Some(capture) = &self.capture
                    && capture.id == session
                    && let Some(surface) = &capture.surface
                {
                    let mut surface = surface
                        .lock()
                        .map_err(|_| AppError::Capture("capture surface unavailable"))?;
                    surface.working = false;
                    surface.overlay.finish_failed(AppError::Cancelled);
                }
                vec![]
            }
            AppCommand::ClosePin(id) => {
                self.pins.remove(&id);
                vec![AppEffect::ClosePin(id)]
            }
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
                let mut effects = vec![AppEffect::Window(self.window)];
                if window != Window::Settings {
                    effects.push(AppEffect::CancelConnectionTest);
                }
                effects
            }
            AppCommand::CloseWindow => {
                self.shortcut_entry_active = None;
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
            AppCommand::AcceptFileSelection(paths) => {
                self.accept_file_selection(paths, DestinationSelection::Inherit)?
            }
            AppCommand::AcceptFileSelectionFor { paths, selection } => {
                self.accept_file_selection(paths, selection)?
            }
            AppCommand::AttachHotkeyWindow(hwnd) => {
                if self.shortcuts.is_none() {
                    let backend = crate::platform::windows::hotkeys::Win32HotkeyBackend::new(hwnd);
                    let (manager, _startup_conflicts) = ShortcutManager::new(
                        Box::new(backend),
                        self.manager.settings().shortcuts.effective(),
                    );
                    let statuses = super::shortcuts::ShortcutAction::all()
                        .iter()
                        .map(|&action| (action, manager.status(action)))
                        .collect();
                    self.shortcuts = Some(manager);
                    return Ok(vec![AppEffect::ShortcutStatuses(statuses)]);
                }
                vec![]
            }
            AppCommand::NativeHotkeyFired(id) => {
                if self.capture.is_some() {
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
            AppCommand::ChooseArchiveFolder => vec![],
            AppCommand::SetArchivePolicy { policy, revision } => {
                self.manager.ensure_revision(revision)?;
                self.manager.set_archive_policy(policy)?;
                vec![AppEffect::SettingsSaved(self.settings().clone())]
            }
            AppCommand::Quit => {
                let mut capture_effects = if let Some(capture) = &self.capture {
                    self.cancel_capture(capture.id)
                } else {
                    vec![]
                };
                self.shortcuts = None;
                self.lifecycle = Lifecycle::Draining;
                // Workflows are deliberately NOT cleared here: a batch file
                // already accepted (its destination reserved in `workflows`)
                // may still be mid-staging when Quit arrives, and a later
                // successful `FileStaged` for it must still find that
                // reservation and durably commit — see `file_staged`. No
                // command but `Quit` itself reaches `apply` once Draining
                // starts (checked in `dispatch`), so nothing new can be
                // accepted into `workflows` from here on regardless.
                capture_effects.push(AppEffect::DrainWorker);
                capture_effects
            }
        })
    }
    pub fn on_event(&mut self, event: AppEvent) -> Vec<AppEffect> {
        match event {
            AppEvent::CaptureAcquired { session, result } => self.capture_acquired(session, result),
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
            AppEvent::Queue(crate::vectr::queue::QueueEffect::Restarted { previous, .. }) => {
                // An explicit restart (never automatic) retires the OLD job
                // as cancelled and admits a brand-new replacement job that
                // is not part of any batch; if `previous` was a batch
                // member, close its slot truthfully instead of leaving it
                // stuck (`job_batch` has no entry for the replacement, so a
                // later `Changed` for it is just an ordinary standalone job).
                let mut effects = vec![];
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
