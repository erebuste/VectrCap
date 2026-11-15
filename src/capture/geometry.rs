use crate::app::error::AppError;
use serde::{Deserialize, Serialize};

/// Shared decoded-pixel budget (decimal megapixels). Larger requests are refused, never downscaled.
pub const MAX_CAPTURE_PIXELS: u64 = 64_000_000;
const MAX_MONITOR_ID_SCALARS: usize = 256;
/// Windows display scaling tops out at 500%; allow headroom without accepting nonsense.
const MAX_SCALE_FACTOR: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

/// Nonempty rectangle in physical pixels whose exclusive right/bottom edges fit in `i32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RectData", into = "RectData")]
pub struct PhysicalRect {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

impl PhysicalRect {
    pub fn from_points(a: PhysicalPoint, b: PhysicalPoint) -> Result<Self, AppError> {
        Self::new(
            a.x.min(b.x),
            a.y.min(b.y),
            a.x.abs_diff(b.x),
            a.y.abs_diff(b.y),
        )
    }
    pub fn translated(&self, dx: i32, dy: i32) -> Result<Self, AppError> {
        let left = self
            .left
            .checked_add(dx)
            .ok_or(AppError::InvalidInput("crop movement overflows"))?;
        let top = self
            .top
            .checked_add(dy)
            .ok_or(AppError::InvalidInput("crop movement overflows"))?;
        Self::new(left, top, self.width, self.height)
    }
    pub fn right(&self) -> i32 {
        (i64::from(self.left) + i64::from(self.width)) as i32
    }
    pub fn bottom(&self) -> i32 {
        (i64::from(self.top) + i64::from(self.height)) as i32
    }
    pub fn contains(&self, p: PhysicalPoint) -> bool {
        p.x >= self.left && p.x < self.right() && p.y >= self.top && p.y < self.bottom()
    }
    pub fn contains_rect(&self, other: Self) -> bool {
        other.left >= self.left
            && other.top >= self.top
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
    pub fn intersection(&self, other: Self) -> Option<Self> {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > left && bottom > top).then(|| Self {
            left,
            top,
            width: left.abs_diff(right),
            height: top.abs_diff(bottom),
        })
    }
    pub fn pixel_offset(&self, point: PhysicalPoint) -> Result<(u32, u32), AppError> {
        if !self.contains(point) {
            return Err(AppError::InvalidInput("point is outside source pixels"));
        }
        Ok((self.left.abs_diff(point.x), self.top.abs_diff(point.y)))
    }
    pub fn new(left: i32, top: i32, width: u32, height: u32) -> Result<Self, AppError> {
        if width == 0 || height == 0 {
            return Err(AppError::InvalidInput("rectangle must not be empty"));
        }
        let fits = |start: i32, len: u32| i64::from(start) + i64::from(len) <= i64::from(i32::MAX);
        if !fits(left, width) || !fits(top, height) {
            return Err(AppError::InvalidInput(
                "rectangle exceeds the physical coordinate range",
            ));
        }
        Ok(PhysicalRect {
            left,
            top,
            width,
            height,
        })
    }

    pub fn left(&self) -> i32 {
        self.left
    }

    pub fn top(&self) -> i32 {
        self.top
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixel_count(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Byte length of an RGBA buffer for this rectangle, checked against the pixel budget
    /// before any allocation.
    pub fn rgba_len(&self) -> Result<usize, AppError> {
        let pixels = self.pixel_count();
        if pixels > MAX_CAPTURE_PIXELS {
            return Err(AppError::InvalidInput(
                "capture exceeds the 64-megapixel budget",
            ));
        }
        usize::try_from(pixels * 4)
            .map_err(|_| AppError::InvalidInput("capture buffer does not fit in memory"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorGeometry {
    id: String,
    bounds: PhysicalRect,
    scale_factor: f64,
}

impl MonitorGeometry {
    /// Logical coordinates are local to this monitor, never a globally scaled desktop.
    pub fn logical_to_physical(&self, x: f64, y: f64) -> Result<PhysicalPoint, AppError> {
        let convert = |value: f64, origin: i32| -> Result<i32, AppError> {
            let physical = (value * self.scale_factor + f64::from(origin)).round();
            if !physical.is_finite()
                || physical < f64::from(i32::MIN)
                || physical > f64::from(i32::MAX)
            {
                return Err(AppError::InvalidInput(
                    "DPI conversion exceeds coordinate range",
                ));
            }
            Ok(physical as i32)
        };
        Ok(PhysicalPoint {
            x: convert(x, self.bounds.left)?,
            y: convert(y, self.bounds.top)?,
        })
    }
    pub fn physical_to_logical(&self, p: PhysicalPoint) -> Result<(f64, f64), AppError> {
        Ok((
            (f64::from(p.x) - f64::from(self.bounds.left)) / self.scale_factor,
            (f64::from(p.y) - f64::from(self.bounds.top)) / self.scale_factor,
        ))
    }
    pub fn new(id: String, bounds: PhysicalRect, scale_factor: f64) -> Result<Self, AppError> {
        if id.is_empty()
            || id.chars().count() > MAX_MONITOR_ID_SCALARS
            || id.chars().any(char::is_control)
        {
            return Err(AppError::InvalidInput(
                "monitor identifier is empty, too long, or has control characters",
            ));
        }
        if !scale_factor.is_finite() || scale_factor <= 0.0 || scale_factor > MAX_SCALE_FACTOR {
            return Err(AppError::InvalidInput(
                "monitor scale factor is out of range",
            ));
        }
        Ok(MonitorGeometry {
            id,
            bounds,
            scale_factor,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn bounds(&self) -> PhysicalRect {
        self.bounds
    }

    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RectData {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}
impl TryFrom<RectData> for PhysicalRect {
    type Error = AppError;
    fn try_from(r: RectData) -> Result<Self, AppError> {
        Self::new(r.left, r.top, r.width, r.height)
    }
}
impl From<PhysicalRect> for RectData {
    fn from(r: PhysicalRect) -> Self {
        Self {
            left: r.left,
            top: r.top,
            width: r.width,
            height: r.height,
        }
    }
}

pub struct RgbaFrame {
    pub(crate) bounds: PhysicalRect,
    pub(crate) pixels: Vec<u8>,
}
impl RgbaFrame {
    pub fn new(bounds: PhysicalRect, pixels: Vec<u8>) -> Result<Self, AppError> {
        if pixels.len() != bounds.rgba_len()? {
            return Err(AppError::Capture("source buffer dimensions do not match"));
        }
        Ok(Self { bounds, pixels })
    }
}
pub fn virtual_bounds(rects: &[PhysicalRect]) -> Result<PhysicalRect, AppError> {
    let first = rects
        .first()
        .ok_or(AppError::Capture("no display source is available"))?;
    let (mut left, mut top, mut right, mut bottom) =
        (first.left, first.top, first.right(), first.bottom());
    for rect in rects {
        left = left.min(rect.left);
        top = top.min(rect.top);
        right = right.max(rect.right());
        bottom = bottom.max(rect.bottom());
    }
    PhysicalRect::new(left, top, left.abs_diff(right), top.abs_diff(bottom))
}
pub fn allocate_rgba(bounds: PhysicalRect) -> Result<Vec<u8>, AppError> {
    let len = bounds.rgba_len()?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .map_err(|_| AppError::Capture("capture allocation failed"))?;
    pixels.resize(len, 0);
    Ok(pixels)
}
/// Monitor gaps remain transparent. Frames are copied only at physical intersections.
pub fn compose_intersections(
    crop: PhysicalRect,
    frames: &[RgbaFrame],
) -> Result<Vec<u8>, AppError> {
    let mut out = allocate_rgba(crop)?;
    for frame in frames {
        if let Some(part) = crop.intersection(frame.bounds) {
            let (sx, sy) = frame.bounds.pixel_offset(PhysicalPoint {
                x: part.left,
                y: part.top,
            })?;
            let (dx, dy) = crop.pixel_offset(PhysicalPoint {
                x: part.left,
                y: part.top,
            })?;
            for row in 0..part.height {
                let source = ((u64::from(sy + row) * u64::from(frame.bounds.width) + u64::from(sx))
                    * 4) as usize;
                let dest =
                    ((u64::from(dy + row) * u64::from(crop.width) + u64::from(dx)) * 4) as usize;
                let len = part.width as usize * 4;
                out[dest..dest + len].copy_from_slice(&frame.pixels[source..source + len]);
            }
        }
    }
    Ok(out)
}

pub fn validate_layout(monitors: &[MonitorGeometry]) -> Result<(), AppError> {
    if monitors.is_empty() || monitors.len() > 64 {
        return Err(AppError::Capture("monitor count is out of range"));
    }
    for (i, m) in monitors.iter().enumerate() {
        if monitors[..i].iter().any(|other| other.id == m.id) {
            return Err(AppError::Capture("duplicate monitor identity"));
        }
    }
    Ok(())
}
pub fn same_layout(a: &[MonitorGeometry], b: &[MonitorGeometry]) -> bool {
    a.len() == b.len() && a.iter().all(|m| b.iter().any(|other| other == m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_rejects_empty_and_overflowing_bounds() {
        assert!(PhysicalRect::new(0, 0, 0, 10).is_err());
        assert!(PhysicalRect::new(0, 0, 10, 0).is_err());
        assert!(PhysicalRect::new(i32::MAX, 0, 1, 1).is_err());
        assert!(PhysicalRect::new(0, i32::MAX - 4, 1, 5).is_err());
        assert!(PhysicalRect::new(-5000, -100, u32::MAX, 1).is_err());
        let r = PhysicalRect::new(-1920, -200, 1920, 1080).unwrap();
        assert_eq!(
            (r.left(), r.top(), r.width(), r.height()),
            (-1920, -200, 1920, 1080)
        );
        assert_eq!(r.pixel_count(), 1920 * 1080);
        let edge = PhysicalRect::new(i32::MAX - 1, 0, 1, 1).unwrap();
        assert_eq!(edge.pixel_count(), 1);
    }

    #[test]
    fn rgba_allocation_respects_pixel_budget() {
        assert_eq!(
            PhysicalRect::new(0, 0, 8000, 8000)
                .unwrap()
                .rgba_len()
                .unwrap(),
            256_000_000
        );
        assert!(matches!(
            PhysicalRect::new(0, 0, 8000, 8001).unwrap().rgba_len(),
            Err(AppError::InvalidInput(_))
        ));
        assert!(
            PhysicalRect::new(0, 0, u32::MAX / 2, u32::MAX / 2)
                .unwrap()
                .rgba_len()
                .is_err()
        );
    }

    #[test]
    fn monitor_rejects_nonfinite_or_nonpositive_scale() {
        let b = PhysicalRect::new(0, 0, 100, 100).unwrap();
        for s in [f64::NAN, f64::INFINITY, 0.0, -1.0, 100.0] {
            assert!(
                MonitorGeometry::new("DISPLAY1".into(), b, s).is_err(),
                "{s}"
            );
        }
        assert!(MonitorGeometry::new(String::new(), b, 1.0).is_err());
        assert!(MonitorGeometry::new("bad\u{0}id".into(), b, 1.0).is_err());
        let m = MonitorGeometry::new("DISPLAY1".into(), b, 1.5).unwrap();
        assert_eq!((m.id(), m.bounds(), m.scale_factor()), ("DISPLAY1", b, 1.5));
    }
}
