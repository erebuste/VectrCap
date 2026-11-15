use super::{Annotation, EditorDocument};
use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, allocate_rgba},
};
use image::RgbaImage;
/// Flatten only source pixels and approved annotation geometry. Chrome is never
/// an input. Redaction writes straight-alpha RGBA last, without antialiasing.
pub fn render_export(document: &EditorDocument) -> Result<RgbaImage, AppError> {
    let crop = document.crop().ok_or(AppError::InvalidInput(
        "select an output region before finishing",
    ))?;
    let mut data = allocate_rgba(crop)?;
    for y in 0..crop.height() {
        for x in 0..crop.width() {
            let p = PhysicalPoint {
                x: (i64::from(crop.left()) + i64::from(x)) as i32,
                y: (i64::from(crop.top()) + i64::from(y)) as i32,
            };
            let (sx, sy) = document.bounds().pixel_offset(p)?;
            let source = document.source().get_pixel(sx, sy).0;
            let index = ((u64::from(y) * u64::from(crop.width()) + u64::from(x)) * 4) as usize;
            for c in 0..3 {
                data[index + c] = ((u32::from(source[c]) * u32::from(source[3]) + 127) / 255) as u8;
            }
            data[index + 3] = source[3];
        }
    }
    let size = tiny_skia::IntSize::from_wh(crop.width(), crop.height())
        .ok_or(AppError::Encoder("output dimensions are invalid"))?;
    let mut canvas = tiny_skia::Pixmap::from_vec(data, size)
        .ok_or(AppError::Encoder("output buffer dimensions are invalid"))?;
    super::effects::spotlight(&mut canvas, crop, document.annotations());
    for a in document.annotations() {
        a.validate()?;
        match a {
            Annotation::Ellipse { bounds, style, .. } => {
                super::shapes::shape(&mut canvas, &super::shapes::ellipse(*bounds, crop)?, style)
            }
            Annotation::Line {
                start,
                end,
                color,
                width,
                ..
            } => super::shapes::stroke(&mut canvas, &[*start, *end], crop, *color, *width)?,
            Annotation::Arrow {
                start,
                end,
                color,
                width,
                ..
            } => super::shapes::arrow(&mut canvas, *start, *end, crop, *color, *width)?,
            Annotation::Step {
                center,
                radius,
                color,
                number,
                ..
            } => super::shapes::step(&mut canvas, crop, *center, *radius, *color, *number)?,
            Annotation::Highlight {
                bounds,
                color,
                opacity,
                ..
            } => {
                let color = [
                    color[0],
                    color[1],
                    color[2],
                    (opacity * 255.0).round() as u8,
                ];
                canvas.fill_path(
                    &super::shapes::rectangle(*bounds, crop)?,
                    &super::shapes::paint(color),
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
            Annotation::Spotlight { .. } => {}
            Annotation::SmartEraseStroke {
                points,
                color,
                width,
                ..
            } => super::shapes::stroke(
                &mut canvas,
                points,
                crop,
                [color[0], color[1], color[2], 255],
                *width,
            )?,
            Annotation::Rectangle { bounds, style, .. } => super::shapes::shape(
                &mut canvas,
                &super::shapes::rectangle(*bounds, crop)?,
                style,
            ),
            Annotation::Freehand {
                points,
                color,
                width,
                ..
            } => super::shapes::stroke(&mut canvas, points, crop, *color, *width)?,
            Annotation::Text {
                position,
                content,
                size,
                color,
                ..
            } => super::text::draw(&mut canvas, crop, *position, content, *size, *color)?,
            Annotation::Redaction { .. } => {}
        }
    }
    let mut data = canvas.take();
    for pixel in data.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
    for a in document.annotations() {
        if let Annotation::Redaction { bounds, color, .. } = a
            && let Some(part) = bounds.intersection(crop)
        {
            let (dx, dy) = crop.pixel_offset(PhysicalPoint {
                x: part.left(),
                y: part.top(),
            })?;
            for y in dy..dy + part.height() {
                for x in dx..dx + part.width() {
                    let i = ((u64::from(y) * u64::from(crop.width()) + u64::from(x)) * 4) as usize;
                    data[i..i + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
                }
            }
        }
    }
    RgbaImage::from_raw(crop.width(), crop.height(), data)
        .ok_or(AppError::Encoder("output buffer dimensions are invalid"))
}
pub fn sample_source_color(
    document: &EditorDocument,
    point: PhysicalPoint,
) -> Result<[u8; 4], AppError> {
    if !document.available(point) {
        return Err(AppError::InvalidInput("source pixel is unavailable"));
    }
    let (x, y) = document.bounds().pixel_offset(point)?;
    Ok(document.source().get_pixel(x, y).0)
}
