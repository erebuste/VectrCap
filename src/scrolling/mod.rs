//! Manual capture: pixels and typed lifecycle effects only, no input injection.
pub mod overlap;
pub mod runtime;
pub mod session;
pub mod stable;
pub mod stitch;
use crate::{
    app::error::AppError,
    capture::{
        geometry::{MAX_CAPTURE_PIXELS, PhysicalRect},
        windows::WindowId,
    },
};
use std::time::Duration;
#[derive(Debug, Clone)]
pub struct ScrollOptions {
    pub target: WindowId,
    pub monitor_id: String,
    pub rect: PhysicalRect,
    pub maximum_duration: Duration,
    pub maximum_segments: u32,
}
impl ScrollOptions {
    pub fn validate(&self) -> Result<(), AppError> {
        self.rect.rgba_len()?;
        if self.monitor_id.is_empty()
            || self.monitor_id.len() > 1024
            || self.monitor_id.chars().any(char::is_control)
            || self.maximum_duration.is_zero()
            || self.maximum_duration > Duration::from_secs(600)
            || self.maximum_segments == 0
            || self.maximum_segments > 1000
        {
            return Err(AppError::InvalidInput(
                "invalid scrolling capture limits or monitor",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetIdentity {
    pub target: WindowId,
    pub monitor_id: String,
    pub window_rect: PhysicalRect,
    pub viewport: PhysicalRect,
    pub focused: bool,
}
pub(crate) fn validate_image(i: &image::RgbaImage) -> Result<(), AppError> {
    let length = PhysicalRect::new(0, 0, i.width(), i.height())?.rgba_len()?;
    if i.as_raw().len() != length {
        return Err(AppError::InvalidInput(
            "scroll image backing layout is invalid",
        ));
    }
    Ok(())
}
