//! Shared physical-coordinate paths for preview and export.
use super::Style;
use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, PhysicalRect},
};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

pub(crate) fn local(point: PhysicalPoint, crop: PhysicalRect) -> (f32, f32) {
    (
        (i64::from(point.x) - i64::from(crop.left())) as f32,
        (i64::from(point.y) - i64::from(crop.top())) as f32,
    )
}
pub(crate) fn rectangle(bounds: PhysicalRect, crop: PhysicalRect) -> Result<Path, AppError> {
    let (x, y) = local(
        PhysicalPoint {
            x: bounds.left(),
            y: bounds.top(),
        },
        crop,
    );
    let r = tiny_skia::Rect::from_xywh(x, y, bounds.width() as f32, bounds.height() as f32)
        .ok_or(AppError::Encoder("shape geometry is invalid"))?;
    Ok(PathBuilder::from_rect(r))
}
pub(crate) fn paint(color: [u8; 4]) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    paint
}
pub(crate) fn shape(canvas: &mut Pixmap, path: &Path, style: &Style) {
    if let Some(fill) = style.fill {
        canvas.fill_path(
            path,
            &paint(fill),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    canvas.stroke_path(
        path,
        &paint(style.stroke),
        &Stroke {
            width: style.width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        },
        Transform::identity(),
        None,
    );
}
pub(crate) fn stroke(
    canvas: &mut Pixmap,
    points: &[PhysicalPoint],
    crop: PhysicalRect,
    color: [u8; 4],
    width: f32,
) -> Result<(), AppError> {
    let mut builder = PathBuilder::new();
    let first = points
        .first()
        .ok_or(AppError::Encoder("stroke has no points"))?;
    let (x, y) = local(*first, crop);
    if points.iter().all(|p| p == first) {
        let path = PathBuilder::from_circle(x, y, width / 2.0)
            .ok_or(AppError::Encoder("stroke geometry is invalid"))?;
        canvas.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        return Ok(());
    }
    builder.move_to(x, y);
    for p in &points[1..] {
        let (x, y) = local(*p, crop);
        builder.line_to(x, y);
    }
    let path = builder
        .finish()
        .ok_or(AppError::Encoder("stroke geometry is invalid"))?;
    canvas.stroke_path(
        &path,
        &paint(color),
        &Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        },
        Transform::identity(),
        None,
    );
    Ok(())
}
