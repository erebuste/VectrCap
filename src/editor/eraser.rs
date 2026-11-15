//! Whole-object brush intersections; this module never mutates source pixels.
use super::{Annotation, EditorDocument};
use crate::{app::error::AppError, capture::geometry::PhysicalPoint};
use uuid::Uuid;
type Point = (f64, f64);
type Segment = (Point, Point);
const MAX_COMPARISONS: usize = 2_000_000;
const TOO_COMPLEX: AppError =
    AppError::InvalidInput("eraser gesture is too complex; use a shorter gesture");
fn local(p: PhysicalPoint, bounds: crate::capture::geometry::PhysicalRect) -> Point {
    (
        (i64::from(p.x) - i64::from(bounds.left())) as f64,
        (i64::from(p.y) - i64::from(bounds.top())) as f64,
    )
}
fn distance(p: Point, a: Point, b: Point) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        ((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length
    }
    .clamp(0.0, 1.0);
    (p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)
}
fn cross(a: Point, b: Point, c: Point) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}
fn overlap(a: Segment, b: Segment, r: f64) -> bool {
    a.0.0.min(a.1.0) - r <= b.0.0.max(b.1.0)
        && a.0.0.max(a.1.0) + r >= b.0.0.min(b.1.0)
        && a.0.1.min(a.1.1) - r <= b.0.1.max(b.1.1)
        && a.0.1.max(a.1.1) + r >= b.0.1.min(b.1.1)
}
fn segment_distance(a: Segment, b: Segment) -> f64 {
    let (a0, a1) = a;
    let (b0, b1) = b;
    if overlap(a, b, 0.0)
        && cross(a0, a1, b0) * cross(a0, a1, b1) <= 0.0
        && cross(b0, b1, a0) * cross(b0, b1, a1) <= 0.0
    {
        return 0.0;
    }
    distance(a0, b0, b1)
        .min(distance(a1, b0, b1))
        .min(distance(b0, a0, a1))
        .min(distance(b1, a0, a1))
}
fn midpoint(a: Point, b: Point) -> Point {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}
fn cubic(
    a: Point,
    b: Point,
    c: Point,
    d: Point,
    depth: u8,
    out: &mut Vec<Point>,
) -> Result<(), AppError> {
    if distance(b, a, d).max(distance(c, a, d)) <= 0.015625 {
        out.push(d);
        return Ok(());
    }
    if depth >= 12 || out.len() >= 20000 {
        return Err(TOO_COMPLEX);
    }
    let ab = midpoint(a, b);
    let bc = midpoint(b, c);
    let cd = midpoint(c, d);
    let abc = midpoint(ab, bc);
    let bcd = midpoint(bc, cd);
    let m = midpoint(abc, bcd);
    cubic(a, ab, abc, m, depth + 1, out)?;
    cubic(m, bcd, cd, d, depth + 1, out)
}
fn flatten(path: &tiny_skia::Path) -> Result<Vec<Point>, AppError> {
    let mut out = vec![];
    let mut current = (0.0, 0.0);
    let mut first = current;
    let point = |p: tiny_skia::Point| (f64::from(p.x), f64::from(p.y));
    for s in path.segments() {
        match s {
            tiny_skia::PathSegment::MoveTo(p) => {
                current = point(p);
                first = current;
                out.push(current);
            }
            tiny_skia::PathSegment::LineTo(p) => {
                current = point(p);
                out.push(current);
            }
            tiny_skia::PathSegment::QuadTo(b, d) => {
                let b = point(b);
                let d = point(d);
                let c1 = (
                    current.0 + (b.0 - current.0) * 2.0 / 3.0,
                    current.1 + (b.1 - current.1) * 2.0 / 3.0,
                );
                let c2 = (d.0 + (b.0 - d.0) * 2.0 / 3.0, d.1 + (b.1 - d.1) * 2.0 / 3.0);
                cubic(current, c1, c2, d, 0, &mut out)?;
                current = d;
            }
            tiny_skia::PathSegment::CubicTo(b, c, d) => {
                let d = point(d);
                cubic(current, point(b), point(c), d, 0, &mut out)?;
                current = d;
            }
            tiny_skia::PathSegment::Close => {
                if current != first {
                    out.push(first);
                }
                current = first;
            }
        }
    }
    Ok(out)
}
struct Geometry {
    points: Vec<Point>,
    extra: Vec<Segment>,
    filled: bool,
    radius: f64,
}
fn geometry(
    a: &Annotation,
    bounds: crate::capture::geometry::PhysicalRect,
) -> Result<Geometry, AppError> {
    let mut g = Geometry {
        points: vec![],
        extra: vec![],
        filled: false,
        radius: 0.0,
    };
    match a {
        Annotation::Rectangle {
            bounds: r, style, ..
        }
        | Annotation::Ellipse {
            bounds: r, style, ..
        } => {
            let path = if matches!(a, Annotation::Ellipse { .. }) {
                super::shapes::ellipse(*r, bounds)?
            } else {
                super::shapes::rectangle(*r, bounds)?
            };
            g.points = flatten(&path)?;
            g.filled = style.fill.is_some();
            g.radius = f64::from(style.width) / 2.0;
        }
        Annotation::Line {
            start, end, width, ..
        }
        | Annotation::Arrow {
            start, end, width, ..
        } => {
            g.points = vec![local(*start, bounds), local(*end, bounds)];
            g.radius = f64::from(*width) / 2.0;
            if matches!(a, Annotation::Arrow { .. }) {
                let p = super::shapes::arrow_head(*start, *end, *width)
                    .map(|(x, y)| (x - f64::from(bounds.left()), y - f64::from(bounds.top())));
                g.extra = vec![(p[0], p[1]), (p[1], p[2])];
            }
        }
        Annotation::Freehand { points, width, .. }
        | Annotation::SmartEraseStroke { points, width, .. } => {
            g.points = points.iter().map(|p| local(*p, bounds)).collect();
            g.radius = f64::from(*width) / 2.0;
        }
        Annotation::Highlight { bounds: r, .. }
        | Annotation::Spotlight { bounds: r, .. }
        | Annotation::Redaction { bounds: r, .. } => {
            g.points = flatten(&super::shapes::rectangle(*r, bounds)?)?;
            g.filled = true;
        }
        Annotation::Step { center, radius, .. } => {
            let (x, y) = super::shapes::local(*center, bounds);
            let path = tiny_skia::PathBuilder::from_circle(x, y, *radius)
                .ok_or(AppError::InvalidInput("step geometry is invalid"))?;
            g.points = flatten(&path)?;
            g.filled = true;
        }
        Annotation::Text {
            position,
            content,
            size,
            ..
        } => {
            let (w, h) = super::text::measure(content, *size)?;
            let (x, y) = local(*position, bounds);
            let (w, h) = (f64::from(w), f64::from(h));
            g.points = vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)];
            g.filled = true;
        }
    }
    Ok(g)
}
fn spend(budget: &mut usize) -> Result<(), AppError> {
    *budget = budget.checked_sub(1).ok_or(TOO_COMPLEX)?;
    Ok(())
}
fn inside(p: Point, polygon: &[Point], budget: &mut usize) -> Result<bool, AppError> {
    let mut inside = false;
    for edge in polygon.windows(2) {
        spend(budget)?;
        let (a, b) = (edge[0], edge[1]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
    }
    Ok(inside)
}
/// An entire gesture is tested, including its connecting segments. Returns
/// stable insertion-order IDs once each; callers apply one Erase command.
pub fn erase_annotation_hits(
    document: &EditorDocument,
    points: &[PhysicalPoint],
    radius: f32,
) -> Result<Vec<Uuid>, AppError> {
    if points.is_empty()
        || points.len() > super::document::MAX_STROKE_POINTS
        || !radius.is_finite()
        || !(0.5..=1024.0).contains(&radius)
    {
        return Err(AppError::InvalidInput(
            "eraser radius or gesture length is invalid",
        ));
    }
    let brush: Vec<_> = points
        .iter()
        .map(|p| local(*p, document.bounds()))
        .collect();
    let brush_segments: Vec<_> = if brush.len() == 1 {
        vec![(brush[0], brush[0])]
    } else {
        brush.windows(2).map(|p| (p[0], p[1])).collect()
    };
    let mut hits = vec![];
    let mut budget = MAX_COMPARISONS;
    for a in document.annotations() {
        let g = geometry(a, document.bounds())?;
        let segments: Vec<Segment> = if g.points.len() == 1 {
            vec![(g.points[0], g.points[0])]
        } else {
            g.points
                .windows(2)
                .map(|p| (p[0], p[1]))
                .chain(g.extra.iter().copied())
                .collect()
        };
        let mut hit = false;
        let r = f64::from(radius) + g.radius + 0.125;
        for b in &brush_segments {
            if g.filled
                && (inside(b.0, &g.points, &mut budget)? || inside(b.1, &g.points, &mut budget)?)
            {
                hit = true;
                break;
            }
            for s in &segments {
                spend(&mut budget)?;
                if overlap(*b, *s, r) && segment_distance(*b, *s) <= r * r {
                    hit = true;
                    break;
                }
            }
            if hit {
                break;
            }
        }
        if hit {
            hits.push(a.id());
        }
    }
    Ok(hits)
}
