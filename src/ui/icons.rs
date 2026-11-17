//! Painted icons matching the mockup's Lucide strokes (24-unit box, stroke 2).
use eframe::egui::{
    Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Brand,
    History,
    Settings,
    Power,
    PanelRight,
    Upload,
    Grid,
    List,
    Search,
    Pause,
    Play,
    X,
    Retry,
    Check,
    Alert,
    Attention,
    Copy,
    External,
    Folder,
    File,
    Pin,
    Star,
    ChevronRight,
    ChevronDown,
    Plus,
}

impl Icon {
    pub const ALL: [Icon; 25] = [
        Icon::Brand,
        Icon::History,
        Icon::Settings,
        Icon::Power,
        Icon::PanelRight,
        Icon::Upload,
        Icon::Grid,
        Icon::List,
        Icon::Search,
        Icon::Pause,
        Icon::Play,
        Icon::X,
        Icon::Retry,
        Icon::Check,
        Icon::Alert,
        Icon::Attention,
        Icon::Copy,
        Icon::External,
        Icon::Folder,
        Icon::File,
        Icon::Pin,
        Icon::Star,
        Icon::ChevronRight,
        Icon::ChevronDown,
        Icon::Plus,
    ];
}

pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    painter.extend(shapes(icon, rect, color));
}

/// The icon's strokes, mapped from the 24-unit Lucide grid onto `rect`.
pub fn shapes(icon: Icon, rect: Rect, color: Color32) -> Vec<Shape> {
    let k = rect.width().min(rect.height()) / 24.0;
    let o = rect.center() - vec2(12.0, 12.0) * k;
    let p = |x: f32, y: f32| -> Pos2 { o + vec2(x, y) * k };
    let width = if icon == Icon::Brand { 2.4 } else { 2.0 };
    let stroke = Stroke::new((width * k).max(1.0), color);
    let line =
        |points: &[(f32, f32)]| Shape::line(points.iter().map(|&(x, y)| p(x, y)).collect(), stroke);
    let closed = |points: &[(f32, f32)]| {
        Shape::closed_line(points.iter().map(|&(x, y)| p(x, y)).collect(), stroke)
    };
    let circle = |x: f32, y: f32, r: f32| Shape::circle_stroke(p(x, y), r * k, stroke);
    let dot = |x: f32, y: f32| Shape::circle_filled(p(x, y), 1.1 * k.max(0.9), color);
    let rect_at = |x: f32, y: f32, w: f32, h: f32, r: f32| {
        Shape::rect_stroke(
            Rect::from_min_size(p(x, y), vec2(w, h) * k),
            CornerRadius::same((r * k).round() as u8),
            stroke,
            StrokeKind::Middle,
        )
    };
    // Arc around (cx, cy) from `from` to `to` degrees (y down), as a polyline.
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| {
        let steps = 24;
        let points = (0..=steps)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
                p(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        Shape::line(points, stroke)
    };
    match icon {
        Icon::Brand => vec![
            line(&[(3.0, 4.0), (11.0, 20.0), (21.0, 4.0)]),
            line(&[(7.0, 4.0), (12.0, 14.0), (18.0, 4.0)]),
        ],
        Icon::History => vec![
            arc(12.0, 12.0, 9.0, 180.0, -135.0),
            line(&[(3.0, 3.0), (3.0, 8.0), (8.0, 8.0)]),
            line(&[(12.0, 7.0), (12.0, 12.0), (16.0, 14.0)]),
        ],
        Icon::Retry => vec![
            arc(12.0, 12.0, 9.0, 180.0, -135.0),
            line(&[(3.0, 3.0), (3.0, 8.0), (8.0, 8.0)]),
        ],
        Icon::Settings => vec![
            line(&[(20.0, 7.0), (10.0, 7.0)]),
            line(&[(14.0, 17.0), (4.0, 17.0)]),
            circle(17.0, 17.0, 3.0),
            circle(7.0, 7.0, 3.0),
        ],
        Icon::Power => vec![
            line(&[(12.0, 2.0), (12.0, 12.0)]),
            arc(12.0, 13.0, 9.0, -45.0, 225.0),
        ],
        Icon::PanelRight => vec![
            rect_at(3.0, 3.0, 18.0, 18.0, 2.0),
            line(&[(15.0, 3.0), (15.0, 21.0)]),
        ],
        Icon::Upload => vec![
            line(&[
                (3.0, 15.0),
                (3.0, 19.0),
                (5.0, 21.0),
                (19.0, 21.0),
                (21.0, 19.0),
                (21.0, 15.0),
            ]),
            line(&[(17.0, 8.0), (12.0, 3.0), (7.0, 8.0)]),
            line(&[(12.0, 3.0), (12.0, 15.0)]),
        ],
        Icon::Grid => vec![
            rect_at(3.0, 3.0, 7.0, 7.0, 1.0),
            rect_at(14.0, 3.0, 7.0, 7.0, 1.0),
            rect_at(14.0, 14.0, 7.0, 7.0, 1.0),
            rect_at(3.0, 14.0, 7.0, 7.0, 1.0),
        ],
        Icon::List => vec![
            line(&[(8.0, 6.0), (21.0, 6.0)]),
            line(&[(8.0, 12.0), (21.0, 12.0)]),
            line(&[(8.0, 18.0), (21.0, 18.0)]),
            dot(3.5, 6.0),
            dot(3.5, 12.0),
            dot(3.5, 18.0),
        ],
        Icon::Search => vec![circle(11.0, 11.0, 7.0), line(&[(21.0, 21.0), (16.3, 16.3)])],
        Icon::Pause => vec![
            rect_at(14.0, 4.0, 4.0, 16.0, 1.0),
            rect_at(6.0, 4.0, 4.0, 16.0, 1.0),
        ],
        Icon::Play => vec![closed(&[(6.0, 3.0), (20.0, 12.0), (6.0, 21.0)])],
        Icon::X => vec![
            line(&[(18.0, 6.0), (6.0, 18.0)]),
            line(&[(6.0, 6.0), (18.0, 18.0)]),
        ],
        Icon::Check => vec![line(&[(20.0, 6.0), (9.0, 17.0), (4.0, 12.0)])],
        Icon::Alert => vec![
            circle(12.0, 12.0, 10.0),
            line(&[(12.0, 8.0), (12.0, 12.0)]),
            dot(12.0, 16.0),
        ],
        Icon::Attention => vec![
            closed(&[(12.0, 3.0), (22.0, 20.0), (2.0, 20.0)]),
            line(&[(12.0, 9.0), (12.0, 13.0)]),
            dot(12.0, 16.8),
        ],
        Icon::Copy => vec![
            rect_at(8.0, 8.0, 14.0, 14.0, 2.0),
            line(&[
                (4.0, 16.0),
                (2.0, 14.0),
                (2.0, 4.0),
                (4.0, 2.0),
                (14.0, 2.0),
                (16.0, 4.0),
            ]),
        ],
        Icon::External => vec![
            line(&[(15.0, 3.0), (21.0, 3.0), (21.0, 9.0)]),
            line(&[(10.0, 14.0), (21.0, 3.0)]),
            line(&[
                (18.0, 13.0),
                (18.0, 19.0),
                (16.0, 21.0),
                (5.0, 21.0),
                (3.0, 19.0),
                (3.0, 8.0),
                (5.0, 6.0),
                (11.0, 6.0),
            ]),
        ],
        Icon::Folder => vec![closed(&[
            (2.0, 5.0),
            (4.0, 3.0),
            (8.0, 3.0),
            (10.0, 6.0),
            (20.0, 6.0),
            (22.0, 8.0),
            (22.0, 18.0),
            (20.0, 20.0),
            (4.0, 20.0),
            (2.0, 18.0),
        ])],
        Icon::File => vec![
            closed(&[
                (15.0, 2.0),
                (6.0, 2.0),
                (4.0, 4.0),
                (4.0, 20.0),
                (6.0, 22.0),
                (18.0, 22.0),
                (20.0, 20.0),
                (20.0, 7.0),
            ]),
            line(&[(14.0, 2.0), (14.0, 6.0), (16.0, 8.0), (20.0, 8.0)]),
        ],
        Icon::Pin => vec![
            line(&[(12.0, 17.0), (12.0, 22.0)]),
            closed(&[
                (9.0, 7.0),
                (9.0, 10.8),
                (5.0, 15.2),
                (5.0, 17.0),
                (19.0, 17.0),
                (19.0, 15.2),
                (15.0, 10.8),
                (15.0, 7.0),
            ]),
            closed(&[(8.0, 2.0), (16.0, 2.0), (16.0, 6.0), (8.0, 6.0)]),
        ],
        Icon::Star => {
            let points: Vec<(f32, f32)> = (0..10)
                .map(|i| {
                    let r = if i % 2 == 0 { 10.0 } else { 4.2 };
                    let a = (-90.0 + 36.0 * i as f32).to_radians();
                    (12.0 + r * a.cos(), 12.5 + r * a.sin())
                })
                .collect();
            // Concave: fill as the inner pentagon plus one triangle per tip.
            let at = |i: usize| p(points[i % 10].0, points[i % 10].1);
            let mut fill = vec![Shape::convex_polygon(
                (0..5).map(|i| at(2 * i + 1)).collect(),
                color,
                Stroke::NONE,
            )];
            fill.extend((0..5).map(|i| {
                Shape::convex_polygon(
                    vec![at(2 * i), at(2 * i + 1), at(2 * i + 9)],
                    color,
                    Stroke::NONE,
                )
            }));
            fill.push(closed(&points));
            fill
        }
        Icon::ChevronRight => vec![line(&[(9.0, 18.0), (15.0, 12.0), (9.0, 6.0)])],
        Icon::ChevronDown => vec![line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)])],
        Icon::Plus => vec![
            line(&[(12.0, 5.0), (12.0, 19.0)]),
            line(&[(5.0, 12.0), (19.0, 12.0)]),
        ],
    }
}

/// A square `size` rect for an icon at the start of `area`, vertically centred.
pub fn slot(area: Rect, size: f32) -> Rect {
    Rect::from_center_size(
        pos2(area.left() + size / 2.0, area.center().y),
        vec2(size, size),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Color32, Pos2, Rect, vec2};

    #[test]
    fn every_icon_paints_something() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(24.0, 24.0));
        for icon in Icon::ALL {
            assert!(
                !shapes(icon, rect, Color32::WHITE).is_empty(),
                "{icon:?} drew nothing"
            );
        }
    }
}
