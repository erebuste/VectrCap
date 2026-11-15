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
    pub session: Uuid,
    pub overlay: CaptureOverlay,
    pub archive_failed: bool,
    pub pending_finish: bool,
    pub native_scale: f32,
    pub dialog_open: bool,
    pub working: bool,
}
#[derive(Clone)]
pub struct PinImage {
    pub id: Uuid,
    pub pixels: Arc<image::RgbaImage>,
    pub screen_location: Option<crate::capture::geometry::PhysicalPoint>,
}
pub(super) struct CaptureWorkflow {
    pub(super) id: Uuid,
    pub(super) surface: Option<Arc<Mutex<CaptureSurface>>>,
    mode: CaptureMode,
    selection: DestinationSelection,
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
        if self.capture.is_some() {
            return Err(AppError::InUse("a capture session is already active"));
        }
        self.manager.ensure_not_corrupted()?;
        let request = self.settings().capture_request(mode)?;
        let id = Uuid::new_v4();
        self.capture = Some(CaptureWorkflow {
            id,
            surface: None,
            mode,
            selection,
            monitors: vec![],
            pending: None,
            save_path: None,
        });
        self.window = None;
        Ok(vec![
            AppEffect::Window(None),
            AppEffect::CancelConnectionTest,
            AppEffect::AcquireCapture {
                session: id,
                request,
            },
        ])
    }
    pub(super) fn capture_acquired(
        &mut self,
        session: Uuid,
        result: Result<CaptureSnapshot, AppError>,
    ) -> Vec<AppEffect> {
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
            let overlay = CaptureOverlay::from_snapshot(snapshot, pointer, (4, 4))?;
            Ok(Arc::new(Mutex::new(CaptureSurface {
                session,
                overlay,
                archive_failed: false,
                pending_finish: false,
                native_scale,
                dialog_open: false,
                working: false,
            })))
        });
        match result {
            Ok(surface) => {
                capture.surface = Some(surface.clone());
                vec![AppEffect::CaptureSurface(Some(surface))]
            }
            Err(error) => {
                self.capture = None;
                self.window = Some(Window::Settings);
                vec![
                    AppEffect::CaptureSurface(None),
                    AppEffect::Window(self.window),
                    AppEffect::Error(error),
                ]
            }
        }
    }
    pub(super) fn cancel_capture(&mut self, session: Uuid) -> Vec<AppEffect> {
        let Some(capture) = self.capture.as_mut().filter(|c| c.id == session) else {
            return vec![];
        };
        let mut effects = vec![];
        if let Some(pending) = &mut capture.pending
            && let Err(error) = pending.discard_unadmitted(&self.artifacts)
        {
            // A locked orphan can be recovered later. It must not trap a
            // cancelled editor or replay an external finish action.
            effects.push(AppEffect::Error(error));
        }
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
        if action == OverlayAction::Pin && self.pins.len() >= 32 {
            return Err(AppError::InUse("close a pin before creating another"));
        }
        if action == OverlayAction::Upload && !self.workflows.contains_key(&capture.id) {
            let snapshot = resolve_destination(
                capture.selection,
                DestinationSelection::Inherit,
                &self.settings().destinations,
            )?;
            self.store.read(&snapshot.credential)?;
            self.workflows.insert(capture.id, snapshot);
            self.update_active()?;
        }
        pending.persist(
            &self.artifacts,
            self.history.clone(),
            &self.settings().archive,
            &self.settings().filename,
            capture.save_path.as_deref(),
            decision,
        )?;
        let mut effects = vec![];
        if let Some(error) = discard_error {
            effects.push(AppEffect::Error(error));
        }
        match action {
            OverlayAction::Upload => {
                let artifact = pending
                    .artifact()
                    .ok_or(AppError::Persistence("upload source was not staged"))?;
                self.artifacts.verify(artifact)?;
                let job = self.history.create_job(
                    artifact,
                    self.workflows
                        .get(&capture.id)
                        .ok_or(AppError::Persistence("upload destination unavailable"))?,
                )?;
                effects.extend([
                    AppEffect::JobAccepted(job.id),
                    AppEffect::Queue(crate::vectr::queue::QueueCommand::Admit(job.id)),
                ]);
            }
            OverlayAction::Copy => self
                .clipboard
                .write_image(pending.finish().prepared.pixels())?,
            OverlayAction::Pin => {
                self.pins.insert(capture.id);
                effects.push(AppEffect::PinImage(PinImage {
                    id: capture.id,
                    pixels: pending.finish().prepared.pixels().clone(),
                    screen_location: pending.finish().screen_location,
                }));
            }
            OverlayAction::Save => {}
            _ => return Err(AppError::InvalidInput("choose an image finish action")),
        }
        // Once an external effect succeeds it must never be replayed because
        // best-effort cleanup/settings bookkeeping failed afterwards.
        if action != OverlayAction::Upload
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
