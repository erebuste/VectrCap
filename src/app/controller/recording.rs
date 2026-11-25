use super::*;
use crate::{
    capture::{
        geometry::{MonitorGeometry, PhysicalRect},
        request::CaptureSnapshot,
    },
    recording::{
        options::RecordingOptions,
        review::{RecordingMetadata, RecordingReview},
        session::*,
    },
    ui::capture_overlay::CaptureOverlay,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
pub struct RecordingSelection {
    pub overlay: CaptureOverlay,
    pub scale: f32,
    pub session: Uuid,
}
#[derive(Clone)]
pub struct RecordingView {
    pub task_target: String,
    pub id: Uuid,
    pub mp4: bool,
    pub state: RecordingState,
    pub selection: Option<Arc<Mutex<RecordingSelection>>>,
    pub controls: Option<PhysicalRect>,
    pub options: Option<RecordingOptions>,
    pub elapsed: Duration,
    pub countdown: Duration,
    pub confirmation: bool,
    pub interruption: Option<Interruption>,
    pub error: Option<AppError>,
    pub metadata: Option<RecordingMetadata>,
    pub playback_path: Option<PathBuf>,
    pub archive_path: Option<PathBuf>,
    pub archive_failed: bool,
    pub uploaded: bool,
}
pub(super) struct RecordingWorkflow {
    pub(super) id: Uuid,
    mp4: bool,
    pub(super) session: RecordingSession,
    pub(super) selection: Option<Arc<Mutex<RecordingSelection>>>,
    controls: Option<PhysicalRect>,
    sources: Vec<ResolvedSource>,
    selection_ready: bool,
    monitors: Vec<MonitorGeometry>,
    review: Option<RecordingReview>,
    context: crate::workflow::runner::WorkflowContext,
    quit_pending: bool,
}
impl AppController {
    pub fn recording_view(&self) -> Option<RecordingView> {
        self.recording.as_ref().map(|workflow| RecordingView {
            task_target:workflow.context.destination.as_ref().map_or_else(||"Upload destination needs setup".into(),|d|format!("Upload target: {} ({})",d.label,d.origin.as_str())),
            id: workflow.id,
            mp4: workflow.mp4,
            state: workflow.session.state(),
            selection: workflow.selection.clone(),
            controls: workflow.controls,
            options: workflow.session.options().cloned(),
            elapsed: workflow.session.active_elapsed(),
            countdown: workflow.session.countdown_remaining(self.recording_origin.elapsed()),
            confirmation: workflow.session.confirmation_pending(),
            interruption: workflow.session.interruption(),
            error: workflow.session.error().cloned().or_else(|| {
                workflow.review.as_ref().filter(|review| review.archive_failed()).map(|_| {
                    AppError::Persistence("archive save failed; retry, choose folder, or continue without a local copy")
                })
            }),
            metadata: workflow.review.as_ref().map(|review| review.metadata().clone()),
            playback_path: workflow.review.as_ref().map(|review| review.source_path().to_owned()),
            archive_path: workflow.review.as_ref().and_then(|review| review.archive_path().map(PathBuf::from)).or_else(||self.task_runners.get(&workflow.id).and_then(|r|r.context().durable_path.clone())),
            archive_failed: workflow.review.as_ref().is_some_and(|review| review.archive_failed()),
            uploaded: workflow.review.as_ref().is_some_and(|review| review.admitted()),
        })
    }
    pub fn recording_origin(&self) -> Instant {
        self.recording_origin
    }
    pub(super) fn toggle_recording(
        &mut self,
        mp4: bool,
        selection: DestinationSelection,
    ) -> Result<Vec<AppEffect>, AppError> {
        if self.recording.is_some() {
            return self.recording_input(RecordingInput::ToggleFormat(mp4));
        }
        if !self.pending_acquisitions.is_empty()
            || self.utility.is_some()
            || self.scrolling.is_some()
            || self.capture.is_some()
        {
            return Err(AppError::InUse("a capture session is already active"));
        }
        self.manager.ensure_not_corrupted()?;
        let id = Uuid::new_v4();
        let mut session = RecordingSession::default();
        session.handle(RecordingInput::Select);
        let mut context = self.task_context(
            if mp4 {
                ArtifactKind::Mp4
            } else {
                ArtifactKind::Gif
            },
            Some(if mp4 {
                super::super::shortcuts::ShortcutAction::ToggleMp4Recording
            } else {
                super::super::shortcuts::ShortcutAction::ToggleGifRecording
            }),
            selection,
        );
        context.id = id;
        if let Some(snapshot) = &context.destination {
            self.workflows.insert(id, snapshot.clone());
            self.update_active()?;
        }
        self.recording = Some(RecordingWorkflow {
            id,
            mp4,
            session,
            selection: None,
            controls: None,
            sources: vec![],
            selection_ready: false,
            monitors: vec![],
            review: None,
            context,
            quit_pending: false,
        });
        let mut effects: Vec<_> = self
            .pins
            .suspend_for_capture()
            .into_iter()
            .map(AppEffect::Pin)
            .collect();
        self.window = None;
        effects.extend([
            AppEffect::Window(None),
            AppEffect::StopAudioTest,
            AppEffect::CancelConnectionTest,
            AppEffect::AcquireRecording {
                session: id,
                request: self
                    .settings()
                    .capture_request(crate::capture::request::CaptureMode::Region)?,
            },
        ]);
        self.pending_acquisitions.insert(id);
        Ok(effects)
    }
    pub(super) fn recording_acquired(
        &mut self,
        id: Uuid,
        result: Result<CaptureSnapshot, AppError>,
    ) -> Vec<AppEffect> {
        let Some(w) = self
            .recording
            .as_mut()
            .filter(|w| w.id == id && w.session.state() == RecordingState::Selecting)
        else {
            return vec![];
        };
        let result = result.and_then(|snapshot| {
            w.monitors = snapshot.monitors().to_vec();
            let scale = w.monitors.first().map_or(1.0, |m| m.scale_factor() as f32);
            let pointer =
                snapshot
                    .launch_pointer()
                    .unwrap_or(crate::capture::geometry::PhysicalPoint {
                        x: snapshot.bounds().left(),
                        y: snapshot.bounds().top(),
                    });
            let mut overlay = CaptureOverlay::from_snapshot(snapshot, pointer, (4, 4))?;
            overlay.set_recording_selection();
            Ok(Arc::new(Mutex::new(RecordingSelection {
                overlay,
                scale,
                session: id,
            })))
        });
        match result {
            Ok(surface) => {
                w.selection = Some(surface);
                vec![]
            }
            Err(e) => self
                .recording_input(RecordingInput::Failed(e))
                .unwrap_or_else(|e| vec![AppEffect::Error(e)]),
        }
    }
    pub(super) fn recording_selection_closed(&mut self, id: Uuid) {
        if let Some(w) = self.recording.as_mut().filter(|w| w.id == id) {
            w.selection_ready = true;
        }
    }
    pub(super) fn select_recording(
        &mut self,
        id: Uuid,
        rect: PhysicalRect,
    ) -> Result<Vec<AppEffect>, AppError> {
        let w = self
            .recording
            .as_ref()
            .filter(|w| w.id == id && w.session.state() == RecordingState::Selecting)
            .ok_or(AppError::InvalidInput("recording selection expired"))?;
        let monitor = w
            .monitors
            .iter()
            .find(|m| m.bounds().contains_rect(rect))
            .cloned()
            .ok_or(AppError::InvalidInput(
                "recording must stay inside one monitor",
            ))?;
        let zoom = monitor.scale_factor() as f32
            * self.settings().appearance.ui_scale_percent as f32
            / 100.0;
        let controls = crate::ui::recording_overlay::control_bounds(rect, monitor.bounds(), {
            let (w, h) = crate::ui::recording_overlay::CONTROLS_SIZE;
            ((w * zoom).ceil() as u32, (h * zoom).ceil() as u32)
        })?;
        self.recording
            .as_mut()
            .expect("recording selection")
            .controls = Some(controls);
        let w = self.recording.as_ref().expect("recording selection");
        let mut options = self.settings().recording_options.options(
            w.mp4,
            monitor.id().to_owned(),
            rect,
            self.settings().recording,
        );
        let sources =
            match crate::recording::audio::wasapi::resolve_recording_sources(options.audio()) {
                Ok(s) => s,
                Err(error) => {
                    let mut effects = self.recording_input(RecordingInput::Start {
                        options,
                        monitor,
                        now: self.recording_origin.elapsed(),
                    })?;
                    effects.extend(self.recording_input(RecordingInput::Failed(error))?);
                    return Ok(effects);
                }
            };
        if let crate::recording::options::FormatOptions::Mp4(m) = &mut options.format {
            for source in &sources {
                match source.kind {
                    crate::recording::audio::devices::SourceKind::System => {
                        m.audio.system =
                            crate::recording::options::EndpointSelection::Id(source.id.clone())
                    }
                    crate::recording::audio::devices::SourceKind::Microphone => {
                        m.audio.microphone =
                            crate::recording::options::EndpointSelection::Id(source.id.clone())
                    }
                }
            }
        }
        self.recording
            .as_mut()
            .expect("recording selection")
            .sources = sources;
        self.recording_input(RecordingInput::Start {
            options,
            monitor,
            now: self.recording_origin.elapsed(),
        })
    }
    pub(super) fn recording_input(
        &mut self,
        input: RecordingInput,
    ) -> Result<Vec<AppEffect>, AppError> {
        let Some(mut w) = self.recording.take() else {
            return Ok(vec![]);
        };
        if matches!(input, RecordingInput::Tick(_)) && !w.selection_ready {
            self.recording = Some(w);
            return Ok(vec![]);
        }
        if matches!(input, RecordingInput::ConfirmDiscard(false)) {
            w.quit_pending = false;
        }
        let input = match input {
            RecordingInput::StartWithoutAudio { .. } => RecordingInput::StartWithoutAudio {
                now: self.recording_origin.elapsed(),
            },
            other => other,
        };
        let result = (|| {
            let effects = w.session.handle(input);
            let mut out = vec![];
            for effect in effects {
                match effect {
                    RecordingEffect::StopMeters => {
                        w.selection = None;
                        out.push(AppEffect::StopAudioTest);
                    }
                    RecordingEffect::StartWorker(options) => {
                        out.push(AppEffect::StartRecordingWorker {
                            session: w.id,
                            options,
                            sources: w.sources.clone(),
                        })
                    }
                    RecordingEffect::Control(control) => {
                        out.push(AppEffect::RecordingControl(control))
                    }
                    RecordingEffect::Error(e) => out.push(AppEffect::Error(e)),
                    RecordingEffect::Review => {
                        if let Some(output) = w.session.take_output() {
                            let mut review = RecordingReview::new(output);
                            if let Err(e) = review.persist(
                                &self.artifacts,
                                self.history.clone(),
                                &w.context.effective.archive,
                                &self.settings().filename,
                                false,
                                false,
                            ) {
                                out.push(AppEffect::Error(e));
                            } else if review.history_record_failed() {
                                out.push(AppEffect::Error(AppError::Persistence(
                                    "recording saved, but its History entry could not be recorded",
                                )));
                            }
                            w.review = Some(review);
                        }
                    }
                    RecordingEffect::Save => {
                        if let Some(r) = &mut w.review {
                            r.persist(
                                &self.artifacts,
                                self.history.clone(),
                                &w.context.effective.archive,
                                &self.settings().filename,
                                false,
                                true,
                            )?;
                            if r.history_record_failed() {
                                out.push(AppEffect::Error(AppError::Persistence(
                                    "recording saved, but its History entry could not be recorded",
                                )));
                            }
                        }
                    }
                    RecordingEffect::Upload => {
                        let r = w
                            .review
                            .as_mut()
                            .ok_or(AppError::InvalidInput("no finalized recording"))?;
                        if r.admitted() {
                            continue;
                        }
                        r.persist(
                            &self.artifacts,
                            self.history.clone(),
                            &w.context.effective.archive,
                            &self.settings().filename,
                            true,
                            false,
                        )?;
                        if r.history_record_failed() {
                            out.push(AppEffect::Error(AppError::Persistence(
                                "recording saved, but its History entry could not be recorded",
                            )));
                        }
                        let artifact = r
                            .artifact()
                            .cloned()
                            .ok_or(AppError::Persistence("recording staging failed"))?;
                        let mut context = w.context.clone();
                        context.intent = crate::workflow::runner::TaskIntent::Direct(
                            crate::workflow::profile::CaptureTask::Upload,
                        );
                        if r.archive_skipped() {
                            context.effective.archive.storage_mode =
                                crate::persistence::settings::StorageMode::NeverAutomatic;
                        }
                        let mut runner = crate::workflow::runner::TaskRunner::new(context);
                        runner.accept_finalized(
                            Arc::new(artifact),
                            r.archive_path().map(PathBuf::from),
                        );
                        out.extend(self.launch_tasks(runner, None)?);
                        if self
                            .task_runners
                            .get(&w.id)
                            .is_some_and(|r| r.context().upload_job.is_some())
                        {
                            r.mark_admitted();
                        }
                    }
                    RecordingEffect::Discard => {
                        if let Some(job) = self
                            .task_runners
                            .get(&w.id)
                            .and_then(|r| r.context().upload_job)
                        {
                            out.extend(self.task_upload_changed(job));
                        }
                        if let Some(r) = &mut w.review {
                            r.discard(&self.artifacts, &self.history)?;
                        }
                        w.session = RecordingSession::default();
                    }
                }
            }
            Ok(out)
        })();
        let mut out = match result {
            Ok(e) => e,
            Err(e) => {
                self.recording = Some(w);
                return Err(e);
            }
        };
        if w.session.state() == RecordingState::Idle {
            self.workflows.remove(&w.id);
            self.update_active()?;
            out.push(AppEffect::CancelRecordingAcquisition);
            out.extend(
                self.pins
                    .restore_after_capture()
                    .into_iter()
                    .map(AppEffect::Pin),
            );
            if w.quit_pending {
                out.extend(self.apply(AppCommand::Quit)?);
            }
        } else {
            if matches!(
                w.session.state(),
                RecordingState::Review | RecordingState::Failed
            ) {
                out.extend(
                    self.pins
                        .restore_after_capture()
                        .into_iter()
                        .map(AppEffect::Pin),
                );
            }
            self.recording = Some(w);
        }
        Ok(out)
    }
    pub(super) fn retry_recording_archive(
        &mut self,
        continue_without: bool,
    ) -> Result<Vec<AppEffect>, AppError> {
        let Some(mut w) = self.recording.take() else {
            return Ok(vec![]);
        };
        w.context.effective.archive.archive_root = self.settings().archive.archive_root.clone();
        let result = (|| {
            if let Some(r) = &mut w.review {
                if continue_without {
                    r.continue_without_archive()?;
                }
                r.persist(
                    &self.artifacts,
                    self.history.clone(),
                    &w.context.effective.archive,
                    &self.settings().filename,
                    false,
                    false,
                )?;
            }
            Ok(vec![])
        })();
        self.recording = Some(w);
        result
    }
    pub(super) fn close_recording_review(&mut self) -> Result<Vec<AppEffect>, AppError> {
        let Some(w) = &self.recording else {
            return Ok(vec![]);
        };
        if w.session.state() != RecordingState::Review {
            return Err(AppError::InUse("finish or discard the recording first"));
        }
        if w.review
            .as_ref()
            .is_some_and(|r| r.archive_path().is_none() && !r.admitted())
            && !self
                .task_runners
                .get(&w.id)
                .is_some_and(|r| r.context().durable_path.is_some())
        {
            return Err(AppError::InUse(
                "save, upload, or discard the finalized recording first",
            ));
        }
        self.workflows.remove(&w.id);
        self.update_active()?;
        self.recording = None;
        Ok(vec![])
    }
    pub(super) fn quit_recording(&mut self) -> Result<Vec<AppEffect>, AppError> {
        if let Some(w) = &mut self.recording {
            if w.session.acquiring() {
                w.quit_pending = true;
                return self.recording_input(RecordingInput::Cancel);
            }
            if let Some(r) = &w.review
                && r.archive_path().is_none()
                && !r.admitted()
                && !self
                    .task_runners
                    .get(&w.id)
                    .is_some_and(|r| r.context().durable_path.is_some())
            {
                return Err(AppError::InUse(
                    "save or discard the finalized recording before quitting",
                ));
            }
            if let Some(w) = &self.recording {
                self.workflows.remove(&w.id);
                self.update_active()?;
            }
            self.recording = None;
        }
        Ok(vec![])
    }
}

impl AppController {
    pub(super) fn run_recording_tasks(&mut self) -> Result<Vec<AppEffect>, AppError> {
        let w = self
            .recording
            .as_ref()
            .ok_or(AppError::InUse("no recording review is open"))?;
        if w.session.state() != RecordingState::Review {
            return Err(AppError::InUse("finish recording review first"));
        }
        if self.task_runners.contains_key(&w.id) {
            let id = w.id;
            return Ok(vec![
                self.focus_history(crate::app::events::HistoryFocus::Workflow(id)),
            ]);
        }
        let review = w
            .review
            .as_ref()
            .ok_or(AppError::InUse("recording is not finalized"))?;
        let a = review
            .artifact()
            .cloned()
            .ok_or(AppError::InUse("retry recording persistence first"))?;
        let mut context = w.context.clone();
        context.intent = crate::workflow::runner::TaskIntent::RunSelectedTasks;
        if review.archive_skipped() {
            context.effective.archive.storage_mode =
                crate::persistence::settings::StorageMode::NeverAutomatic;
        }
        let mut runner = crate::workflow::runner::TaskRunner::new(context);
        runner.accept_finalized(Arc::new(a), review.archive_path().map(PathBuf::from));
        let id = runner.context().id;
        let mut out = self.launch_tasks(runner, None)?;
        out.push(self.focus_history(crate::app::events::HistoryFocus::Workflow(id)));
        Ok(out)
    }
}

impl AppController {
    pub(super) fn mark_recording_task_admitted(&mut self, id: Uuid) {
        if let Some(w) = self.recording.as_mut().filter(|w| w.id == id)
            && let Some(review) = &mut w.review
        {
            review.mark_admitted();
        }
    }
}

impl AppController {
    pub(super) fn recording_task_archive_saved(
        &mut self,
        id: Uuid,
        entry: crate::persistence::archive::ArchiveEntry,
    ) -> Result<(), AppError> {
        if let Some(w) = self.recording.as_mut().filter(|w| w.id == id)
            && let Some(review) = &mut w.review
        {
            // This association gates subsequent manual-origin protection and
            // discard decisions; a mismatch must stop dependent upload work.
            review.accept_task_archive(entry)?;
        }
        Ok(())
    }
}
