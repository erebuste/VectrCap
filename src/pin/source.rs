//! Bounded raster ingress, run on the source worker rather than the UI thread.
use super::PinnedImage;
use crate::{
    app::{error::AppError, types::Artifact},
    capture::geometry::PhysicalRect,
    editor::export::MAX_ENCODED_BYTES,
    persistence::history::browser::{open_local_file, verified_local},
};
use image::{ImageFormat, ImageReader, RgbaImage};
use std::{
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

pub fn load_pin_file(path: &Path) -> Result<PinnedImage, AppError> {
    let file = open_local_file(path)?;
    let size = file
        .metadata()
        .map_err(|e| AppError::io("checking pin file", &e))?
        .len();
    if size == 0 || size > MAX_ENCODED_BYTES as u64 {
        return Err(AppError::InvalidInput(
            "pin file exceeds the 128-MiB encoded-file limit or is empty",
        ));
    }
    decode(BufReader::new(file))
}
pub fn load_pin_history(artifact: &Artifact) -> Result<PinnedImage, AppError> {
    // Retain the verified handle through decode. No URL enters this boundary.
    decode(BufReader::new(verified_local(
        artifact,
        MAX_ENCODED_BYTES as u64,
    )?))
}
fn decode<R: BufRead + Seek>(mut source: R) -> Result<PinnedImage, AppError> {
    let reader = ImageReader::new(&mut source)
        .with_guessed_format()
        .map_err(|_| invalid_image())?;
    let format = reader.format().ok_or_else(invalid_image)?;
    if !matches!(
        format,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::Bmp
            | ImageFormat::WebP
    ) {
        return Err(invalid_image());
    }
    let (width, height) = reader.into_dimensions().map_err(|_| invalid_image())?;
    PhysicalRect::new(0, 0, width, height)?.rgba_len()?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|_| invalid_image())?;
    let mut reader = ImageReader::with_format(source, format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    // Match the established export decoder's bounded working allocation.
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let pixels = reader.decode().map_err(|_| invalid_image())?.to_rgba8();
    if pixels.dimensions() != (width, height) {
        return Err(invalid_image());
    }
    let label = if format == ImageFormat::Gif {
        Some("GIF — first frame".to_owned())
    } else {
        None
    };
    PinnedImage::new(Arc::new(pixels), label)
}
fn invalid_image() -> AppError {
    AppError::InvalidInput(
        "pin source must be a readable PNG, JPEG, GIF (first frame), BMP, or WebP image",
    )
}

/// Validated layout for untrusted CF_DIB/CF_DIBV5 global memory. Header parsing
/// and the pixel ceiling are checked before allocating/copying pixel memory.
pub struct DibLayout {
    width: u32,
    height: u32,
    top_down: bool,
    bits: u16,
    offset: usize,
    stride: usize,
    alpha: bool,
}
impl DibLayout {
    pub fn parse(bytes: &[u8], allocation_len: usize) -> Result<Self, AppError> {
        let u32_at = |offset| -> Result<u32, AppError> {
            Ok(u32::from_le_bytes(
                bytes
                    .get(offset..offset + 4)
                    .ok_or_else(invalid_image)?
                    .try_into()
                    .map_err(|_| invalid_image())?,
            ))
        };
        let header = u32_at(0)? as usize;
        if !matches!(header, 40 | 108 | 124) || bytes.len() < header {
            return Err(invalid_image());
        }
        let w = u32_at(4)? as i32;
        let h = u32_at(8)? as i32;
        if w <= 0 || h == 0 || h == i32::MIN || bytes[12..14] != 1u16.to_le_bytes() {
            return Err(invalid_image());
        }
        let (width, height) = (w as u32, h.unsigned_abs());
        PhysicalRect::new(0, 0, width, height)?.rgba_len()?;
        let bits = u16::from_le_bytes([bytes[14], bytes[15]]);
        let compression = u32_at(16)?;
        if !matches!((bits, compression), (24 | 32, 0) | (32, 3)) {
            return Err(invalid_image());
        }
        let mut offset = header;
        let alpha = compression == 3 && header >= 108 && u32_at(52)? == 0xff000000;
        if compression == 3 {
            if u32_at(40)? != 0x00ff0000 || u32_at(44)? != 0x0000ff00 || u32_at(48)? != 0x000000ff {
                return Err(invalid_image());
            }
            if header == 40 {
                offset += 12;
            }
            if header >= 108 && !matches!(u32_at(52)?, 0 | 0xff000000) {
                return Err(invalid_image());
            }
        }
        if u32_at(32)? != 0 {
            return Err(invalid_image());
        }
        let stride = ((u64::from(width) * u64::from(bits)).div_ceil(32) * 4) as usize;
        let end = offset
            .checked_add(
                stride
                    .checked_mul(height as usize)
                    .ok_or_else(invalid_image)?,
            )
            .ok_or_else(invalid_image)?;
        if end > allocation_len {
            return Err(invalid_image());
        }
        Ok(Self {
            width,
            height,
            top_down: h < 0,
            bits,
            offset,
            stride,
            alpha,
        })
    }
    pub fn decode(&self, bytes: &[u8]) -> Result<PinnedImage, AppError> {
        let len = PhysicalRect::new(0, 0, self.width, self.height)?.rgba_len()?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(len)
            .map_err(|_| AppError::Capture("pin image allocation failed"))?;
        for y in 0..self.height as usize {
            let row = if self.top_down {
                y
            } else {
                self.height as usize - 1 - y
            };
            let start = self.offset + row * self.stride;
            let data = bytes
                .get(start..start + self.stride)
                .ok_or_else(invalid_image)?;
            for x in 0..self.width as usize {
                let p = x * usize::from(self.bits / 8);
                rgba.extend_from_slice(&[
                    data[p + 2],
                    data[p + 1],
                    data[p],
                    if self.alpha { data[p + 3] } else { 255 },
                ]);
            }
        }
        PinnedImage::new(
            Arc::new(RgbaImage::from_raw(self.width, self.height, rgba).ok_or_else(invalid_image)?),
            Some("Clipboard image".to_owned()),
        )
    }
}
pub fn load_pin_dib(bytes: &[u8]) -> Result<PinnedImage, AppError> {
    DibLayout::parse(bytes, bytes.len())?.decode(bytes)
}
