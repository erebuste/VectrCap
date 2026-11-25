use crate::app::error::AppError;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LensShape {
    Circle,
    Square,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureMouseAction {
    None,
    CancelActiveOrCapture,
    ToggleSelectionDrawing,
    SelectFullscreen,
    SelectActiveMonitor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Right,
    Middle,
    Button4,
    Button5,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MouseBindings {
    pub right: CaptureMouseAction,
    pub middle: CaptureMouseAction,
    pub button4: CaptureMouseAction,
    pub button5: CaptureMouseAction,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizePreset {
    pub width: u32,
    pub height: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RegionCapturePreferences {
    pub window_detection: bool,
    pub dim_percent: u8,
    pub show_position_size: bool,
    /// The legacy position/size switch remains a master gate for older files.
    pub show_coordinates: bool,
    pub show_dimensions: bool,
    pub show_magnifier: bool,
    pub lens_shape: LensShape,
    pub lens_source_count: u8,
    pub lens_scale: u8,
    pub show_rgb: bool,
    pub show_hex: bool,
    pub center_crosshair: bool,
    pub screen_crosshair: bool,
    pub fixed_size: bool,
    pub fixed_dimensions: SizePreset,
    pub presets: Vec<SizePreset>,
    pub active_monitor_only: bool,
    pub mouse: MouseBindings,
}
impl Default for RegionCapturePreferences {
    fn default() -> Self {
        Self {
            window_detection: true,
            dim_percent: 20,
            show_position_size: true,
            show_coordinates: true,
            show_dimensions: true,
            show_magnifier: true,
            lens_shape: LensShape::Circle,
            lens_source_count: 15,
            lens_scale: 20,
            show_rgb: false,
            show_hex: false,
            center_crosshair: true,
            screen_crosshair: false,
            fixed_size: false,
            fixed_dimensions: SizePreset {
                width: 640,
                height: 480,
            },
            presets: vec![
                SizePreset {
                    width: 640,
                    height: 480,
                },
                SizePreset {
                    width: 1280,
                    height: 720,
                },
                SizePreset {
                    width: 1920,
                    height: 1080,
                },
            ],
            active_monitor_only: false,
            mouse: MouseBindings {
                right: CaptureMouseAction::CancelActiveOrCapture,
                middle: CaptureMouseAction::ToggleSelectionDrawing,
                // ShareX: back = whole desktop, forward = active monitor.
                button4: CaptureMouseAction::SelectFullscreen,
                button5: CaptureMouseAction::SelectActiveMonitor,
            },
        }
    }
}
impl SizePreset {
    pub fn validate(&self) -> Result<(), AppError> {
        super::geometry::PhysicalRect::new(0, 0, self.width, self.height)?.rgba_len()?;
        Ok(())
    }
}
impl MouseBindings {
    pub fn action(&self, button: MouseButton) -> CaptureMouseAction {
        match button {
            MouseButton::Right => self.right,
            MouseButton::Middle => self.middle,
            MouseButton::Button4 => self.button4,
            MouseButton::Button5 => self.button5,
        }
    }
}
impl RegionCapturePreferences {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.dim_percent > 80 {
            return Err(AppError::InvalidInput(
                "region dim strength must be 0 to 80 percent",
            ));
        }
        if !(9..=35).contains(&self.lens_source_count)
            || self.lens_source_count.is_multiple_of(2)
            || !(4..=24).contains(&self.lens_scale)
        {
            return Err(AppError::InvalidInput(
                "magnifier count or display scale is out of range",
            ));
        }
        self.fixed_dimensions.validate()?;
        if self.presets.len() > 16 {
            return Err(AppError::InvalidInput(
                "at most 16 size presets are allowed",
            ));
        }
        for preset in &self.presets {
            preset.validate()?;
        }
        Ok(())
    }
}
