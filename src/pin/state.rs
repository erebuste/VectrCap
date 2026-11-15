use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, PhysicalRect},
};
use image::RgbaImage;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PinId(Uuid);
impl PinId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Clone)]
pub struct PinnedImage {
    pixels: Arc<RgbaImage>,
    source_label: Option<String>,
}
impl PinnedImage {
    pub fn new(pixels: Arc<RgbaImage>, source_label: Option<String>) -> Result<Self, AppError> {
        PhysicalRect::new(0, 0, pixels.width(), pixels.height())?.rgba_len()?;
        if source_label
            .as_ref()
            .is_some_and(|s| s.chars().count() > 256 || s.chars().any(char::is_control))
        {
            return Err(AppError::InvalidInput("pin source label is invalid"));
        }
        Ok(Self {
            pixels,
            source_label,
        })
    }
    pub fn pixels(&self) -> &Arc<RgbaImage> {
        &self.pixels
    }
    pub fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }
    pub fn pixel_count(&self) -> u64 {
        u64::from(self.pixels.width()) * u64::from(self.pixels.height())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinOptions {
    pub always_on_top: bool,
    pub smooth: bool,
    pub border_color: [u8; 4],
    border_width: u8,
    /// None selects the default neutral checkerboard.
    pub background: Option<[u8; 4]>,
}
impl Default for PinOptions {
    fn default() -> Self {
        Self {
            always_on_top: true,
            smooth: true,
            border_color: [128, 128, 128, 255],
            border_width: 1,
            background: None,
        }
    }
}
impl PinOptions {
    pub fn border_width(&self) -> u8 {
        self.border_width
    }
    pub fn set_border_width(&mut self, width: u8) -> Result<(), AppError> {
        if width > 16 {
            return Err(AppError::InvalidInput(
                "pin border width must be 0–16 pixels",
            ));
        }
        self.border_width = width;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PinState {
    pub(crate) position: PhysicalPoint,
    pub(crate) zoom: u16,
    pub(crate) opacity: u8,
    pub(crate) minimized: bool,
    pub(crate) options: PinOptions,
}
impl PinState {
    pub(crate) fn new(position: PhysicalPoint) -> Self {
        Self {
            position,
            zoom: 100,
            opacity: 100,
            minimized: false,
            options: PinOptions::default(),
        }
    }
    pub fn position(&self) -> PhysicalPoint {
        self.position
    }
    pub fn zoom_percent(&self) -> u16 {
        self.zoom
    }
    pub fn opacity_percent(&self) -> u8 {
        self.opacity
    }
    pub fn minimized(&self) -> bool {
        self.minimized
    }
    pub fn options(&self) -> &PinOptions {
        &self.options
    }
    pub fn display_size(&self, image: &PinnedImage) -> (u32, u32) {
        let (w, h) = image.pixels.dimensions();
        if self.minimized {
            let longest = u64::from(w.max(h));
            return (
                ((u64::from(w) * 96 / longest) as u32).max(1),
                ((u64::from(h) * 96 / longest) as u32).max(1),
            );
        }
        (
            ((u64::from(w) * u64::from(self.zoom) / 100) as u32).max(1),
            ((u64::from(h) * u64::from(self.zoom) / 100) as u32).max(1),
        )
    }
}
