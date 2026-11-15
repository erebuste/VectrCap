use super::*;
use crate::workflow::{profile::*, runner::*, steps::*};
use std::path::Path;
impl AppController {
    pub fn task_views(&self) -> Vec<TaskRunner> {
        let mut rows: Vec<_> = self.task_runners.values().cloned().collect();
        rows.sort_by_key(|r| r.context().id);
        rows
    }
    pub(super) fn task_context(
        &self,
        kind: ArtifactKind,
        action: Option<super::super::shortcuts::ShortcutAction>,
        hotkey: DestinationSelection,
    ) -> WorkflowContext {
        WorkflowContext::new(
            kind,
            EffectiveProfile::snapshot(self.settings(), action),
            TaskIntent::RunSelectedTasks,
            hotkey,
            &self.settings().destinations,
            None,
        )
    }
    pub(super) fn launch_tasks(
        &mut self,
        runner: TaskRunner,
        image: Option<crate::ui::image_export::ImageFinish>,
    ) -> Result<Vec<AppEffect>, AppError> {
        let id = runner.context().id;
        if self.task_runners.contains_key(&id) {
            self.window = Some(Window::Tasks);
            return Ok(vec![AppEffect::Window(self.window)]);
        }
        // Acceptance is safety-critical: a checkpoint failure must prevent all
        // downstream publication. Save before exposing the workflow to effects.
        self.history.save_task_workflow(&runner)?;
        if let Some(image) = image {
            self.task_images.insert(id, image);
        }
        self.task_runners.insert(id, runner);
        // A persisted workflow already owns the source. Reference-refresh
        // errors are surfaced without reporting acceptance as rolled back.
        let mut out = self.drive_tasks(id, TaskInput::Start);
        if let Err(e) = self.update_active() {
            out.push(AppEffect::Error(e));
        }
        Ok(out)
    }
    fn execute_task(
        &mut self,
        r: &mut TaskRunner,
        step: TaskStep,
        out: &mut Vec<AppEffect>,
    ) -> Result<Option<StepResult>, AppError> {
        let c = r.context();
        match step {
            TaskStep::Finalize => Err(AppError::InUse("return to review to finalize the output")),
            TaskStep::Capture(CaptureTask::SaveAs)
                if self.task_images.get(&c.id).is_some_and(|image| {
                    image.action == crate::ui::capture_overlay::OverlayAction::Save
                }) && c.durable_path.is_some() =>
            {
                Ok(Some(StepResult::Durable(
                    c.durable_path.clone().expect("explicit Save completed"),
                )))
            }
            TaskStep::Capture(
                CaptureTask::QuickMenu
                | CaptureTask::Review
                | CaptureTask::BeforeUploadReview
                | CaptureTask::SaveAs,
            ) => {
                self.window = Some(Window::Tasks);
                out.push(AppEffect::Window(self.window));
                Ok(None)
            }
            TaskStep::Capture(CaptureTask::SaveFinal) => {
                let a = c
                    .artifact
                    .as_ref()
                    .ok_or(AppError::Persistence("final output is missing"))?;
                self.artifacts.verify(a)?;
                if let Some(existing) = self.history.archive_entry_for_artifact(a.id)? {
                    let mut durable = (**a).clone();
                    durable.path = existing.entry.path.clone();
                    let _verified =
                        crate::persistence::history::browser::verified_local(&durable, u64::MAX)?;
                    let path = existing.entry.path.clone();
                    self.recording_task_archive_saved(c.id, existing.entry)?;
                    return Ok(Some(StepResult::Durable(path)));
                }
                let archive = crate::persistence::archive::ArchiveStore::new(
                    self.history.clone(),
                    c.effective.archive.resolved_root()?,
                    self.settings().filename.clone(),
                );
                let entry = archive.save_finalized(a, chrono::Local::now())?;
                let path = entry.path.clone();
                self.recording_task_archive_saved(c.id, entry)?;
                Ok(Some(StepResult::Durable(path)))
            }
            TaskStep::Capture(CaptureTask::CopyImage | CaptureTask::Pin) => {
                let a = c
                    .artifact
                    .as_ref()
                    .ok_or(AppError::Persistence("final output is missing"))?;
                let image = if let Some(f) = self.task_images.get(&c.id) {
                    crate::pin::PinnedImage::new(f.prepared.pixels().clone(), None)?
                } else {
                    crate::pin::source::load_pin_history(a)?
                };
                if step == TaskStep::Capture(CaptureTask::CopyImage) {
                    self.clipboard.write_image(image.pixels())?;
                } else {
                    let location = self.task_images.get(&c.id).and_then(|f| f.screen_location);
                    let id = self.pins.open(image, location)?;
                    out.push(AppEffect::Pin(crate::pin::PinEffect::UpdateViewport(
                        self.pins.get(id).expect("opened pin").clone(),
                    )));
                }
                Ok(Some(StepResult::Succeeded))
            }
            TaskStep::Capture(
                CaptureTask::CopyFile
                | CaptureTask::CopyFilePath
                | CaptureTask::CopyFolderPath
                | CaptureTask::Reveal,
            ) => {
                let path = c
                    .durable_path
                    .as_deref()
                    .ok_or(AppError::InUse("save a durable copy first"))?;
                // Fail closed if the durable file was removed/replaced. Never
                // substitute the temporary spool path for a file clipboard action.
                let a = c
                    .artifact
                    .as_ref()
                    .ok_or(AppError::Persistence("final output is missing"))?;
                let mut durable = (**a).clone();
                durable.path = path.to_owned();
                let _verified =
                    crate::persistence::history::browser::verified_local(&durable, u64::MAX)?;
                match step {
                    TaskStep::Capture(CaptureTask::CopyFile) => self.clipboard.write_file(path)?,
                    TaskStep::Capture(CaptureTask::Reveal) => self.shell.reveal_path(path)?,
                    TaskStep::Capture(CaptureTask::CopyFolderPath) => {
                        self.clipboard
                            .write_text(path.parent().and_then(Path::to_str).ok_or(
                                AppError::InvalidInput("folder path cannot be copied as Unicode"),
                            )?)?
                    }
                    _ => self.clipboard.write_text(path.to_str().ok_or(
                        AppError::InvalidInput("file path cannot be copied as Unicode"),
                    )?)?,
                }
                Ok(Some(StepResult::Succeeded))
            }
            TaskStep::Capture(CaptureTask::Upload) => {
                let a = c
                    .artifact
                    .as_ref()
                    .ok_or(AppError::Persistence("final output is missing"))?;
                self.artifacts.verify(a)?;
                let d = c
                    .destination
                    .as_ref()
                    .ok_or(AppError::NeedsSetup("selected destination needs setup"))?;
                self.store.read(&d.credential)?;
                let workflow_id = c.id;
                let job = self.history.admit_task_upload(r)?;
                self.mark_recording_task_admitted(workflow_id);
                if let JobStatus::Completed(receipt) = job.status {
                    return Ok(Some(StepResult::Receipt(receipt)));
                }
                if let Some((batch, index)) = self.task_batches.remove(&workflow_id) {
                    self.job_batch.insert(job.id, (batch, index));
                }
                out.extend([
                    AppEffect::JobAccepted(job.id),
                    AppEffect::Queue(crate::vectr::queue::QueueCommand::Admit(job.id)),
                ]);
                Ok(None)
            }
            TaskStep::AfterUpload(t) => {
                let receipt = c
                    .receipt
                    .as_ref()
                    .ok_or(AppError::InUse("a confirmed receipt is required"))?;
                match t {
                    UploadTask::CopyUrl if c.batch_id.is_some() => {}
                    UploadTask::CopyUrl => self.clipboard.write_text(receipt.url())?,
                    UploadTask::OpenUrl => self.shell.open_url(receipt.url())?,
                    UploadTask::ShowResult => {
                        self.window = Some(Window::Tasks);
                        out.push(AppEffect::Window(self.window));
                    }
                }
                Ok(Some(StepResult::Succeeded))
            }
        }
    }
    pub(super) fn drive_tasks(&mut self, id: Uuid, input: TaskInput) -> Vec<AppEffect> {
        let Some(mut r) = self.task_runners.remove(&id) else {
            return vec![];
        };
        if matches!(input, TaskInput::Cancel) {
            // Cancel skips every unfinished step at once with no Execute effect
            // to roll back individually via CheckpointFailed, unlike the ordinary
            // per-step chain below. Probe the checkpoint on a clone first, so the
            // real runner is only ever mutated to "cancelled" once that state is
            // actually committed -- otherwise disk recovery could resurrect work
            // the user asked to cancel.
            let mut probe = r.clone();
            probe.handle(TaskInput::Cancel);
            if let Err(e) = self.history.save_task_workflow(&probe) {
                self.task_runners.insert(id, r);
                return vec![AppEffect::Error(e)];
            }
        }
        let mut pending = r.handle(input);
        let mut out = vec![];
        // Each transition is durably checkpointed BEFORE its next effect.
        // A failure after a successful effect preserves the in-memory outcome
        // and surfaces the checkpoint error, instead of replaying that effect.
        loop {
            if let Err(e) = self.history.save_task_workflow(&r) {
                if let Some(TaskEffect::Execute(step)) = pending.last() {
                    r.handle(TaskInput::CheckpointFailed(*step));
                }
                out.push(AppEffect::Error(e));
                break;
            }
            let Some(TaskEffect::Execute(step)) = pending.pop() else {
                break;
            };
            match self.execute_task(&mut r, step, &mut out) {
                Ok(Some(result)) => pending.extend(r.handle(TaskInput::Completed { step, result })),
                Ok(None) => break,
                Err(e) => {
                    out.push(AppEffect::Error(e));
                    pending.extend(r.handle(TaskInput::Completed {
                        step,
                        result: StepResult::Failed("operation_failed".into()),
                    }));
                }
            }
        }
        if r.complete() {
            self.task_images.remove(&id);
            if let Some((batch, index)) = self.task_batches.remove(&id) {
                if let Some(state) = self.batches.get_mut(&batch) {
                    state.jobs_remaining = state.jobs_remaining.saturating_sub(1);
                    if let Err(error) = state
                        .results
                        .resolve(index, super::super::batches::BatchOutcome::Cancelled)
                    {
                        out.push(AppEffect::Error(error));
                    }
                }
                self.maybe_flush_batch(batch, &mut out);
            }
        }
        self.task_runners.insert(id, r);
        if let Err(e) = self.update_active() {
            out.push(AppEffect::Error(e));
        }
        out
    }
    pub(super) fn task_command(
        &mut self,
        id: Uuid,
        command: TaskCommand,
    ) -> Result<Vec<AppEffect>, AppError> {
        if let TaskCommand::Retry(step) = &command {
            if *step == TaskStep::Capture(CaptureTask::Upload)
                && self
                    .task_runners
                    .get(&id)
                    .is_some_and(|r| r.context().destination.is_none())
            {
                let destinations = self.settings().destinations.clone();
                self.task_runners
                    .get_mut(&id)
                    .expect("checked task")
                    .repair_destination(&destinations)?;
            }
            if *step == TaskStep::Capture(CaptureTask::SaveFinal) {
                let root = self.settings().archive.archive_root.clone();
                if let Some(r) = self.task_runners.get_mut(&id) {
                    r.retry_archive_root(root);
                }
            }
        }
        let r = self
            .task_runners
            .get(&id)
            .ok_or(AppError::InvalidInput("task workflow is unavailable"))?;
        let input = match command {
            TaskCommand::Inspect => {
                let a = r
                    .context()
                    .artifact
                    .as_ref()
                    .ok_or(AppError::InUse("finalized output is unavailable"))?;
                let extension = Self::viewer_extension(a).ok_or(AppError::InvalidInput(
                    "inspect this file using Show in Explorer",
                ))?;
                let mut source = (**a).clone();
                if let Some(path) = &r.context().durable_path {
                    source.path = path.clone();
                }
                let _verified =
                    crate::persistence::history::browser::verified_local(&source, u64::MAX)?;
                self.shell.open_document(&source.path, extension)?;
                return Ok(vec![]);
            }
            TaskCommand::Choose(intent) => TaskInput::Choose(intent),
            TaskCommand::Approve => {
                let step = r
                    .states()
                    .iter()
                    .find(|s| {
                        s.outcome == StepOutcome::Running
                            && matches!(
                                s.step,
                                TaskStep::Capture(
                                    CaptureTask::Review | CaptureTask::BeforeUploadReview
                                )
                            )
                    })
                    .map(|s| s.step)
                    .ok_or(AppError::InUse("no review is awaiting approval"))?;
                TaskInput::Completed {
                    step,
                    result: StepResult::Succeeded,
                }
            }
            TaskCommand::Retry(step) => {
                // Upload admission is idempotent. Once admitted it is the queue,
                // never the task chain, that owns retry/uncertain resolution.
                if step == TaskStep::Capture(CaptureTask::Upload)
                    && r.context().upload_job.is_some()
                {
                    return Err(AppError::InUse("manage the admitted upload in Uploads"));
                }
                TaskInput::Retry(step)
            }
            TaskCommand::Skip(step) => TaskInput::Skip(step),
            TaskCommand::Cancel => TaskInput::Cancel,
            TaskCommand::SaveAs(path) => {
                let saving_as = r.outcome(TaskStep::Capture(CaptureTask::SaveAs))
                    == Some(&StepOutcome::Running);
                let needs_durable = r.states().iter().any(|s| {
                    matches!(s.step,TaskStep::Capture(t) if t.needs_durable())
                        && matches!(s.outcome, StepOutcome::NeedsUserAction(_))
                });
                if !saving_as && !needs_durable {
                    return Err(AppError::InUse("no task is awaiting a durable destination"));
                }
                let a = r
                    .context()
                    .artifact
                    .as_ref()
                    .ok_or(AppError::Persistence("final output is missing"))?;
                if let Some(f) = self.task_images.get(&id) {
                    crate::app::image_finish::save_image_as(&f.prepared, &path)?;
                } else {
                    crate::app::image_finish::save_artifact_as(a, &path)?;
                }
                if saving_as {
                    TaskInput::Completed {
                        step: TaskStep::Capture(CaptureTask::SaveAs),
                        result: StepResult::Durable(path),
                    }
                } else {
                    TaskInput::ProvideDurable(path)
                }
            }
        };
        let mut out = self.drive_tasks(id, input);
        self.window = Some(Window::Tasks);
        out.push(AppEffect::Window(self.window));
        Ok(out)
    }
    pub(super) fn task_upload_changed(
        &mut self,
        job_id: super::super::types::JobId,
    ) -> Vec<AppEffect> {
        let id = self
            .task_runners
            .values()
            .find(|r| r.context().upload_job == Some(job_id))
            .map(|r| r.context().id);
        let Some(id) = id else {
            return vec![];
        };
        match self.history.job(job_id) {
            Ok(Some(job)) => match job.status {
                JobStatus::Completed(receipt) => {
                    self.drive_tasks(id, TaskInput::ConfirmedReceipt(receipt))
                }
                JobStatus::Cancelled => self.drive_tasks(id, TaskInput::Cancel),
                JobStatus::Pending | JobStatus::Running | JobStatus::Interrupted => {
                    self.drive_tasks(id, TaskInput::UploadInProgress)
                }
                JobStatus::Failed { code } => self.drive_tasks(
                    id,
                    TaskInput::Completed {
                        step: TaskStep::Capture(CaptureTask::Upload),
                        result: StepResult::Failed(code),
                    },
                ),
                JobStatus::Uncertain => self.drive_tasks(
                    id,
                    TaskInput::Completed {
                        step: TaskStep::Capture(CaptureTask::Upload),
                        result: StepResult::Failed("uncertain".into()),
                    },
                ),
            },
            Ok(None) => vec![AppEffect::Error(AppError::Persistence(
                "task upload is missing",
            ))],
            Err(e) => vec![AppEffect::Error(e)],
        }
    }
    pub(crate) fn recover_tasks(&mut self) -> Vec<AppEffect> {
        let ids: Vec<_> = self.task_runners.keys().copied().collect();
        let mut out = vec![];
        for id in ids {
            out.extend(self.drive_tasks(id, TaskInput::Resume));
            if let Some(job) = self
                .task_runners
                .get(&id)
                .and_then(|r| r.context().upload_job)
            {
                out.extend(self.task_upload_changed(job));
            }
        }
        out
    }
}

impl AppController {
    pub(super) fn task_upload_replaced(
        &mut self,
        previous: super::super::types::JobId,
        replacement: super::super::types::JobId,
    ) -> Vec<AppEffect> {
        let id = self
            .task_runners
            .values()
            .find(|r| r.context().upload_job == Some(previous))
            .map(|r| r.context().id);
        let Some(id) = id else {
            return vec![];
        };
        self.drive_tasks(
            id,
            TaskInput::UploadReplaced {
                previous,
                replacement,
            },
        )
    }
}
