use crate::app::error::AppError;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalRect {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

impl PhysicalRect {
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
