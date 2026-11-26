//! Frozen-snapshot selection only. Outcomes never perform output or task actions.
use super::{
    geometry::{MonitorGeometry, PhysicalPoint, PhysicalRect, same_layout},
    preferences::{CaptureMouseAction, MouseButton, RegionCapturePreferences, SizePreset},
    request::{CaptureMode, CaptureSnapshot},
    windows::WindowCandidate,
};
use crate::app::error::AppError;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputContext {
    Region,
    Drawing,
    Text,
    Chrome,
}
#[derive(Debug)]
pub enum SelectionInput {
    PointerMove(PhysicalPoint),
    PrimaryDown {
        point: PhysicalPoint,
        context: InputContext,
    },
    PrimaryUp(PhysicalPoint),
    MoveCrop {
        dx: i32,
        dy: i32,
    },
    ResizeCrop(PhysicalRect),
    Clear,
    LayoutChanged(Vec<MonitorGeometry>),
    Escape,
    Auxiliary(MouseButton),
    /// Held modifiers: Alt snaps to a size preset, Shift draws a square and
    /// Ctrl moves the new selection instead of resizing it (as in ShareX).
    Modifiers {
        alt: bool,
        shift: bool,
        ctrl: bool,
    },
    /// Space: the whole capture source.
    SelectFullscreen,
    /// `~`: the monitor under this point.
    SelectMonitorAt(PhysicalPoint),
    Context(InputContext),
}
#[derive(Debug, PartialEq, Eq)]
pub enum SelectionOutcome {
    Hover(Option<PhysicalRect>),
    Selected(PhysicalRect),
    Changed(PhysicalRect),
    Cancelled,
    Unchanged,
    Rejected(AppError),
    EditCancelled,
    ToolChanged(InputContext),
}
#[derive(Clone, Copy)]
enum GestureKind {
    New,
    Move(PhysicalRect),
    Resize {
        bounds: PhysicalRect,
        left: bool,
        top: bool,
        right: bool,
        bottom: bool,
    },
}
struct Gesture {
    anchor: PhysicalPoint,
    last: PhysicalPoint,
    kind: GestureKind,
    dragged: bool,
}
pub struct SelectionModel {
    bounds: PhysicalRect,
    permitted: PhysicalRect,
    active_monitor: PhysicalRect,
    monitors: Vec<MonitorGeometry>,
    windows: Vec<WindowCandidate>,
    hover_areas: Vec<super::windows::HoverArea>,
    preferences: RegionCapturePreferences,
    threshold: (u32, u32),
    crop: Option<PhysicalRect>,
    gesture: Option<Gesture>,
    context: InputContext,
    alt: bool,
    shift: bool,
    ctrl: bool,
    cancelled: bool,
    /// The crop is still the one the capture opened with (Fullscreen, Window,
    /// Last region): a plain click on it confirms, like releasing a drag does.
    preset: bool,
}
impl SelectionModel {
    pub fn new(
        snapshot: &CaptureSnapshot,
        preferences: RegionCapturePreferences,
        pointer: PhysicalPoint,
        threshold: (u32, u32),
    ) -> Result<Self, AppError> {
        preferences.validate()?;
        if threshold.0 == 0 || threshold.1 == 0 || threshold.0 > 1024 || threshold.1 > 1024 {
            return Err(AppError::InvalidInput("drag threshold is out of range"));
        }
        let active_monitor = snapshot
            .monitors
            .iter()
            .find(|m| m.bounds().contains(pointer))
            .and_then(|m| {
                if snapshot.mode == CaptureMode::Window {
                    // An explicit window fixes the source independently of the launch
                    // monitor. Region-only restrictions must not clip its initial crop.
                    Some(snapshot.bounds)
                } else {
                    m.bounds().intersection(snapshot.bounds)
                }
            })
            .ok_or(AppError::Capture(
                "launch pointer is outside the capture source",
            ))?;
        // Fullscreen is the whole desktop, so the active-monitor limit can't clip it.
        let permitted = if preferences.active_monitor_only
            && !matches!(snapshot.mode, CaptureMode::Window | CaptureMode::Fullscreen)
        {
            active_monitor
        } else {
            snapshot.bounds
        };
        let model = Self {
            bounds: snapshot.bounds,
            permitted,
            active_monitor,
            monitors: snapshot.monitors.clone(),
            windows: snapshot.windows.clone(),
            hover_areas: snapshot.hover_areas.clone(),
            preferences,
            threshold,
            crop: snapshot.initial_crop,
            preset: snapshot.initial_crop.is_some(),
            gesture: None,
            context: InputContext::Region,
            alt: false,
            shift: false,
            ctrl: false,
            cancelled: false,
        };
        if let Some(crop) = model.crop {
            model.validate_crop(crop)?;
        }
        Ok(model)
    }
    pub fn crop(&self) -> Option<PhysicalRect> {
        self.crop
    }
    /// Restore the editor's authoritative crop after history or a rejected edit.
    pub fn synchronize_crop(&mut self, crop: Option<PhysicalRect>) -> Result<(), AppError> {
        if let Some(crop) = crop {
            self.validate_crop(crop)?;
        }
        self.crop = crop;
        self.gesture = None;
        Ok(())
    }
    pub fn permitted_bounds(&self) -> PhysicalRect {
        self.permitted
    }
    fn point_available(&self, p: PhysicalPoint) -> bool {
        self.permitted.contains(p) && self.monitors.iter().any(|m| m.bounds().contains(p))
    }
    fn validate_crop(&self, r: PhysicalRect) -> Result<(), AppError> {
        r.rgba_len()?;
        if !self.permitted.contains_rect(r)
            || !self
                .monitors
                .iter()
                .any(|m| m.bounds().intersection(r).is_some())
        {
            return Err(AppError::InvalidInput(
                "selection must fit the permitted capture source",
            ));
        }
        Ok(())
    }
    fn hover(&self, p: PhysicalPoint) -> Option<PhysicalRect> {
        if !self.preferences.window_detection || !self.point_available(p) {
            return None;
        }
        if self.hover_areas.is_empty() {
            self.windows.iter().map(|w| w.bounds).find(|r| r.contains(p))
        } else {
            self.hover_areas
                .iter()
                .find(|a| a.hit.contains(p))
                .map(|a| a.select)
        }
        .and_then(|r| r.intersection(self.permitted))
    }
    fn accept(&mut self, r: PhysicalRect, changed: bool) -> SelectionOutcome {
        if let Err(error) = self.validate_crop(r) {
            return SelectionOutcome::Rejected(error);
        }
        self.crop = Some(r);
        self.preset = false;
        if changed {
            SelectionOutcome::Changed(r)
        } else {
            SelectionOutcome::Selected(r)
        }
    }
    fn escape(&mut self) -> SelectionOutcome {
        if self.gesture.take().is_some() {
            return SelectionOutcome::EditCancelled;
        }
        if self.context == InputContext::Text {
            self.context = InputContext::Region;
            return SelectionOutcome::EditCancelled;
        }
        self.cancelled = true;
        self.crop = None;
        SelectionOutcome::Cancelled
    }
    fn sized_rect(
        &self,
        anchor: PhysicalPoint,
        point: PhysicalPoint,
        size: SizePreset,
    ) -> Result<PhysicalRect, AppError> {
        let x = if point.x < anchor.x {
            i64::from(anchor.x) - i64::from(size.width)
        } else {
            i64::from(anchor.x)
        };
        let y = if point.y < anchor.y {
            i64::from(anchor.y) - i64::from(size.height)
        } else {
            i64::from(anchor.y)
        };
        let r = PhysicalRect::new(
            i32::try_from(x).map_err(|_| AppError::InvalidInput("fixed selection overflows"))?,
            i32::try_from(y).map_err(|_| AppError::InvalidInput("fixed selection overflows"))?,
            size.width,
            size.height,
        )?;
        self.validate_crop(r)?;
        Ok(r)
    }
    fn gesture_rect(&self, g: &Gesture, p: PhysicalPoint) -> Result<PhysicalRect, AppError> {
        match g.kind {
            GestureKind::New => {
                if self.preferences.fixed_size {
                    return self.sized_rect(g.anchor, p, self.preferences.fixed_dimensions);
                }
                if self.alt {
                    let (w, h) = (g.anchor.x.abs_diff(p.x), g.anchor.y.abs_diff(p.y));
                    let preset = self
                        .preferences
                        .presets
                        .iter()
                        .min_by_key(|s| {
                            let dw = u128::from(w.abs_diff(s.width));
                            let dh = u128::from(h.abs_diff(s.height));
                            dw * dw + dh * dh
                        })
                        .ok_or(AppError::InvalidInput(
                            "no region size presets are configured",
                        ))?;
                    return self.sized_rect(g.anchor, p, *preset);
                }
                crate::editor::shapes::constrained_shape(g.anchor, p, self.shift)?
                    .intersection(self.permitted)
                    .ok_or(AppError::InvalidInput("selection is empty"))
            }
            GestureKind::Move(r) => {
                let dx = i32::try_from(i64::from(p.x) - i64::from(g.anchor.x))
                    .map_err(|_| AppError::InvalidInput("crop movement overflows"))?;
                let dy = i32::try_from(i64::from(p.y) - i64::from(g.anchor.y))
                    .map_err(|_| AppError::InvalidInput("crop movement overflows"))?;
                let moved = r.translated(dx, dy)?;
                self.validate_crop(moved)?;
                Ok(moved)
            }
            GestureKind::Resize {
                bounds,
                left,
                top,
                right,
                bottom,
            } => {
                if self.preferences.fixed_size {
                    return Err(AppError::InvalidInput(
                        "fixed-size selection cannot be resized",
                    ));
                }
                let a = PhysicalPoint {
                    x: if left { p.x } else { bounds.left() },
                    y: if top { p.y } else { bounds.top() },
                };
                let b = PhysicalPoint {
                    x: if right { p.x } else { bounds.right() },
                    y: if bottom { p.y } else { bounds.bottom() },
                };
                PhysicalRect::from_points(a, b)?
                    .intersection(self.permitted)
                    .ok_or(AppError::InvalidInput("selection is empty"))
            }
        }
    }
    /// Holding Ctrl while drawing a new selection drags it along with the pointer.
    fn carry_anchor(g: &mut Gesture, p: PhysicalPoint, ctrl: bool) -> Result<(), AppError> {
        if ctrl && matches!(g.kind, GestureKind::New) {
            let shift = |a: i32, from: i32, to: i32| {
                i32::try_from(i64::from(a) + i64::from(to) - i64::from(from))
                    .map_err(|_| AppError::InvalidInput("selection movement overflows"))
            };
            g.anchor = PhysicalPoint {
                x: shift(g.anchor.x, g.last.x, p.x)?,
                y: shift(g.anchor.y, g.last.y, p.y)?,
            };
        }
        g.last = p;
        Ok(())
    }
    /// The whole permitted source, or the monitor under `at` within it.
    fn select_whole(&mut self, at: Option<PhysicalPoint>) -> SelectionOutcome {
        if self.context != InputContext::Region || self.gesture.is_some() {
            return SelectionOutcome::Unchanged;
        }
        let r = match at {
            None => self.permitted.intersection(self.bounds),
            Some(p) => self
                .monitors
                .iter()
                .find(|m| m.bounds().contains(p))
                .and_then(|m| m.bounds().intersection(self.permitted)),
        };
        r.map_or(SelectionOutcome::Unchanged, |r| self.accept(r, false))
    }
    /// A drag preview is separate from the committed crop; Escape restores the latter.
    pub fn preview(&self, p: PhysicalPoint) -> Result<Option<PhysicalRect>, AppError> {
        self.gesture
            .as_ref()
            .filter(|g| g.dragged)
            .map(|g| self.gesture_rect(g, p))
            .transpose()
    }
    pub fn handle(&mut self, input: SelectionInput) -> SelectionOutcome {
        use SelectionInput as I;
        use SelectionOutcome as O;
        if self.cancelled {
            return O::Cancelled;
        }
        match input {
            I::LayoutChanged(layout) => {
                if same_layout(&self.monitors, &layout) {
                    O::Unchanged
                } else {
                    self.gesture = None;
                    self.crop = None;
                    self.cancelled = true;
                    O::Cancelled
                }
            }
            I::Escape => self.escape(),
            I::Context(context) => {
                self.context = context;
                self.gesture = None;
                O::Unchanged
            }
            I::Modifiers { alt, shift, ctrl } => {
                self.alt = alt;
                self.shift = shift;
                self.ctrl = ctrl;
                O::Unchanged
            }
            I::SelectFullscreen => self.select_whole(None),
            I::SelectMonitorAt(p) => self.select_whole(Some(p)),
            I::Clear => {
                self.crop = None;
                self.gesture = None;
                O::Unchanged
            }
            I::PointerMove(p) => {
                if let Some(g) = self.gesture.as_mut() {
                    if let Err(e) = Self::carry_anchor(g, p, self.ctrl) {
                        return O::Rejected(e);
                    }
                    g.dragged |= p.x.abs_diff(g.anchor.x) > self.threshold.0
                        || p.y.abs_diff(g.anchor.y) > self.threshold.1;
                    if !g.dragged {
                        return O::Unchanged;
                    }
                    match self.gesture_rect(self.gesture.as_ref().expect("gesture is present"), p) {
                        Ok(r) => O::Changed(r),
                        Err(e) => O::Rejected(e),
                    }
                } else if self.crop.is_none() && self.context == InputContext::Region {
                    O::Hover(self.hover(p))
                } else {
                    O::Unchanged
                }
            }
            I::PrimaryDown { point, context } => {
                self.context = context;
                self.gesture = None;
                if context != InputContext::Region || !self.point_available(point) {
                    return O::Unchanged;
                }
                let kind = if let Some(r) = self.crop.filter(|r| r.contains(point)) {
                    // Physical-pixel handles: render the same five-pixel edge zones in the overlay.
                    let left = point.x.abs_diff(r.left()) <= 5;
                    let top = point.y.abs_diff(r.top()) <= 5;
                    let right = point.x.abs_diff(r.right()) <= 5;
                    let bottom = point.y.abs_diff(r.bottom()) <= 5;
                    if left || top || right || bottom {
                        GestureKind::Resize {
                            bounds: r,
                            left,
                            top,
                            right,
                            bottom,
                        }
                    } else {
                        GestureKind::Move(r)
                    }
                } else {
                    GestureKind::New
                };
                self.gesture = Some(Gesture {
                    anchor: point,
                    last: point,
                    kind,
                    dragged: false,
                });
                O::Unchanged
            }
            I::PrimaryUp(p) => {
                let Some(mut g) = self.gesture.take() else {
                    return O::Unchanged;
                };
                if let Err(e) = Self::carry_anchor(&mut g, p, self.ctrl) {
                    return O::Rejected(e);
                }
                g.dragged |= p.x.abs_diff(g.anchor.x) > self.threshold.0
                    || p.y.abs_diff(g.anchor.y) > self.threshold.1;
                if g.dragged {
                    match self.gesture_rect(&g, p) {
                        Ok(r) => self.accept(r, !matches!(g.kind, GestureKind::New)),
                        Err(e) => O::Rejected(e),
                    }
                } else if matches!(g.kind, GestureKind::New) {
                    if self.preferences.fixed_size {
                        match self.sized_rect(g.anchor, g.anchor, self.preferences.fixed_dimensions)
                        {
                            Ok(r) => self.accept(r, false),
                            Err(e) => O::Rejected(e),
                        }
                    } else {
                        self.hover(g.anchor)
                            .map_or(O::Unchanged, |r| self.accept(r, false))
                    }
                } else if self.preset
                    && let Some(crop) = self.crop
                {
                    // A click (no drag) on the selection the capture opened
                    // with takes the shot, as releasing a drawn region does.
                    self.preset = false;
                    O::Selected(crop)
                } else {
                    O::Unchanged
                }
            }
            I::MoveCrop { dx, dy } => {
                if self.context != InputContext::Region {
                    return O::Unchanged;
                }
                let Some(r) = self.crop else {
                    return O::Unchanged;
                };
                match r.translated(dx, dy) {
                    Ok(r) => self.accept(r, true),
                    Err(e) => O::Rejected(e),
                }
            }
            I::ResizeCrop(r) => {
                if self.context != InputContext::Region || self.crop.is_none() {
                    return O::Unchanged;
                }
                if self.preferences.fixed_size {
                    return O::Rejected(AppError::InvalidInput(
                        "fixed-size selection cannot be resized",
                    ));
                }
                match r.intersection(self.permitted) {
                    Some(r) => self.accept(r, true),
                    None => O::Rejected(AppError::InvalidInput("selection is outside the source")),
                }
            }
            I::Auxiliary(button) => {
                let action = self.preferences.mouse.action(button);
                if action == CaptureMouseAction::CancelActiveOrCapture {
                    return self.escape();
                }
                if self.context == InputContext::Text
                    || self.context == InputContext::Chrome
                    || self.gesture.is_some()
                {
                    return O::Unchanged;
                }
                match action {
                    CaptureMouseAction::None => O::Unchanged,
                    CaptureMouseAction::ToggleSelectionDrawing => {
                        self.context = if self.context == InputContext::Region {
                            InputContext::Drawing
                        } else {
                            InputContext::Region
                        };
                        O::ToolChanged(self.context)
                    }
                    CaptureMouseAction::SelectFullscreen => self.select_whole(None),
                    CaptureMouseAction::SelectActiveMonitor => {
                        self.accept(self.active_monitor, false)
                    }
                    CaptureMouseAction::CancelActiveOrCapture => unreachable!(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::geometry::{MonitorGeometry, PhysicalPoint, PhysicalRect};

    /// One 200×200 screen captured with its whole area preselected, as
    /// Fullscreen (and Window / Last region) captures open.
    fn preselected() -> (SelectionModel, PhysicalRect) {
        preselected_with(None)
    }
    /// `crop` defaults to the whole screen.
    fn preselected_with(crop: Option<PhysicalRect>) -> (SelectionModel, PhysicalRect) {
        let bounds = PhysicalRect::new(0, 0, 200, 200).unwrap();
        let mut snapshot = CaptureSnapshot::new(
            vec![0; 200 * 200 * 4],
            bounds,
            vec![MonitorGeometry::new("only".into(), bounds, 1.0).unwrap()],
            vec![],
        )
        .unwrap();
        let crop = crop.unwrap_or(bounds);
        snapshot.initial_crop = Some(crop);
        let model = SelectionModel::new(
            &snapshot,
            RegionCapturePreferences::default(),
            PhysicalPoint { x: 50, y: 50 },
            (4, 4),
        )
        .unwrap();
        (model, crop)
    }
    fn press(m: &mut SelectionModel, p: PhysicalPoint) {
        m.handle(SelectionInput::PrimaryDown {
            point: p,
            context: InputContext::Region,
        });
    }

    #[test]
    fn a_click_confirms_a_preselected_capture() {
        let (mut m, bounds) = preselected();
        let p = PhysicalPoint { x: 100, y: 100 };
        press(&mut m, p);
        assert_eq!(
            m.handle(SelectionInput::PrimaryUp(p)),
            SelectionOutcome::Selected(bounds)
        );
    }

    #[test]
    fn dragging_a_preselected_capture_still_moves_it() {
        let (mut m, _) = preselected_with(Some(PhysicalRect::new(50, 50, 80, 80).unwrap()));
        press(&mut m, PhysicalPoint { x: 100, y: 100 });
        let moved = m.handle(SelectionInput::PrimaryUp(PhysicalPoint { x: 60, y: 100 }));
        assert!(matches!(
            moved,
            SelectionOutcome::Changed(_) | SelectionOutcome::Selected(_)
        ));
    }

    /// Two side-by-side 200×200 monitors, nothing preselected.
    fn two_monitors() -> SelectionModel {
        let left = PhysicalRect::new(0, 0, 200, 200).unwrap();
        let right = PhysicalRect::new(200, 0, 200, 200).unwrap();
        let bounds = PhysicalRect::new(0, 0, 400, 200).unwrap();
        let snapshot = CaptureSnapshot::new(
            vec![0; 400 * 200 * 4],
            bounds,
            vec![
                MonitorGeometry::new("left".into(), left, 1.0).unwrap(),
                MonitorGeometry::new("right".into(), right, 1.0).unwrap(),
            ],
            vec![],
        )
        .unwrap();
        SelectionModel::new(
            &snapshot,
            RegionCapturePreferences::default(),
            PhysicalPoint { x: 50, y: 50 },
            (4, 4),
        )
        .unwrap()
    }
    fn mods(m: &mut SelectionModel, shift: bool, ctrl: bool) {
        m.handle(SelectionInput::Modifiers {
            alt: false,
            shift,
            ctrl,
        });
    }

    #[test]
    fn shift_draws_a_square() {
        let mut m = two_monitors();
        mods(&mut m, true, false);
        press(&mut m, PhysicalPoint { x: 10, y: 10 });
        assert_eq!(
            m.handle(SelectionInput::PrimaryUp(PhysicalPoint { x: 50, y: 30 })),
            SelectionOutcome::Selected(PhysicalRect::new(10, 10, 40, 40).unwrap())
        );
    }

    #[test]
    fn ctrl_moves_the_new_selection_instead_of_resizing() {
        let mut m = two_monitors();
        press(&mut m, PhysicalPoint { x: 10, y: 10 });
        m.handle(SelectionInput::PointerMove(PhysicalPoint { x: 50, y: 50 }));
        mods(&mut m, false, true);
        m.handle(SelectionInput::PointerMove(PhysicalPoint { x: 60, y: 70 }));
        assert_eq!(
            m.handle(SelectionInput::PrimaryUp(PhysicalPoint { x: 60, y: 70 })),
            SelectionOutcome::Selected(PhysicalRect::new(20, 30, 40, 40).unwrap())
        );
    }

    #[test]
    fn space_selects_the_desktop_and_tilde_the_monitor_under_the_pointer() {
        let mut m = two_monitors();
        assert_eq!(
            m.handle(SelectionInput::SelectFullscreen),
            SelectionOutcome::Selected(PhysicalRect::new(0, 0, 400, 200).unwrap())
        );
        assert_eq!(
            m.handle(SelectionInput::SelectMonitorAt(PhysicalPoint {
                x: 300,
                y: 10
            })),
            SelectionOutcome::Selected(PhysicalRect::new(200, 0, 200, 200).unwrap())
        );
    }

    #[test]
    fn only_the_first_click_confirms() {
        let (mut m, _) = preselected();
        let p = PhysicalPoint { x: 100, y: 100 };
        press(&mut m, p);
        m.handle(SelectionInput::PrimaryUp(p));
        press(&mut m, p);
        assert_eq!(
            m.handle(SelectionInput::PrimaryUp(p)),
            SelectionOutcome::Unchanged
        );
    }
}
