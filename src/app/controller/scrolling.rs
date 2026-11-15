use super::*;
use crate::{
    capture::{
        geometry::{MonitorGeometry, PhysicalRect},
        windows::WindowCandidate,
    },
    platform::windows::{
        capture::ScrollingSelectionSnapshot,
        windows::{ScrollingTarget, Windows},
    },
    scrolling::{
        runtime::{RuntimeEvent, ScrollControl, ScrollUpdate},
        session::ScrollState,
    },
    ui::capture_overlay::CaptureOverlay,
};
#[derive(Clone)]
pub struct ScrollingView {
    pub id: Uuid,
    pub selection: Option<Arc<Mutex<RecordingSelection>>>,
    pub controls: Option<PhysicalRect>,
    pub update: Option<ScrollUpdate>,
}
pub(super) struct ScrollingWorkflow {
    pub id: Uuid,
    pub selection: Option<Arc<Mutex<RecordingSelection>>>,
    windows: Option<Windows>,
    candidates: Vec<WindowCandidate>,
    monitors: Vec<MonitorGeometry>,
    controls: Option<PhysicalRect>,
    target: Option<ScrollingTarget>,
    selection_ready: bool,
    update: Option<ScrollUpdate>,
    context: crate::workflow::runner::WorkflowContext,
}
impl AppController {
    pub fn scrolling_view(&self) -> Option<ScrollingView> {
        self.scrolling.as_ref().map(|w| ScrollingView {
            id: w.id,
            selection: w.selection.clone(),
            controls: w.controls,
            update: w.update.clone(),
        })
    }
    pub(super) fn toggle_scrolling(
        &mut self,
        selection: DestinationSelection,
    ) -> Result<Vec<AppEffect>, AppError> {
        if let Some(w) = &self.scrolling {
            return Ok(if !w.selection_ready {
                self.cancel_scrolling()
            } else {
                vec![AppEffect::ScrollingControl(ScrollControl::Stop)]
            });
        }
        if self.capture.is_some() || self.recording.is_some() {
            return Err(AppError::InUse("a capture or review is already active"));
        }
        self.manager.ensure_not_corrupted()?;
        let id = Uuid::new_v4();
        let mut context = self.task_context(
            ArtifactKind::Screenshot,
            Some(super::super::shortcuts::ShortcutAction::ToggleScrollingCapture),
            selection,
        );
        context.id = id;
        if let Some(d) = &context.destination {
            self.workflows.insert(id, d.clone());
            self.update_active()?;
        }
        self.scrolling = Some(ScrollingWorkflow {
            id,
            selection: None,
            windows: None,
            candidates: vec![],
            monitors: vec![],
            controls: None,
            target: None,
            selection_ready: false,
            update: None,
            context,
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
            AppEffect::AcquireScrolling(id),
        ]);
        Ok(effects)
    }
    pub(super) fn scrolling_acquired(
        &mut self,
        id: Uuid,
        result: Result<ScrollingSelectionSnapshot, AppError>,
    ) -> Vec<AppEffect> {
        let Some(w) = self
            .scrolling
            .as_mut()
            .filter(|w| w.id == id && w.windows.is_none())
        else {
            return vec![];
        };
        let result = result.and_then(|acquired| {
            let snapshot = acquired.snapshot;
            w.windows = Some(acquired.windows);
            w.candidates = snapshot.windows().to_vec();
            w.monitors = snapshot.monitors().to_vec();
            let scale = w.monitors.first().map_or(1.0, |m|m.scale_factor() as f32);
            let pointer = snapshot.launch_pointer().unwrap_or(crate::capture::geometry::PhysicalPoint {
                x:snapshot.bounds().left(), y:snapshot.bounds().top(),
            });
            let mut overlay = CaptureOverlay::from_snapshot(snapshot,pointer,(4,4))?;
            overlay.set_scrolling_selection();
            overlay.set_recording_summary("Select content inside one window and one monitor. Wait for the first accepted segment, then scroll normally.".into());
            Ok(Arc::new(Mutex::new(RecordingSelection {overlay,scale,session:id})))
        });
        match result {
            Ok(surface) => {
                w.selection = Some(surface);
                vec![]
            }
            Err(e) => {
                let mut effects = self.cancel_scrolling();
                effects.push(AppEffect::Error(e));
                effects
            }
        }
    }
    pub(super) fn select_scrolling(
        &mut self,
        id: Uuid,
        rect: PhysicalRect,
    ) -> Result<Vec<AppEffect>, AppError> {
        let w = self
            .scrolling
            .as_mut()
            .filter(|w| w.id == id && w.selection.is_some())
            .ok_or(AppError::Capture("scroll selection expired"))?;
        let monitor = w
            .monitors
            .iter()
            .find(|m| m.bounds().contains_rect(rect))
            .cloned()
            .ok_or(AppError::InvalidInput(
                "scroll viewport must fit one monitor",
            ))?;
        // The first intersecting window must contain the whole viewport; never select
        // a lower window through a partially overlapping foreground window.
        let candidate = w
            .candidates
            .iter()
            .find(|c| c.bounds.intersection(rect).is_some())
            .copied()
            .filter(|c| c.bounds.contains_rect(rect))
            .ok_or(AppError::InvalidInput(
                "scroll viewport must fit a single unobscured target window",
            ))?;
        let zoom = monitor.scale_factor() as f32
            * self.manager.settings().appearance.ui_scale_percent as f32
            / 100.0;
        let controls = crate::ui::recording_overlay::control_bounds(
            rect,
            monitor.bounds(),
            ((450.0 * zoom).ceil() as u32, (220.0 * zoom).ceil() as u32),
        )?;
        let windows = w
            .windows
            .as_ref()
            .cloned()
            .ok_or(AppError::Capture("scroll identity inventory unavailable"))?;
        w.target = Some(ScrollingTarget::new(windows, candidate, rect, monitor)?);
        w.controls = Some(controls);
        w.windows = None;
        w.selection = None;
        // Worker starts only after the GUI acknowledges hide + DwmFlush.
        Ok(vec![])
    }
    pub(super) fn scrolling_selection_closed(&mut self, id: Uuid) -> Vec<AppEffect> {
        let Some(w) = self
            .scrolling
            .as_mut()
            .filter(|w| w.id == id && !w.selection_ready)
        else {
            return vec![];
        };
        w.selection_ready = true;
        match w.target.take() {
            Some(target) => vec![AppEffect::StartScrolling {
                session: id,
                target,
            }],
            None => vec![],
        }
    }
    pub(super) fn cancel_scrolling(&mut self) -> Vec<AppEffect> {
        let Some(workflow) = self.scrolling.take() else {
            return vec![];
        };
        self.workflows.remove(&workflow.id);
        let mut effects = vec![AppEffect::CancelScrolling];
        if let Err(e) = self.update_active() {
            effects.push(AppEffect::Error(e));
        }
        effects.extend(
            self.pins
                .restore_after_capture()
                .into_iter()
                .map(AppEffect::Pin),
        );
        effects
    }
    pub(super) fn scrolling_event(&mut self, event: RuntimeEvent) -> Vec<AppEffect> {
        let Some(w) = self.scrolling.as_mut().filter(|w| w.id == event.session) else {
            return vec![];
        };
        w.update = Some(event.update.clone());
        let mut effects = vec![];
        if let Some(error) = event.error {
            effects.push(AppEffect::Error(error));
        }
        if let Some(review) = event.review {
            let w = self.scrolling.take().expect("validated scrolling workflow");
            effects.extend(
                self.pins
                    .restore_after_capture()
                    .into_iter()
                    .map(AppEffect::Pin),
            );
            match self.open_scrolling_review(w.id, review, w.monitors, w.context) {
                Ok(out) => effects.extend(out),
                Err(e) => {
                    effects.push(AppEffect::Error(e));
                    self.workflows.remove(&w.id);
                    if let Err(error) = self.update_active() {
                        effects.push(AppEffect::Error(error));
                    }
                }
            }
        } else if event.update.state == ScrollState::Cancelled {
            effects.extend(self.cancel_scrolling());
        }
        effects
    }
}
