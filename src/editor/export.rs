//! One authoritative encoded artifact and its decoded preview. Only flattened,
//! sanitized pixels cross the codec boundary; original images/paths never do.
use super::{EditorDocument, render_export};
use crate::{app::error::AppError, capture::geometry::PhysicalRect};
use image::{ImageEncoder, RgbaImage};
use std::{
    io::{self, Cursor, Write},
    sync::Arc,
};
use uuid::Uuid;

pub const MAX_ENCODED_BYTES: usize = 128 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportFormat {
    Png,
    Jpeg,
    WebpLossy,
    WebpLossless,
}
impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebpLossy | Self::WebpLossless => "webp",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebpLossy | Self::WebpLossless => "image/webp",
        }
    }
    pub fn output_format(self) -> crate::persistence::filename::OutputFormat {
        use crate::persistence::filename::OutputFormat as F;
        match self {
            Self::Png => F::Png,
            Self::Jpeg => F::Jpeg,
            Self::WebpLossy | Self::WebpLossless => F::WebP,
        }
    }
    fn image_format(self) -> image::ImageFormat {
        match self {
            Self::Png => image::ImageFormat::Png,
            Self::Jpeg => image::ImageFormat::Jpeg,
            Self::WebpLossy | Self::WebpLossless => image::ImageFormat::WebP,
        }
    }
    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::WebpLossy)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Resize {
    Original,
    Percent(u8),
    Width(u32),
    Height(u32),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageExportOptions {
    format: ExportFormat,
    quality: u8,
    resize: Resize,
    revision: u64,
}
impl Default for ImageExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Png,
            quality: 90,
            resize: Resize::Original,
            revision: 0,
        }
    }
}
impl ImageExportOptions {
    pub fn new(format: ExportFormat, quality: u8, resize: Resize) -> Result<Self, AppError> {
        let options = Self {
            format,
            quality,
            resize,
            revision: 0,
        };
        options.validate()?;
        Ok(options)
    }
    fn validate(&self) -> Result<(), AppError> {
        if !(1..=100).contains(&self.quality)
            || matches!(
                self.resize,
                Resize::Percent(0 | 101..=255) | Resize::Width(0) | Resize::Height(0)
            )
        {
            return Err(AppError::InvalidInput(
                "export quality or resize setting is out of range",
            ));
        }
        Ok(())
    }
    pub fn format(&self) -> ExportFormat {
        self.format
    }
    pub fn quality(&self) -> Option<u8> {
        self.format.is_lossy().then_some(self.quality)
    }
    pub fn resize(&self) -> Resize {
        self.resize
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    fn update(
        &mut self,
        format: ExportFormat,
        quality: u8,
        resize: Resize,
    ) -> Result<(), AppError> {
        let mut next = Self::new(format, quality, resize)?;
        if (self.format, self.quality, self.resize) == (format, quality, resize) {
            return Ok(());
        }
        next.revision = self
            .revision
            .checked_add(1)
            .ok_or(AppError::InvalidInput("export options revision exhausted"))?;
        *self = next;
        Ok(())
    }
    pub fn set_resize(&mut self, resize: Resize) -> Result<(), AppError> {
        self.update(self.format, self.quality, resize)
    }
    pub fn set_format(&mut self, format: ExportFormat) -> Result<(), AppError> {
        self.update(format, self.quality, self.resize)
    }
    pub fn set_quality(&mut self, quality: u8) -> Result<(), AppError> {
        self.update(self.format, quality, self.resize)
    }
    pub fn dimensions(&self, width: u32, height: u32) -> Result<(u32, u32), AppError> {
        self.validate()?;
        PhysicalRect::new(0, 0, width, height)?.rgba_len()?;
        let round_ratio = |n: u64, d: u64| -> u32 { ((n + d / 2) / d).max(1) as u32 };
        let (w, h) = match self.resize {
            Resize::Original => (width, height),
            Resize::Percent(p) => (
                round_ratio(u64::from(width) * u64::from(p), 100),
                round_ratio(u64::from(height) * u64::from(p), 100),
            ),
            Resize::Width(w) if w <= width => (
                w,
                round_ratio(u64::from(height) * u64::from(w), u64::from(width)),
            ),
            Resize::Height(h) if h <= height => (
                round_ratio(u64::from(width) * u64::from(h), u64::from(height)),
                h,
            ),
            _ => return Err(AppError::InvalidInput("image export cannot upscale")),
        };
        PhysicalRect::new(0, 0, w, h)?.rgba_len()?;
        if matches!(
            self.format,
            ExportFormat::WebpLossy | ExportFormat::WebpLossless
        ) && (w > 16383 || h > 16383)
        {
            return Err(AppError::Encoder(
                "WebP dimensions must not exceed 16383 pixels per axis; choose PNG or downscale",
            ));
        }
        if self.format == ExportFormat::Jpeg && (w > 65535 || h > 65535) {
            return Err(AppError::Encoder(
                "JPEG dimensions must not exceed 65535 pixels per axis",
            ));
        }
        Ok((w, h))
    }
}
/// Frozen, lightweight export snapshot. Creation discards undo/redo references.
#[derive(Clone)]
pub struct DocumentRevision {
    pub(crate) document: EditorDocument,
}
#[derive(Clone)]
pub struct PreparedImageExport {
    bytes: Arc<[u8]>,
    pixels: Arc<RgbaImage>,
    document_id: Uuid,
    document_revision: u64,
    options: ImageExportOptions,
}
impl PreparedImageExport {
    pub fn format(&self) -> ExportFormat {
        self.options.format
    }
    pub fn mime(&self) -> &'static str {
        self.format().mime()
    }
    pub fn extension(&self) -> &'static str {
        self.format().extension()
    }
    pub fn dimensions(&self) -> (u32, u32) {
        self.pixels.dimensions()
    }
    pub fn pixels(&self) -> &Arc<RgbaImage> {
        &self.pixels
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn encoded(&self) -> &Arc<[u8]> {
        &self.bytes
    }
    pub fn encoded_size(&self) -> usize {
        self.bytes.len()
    }
    pub fn options_revision(&self) -> u64 {
        self.options.revision
    }
    pub fn matches(&self, document: &EditorDocument, options: &ImageExportOptions) -> bool {
        self.document_id == document.id()
            && self.document_revision == document.revision()
            && self.options == *options
    }
}
/// Caps Rust codec writes before growing the output buffer. The native WebP
/// adapter applies the same cap to every emitted segment before allocation.
struct BoundedOutput {
    bytes: Vec<u8>,
}
impl Write for BoundedOutput {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > MAX_ENCODED_BYTES.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("encoded image exceeds output budget"));
        }
        self.bytes
            .try_reserve(data.len())
            .map_err(|_| io::Error::other("encoded image allocation failed"))?;
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn prepare_export(
    revision: DocumentRevision,
    options: ImageExportOptions,
) -> Result<PreparedImageExport, AppError> {
    options.validate()?;
    let crop = revision.document.crop().ok_or(AppError::InvalidInput(
        "select an output region before finishing",
    ))?;
    let (width, height) = options.dimensions(crop.width(), crop.height())?;
    let flattened = render_export(&revision.document)?;
    let mut pixels = if flattened.dimensions() == (width, height) {
        flattened
    } else {
        image::imageops::resize(
            &flattened,
            width,
            height,
            image::imageops::FilterType::Lanczos3,
        )
    };
    let mut output = BoundedOutput { bytes: Vec::new() };
    match options.format {
        ExportFormat::Png => image::codecs::png::PngEncoder::new(&mut output)
            .write_image(
                pixels.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|_| AppError::Encoder("PNG encoding failed"))?,
        ExportFormat::Jpeg => {
            let mut rgb = Vec::new();
            rgb.try_reserve_exact(pixels.len() / 4 * 3)
                .map_err(|_| AppError::Encoder("JPEG pixel allocation failed"))?;
            for p in pixels.pixels_mut() {
                let a = u32::from(p[3]);
                for c in &mut p.0[..3] {
                    *c = ((u32::from(*c) * a + 255 * (255 - a) + 127) / 255) as u8;
                }
                p[3] = 255;
                rgb.extend_from_slice(&p.0[..3]);
            }
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, options.quality)
                .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
                .map_err(|_| AppError::Encoder("JPEG encoding failed"))?;
        }
        ExportFormat::WebpLossless => image::codecs::webp::WebPEncoder::new_lossless(&mut output)
            .write_image(
                pixels.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|_| AppError::Encoder("lossless WebP encoding failed"))?,
        ExportFormat::WebpLossy => {
            output.bytes = super::webp::encode_lossy(&pixels, options.quality, MAX_ENCODED_BYTES)?;
        }
    }
    if output.bytes.is_empty() {
        return Err(AppError::Encoder("encoder produced no output"));
    }
    let mut reader =
        image::ImageReader::with_format(Cursor::new(&output.bytes), options.format.image_format());
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| AppError::Encoder("encoded output could not be previewed"))?
        .to_rgba8();
    if decoded.dimensions() != (width, height) {
        return Err(AppError::Encoder("encoded output dimensions changed"));
    }
    Ok(PreparedImageExport {
        bytes: output.bytes.into(),
        pixels: Arc::new(decoded),
        document_id: revision.document.id(),
        document_revision: revision.document.revision(),
        options,
    })
}
