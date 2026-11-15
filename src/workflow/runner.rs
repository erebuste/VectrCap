use super::{profile::*, steps::*};
use crate::app::{
    destinations::{DestinationSelection, DestinationSnapshot, Destinations, resolve_destination},
    types::{Artifact, ArtifactKind, JobId, UploadReceipt},
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskIntent {
    Direct(CaptureTask),
    RunSelectedTasks,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowContext {
    pub id: Uuid,
    pub artifact: Option<Arc<Artifact>>,
    pub durable_path: Option<PathBuf>,
    pub kind: ArtifactKind,
    pub receipt: Option<UploadReceipt>,
    pub batch_id: Option<Uuid>,
    pub upload_job: Option<JobId>,
    pub intent: TaskIntent,
    pub effective: EffectiveProfile,
    pub destination: Option<DestinationSnapshot>,
    pub destination_issue: Option<String>,
    requested_destination: DestinationSelection,
    requested_origin: Option<crate::persistence::settings::ServerOrigin>,
}
impl WorkflowContext {
    pub fn new(
        kind: ArtifactKind,
        effective: EffectiveProfile,
        intent: TaskIntent,
        hotkey: DestinationSelection,
        destinations: &Destinations,
        batch_id: Option<Uuid>,
    ) -> Self {
        let requested_destination = match (effective.profile.destination, hotkey) {
            (DestinationSelection::Specific(id), _)
            | (DestinationSelection::Inherit, DestinationSelection::Specific(id)) => {
                DestinationSelection::Specific(id)
            }
            _ => destinations
                .default_id()
                .map(DestinationSelection::Specific)
                .unwrap_or(DestinationSelection::Inherit),
        };
        let requested_origin = match requested_destination {
            DestinationSelection::Specific(id) => destinations.get(id).map(|d| d.origin.clone()),
            DestinationSelection::Inherit => None,
        };
        let destination = resolve_destination(effective.profile.destination, hotkey, destinations);
        let destination_issue = destination
            .as_ref()
            .err()
            .map(|_| "Configure the selected upload destination and token".into());
        Self {
            id: Uuid::new_v4(),
            artifact: None,
            durable_path: None,
            kind,
            receipt: None,
            batch_id,
            upload_job: None,
            intent,
            effective,
            destination: destination.ok(),
            destination_issue,
            requested_destination,
            requested_origin,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEffect {
    Execute(TaskStep),
}
pub enum StepResult {
    Succeeded,
    Finalized(Arc<Artifact>),
    Durable(PathBuf),
    Receipt(UploadReceipt),
    Failed(String),
}
pub enum TaskInput {
    UploadInProgress,
    /// Only the history-backed upload adapter supplies this event.
    ConfirmedReceipt(UploadReceipt),
    CheckpointFailed(TaskStep),
    UploadReplaced {
        previous: JobId,
        replacement: JobId,
    },
    Start,
    Choose(TaskIntent),
    Completed {
        step: TaskStep,
        result: StepResult,
    },
    Retry(TaskStep),
    Skip(TaskStep),
    ProvideDurable(PathBuf),
    Cancel,
    Resume,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunner {
    context: WorkflowContext,
    states: Vec<StepState>,
    started: bool,
    cancelled: bool,
    quick_chosen: bool,
}
impl TaskRunner {
    pub fn new(context: WorkflowContext) -> Self {
        Self {
            context,
            states: vec![],
            started: false,
            cancelled: false,
            quick_chosen: false,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), crate::app::error::AppError> {
        use crate::app::error::AppError;
        let invalid = || AppError::Persistence("invalid task workflow snapshot");
        let c = &self.context;
        if c.id.is_nil()
            || self.states.len() > 20
            || self
                .states
                .iter()
                .filter(|s| s.outcome == StepOutcome::Running)
                .count()
                > 1
        {
            return Err(invalid());
        }
        for (i, s) in self.states.iter().enumerate() {
            if self.states[..i]
                .iter()
                .any(|previous| previous.step == s.step)
            {
                return Err(invalid());
            }
            if let StepOutcome::Failed(code) = &s.outcome
                && safe_code(code) != *code
            {
                return Err(invalid());
            }
        }
        c.effective.archive.validate()?;
        if let Some(a) = &c.artifact {
            if a.id.0.is_nil() || a.kind != c.kind || !a.path.is_absolute() {
                return Err(invalid());
            }
            crate::persistence::filename::validate_file_name(&a.display_name)?;
        }
        if let Some(path) = &c.durable_path
            && !path.is_absolute()
        {
            return Err(invalid());
        }
        if let Some(d) = &c.destination
            && (d.id.0.is_nil()
                || d.credential.destination() != d.id
                || d.credential.origin() != &d.origin
                || d.credential.generation() == 0
                || c.requested_destination != DestinationSelection::Specific(d.id)
                || c.requested_origin.as_ref() != Some(&d.origin))
        {
            return Err(invalid());
        }
        if let Some(job) = c.upload_job
            && job.0.is_nil()
        {
            return Err(invalid());
        }
        if c.receipt.is_some()
            && self.outcome(TaskStep::Capture(CaptureTask::Upload)) != Some(&StepOutcome::Succeeded)
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn link_job(&mut self, id: JobId) {
        self.context.upload_job = Some(id);
    }
    /// Explicit retry may resolve newly configured credentials, but only for
    /// the originally selected stable ID and origin. Defaults never retarget it.
    pub fn repair_destination(
        &mut self,
        destinations: &Destinations,
    ) -> Result<(), crate::app::error::AppError> {
        use crate::app::error::AppError;
        if self.context.upload_job.is_some() || self.context.destination.is_some() {
            return Err(AppError::InUse("accepted upload destination is immutable"));
        }
        let DestinationSelection::Specific(id) = self.context.requested_destination else {
            return Err(AppError::NeedsSetup(
                "no destination was selected; cancel and start a new workflow",
            ));
        };
        let d = destinations
            .get(id)
            .ok_or(AppError::NeedsSetup("the selected destination was removed"))?;
        if self.context.requested_origin.as_ref() != Some(&d.origin) {
            return Err(AppError::NeedsSetup(
                "the selected origin changed; cancel and confirm a new workflow",
            ));
        }
        let snapshot = resolve_destination(
            self.context.requested_destination,
            DestinationSelection::Inherit,
            destinations,
        )?;
        self.context.destination = Some(snapshot);
        self.context.destination_issue = None;
        Ok(())
    }
    pub(crate) fn retry_archive_root(&mut self, root: Option<PathBuf>) {
        self.context.effective.archive.archive_root = root;
    }
    pub fn context(&self) -> &WorkflowContext {
        &self.context
    }
    pub fn states(&self) -> &[StepState] {
        &self.states
    }
    pub fn outcome(&self, step: TaskStep) -> Option<&StepOutcome> {
        self.states
            .iter()
            .find(|s| s.step == step)
            .map(|s| &s.outcome)
    }
    pub fn set_durable(&mut self, path: PathBuf) {
        self.context.durable_path = Some(path);
    }
    /// Adapter can enter after the existing reviewed, finalized transaction. No
    /// finalize/save effect is replayed for an artifact already owned by it.
    pub fn accept_finalized(&mut self, artifact: Arc<Artifact>, durable: Option<PathBuf>) {
        self.context.artifact = Some(artifact);
        self.context.durable_path = durable;
    }
    pub fn complete(&self) -> bool {
        self.cancelled
            || (self.started
                && self
                    .states
                    .iter()
                    .all(|s| matches!(s.outcome, StepOutcome::Succeeded | StepOutcome::Skipped(_))))
    }
    fn plan(&mut self) {
        use CaptureTask::*;
        let p = &self.context.effective.profile;
        let applicable = match p.scope {
            ProfileScope::All => true,
            ProfileScope::Captures => self.context.kind != ArtifactKind::File,
            ProfileScope::Files => self.context.kind == ArtifactKind::File,
        };
        let selected = match self.context.intent {
            TaskIntent::Direct(t) => std::collections::BTreeSet::from([t]),
            TaskIntent::RunSelectedTasks if applicable => p.capture.clone(),
            TaskIntent::RunSelectedTasks => Default::default(),
        };
        let upload = selected.contains(&Upload);
        let mut steps = vec![];
        if selected.contains(&QuickMenu) && !self.quick_chosen {
            steps.push(TaskStep::Capture(QuickMenu));
        }
        if self.context.artifact.is_none() {
            if selected.contains(&Review) {
                steps.push(TaskStep::Capture(Review));
            }
            steps.push(TaskStep::Finalize);
        }
        if self.context.intent != TaskIntent::Direct(SaveAs)
            && self.context.kind != ArtifactKind::File
            && (selected.contains(&SaveFinal)
                || self.context.effective.archive.should_auto_archive(upload))
            && self.context.durable_path.is_none()
        {
            steps.push(TaskStep::Capture(SaveFinal));
        }
        for task in [
            SaveAs,
            CopyImage,
            CopyFile,
            CopyFilePath,
            CopyFolderPath,
            Reveal,
            Pin,
        ] {
            if selected.contains(&task) {
                steps.push(TaskStep::Capture(task));
            }
        }
        if upload {
            if p.before_upload_review || selected.contains(&BeforeUploadReview) {
                steps.push(TaskStep::Capture(BeforeUploadReview));
            }
            steps.push(TaskStep::Capture(Upload));
            steps.extend(p.after_upload.iter().copied().map(TaskStep::AfterUpload));
        }
        self.states = steps
            .into_iter()
            .map(|step| StepState {
                step,
                outcome: StepOutcome::Pending,
            })
            .collect();
        if self.context.artifact.is_some() {
            self.states.insert(
                0,
                StepState {
                    step: TaskStep::Finalize,
                    outcome: StepOutcome::Succeeded,
                },
            );
            if self.context.durable_path.is_some()
                && self.context.kind != ArtifactKind::File
                && self.context.intent != TaskIntent::Direct(SaveAs)
            {
                self.states.insert(
                    1,
                    StepState {
                        step: TaskStep::Capture(SaveFinal),
                        outcome: StepOutcome::Succeeded,
                    },
                );
            }
        }
    }
    pub fn handle(&mut self, input: TaskInput) -> Vec<TaskEffect> {
        if self.cancelled {
            return vec![];
        }
        match input {
            TaskInput::UploadInProgress => {
                if self.context.upload_job.is_some()
                    && self.context.receipt.is_none()
                    && let Some(s) = self.states.iter_mut().find(|s| {
                        s.step == TaskStep::Capture(CaptureTask::Upload)
                            && !matches!(
                                s.outcome,
                                StepOutcome::Succeeded | StepOutcome::Skipped(_)
                            )
                    })
                {
                    s.outcome = StepOutcome::Running;
                }
                return vec![];
            }
            TaskInput::CheckpointFailed(step) => {
                if let Some(s) = self
                    .states
                    .iter_mut()
                    .find(|s| s.step == step && s.outcome == StepOutcome::Running)
                {
                    s.outcome = StepOutcome::NeedsUserAction(
                        "Checkpoint failed before execution; explicitly retry after fixing storage"
                            .into(),
                    );
                }
                return vec![];
            }
            TaskInput::ConfirmedReceipt(receipt) => {
                let step = TaskStep::Capture(CaptureTask::Upload);
                if self.context.upload_job.is_none() || self.context.receipt.is_some() {
                    return vec![];
                }
                let Some(s) = self.states.iter_mut().find(|s| {
                    s.step == step
                        && !matches!(s.outcome, StepOutcome::Succeeded | StepOutcome::Skipped(_))
                }) else {
                    return vec![];
                };
                s.outcome = StepOutcome::Running;
                return self.handle(TaskInput::Completed {
                    step,
                    result: StepResult::Receipt(receipt),
                });
            }
            TaskInput::UploadReplaced {
                previous,
                replacement,
            } => {
                if self.context.upload_job != Some(previous) || self.context.receipt.is_some() {
                    return vec![];
                }
                self.context.upload_job = Some(replacement);
                if let Some(s) = self
                    .states
                    .iter_mut()
                    .find(|s| s.step == TaskStep::Capture(CaptureTask::Upload))
                {
                    s.outcome = StepOutcome::Running;
                }
                return vec![];
            }
            TaskInput::Start => {
                if self.started {
                    return vec![];
                }
                self.started = true;
                self.plan();
            }
            TaskInput::Choose(intent) => {
                let step = TaskStep::Capture(CaptureTask::QuickMenu);
                if self.outcome(step) != Some(&StepOutcome::Running) {
                    return vec![];
                }
                self.context.intent = intent;
                self.quick_chosen = true;
                self.plan();
            }
            TaskInput::Cancel => {
                self.cancelled = true;
                for s in &mut self.states {
                    if !matches!(s.outcome, StepOutcome::Succeeded) {
                        s.outcome = StepOutcome::Skipped("Cancelled by user".into());
                    }
                }
                return vec![];
            }
            TaskInput::Completed { step, result } => {
                let Some(i) = self
                    .states
                    .iter()
                    .position(|s| s.step == step && s.outcome == StepOutcome::Running)
                else {
                    return vec![];
                };
                let outcome = match result {
                    StepResult::Failed(code) => StepOutcome::Failed(safe_code(&code)),
                    StepResult::Finalized(a)
                        if step == TaskStep::Finalize && a.kind == self.context.kind =>
                    {
                        self.context.artifact = Some(a);
                        StepOutcome::Succeeded
                    }
                    StepResult::Durable(path)
                        if matches!(
                            step,
                            TaskStep::Capture(CaptureTask::SaveFinal | CaptureTask::SaveAs)
                        ) && path.is_absolute() =>
                    {
                        self.context.durable_path = Some(path);
                        StepOutcome::Succeeded
                    }
                    StepResult::Receipt(receipt)
                        if step == TaskStep::Capture(CaptureTask::Upload) =>
                    {
                        self.context.receipt = Some(receipt);
                        StepOutcome::Succeeded
                    }
                    StepResult::Succeeded
                        if !matches!(
                            step,
                            TaskStep::Finalize
                                | TaskStep::Capture(
                                    CaptureTask::Upload
                                        | CaptureTask::SaveFinal
                                        | CaptureTask::SaveAs
                                        | CaptureTask::QuickMenu
                                )
                        ) =>
                    {
                        StepOutcome::Succeeded
                    }
                    _ => StepOutcome::NeedsUserAction("Operation has no confirmed result".into()),
                };
                self.states[i].outcome = outcome;
            }
            TaskInput::Retry(step) => {
                let Some(s) = self.states.iter_mut().find(|s| {
                    s.step == step
                        && matches!(
                            s.outcome,
                            StepOutcome::Failed(_) | StepOutcome::NeedsUserAction(_)
                        )
                }) else {
                    return vec![];
                };
                s.outcome = StepOutcome::Pending;
            }
            TaskInput::Skip(step) => {
                if matches!(
                    step,
                    TaskStep::Finalize
                        | TaskStep::Capture(
                            CaptureTask::Review
                                | CaptureTask::BeforeUploadReview
                                | CaptureTask::QuickMenu
                        )
                ) {
                    return vec![];
                }
                let Some(s) = self.states.iter_mut().find(|s| {
                    s.step == step
                        && matches!(
                            s.outcome,
                            StepOutcome::Failed(_) | StepOutcome::NeedsUserAction(_)
                        )
                }) else {
                    return vec![];
                };
                s.outcome = StepOutcome::Skipped("Explicitly skipped by user".into());
                if step == TaskStep::Capture(CaptureTask::Upload) {
                    for s in &mut self.states {
                        if matches!(s.step, TaskStep::AfterUpload(_)) {
                            s.outcome = StepOutcome::Skipped("Upload was skipped".into());
                        }
                    }
                }
            }
            TaskInput::ProvideDurable(path) => {
                if !path.is_absolute() {
                    return vec![];
                }
                self.set_durable(path);
                for s in &mut self.states {
                    if matches!(s.step,TaskStep::Capture(t) if t.needs_durable())
                        && matches!(s.outcome, StepOutcome::NeedsUserAction(_))
                    {
                        s.outcome = StepOutcome::Pending;
                    }
                }
            }
            TaskInput::Resume => {
                if !self.started {
                    self.started = true;
                    self.plan();
                }
                for s in &mut self.states {
                    if s.step == TaskStep::Capture(CaptureTask::Upload)
                        && self.context.upload_job.is_some()
                    {
                        continue;
                    }
                    if s.outcome == StepOutcome::Running
                        || (s.step.interactive() && s.outcome == StepOutcome::Pending)
                    {
                        s.outcome = StepOutcome::NeedsUserAction(
                            "Interrupted; explicitly retry or skip this action".into(),
                        );
                    }
                }
            }
        }
        self.advance()
    }
    fn advance(&mut self) -> Vec<TaskEffect> {
        if self
            .states
            .iter()
            .any(|s| s.outcome == StepOutcome::Running)
        {
            return vec![];
        }
        for i in 0..self.states.len() {
            let step = self.states[i].step;
            if matches!(
                self.states[i].outcome,
                StepOutcome::Succeeded | StepOutcome::Skipped(_)
            ) {
                continue;
            }
            let gates = matches!(
                step,
                TaskStep::Finalize
                    | TaskStep::Capture(
                        CaptureTask::Review
                            | CaptureTask::QuickMenu
                            | CaptureTask::SaveFinal
                            | CaptureTask::BeforeUploadReview
                            | CaptureTask::Upload
                    )
            );
            if self.states[i].outcome != StepOutcome::Pending {
                if gates || step.interactive() {
                    return vec![];
                }
                continue;
            }
            if let TaskStep::Capture(t) = step {
                if let Some(reason) = t.unavailable_reason(self.context.kind) {
                    self.states[i].outcome = StepOutcome::Skipped(reason.into());
                    continue;
                }
                if t.needs_durable() && self.context.durable_path.is_none() {
                    self.states[i].outcome = StepOutcome::NeedsUserAction(
                        "Save a durable copy before using file or path actions".into(),
                    );
                    continue;
                }
                if t == CaptureTask::Upload && self.context.destination.is_none() {
                    self.states[i].outcome = StepOutcome::NeedsUserAction(
                        "Configure the selected destination; no fallback is allowed".into(),
                    );
                    return vec![];
                }
            }
            if matches!(step, TaskStep::AfterUpload(_)) && self.context.receipt.is_none() {
                return vec![];
            }
            self.states[i].outcome = StepOutcome::Running;
            return vec![TaskEffect::Execute(step)];
        }
        vec![]
    }
}
fn safe_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
    {
        code.into()
    } else {
        "task_failed".into()
    }
}

/// Commands exposed by review/results controls, without arbitrary executable text.
pub enum TaskCommand {
    Inspect,
    Choose(TaskIntent),
    Approve,
    Retry(TaskStep),
    Skip(TaskStep),
    Cancel,
    SaveAs(PathBuf),
}
