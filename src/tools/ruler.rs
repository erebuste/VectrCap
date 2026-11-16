use super::{UtilitySnapshot, sampler::sample_pixel};
use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, PhysicalRect},
};
pub const MAX_MEASUREMENTS: usize = 256;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RectHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}
pub fn resize_rect(
    rect: PhysicalRect,
    handle: RectHandle,
    point: PhysicalPoint,
) -> Result<PhysicalRect, AppError> {
    use RectHandle::*;
    let mut a = PhysicalPoint {
        x: rect.left(),
        y: rect.top(),
    };
    let mut b = PhysicalPoint {
        x: rect.right() - 1,
        y: rect.bottom() - 1,
    };
    if matches!(handle, TopLeft | Left | BottomLeft) {
        a.x = point.x;
    }
    if matches!(handle, TopRight | Right | BottomRight) {
        b.x = point.x;
    }
    if matches!(handle, TopLeft | Top | TopRight) {
        a.y = point.y;
    }
    if matches!(handle, BottomLeft | Bottom | BottomRight) {
        b.y = point.y;
    }
    rect_from_endpoints(a, b)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}
impl Axis {
    pub fn toggle(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct RectMeasurement {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub width: u32,
    pub height: u32,
    pub area: u64,
    pub perimeter: u64,
    pub distance: f64,
    pub angle_degrees: Option<f64>,
}
impl RectMeasurement {
    pub fn text(&self) -> String {
        format!(
            "X: {}  Y: {}\nRight: {}  Bottom: {}\nWidth: {} px  Height: {} px\nArea: {} px²  Perimeter: {} px\nDistance: {:.2} px  Angle: {}",
            self.left,
            self.top,
            self.right,
            self.bottom,
            self.width,
            self.height,
            self.area,
            self.perimeter,
            self.distance,
            self.angle_degrees.map_or_else(
                || "Unavailable (single pixel)".into(),
                |a| format!("{a:.2}°")
            )
        )
    }
}
pub fn rect_from_endpoints(a: PhysicalPoint, b: PhysicalPoint) -> Result<PhysicalRect, AppError> {
    PhysicalRect::new(
        a.x.min(b.x),
        a.y.min(b.y),
        a.x.abs_diff(b.x)
            .checked_add(1)
            .ok_or(AppError::InvalidInput("ruler width overflows"))?,
        a.y.abs_diff(b.y)
            .checked_add(1)
            .ok_or(AppError::InvalidInput("ruler height overflows"))?,
    )
}
pub fn measure_rect(rect: PhysicalRect) -> Result<RectMeasurement, AppError> {
    let dx = f64::from(rect.width() - 1);
    let dy = f64::from(rect.height() - 1);
    Ok(RectMeasurement {
        left: rect.left(),
        top: rect.top(),
        right: rect.right() - 1,
        bottom: rect.bottom() - 1,
        width: rect.width(),
        height: rect.height(),
        area: rect.pixel_count(),
        perimeter: 2 * (u64::from(rect.width()) + u64::from(rect.height())),
        distance: dx.hypot(dy),
        angle_degrees: if dx == 0.0 && dy == 0.0 {
            None
        } else {
            Some(dy.atan2(dx).to_degrees())
        },
    })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMeasurement {
    pub start: PhysicalPoint,
    pub end: PhysicalPoint,
    pub axis: Axis,
    pub length: u32,
}
impl RunMeasurement {
    pub fn rect(&self) -> Result<PhysicalRect, AppError> {
        rect_from_endpoints(self.start, self.end)
    }
}
pub fn measure_run(
    snapshot: &UtilitySnapshot,
    anchor: PhysicalPoint,
    axis: Axis,
    tolerance_percent: u8,
) -> Result<RunMeasurement, AppError> {
    if tolerance_percent > 100 {
        return Err(AppError::InvalidInput(
            "ruler tolerance must be 0 to 100 percent",
        ));
    }
    let color = sample_pixel(snapshot, anchor)?.rgb;
    let monitor = snapshot
        .monitors()
        .iter()
        .find(|m| m.bounds().contains(anchor))
        .ok_or(AppError::Capture("ruler anchor unavailable"))?
        .bounds();
    let threshold = (u16::from(tolerance_percent) * 255 / 100) as u8;
    let (dx, dy) = match axis {
        Axis::Horizontal => (1, 0),
        Axis::Vertical => (0, 1),
    };
    let walk = |direction: i32| -> PhysicalPoint {
        let mut last = anchor;
        while let Some((x, y)) = last
            .x
            .checked_add(dx * direction)
            .zip(last.y.checked_add(dy * direction))
        {
            let next = PhysicalPoint { x, y };
            if !monitor.contains(next) {
                break;
            }
            let Ok(candidate) = sample_pixel(snapshot, next) else {
                break;
            };
            if color
                .iter()
                .zip(candidate.rgb)
                .any(|(&a, b)| a.abs_diff(b) > threshold)
            {
                break;
            }
            last = next;
        }
        last
    };
    let start = walk(-1);
    let end = walk(1);
    let length = match axis {
        Axis::Horizontal => start.x.abs_diff(end.x) + 1,
        Axis::Vertical => start.y.abs_diff(end.y) + 1,
    };
    Ok(RunMeasurement {
        start,
        end,
        axis,
        length,
    })
}
pub struct RulerState {
    measurements: Vec<PhysicalRect>,
    selected: Option<usize>,
    pub axis: Axis,
    pub tolerance_percent: u8,
}
impl Default for RulerState {
    fn default() -> Self {
        Self {
            measurements: vec![],
            selected: None,
            axis: Axis::Horizontal,
            tolerance_percent: 0,
        }
    }
}
impl RulerState {
    pub fn measurements(&self) -> &[PhysicalRect] {
        &self.measurements
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }
    pub fn selected(&self) -> Option<PhysicalRect> {
        self.selected
            .and_then(|i| self.measurements.get(i).copied())
    }
    pub fn select(&mut self, point: PhysicalPoint) -> bool {
        self.selected = self.measurements.iter().rposition(|r| r.contains(point));
        self.selected.is_some()
    }
    pub fn select_index(&mut self, index: Option<usize>) -> Result<(), AppError> {
        if index.is_some_and(|i| i >= self.measurements.len()) {
            return Err(AppError::InvalidInput("measurement no longer exists"));
        }
        self.selected = index;
        Ok(())
    }
    pub fn replace(&mut self, index: usize, rect: PhysicalRect) -> Result<(), AppError> {
        let target = self
            .measurements
            .get_mut(index)
            .ok_or(AppError::InvalidInput("measurement no longer exists"))?;
        *target = rect;
        self.selected = Some(index);
        Ok(())
    }
    pub fn add(&mut self, rect: PhysicalRect) -> Result<(), AppError> {
        if self.measurements.len() >= MAX_MEASUREMENTS {
            return Err(AppError::InvalidInput(
                "ruler measurement limit reached (256)",
            ));
        }
        self.measurements.push(rect);
        self.selected = Some(self.measurements.len() - 1);
        Ok(())
    }
    pub fn move_selected(&mut self, dx: i32, dy: i32, shift: bool) -> Result<(), AppError> {
        if let Some(i) = self.selected {
            let step = if shift { 10 } else { 1 };
            let delta = dx
                .checked_mul(step)
                .zip(dy.checked_mul(step))
                .ok_or(AppError::InvalidInput("ruler movement overflows"))?;
            let moved = self.measurements[i].translated(delta.0, delta.1)?;
            self.measurements[i] = moved;
        }
        Ok(())
    }
    pub fn adjust_tolerance(&mut self, delta: i8) {
        self.tolerance_percent =
            (i16::from(self.tolerance_percent) + i16::from(delta)).clamp(0, 100) as u8;
    }
    pub fn clear(&mut self) {
        self.measurements.clear();
        self.selected = None;
    }
    pub fn remove_selected(&mut self) -> bool {
        if let Some(i) = self.selected.take() {
            self.measurements.remove(i);
            true
        } else {
            false
        }
    }
}
