//! Incremental row buffer with bounded geometric growth; failed append is atomic.
use crate::{app::error::AppError, capture::geometry::MAX_CAPTURE_PIXELS};
use image::RgbaImage;
pub struct Stitcher {
    width: u32,
    height: u32,
    viewport_height: u32,
    bytes: Vec<u8>,
    max_pixels: u64,
}
impl Stitcher {
    pub fn new(first: RgbaImage, max_pixels: u64) -> Result<Self, AppError> {
        super::validate_image(&first)?;
        let max_pixels = max_pixels.min(MAX_CAPTURE_PIXELS);
        if u64::from(first.width()) * u64::from(first.height()) > max_pixels {
            return Err(AppError::Capture("scroll output pixel limit reached"));
        }
        Ok(Self {
            width: first.width(),
            height: first.height(),
            viewport_height: first.height(),
            bytes: first.into_raw(),
            max_pixels,
        })
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn append(&mut self, candidate: &RgbaImage, new_rows: u32) -> Result<(), AppError> {
        super::validate_image(candidate)?;
        if candidate.dimensions() != (self.width, self.viewport_height)
            || new_rows == 0
            || new_rows >= self.viewport_height
        {
            return Err(AppError::Capture(
                "invalid scrolling append dimensions or row count",
            ));
        }
        let height = self
            .height
            .checked_add(new_rows)
            .ok_or(AppError::Capture("scroll height overflow"))?;
        let pixels = u64::from(self.width) * u64::from(height);
        if pixels > self.max_pixels || height > i32::MAX as u32 {
            return Err(AppError::Capture("scroll output pixel limit reached"));
        }
        let size = usize::try_from(
            pixels
                .checked_mul(4)
                .ok_or(AppError::Capture("scroll byte size overflow"))?,
        )
        .map_err(|_| AppError::Capture("scroll buffer exceeds address space"))?;
        let overlap = (self.viewport_height - new_rows) as usize * self.width as usize * 4;
        if self.bytes[self.bytes.len() - overlap..] != candidate.as_raw()[..overlap] {
            return Err(AppError::Capture("scroll append lacks exact overlap"));
        }
        if size > self.bytes.capacity() {
            let limit = usize::try_from(self.max_pixels * 4)
                .map_err(|_| AppError::Capture("scroll budget exceeds address space"))?;
            let capacity = size.max(self.bytes.capacity().saturating_mul(2)).min(limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| AppError::Capture("scroll buffer allocation failed"))?;
        }
        self.bytes.extend_from_slice(&candidate.as_raw()[overlap..]);
        self.height = height;
        Ok(())
    }
    pub fn finish(self) -> Result<RgbaImage, AppError> {
        RgbaImage::from_raw(self.width, self.height, self.bytes)
            .ok_or(AppError::Capture("invalid stitched output layout"))
    }
}
