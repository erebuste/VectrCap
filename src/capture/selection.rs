//! Frozen-snapshot selection only. Outcomes never perform output or task actions.
use super::{
    geometry::{MonitorGeometry, PhysicalPoint, PhysicalRect, same_layout},
    preferences::{CaptureMouseAction, MouseButton, RegionCapturePreferences, SizePreset},
    request::CaptureSnapshot,
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
    Alt(bool),
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
    kind: GestureKind,
    dragged: bool,
}
pub struct SelectionModel {
    bounds: PhysicalRect,
    permitted: PhysicalRect,
    active_monitor: PhysicalRect,
    monitors: Vec<MonitorGeometry>,
    windows: Vec<WindowCandidate>,
    preferences: RegionCapturePreferences,
    threshold: (u32, u32),
    crop: Option<PhysicalRect>,
    gesture: Option<Gesture>,
    context: InputContext,
    alt: bool,
    cancelled: bool,
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
            .and_then(|m| m.bounds().intersection(snapshot.bounds))
            .ok_or(AppError::Capture(
                "launch pointer is outside the capture source",
            ))?;
        let permitted = if preferences.active_monitor_only {
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
            preferences,
            threshold,
            crop: snapshot.initial_crop,
            gesture: None,
            context: InputContext::Region,
            alt: false,
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
        self.windows
            .iter()
            .find(|w| w.bounds.contains(p))
            .and_then(|w| w.bounds.intersection(self.permitted))
    }
    fn accept(&mut self, r: PhysicalRect, changed: bool) -> SelectionOutcome {
        if let Err(error) = self.validate_crop(r) {
            return SelectionOutcome::Rejected(error);
        }
        self.crop = Some(r);
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
                PhysicalRect::from_points(g.anchor, p)?
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
            I::Alt(alt) => {
                self.alt = alt;
                O::Unchanged
            }
            I::Clear => {
                self.crop = None;
                self.gesture = None;
                O::Unchanged
            }
            I::PointerMove(p) => {
                if let Some(g) = self.gesture.as_mut() {
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
                    kind,
                    dragged: false,
                });
                O::Unchanged
            }
            I::PrimaryUp(p) => {
                let Some(mut g) = self.gesture.take() else {
                    return O::Unchanged;
                };
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
                    CaptureMouseAction::SelectFullscreen => self.accept(
                        self.permitted
                            .intersection(self.bounds)
                            .expect("permitted source bounds"),
                        false,
                    ),
                    CaptureMouseAction::SelectActiveMonitor => {
                        self.accept(self.active_monitor, false)
                    }
                    CaptureMouseAction::CancelActiveOrCapture => unreachable!(),
                }
            }
        }
    }
}
