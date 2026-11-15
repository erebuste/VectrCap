use super::options::OutputSize;
use crate::{app::error::AppError, capture::geometry::PhysicalRect};
use std::time::Duration;

#[derive(Debug)]
pub struct CapturedFrame {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    pub timestamp: Duration,
}
impl CapturedFrame {
    pub fn new(
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        timestamp: Duration,
    ) -> Result<Self, AppError> {
        if rgba.len() != PhysicalRect::new(0, 0, width, height)?.rgba_len()? {
            return Err(AppError::Capture(
                "RGBA buffer length does not match dimensions",
            ));
        }
        Ok(Self {
            rgba,
            width,
            height,
            timestamp,
        })
    }
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Source coordinates are relative to this buffer, not the virtual desktop.
    /// Only selected pixels are copied before resize; padding is opaque black.
    pub fn crop(
        source: &[u8],
        size: (u32, u32),
        stride: usize,
        rect: PhysicalRect,
        output: OutputSize,
        even: bool,
        timestamp: Duration,
    ) -> Result<Self, AppError> {
        let (w, h) = size;
        PhysicalRect::new(0, 0, w, h)?.rgba_len()?;
        let x =
            usize::try_from(rect.left()).map_err(|_| AppError::Capture("negative source crop"))?;
        let y =
            usize::try_from(rect.top()).map_err(|_| AppError::Capture("negative source crop"))?;
        if stride < (w as usize) * 4
            || source.len()
                < stride
                    .checked_mul(h as usize)
                    .ok_or(AppError::Capture("source stride overflow"))?
            || x + rect.width() as usize > w as usize
            || y + rect.height() as usize > h as usize
        {
            return Err(AppError::Capture("crop or stride exceeds source buffer"));
        }
        let mut cropped = vec![0; rect.rgba_len()?];
        let row = rect.width() as usize * 4;
        for r in 0..rect.height() as usize {
            cropped[r * row..(r + 1) * row]
                .copy_from_slice(&source[(y + r) * stride + x * 4..(y + r) * stride + x * 4 + row]);
        }
        let (ow, oh) = output.dimensions(rect.width(), rect.height())?;
        let cropped = if (ow, oh) != (rect.width(), rect.height()) {
            let image = image::RgbaImage::from_raw(rect.width(), rect.height(), cropped)
                .ok_or(AppError::Capture("invalid crop"))?;
            image::imageops::resize(&image, ow, oh, image::imageops::FilterType::Triangle)
                .into_raw()
        } else {
            cropped
        };
        let (pw, ph) = if even {
            ((ow + 1) & !1, (oh + 1) & !1)
        } else {
            (ow, oh)
        };
        if (pw, ph) == (ow, oh) {
            return Self::new(cropped, ow, oh, timestamp);
        }
        let mut padded = vec![0; PhysicalRect::new(0, 0, pw, ph)?.rgba_len()?];
        for p in padded.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        for r in 0..oh as usize {
            padded[r * pw as usize * 4..r * pw as usize * 4 + ow as usize * 4]
                .copy_from_slice(&cropped[r * ow as usize * 4..(r + 1) * ow as usize * 4]);
        }
        Self::new(padded, pw, ph, timestamp)
    }
}
