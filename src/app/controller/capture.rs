use super::*;
use crate::{
    app::image_finish::{ArchiveDecision, ImageFinishTransaction},
    capture::{
        geometry::MonitorGeometry,
        last_region::{FinishOutcome, LastRegion},
        request::{CaptureMode, CaptureSnapshot},
    },
    ui::{
        capture_overlay::{CaptureOverlay, OverlayAction},
        image_export::ImageFinish,
    },
};
use std::path::PathBuf;

/// UI state shared with exactly one deferred viewport. The controller validates
/// against this same document; snapshots never duplicate frozen image buffers.
pub struct CaptureSurface {
    pub task_target: String,
    pub image_local: bool,
    pub intended_pin: bool,
    pub scrolling_review: Option<crate::ui::scrolling_review::ScrollReviewStatus>,
    pub review_viewport: crate::ui::image_review::ImageViewport,
    pub session: Uuid,
    pub overlay: CaptureOverlay,
    pub archive_failed: bool,
    pub pending_finish: bool,
    pub native_scale: f32,
    pub dialog_open: bool,
    pub working: bool,
}
pub(super) struct CaptureWorkflow {
    pub(super) intended_pin: bool,
    pub(super) id: Uuid,
    pub(super) surface: Option<Arc<Mutex<CaptureSurface>>>,
    mode: CaptureMode,
    context: crate::workflow::runner::WorkflowContext,
    monitors: Vec<MonitorGeometry>,
    pending: Option<ImageFinishTransaction>,
    save_path: Option<PathBuf>,
}
impl AppController {
    pub(super) fn start_capture(
        &mut self,
        mode: CaptureMode,
        selection: DestinationSelection,
    ) -> Result<Vec<AppEffect>, AppError> {
        self.start_capture_profile(mode, selection, None)
    }
    pub(super) fn start_capture_profile(
        &mut self,
        mode: CaptureMode,
        selection: DestinationSelection,
        action: Option<super::super::shortcuts::ShortcutAction>,
    ) -> Result<Vec<AppEffect>, AppError> {
        if !self.pending_acquisitions.is_empty()
            || self.utility.is_some()
            || self.scrolling.is_some()
            || self.capture.is_some()
            || self
                .recording
                .as_ref()
                .is_some_and(|w| w.session.acquiring())
        {
            return Err(AppError::InUse("a capture session is already active"));
        }
        self.manager.ensure_not_corrupted()?;
        let request = self.settings().capture_request(mode)?;
        let id = Uuid::new_v4();
        let action = action.unwrap_or(match mode {
            CaptureMode::Region => super::super::shortcuts::ShortcutAction::RegionScreenshot,
            CaptureMode::Window => super::super::shortcuts::ShortcutAction::WindowScreenshot,
            CaptureMode::Fullscreen => {
                super::super::shortcuts::ShortcutAction::FullscreenScreenshot
            }
            CaptureMode::LastRegion => super::super::shortcuts::ShortcutAction::CaptureLastRegion,
        });
        let mut context = self.task_context(ArtifactKind::Screenshot, Some(action), selection);
        context.id = id;
        if let Some(d) = &context.destination {
            self.workflows.insert(id, d.clone());
            self.update_active()?;
        }
        self.capture = Some(CaptureWorkflow {
            intended_pin: false,
            id,
            surface: None,
            mode,
            context,
            monitors: vec![],
            pending: None,
            save_path: None,
        });
        self.window = None;
        let mut effects: Vec<_> = self
            .pins
            .suspend_for_capture()
            .into_iter()
            .map(AppEffect::Pin)
            .collect();
        effects.extend([
            AppEffect::Window(None),
            AppEffect::StopAudioTest,
            AppEffect::CancelConnectionTest,
            AppEffect::AcquireCapture {
                session: id,
                request,
            },
        ]);
        self.pending_acquisitions.insert(id);
        Ok(effects)
    }
    pub(super) fn capture_acquired(
        &mut self,
        session: Uuid,
        result: Result<CaptureSnapshot, AppError>,
    ) -> Vec<AppEffect> {
        let preferences = self.settings().clone();
        let Some(capture) = self
            .capture
            .as_mut()
            .filter(|c| c.id == session && c.surface.is_none())
        else {
            return vec![];
        };
        let result = result.and_then(|snapshot| {
            if snapshot.mode != capture.mode {
                return Err(AppError::Capture("capture mode changed during acquisition"));
            }
            capture.monitors = snapshot.monitors().to_vec();
            let native_scale = snapshot
                .monitors()
                .iter()
                .find(|m| {
                    m.bounds()
                        .contains(crate::capture::geometry::PhysicalPoint {
                            x: snapshot.bounds().left(),
                            y: snapshot.bounds().top(),
                        })
                })
                .map_or(1.0, |m| m.scale_factor() as f32);
            let pointer =
                snapshot
                    .launch_pointer()
                    .unwrap_or(crate::capture::geometry::PhysicalPoint {
                        x: snapshot.bounds().left(),
                        y: snapshot.bounds().top(),
                    });
            let mut overlay = CaptureOverlay::from_snapshot(snapshot, pointer, (4, 4))?;
            overlay.initialize_preferences(&preferences)?;
            if capture.intended_pin {
                overlay.retain_editor_after_selection();
            }
            Ok(Arc::new(Mutex::new(CaptureSurface {
                task_target: capture.context.destination.as_ref().map_or_else(
                    || "Upload destination needs setup".into(),
                    |d| format!("Upload target: {} ({})", d.label, d.origin.as_str()),
                ),
                image_local: false,
                intended_pin: capture.intended_pin,
                scrolling_review: None,
                review_viewport: Default::default(),
                session,
                overlay,
                archive_failed: false,
                pending_finish: false,
                native_scale,
                dialog_open: false,
                working: false,
            })))
        });
        let pin_effects = self.pins.restore_after_capture();
        let mut effects = match result {
            Ok(surface) => {
                capture.surface = Some(surface.clone());
                vec![AppEffect::CaptureSurface(Some(surface))]
            }
            Err(error) => {
                self.workflows.remove(&session);
                let reference_error = self.update_active().err();
                self.capture = None;
                self.window = Some(Window::Settings);
                vec![
                    AppEffect::CaptureSurface(None),
                    AppEffect::Window(self.window),
                    AppEffect::Error(error),
                ]
                .into_iter()
                .chain(reference_error.map(AppEffect::Error))
                .collect()
            }
        };
        effects.splice(0..0, pin_effects.into_iter().map(AppEffect::Pin));
        effects
    }
    pub(super) fn cancel_capture(&mut self, session: Uuid) -> Vec<AppEffect> {
        let Some(capture) = self.capture.as_mut().filter(|c| c.id == session) else {
            return vec![];
        };
        let mut effects = vec![];
        if capture.surface.is_none() {
            effects.extend(
                self.pins
                    .restore_after_capture()
                    .into_iter()
                    .map(AppEffect::Pin),
            );
        }
        if let Some(r) = self.task_runners.get_mut(&capture.context.id) {
            let mut snapshot = r.clone();
            snapshot.handle(crate::workflow::runner::TaskInput::Cancel);
            if let Err(e) = self.history.save_task_workflow(&snapshot) {
                // The cancelled state did not commit; keep the capture session
                // (and its still-active on-disk row) rather than discarding or
                // closing, so disk recovery cannot resurrect work the user asked
                // to cancel, and the user can retry the cancel.
                effects.push(AppEffect::Error(e));
                return effects;
            }
            *r = snapshot;
        }
        if let Some(pending) = &mut capture.pending
            && let Err(error) = pending.discard_unadmitted(&self.artifacts)
        {
            // A locked orphan can be recovered later. It must not trap a
            // cancelled editor or replay an external finish action.
            effects.push(AppEffect::Error(error));
        }
        self.task_images.remove(&capture.context.id);
        self.capture = None;
        self.workflows.remove(&session);
        if let Err(error) = self.update_active() {
            effects.push(AppEffect::Error(error));
        }
        effects.extend([
            AppEffect::CancelCaptureAcquisition,
            AppEffect::CaptureSurface(None),
        ]);
        effects
    }
    /// Returns to editing after a failed/pending finish without ending the
    /// capture session. A stale staged artifact must not survive a resume,
    /// so it is discarded (best-effort) the same way `cancel_capture` does.
    pub(super) fn resume_capture(&mut self, session: Uuid) -> Result<Vec<AppEffect>, AppError> {
        let mut effects = vec![];
        if let Some(capture) = self.capture.as_mut()
            && capture.id == session
        {
            if let Some(r) = self.task_runners.get_mut(&capture.context.id) {
                let mut snapshot = r.clone();
                snapshot.handle(crate::workflow::runner::TaskInput::Cancel);
                self.history.save_task_workflow(&snapshot)?;
                *r = snapshot;
            }
            self.task_images.remove(&capture.context.id);
            capture.context.id = Uuid::new_v4();
            if let Some(mut pending) = capture.pending.take()
                && let Err(error) = pending.discard_unadmitted(&self.artifacts)
            {
                effects.push(AppEffect::Error(error));
            }
            if let Some(surface) = &capture.surface {
                let mut surface = surface
                    .lock()
                    .map_err(|_| AppError::Capture("capture surface unavailable"))?;
                surface.working = false;
                surface.archive_failed = false;
                surface.pending_finish = false;
                surface.overlay.finish_failed(AppError::Cancelled);
            }
        }
        Ok(effects)
    }
    pub(super) fn finish_capture(
        &mut self,
        session: Uuid,
        finish: Option<ImageFinish>,
        save_path: Option<PathBuf>,
        decision: ArchiveDecision,
    ) -> Vec<AppEffect> {
        let Some(mut capture) = self.capture.take() else {
            return vec![AppEffect::Error(AppError::InvalidInput(
                "capture session is no longer active",
            ))];
        };
        if capture.id != session {
            self.capture = Some(capture);
            return vec![AppEffect::Error(AppError::InvalidInput(
                "capture session changed",
            ))];
        }
        let result = self.finish_capture_inner(&mut capture, finish, save_path, decision);
        match result {
            Ok(mut effects) => {
                self.workflows.remove(&session);
                if let Err(error) = self.update_active() {
                    effects.push(AppEffect::Error(error));
                }
                effects.push(AppEffect::CaptureSurface(None));
                effects
            }
            Err(error) => {
                if let Some(surface) = &capture.surface
                    && let Ok(mut surface) = surface.lock()
                {
                    surface.working = false;
                    surface.archive_failed =
                        capture.pending.as_ref().is_some_and(|p| p.archive_failed());
                    surface.pending_finish = capture.pending.is_some();
                    surface.overlay.finish_failed(error.clone());
                }
                self.capture = Some(capture);
                vec![AppEffect::Error(error)]
            }
        }
    }
    fn finish_capture_inner(
        &mut self,
        capture: &mut CaptureWorkflow,
        finish: Option<ImageFinish>,
        save_path: Option<PathBuf>,
        decision: ArchiveDecision,
    ) -> Result<Vec<AppEffect>, AppError> {
        self.manager.ensure_not_corrupted()?;
        let surface = capture
            .surface
            .as_ref()
            .ok_or(AppError::InUse("capture is still preparing"))?;
        // Hold the same editor lock through acceptance and IO. The UI cannot
        // change the current document between validation and committing effects.
        let mut surface = surface
            .lock()
            .map_err(|_| AppError::Capture("capture surface unavailable"))?;
        if surface
            .scrolling_review
            .as_ref()
            .is_some_and(|r| !r.accepted)
        {
            return Err(AppError::InUse(
                "accept the partial scrolling result before output",
            ));
        }
        let retrying = finish.is_none();
        let mut discard_error = None;
        if let Some(finish) = finish {
            surface.overlay.validate_finish(&finish)?;
            let same = capture.pending.as_ref().is_some_and(|previous| {
                previous.finish().action == finish.action
                    && Arc::ptr_eq(
                        previous.finish().prepared.encoded(),
                        finish.prepared.encoded(),
                    )
            });
            if !same {
                if let Some(r) = self.task_runners.get_mut(&capture.context.id) {
                    let mut snapshot = r.clone();
                    snapshot.handle(crate::workflow::runner::TaskInput::Cancel);
                    // This checkpoint gates retirement of the old source.
                    self.history.save_task_workflow(&snapshot)?;
                    *r = snapshot;
                    self.task_images.remove(&capture.context.id);
                    capture.context.id = Uuid::new_v4();
                }
                // A locked orphan spool file can be recovered later; it must
                // not drop the user's new finish/edit, which is the actual
                // intent that just happened.
                if let Some(previous) = &mut capture.pending
                    && let Err(error) = previous.discard_unadmitted(&self.artifacts)
                {
                    discard_error = Some(error);
                }
                capture.pending = Some(ImageFinishTransaction::new(finish)?);
            }
            if save_path.is_some() {
                capture.save_path = save_path;
            }
        }
        let pending = capture
            .pending
            .as_mut()
            .ok_or(AppError::InvalidInput("no image finish is waiting"))?;
        surface.overlay.validate_finish(pending.finish())?;
        let crop = surface
            .overlay
            .document()
            .crop()
            .ok_or(AppError::InvalidInput("select an output region"))?;
        // Validate geometry before any external effect, but do not call
        // LastRegion::after_finish until the explicit action actually succeeds.
        if matches!(capture.mode, CaptureMode::Region | CaptureMode::LastRegion) {
            LastRegion::new(crop, &capture.monitors)?;
        }
        let action = pending.finish().action;
        if action == OverlayAction::RunSelectedTasks && self.pins.snapshots().is_empty() {
            self.pins.update_work_areas(
                capture.monitors.iter().map(|m| m.bounds()).collect(),
                pending.finish().screen_location.unwrap_or(
                    crate::capture::geometry::PhysicalPoint {
                        x: crop.left(),
                        y: crop.top(),
                    },
                ),
            )?;
        }
        let pin_image = if action == OverlayAction::Pin {
            let image =
                crate::pin::PinnedImage::new(pending.finish().prepared.pixels().clone(), None)?;
            self.pins.check_capacity(&image)?;
            if self.pins.snapshots().is_empty() {
                self.pins.update_work_areas(
                    capture.monitors.iter().map(|m| m.bounds()).collect(),
                    pending.finish().screen_location.unwrap_or(
                        crate::capture::geometry::PhysicalPoint {
                            x: crop.left(),
                            y: crop.top(),
                        },
                    ),
                )?;
            }
            self.pins
                .validate_open(&image, pending.finish().screen_location)?;
            Some(image)
        } else {
            None
        };
        if action == OverlayAction::Upload && !self.workflows.contains_key(&capture.id) {
            if capture.context.destination.is_none() {
                let mut resolver =
                    crate::workflow::runner::TaskRunner::new(capture.context.clone());
                resolver.repair_destination(&self.settings().destinations)?;
                capture.context = resolver.context().clone();
            }
            let snapshot = capture
                .context
                .destination
                .clone()
                .ok_or(AppError::NeedsSetup(
                    "the snapshotted destination needs setup",
                ))?;
            self.store.read(&snapshot.credential)?;
            self.workflows.insert(capture.id, snapshot);
            self.update_active()?;
        }
        if action != OverlayAction::RunSelectedTasks {
            if retrying {
                capture.context.effective.archive.archive_root =
                    self.settings().archive.archive_root.clone();
            }
            pending.persist(
                &self.artifacts,
                self.history.clone(),
                &capture.context.effective.archive,
                &self.settings().filename,
                capture.save_path.as_deref(),
                decision,
            )?;
        }
        // Save already published a durable file. Other actions share the one
        // staged authoritative export, including configured multi-action chains.
        let task_artifact = pending.task_artifact(&self.artifacts, &self.settings().filename)?;
        let mut effects = vec![];
        let mut primary_task_error = None;
        if let Some(error) = discard_error {
            effects.push(AppEffect::Error(error));
        }
        let _ = pin_image; // preflight above runs before any save or upload
        let intent = match action {
            OverlayAction::Upload => crate::workflow::runner::TaskIntent::Direct(
                crate::workflow::profile::CaptureTask::Upload,
            ),
            OverlayAction::Copy => crate::workflow::runner::TaskIntent::Direct(
                crate::workflow::profile::CaptureTask::CopyImage,
            ),
            OverlayAction::Pin => crate::workflow::runner::TaskIntent::Direct(
                crate::workflow::profile::CaptureTask::Pin,
            ),
            OverlayAction::Save => crate::workflow::runner::TaskIntent::Direct(
                crate::workflow::profile::CaptureTask::SaveAs,
            ),
            OverlayAction::RunSelectedTasks => {
                crate::workflow::runner::TaskIntent::RunSelectedTasks
            }
            _ => return Err(AppError::InvalidInput("choose an image finish action")),
        };
        let retryable = self
            .task_runners
            .get(&capture.context.id)
            .is_some_and(|r| !r.complete());
        if retryable {
            let step = self.task_runners[&capture.context.id]
                .states()
                .iter()
                .find(|s| {
                    matches!(
                        s.outcome,
                        crate::workflow::steps::StepOutcome::Failed(_)
                            | crate::workflow::steps::StepOutcome::NeedsUserAction(_)
                    )
                })
                .map(|s| s.step);
            if let Some(step) = step {
                let out = self.drive_tasks(
                    capture.context.id,
                    crate::workflow::runner::TaskInput::Retry(step),
                );
                primary_task_error = out.iter().find_map(|effect| match effect {
                    AppEffect::Error(e) => Some(e.clone()),
                    _ => None,
                });
                effects.extend(out);
            }
        } else {
            // A missing or already-cancelled/complete runner must never be treated
            // as "nothing to do, so the finish succeeded" -- that silently drops the
            // action the user just requested (e.g. re-finishing the same image after
            // cancelling a prior failed attempt). Start a fresh runner under a new id.
            if self.task_runners.contains_key(&capture.context.id) {
                self.task_images.remove(&capture.context.id);
                capture.context.id = Uuid::new_v4();
            }
            let mut context = capture.context.clone();
            context.intent = intent;
            if pending.archive_skipped() {
                context.effective.archive.storage_mode =
                    crate::persistence::settings::StorageMode::NeverAutomatic;
            }
            let mut runner = crate::workflow::runner::TaskRunner::new(context);
            runner.accept_finalized(
                Arc::new(task_artifact),
                pending
                    .saved_path()
                    .map(PathBuf::from)
                    .or_else(|| pending.archive().map(|a| a.path.clone())),
            );
            match self.launch_tasks(runner, Some(pending.finish().clone())) {
                Ok(out) => {
                    primary_task_error = out.iter().find_map(|effect| match effect {
                        AppEffect::Error(e) => Some(e.clone()),
                        _ => None,
                    });
                    effects.extend(out);
                }
                Err(e) if action == OverlayAction::Save => effects.push(AppEffect::Error(e)),
                Err(e) => return Err(e),
            }
        }

        if action != OverlayAction::RunSelectedTasks
            && let Some(primary_task_error) = primary_task_error
            && self.task_runners.get(&capture.context.id).is_some_and(|r| {
                r.context().upload_job.is_none()
                    && r.states().iter().any(|s| {
                        matches!(
                            s.outcome,
                            crate::workflow::steps::StepOutcome::Failed(_)
                                | crate::workflow::steps::StepOutcome::NeedsUserAction(_)
                        )
                    })
            })
        {
            return Err(primary_task_error);
        }
        let needs_attention = self
            .task_runners
            .get(&capture.context.id)
            .is_some_and(|runner| {
                runner.states().iter().any(|step| {
                    matches!(
                        step.outcome,
                        crate::workflow::steps::StepOutcome::NeedsUserAction(_)
                            | crate::workflow::steps::StepOutcome::Failed(_)
                    )
                })
            });
        if action == OverlayAction::RunSelectedTasks
            && (!surface.overlay.finished_on_release() || needs_attention)
        {
            self.window = Some(Window::Tasks);
            effects.push(AppEffect::Window(self.window));
        }
        // Once an external effect succeeds it must never be replayed because
        // best-effort cleanup/settings bookkeeping failed afterwards.
        if self
            .task_runners
            .get(&capture.context.id)
            .is_some_and(|r| r.complete() && r.context().upload_job.is_none())
            && let Err(error) = pending.discard_unadmitted(&self.artifacts)
        {
            effects.push(AppEffect::Error(error));
        }
        if matches!(capture.mode, CaptureMode::Region | CaptureMode::LastRegion) {
            let region = LastRegion::after_finish(
                self.settings().last_region.clone(),
                capture.mode,
                FinishOutcome::Confirmed(crop),
                &capture.monitors,
            );
            match region.and_then(|region| self.manager.set_last_region(region)) {
                Ok(()) => effects.push(AppEffect::SettingsSaved(self.settings().clone())),
                Err(error) => effects.push(AppEffect::Error(error)),
            }
        }
        surface.pending_finish = false;
        Ok(effects)
    }
}

impl AppController {
    pub(super) fn open_scrolling_review(
        &mut self,
        id: Uuid,
        review: crate::scrolling::session::ScrollReview,
        monitors: Vec<MonitorGeometry>,
        context: crate::workflow::runner::WorkflowContext,
    ) -> Result<Vec<AppEffect>, AppError> {
        let (mut overlay, status) =
            crate::ui::scrolling_review::ScrollingReview::new(review)?.into_parts();
        overlay.initialize_preferences(self.settings())?;
        let surface = Arc::new(Mutex::new(CaptureSurface {
            task_target: context.destination.as_ref().map_or_else(
                || "Upload destination needs setup".into(),
                |d| format!("Upload target: {} ({})", d.label, d.origin.as_str()),
            ),
            image_local: true,
            intended_pin: false,
            scrolling_review: Some(status),
            review_viewport: Default::default(),
            session: id,
            overlay,
            archive_failed: false,
            pending_finish: false,
            native_scale: 1.0,
            dialog_open: false,
            working: false,
        }));
        self.capture = Some(CaptureWorkflow {
            intended_pin: false,
            id,
            surface: Some(surface.clone()),
            mode: CaptureMode::Fullscreen,
            context,
            monitors,
            pending: None,
            save_path: None,
        });
        Ok(vec![AppEffect::CaptureSurface(Some(surface))])
    }
}

impl AppController {
    pub(super) fn accept_upload_source(
        &mut self,
        source: super::super::upload_sources::UploadSource,
    ) -> Result<Vec<AppEffect>, AppError> {
        self.accept_upload_source_for(source, DestinationSelection::Inherit, None)
    }
    pub(super) fn accept_upload_source_for(
        &mut self,
        source: super::super::upload_sources::UploadSource,
        selection: DestinationSelection,
        action: Option<super::super::shortcuts::ShortcutAction>,
    ) -> Result<Vec<AppEffect>, AppError> {
        use super::super::upload_sources::*;
        match route_source(source, SourceFocus::ExplicitAction)? {
            SourceAction::UploadFiles(paths) => self
                .accept_launch_for(
                    super::super::cli::LaunchRequest::upload(paths)?,
                    selection,
                    action,
                )
                .map(|(_, effects)| effects),
            SourceAction::ReviewImage(image) => {
                if !self.pending_acquisitions.is_empty()
                    || self.utility.is_some()
                    || self.capture.is_some()
                    || self.scrolling.is_some()
                    || self
                        .recording
                        .as_ref()
                        .is_some_and(|w| w.session.acquiring())
                {
                    return Err(AppError::InUse(
                        "finish the active capture before opening clipboard review",
                    ));
                }
                let mut overlay =
                    crate::ui::image_review::ImageReview::from_image(image)?.into_editor();
                overlay.initialize_preferences(self.settings())?;
                let context = self.task_context(ArtifactKind::Screenshot, action, selection);
                let id = context.id;
                let surface = Arc::new(Mutex::new(CaptureSurface {
                    image_local: true,
                    intended_pin: false,
                    task_target: context.destination.as_ref().map_or_else(
                        || "Upload destination needs setup".into(),
                        |d| format!("Upload target: {} ({})", d.label, d.origin.as_str()),
                    ),
                    scrolling_review: None,
                    review_viewport: Default::default(),
                    session: id,
                    overlay,
                    archive_failed: false,
                    pending_finish: false,
                    native_scale: 1.0,
                    dialog_open: false,
                    working: false,
                }));
                if let Some(d) = &context.destination {
                    self.workflows.insert(id, d.clone());
                    if let Err(error) = self.update_active() {
                        self.workflows.remove(&id);
                        return Err(error);
                    }
                }
                self.capture = Some(CaptureWorkflow {
                    intended_pin: false,
                    id,
                    surface: Some(surface.clone()),
                    mode: CaptureMode::Fullscreen,
                    context,
                    monitors: vec![],
                    pending: None,
                    save_path: None,
                });
                Ok(vec![AppEffect::CaptureSurface(Some(surface))])
            }
            SourceAction::KeepTextPaste => Ok(vec![]),
        }
    }
}
