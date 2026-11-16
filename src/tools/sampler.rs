use super::UtilitySnapshot;
use crate::{
    app::error::AppError,
    capture::{
        geometry::PhysicalPoint,
        preferences::{LensShape, RegionCapturePreferences},
    },
    editor::sample_source_color,
    ui::magnifier::CaptureReadout,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampledColor {
    pub rgb: [u8; 3],
    pub point: PhysicalPoint,
}
impl SampledColor {
    pub fn hex_text(&self) -> String {
        crate::ui::magnifier::hex_text(self.rgb)
    }
    pub fn rgb_text(&self) -> String {
        crate::ui::magnifier::rgb_text(self.rgb)
    }
}
pub fn sample_pixel(
    snapshot: &UtilitySnapshot,
    point: PhysicalPoint,
) -> Result<SampledColor, AppError> {
    let [r, g, b, _] = sample_source_color(snapshot.document(), point)?;
    Ok(SampledColor {
        rgb: [r, g, b],
        point,
    })
}
#[derive(Debug)]
pub struct SamplerState {
    pub point: PhysicalPoint,
}
impl SamplerState {
    pub fn new(point: PhysicalPoint) -> Self {
        Self { point }
    }
    pub fn preferences() -> RegionCapturePreferences {
        RegionCapturePreferences {
            lens_source_count: 15,
            lens_scale: 20,
            lens_shape: LensShape::Circle,
            show_rgb: true,
            show_hex: true,
            show_coordinates: true,
            show_dimensions: false,
            center_crosshair: true,
            ..Default::default()
        }
    }
    pub fn readout(&self, snapshot: &UtilitySnapshot) -> Result<CaptureReadout, AppError> {
        CaptureReadout::sample(snapshot.document(), self.point, &Self::preferences())
    }
    pub fn move_by(&mut self, dx: i32, dy: i32, shift: bool) -> Result<(), AppError> {
        let step = if shift { 10 } else { 1 };
        let point = PhysicalPoint {
            x: self
                .point
                .x
                .checked_add(
                    dx.checked_mul(step)
                        .ok_or(AppError::InvalidInput("sample movement overflows"))?,
                )
                .ok_or(AppError::InvalidInput("sample movement overflows"))?,
            y: self
                .point
                .y
                .checked_add(
                    dy.checked_mul(step)
                        .ok_or(AppError::InvalidInput("sample movement overflows"))?,
                )
                .ok_or(AppError::InvalidInput("sample movement overflows"))?,
        };
        self.point = point;
        Ok(())
    }
}
