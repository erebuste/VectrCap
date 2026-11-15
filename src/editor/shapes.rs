//! Shared physical-coordinate paths for preview and export.
use super::Style;
pub fn constrained_shape(
    start: PhysicalPoint,
    end: PhysicalPoint,
    shift: bool,
) -> Result<PhysicalRect, AppError> {
    if !shift {
        return PhysicalRect::from_points(start, end);
    }
    let extent = start.x.abs_diff(end.x).max(start.y.abs_diff(end.y));
    let left = if end.x < start.x {
        i64::from(start.x) - i64::from(extent)
    } else {
        i64::from(start.x)
    };
    let top = if end.y < start.y {
        i64::from(start.y) - i64::from(extent)
    } else {
        i64::from(start.y)
    };
    PhysicalRect::new(
        i32::try_from(left).map_err(|_| AppError::InvalidInput("shape constraint overflows"))?,
        i32::try_from(top).map_err(|_| AppError::InvalidInput("shape constraint overflows"))?,
        extent,
        extent,
    )
}
pub fn constrained_segment(
    start: PhysicalPoint,
    end: PhysicalPoint,
    shift: bool,
) -> Result<PhysicalPoint, AppError> {
    if !shift {
        return Ok(end);
    }
    let dx = f64::from(end.x) - f64::from(start.x);
    let dy = f64::from(end.y) - f64::from(start.y);
    let angle = (dy.atan2(dx) / std::f64::consts::FRAC_PI_4).round() * std::f64::consts::FRAC_PI_4;
    let distance = dx.hypot(dy);
    let x = f64::from(start.x) + (distance * angle.cos()).round();
    let y = f64::from(start.y) + (distance * angle.sin()).round();
    if x < f64::from(i32::MIN)
        || x > f64::from(i32::MAX)
        || y < f64::from(i32::MIN)
        || y > f64::from(i32::MAX)
    {
        return Err(AppError::InvalidInput("line angle constraint overflows"));
    }
    Ok(PhysicalPoint {
        x: x as i32,
        y: y as i32,
    })
}
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
pub(crate) fn ellipse(bounds: PhysicalRect, crop: PhysicalRect) -> Result<Path, AppError> {
    let (x, y) = local(
        PhysicalPoint {
            x: bounds.left(),
            y: bounds.top(),
        },
        crop,
    );
    let r = tiny_skia::Rect::from_xywh(x, y, bounds.width() as f32, bounds.height() as f32)
        .ok_or(AppError::Encoder("ellipse geometry is invalid"))?;
    PathBuilder::from_oval(r).ok_or(AppError::Encoder("ellipse geometry is invalid"))
}
pub(crate) fn arrow_head(start: PhysicalPoint, end: PhysicalPoint, width: f32) -> [(f64, f64); 3] {
    let dx = f64::from(end.x) - f64::from(start.x);
    let dy = f64::from(end.y) - f64::from(start.y);
    let length = dx.hypot(dy);
    let angle = dy.atan2(dx);
    let size = (f64::from(width) * 4.0).max(8.0).min(length * 0.5);
    let wing = |offset: f64| {
        (
            f64::from(end.x) - size * (angle + offset).cos(),
            f64::from(end.y) - size * (angle + offset).sin(),
        )
    };
    [
        wing(std::f64::consts::FRAC_PI_6),
        (f64::from(end.x), f64::from(end.y)),
        wing(-std::f64::consts::FRAC_PI_6),
    ]
}
pub(crate) fn arrow(
    canvas: &mut Pixmap,
    start: PhysicalPoint,
    end: PhysicalPoint,
    crop: PhysicalRect,
    color: [u8; 4],
    width: f32,
) -> Result<(), AppError> {
    stroke(canvas, &[start, end], crop, color, width)?;
    let mut builder = PathBuilder::new();
    for (i, (x, y)) in arrow_head(start, end, width).into_iter().enumerate() {
        let x = (x - f64::from(crop.left())) as f32;
        let y = (y - f64::from(crop.top())) as f32;
        if i == 0 {
            builder.move_to(x, y);
        } else {
            builder.line_to(x, y);
        }
    }
    let path = builder
        .finish()
        .ok_or(AppError::Encoder("arrow geometry is invalid"))?;
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
pub(crate) fn step(
    canvas: &mut Pixmap,
    crop: PhysicalRect,
    center: PhysicalPoint,
    radius: f32,
    color: [u8; 4],
    number: u32,
) -> Result<(), AppError> {
    let (x, y) = local(center, crop);
    let path = PathBuilder::from_circle(x, y, radius)
        .ok_or(AppError::Encoder("step geometry is invalid"))?;
    canvas.fill_path(
        &path,
        &paint(color),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    let text = number.to_string();
    let mut size = radius * 1.2;
    let (width, _) = super::text::measure(&text, size)?;
    if width > radius * 1.6 {
        size *= radius * 1.6 / width;
    }
    let (width, _) = super::text::measure(&text, size)?;
    let tx = f64::from(center.x) - f64::from(width) / 2.0;
    let ty = f64::from(center.y) - f64::from(size) * 0.65;
    if tx < f64::from(i32::MIN) || ty < f64::from(i32::MIN) {
        return Err(AppError::InvalidInput("step text position overflows"));
    }
    let foreground =
        if u32::from(color[0]) * 299 + u32::from(color[1]) * 587 + u32::from(color[2]) * 114
            > 150000
        {
            [0, 0, 0, 255]
        } else {
            [255; 4]
        };
    super::text::draw(
        canvas,
        crop,
        PhysicalPoint {
            x: tx.round() as i32,
            y: ty.round() as i32,
        },
        &text,
        size,
        foreground,
    )
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
