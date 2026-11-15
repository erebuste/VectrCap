//! Bounded Unicode text rasterization.
use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, PhysicalRect},
};
use fontdue::{Font, FontSettings};
use std::{fs::File, io::Read, sync::OnceLock};
use tiny_skia::Pixmap;

fn fonts() -> Result<&'static [Font], AppError> {
    static FONTS: OnceLock<Result<Vec<Font>, AppError>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let embedded = Font::from_bytes(
                include_bytes!("../../assets/fonts/Geist-Regular.ttf") as &[u8],
                FontSettings::default(),
            )
            .map_err(|_| AppError::Encoder("embedded annotation font could not be loaded"))?;
            let mut fonts = vec![embedded];
            // Fixed, bounded fallback inventory. User-selected font paths are never read.
            if let Some(windows) = std::env::var_os("WINDIR") {
                for name in ["segoeui.ttf", "seguisym.ttf", "msyh.ttc"] {
                    let path = std::path::PathBuf::from(&windows).join("Fonts").join(name);
                    if let Ok(file) = File::open(path) {
                        let mut bytes = Vec::new();
                        if file
                            .take(16 * 1024 * 1024 + 1)
                            .read_to_end(&mut bytes)
                            .is_ok()
                            && bytes.len() <= 16 * 1024 * 1024
                            && let Ok(font) = Font::from_bytes(bytes, FontSettings::default())
                        {
                            fonts.push(font);
                        }
                    }
                }
            }
            Ok(fonts)
        })
        .as_deref()
        .map_err(Clone::clone)
}
fn font_for(c: char) -> Result<&'static Font, AppError> {
    fonts()?.iter().find(|f|f.lookup_glyph_index(c)!=0).ok_or(AppError::InvalidInput("text contains an unsupported glyph; choose another character or install a supported font"))
}
pub(crate) fn validate_text(text: &str) -> Result<(), AppError> {
    for c in text.chars() {
        if matches!(c, '\n' | '\t') {
            continue;
        }
        if c.is_control() {
            return Err(AppError::InvalidInput(
                "text contains an unsupported control character",
            ));
        }
        font_for(c)?;
    }
    Ok(())
}
pub(crate) fn measure(text: &str, size: f32) -> Result<(f32, f32), AppError> {
    let mut width = 0.0f32;
    let mut line = 0.0f32;
    let mut height = size * 1.3;
    for c in text.chars() {
        if c == '\n' {
            width = width.max(line);
            line = 0.0;
            height += size * 1.3;
        } else if c == '\t' {
            line += size * 2.0;
        } else {
            line += font_for(c)?.metrics(c, size).advance_width;
        }
    }
    Ok((width.max(line), height))
}
pub(crate) fn draw(
    canvas: &mut Pixmap,
    crop: PhysicalRect,
    position: PhysicalPoint,
    text: &str,
    size: f32,
    color: [u8; 4],
) -> Result<(), AppError> {
    let (origin_x, origin_y) = super::shapes::local(position, crop);
    let mut x = origin_x;
    let mut y = origin_y;
    for c in text.chars() {
        if c == '\n' {
            x = origin_x;
            y += size * 1.3;
            continue;
        }
        if c == '\t' {
            x += size * 2.0;
            continue;
        }
        let font = font_for(c)?;
        let metrics = font.metrics(c, size);
        let ascent = font
            .horizontal_line_metrics(size)
            .map_or(size, |m| m.ascent);
        let left = x + metrics.xmin as f32;
        let top = y + ascent - metrics.height as f32 - metrics.ymin as f32;
        // Do not rasterize large off-crop glyphs. Whitespace still advances.
        if metrics.width > 0
            && metrics.height > 0
            && left < canvas.width() as f32
            && top < canvas.height() as f32
            && left + metrics.width as f32 > 0.0
            && top + metrics.height as f32 > 0.0
        {
            let (_, bitmap) = font.rasterize(c, size);
            for gy in 0..metrics.height {
                for gx in 0..metrics.width {
                    let px = left.round() as i64 + gx as i64;
                    let py = top.round() as i64 + gy as i64;
                    if px >= 0
                        && py >= 0
                        && px < i64::from(canvas.width())
                        && py < i64::from(canvas.height())
                    {
                        let alpha = (u32::from(color[3])
                            * u32::from(bitmap[gy * metrics.width + gx])
                            + 127)
                            / 255;
                        let i = ((py as usize * canvas.width() as usize) + px as usize) * 4;
                        let pixel = &mut canvas.data_mut()[i..i + 4];
                        for channel in 0..3 {
                            pixel[channel] = ((u32::from(color[channel]) * alpha
                                + u32::from(pixel[channel]) * (255 - alpha)
                                + 127)
                                / 255) as u8;
                        }
                        pixel[3] =
                            (alpha + (u32::from(pixel[3]) * (255 - alpha) + 127) / 255) as u8;
                    }
                }
            }
        }
        x += metrics.advance_width;
    }
    Ok(())
}
