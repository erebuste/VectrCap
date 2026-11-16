//! Read-only frozen-source sampling. Lens, guides and labels are chrome only.
use crate::{
    app::error::AppError,
    capture::{
        geometry::PhysicalPoint,
        preferences::{LensShape, RegionCapturePreferences},
    },
    editor::{EditorDocument, sample_source_color},
};
use eframe::egui::{self, Color32, Context, Id, LayerId, Order, Pos2, Rect, Stroke, Vec2};

pub fn hex_text([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}
pub fn rgb_text([r, g, b]: [u8; 3]) -> String {
    format!("{r}, {g}, {b}")
}

pub struct CaptureReadout {
    count: u8,
    pixels: Vec<Option<[u8; 4]>>,
    lines: Vec<String>,
    texture: Option<egui::TextureHandle>,
}
pub struct MagnifierLayout {
    pub rect: Rect,
    pub pixel_scale: f32,
}
impl MagnifierLayout {
    pub fn fit(pointer: Pos2, viewport: Rect, count: u8, scale: u8) -> Result<Self, AppError> {
        Self::fit_physical(pointer, viewport, count, scale, 1.0)
    }
    pub fn fit_physical(
        pointer: Pos2,
        viewport: Rect,
        count: u8,
        scale: u8,
        pixels_per_point: f32,
    ) -> Result<Self, AppError> {
        if !pixels_per_point.is_finite() || !(0.1..=20.0).contains(&pixels_per_point) {
            return Err(AppError::InvalidInput("magnifier display scale is invalid"));
        }
        if !(9..=35).contains(&count)
            || count.is_multiple_of(2)
            || !(4..=24).contains(&scale)
            || !viewport.is_finite()
            || viewport.width() <= 0.0
            || viewport.height() <= 0.0
            || !pointer.x.is_finite()
            || !pointer.y.is_finite()
        {
            return Err(AppError::InvalidInput("magnifier geometry is invalid"));
        }
        // Fitting affects presentation only, never count, center or sample positions.
        let pixel_scale = (f32::from(scale) / pixels_per_point)
            .min(viewport.width() / f32::from(count))
            .min(viewport.height() / f32::from(count));
        let side = pixel_scale * f32::from(count);
        let x = if pointer.x + 24.0 + side <= viewport.right() {
            pointer.x + 24.0
        } else {
            pointer.x - 24.0 - side
        };
        let y = if pointer.y + 24.0 + side <= viewport.bottom() {
            pointer.y + 24.0
        } else {
            pointer.y - 24.0 - side
        };
        let min = Pos2::new(
            x.clamp(
                viewport.left(),
                (viewport.right() - side).max(viewport.left()),
            ),
            y.clamp(
                viewport.top(),
                (viewport.bottom() - side).max(viewport.top()),
            ),
        );
        Ok(Self {
            rect: Rect::from_min_size(min, Vec2::splat(side)),
            pixel_scale,
        })
    }
}
impl CaptureReadout {
    pub fn sample(
        document: &EditorDocument,
        point: PhysicalPoint,
        prefs: &RegionCapturePreferences,
    ) -> Result<Self, AppError> {
        prefs.validate()?;
        let count = prefs.lens_source_count;
        let radius = i32::from(count / 2);
        let mut pixels = Vec::with_capacity(usize::from(count).pow(2));
        for y in 0..count {
            for x in 0..count {
                let source = point
                    .x
                    .checked_add(i32::from(x) - radius)
                    .zip(point.y.checked_add(i32::from(y) - radius));
                pixels.push(
                    source.and_then(|(x, y)| {
                        sample_source_color(document, PhysicalPoint { x, y }).ok()
                    }),
                );
            }
        }
        let center = pixels[pixels.len() / 2];
        let mut lines = vec![];
        if prefs.show_position_size && prefs.show_coordinates {
            lines.push(format!("X: {}  Y: {}", point.x, point.y));
        }
        if prefs.show_position_size
            && prefs.show_dimensions
            && let Some(crop) = document.crop()
        {
            lines.push(format!(
                "Width: {}  Height: {}",
                crop.width(),
                crop.height()
            ));
        }
        if prefs.show_rgb {
            lines.push(center.map_or_else(
                || "RGB: Unavailable".into(),
                |[r, g, b, _]| format!("RGB: {}", rgb_text([r, g, b])),
            ));
        }
        if prefs.show_hex {
            lines.push(center.map_or_else(
                || "HEX: Unavailable".into(),
                |[r, g, b, _]| format!("HEX: {}", hex_text([r, g, b])),
            ));
        }
        Ok(Self {
            count,
            pixels,
            lines,
            texture: None,
        })
    }
    pub fn center(&self) -> Option<[u8; 4]> {
        self.pixel(self.count / 2, self.count / 2)
    }
    pub fn pixel(&self, x: u8, y: u8) -> Option<[u8; 4]> {
        if x >= self.count || y >= self.count {
            return None;
        }
        self.pixels[usize::from(y) * usize::from(self.count) + usize::from(x)]
    }
    pub fn grid_size(&self) -> u8 {
        self.count
    }
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    pub fn paint(
        &mut self,
        ctx: &Context,
        pointer: Pos2,
        viewport: Rect,
        prefs: &RegionCapturePreferences,
    ) -> Result<(), AppError> {
        self.paint_scaled(ctx, pointer, viewport, prefs, 1.0)
    }
    /// The utility lens is specified in physical display pixels. Screenshot
    /// chrome retains its existing logical sizing policy via `paint` above.
    pub fn paint_scaled(
        &mut self,
        ctx: &Context,
        pointer: Pos2,
        viewport: Rect,
        prefs: &RegionCapturePreferences,
        pixels_per_point: f32,
    ) -> Result<(), AppError> {
        prefs.validate()?;
        let layout = MagnifierLayout::fit_physical(
            pointer,
            viewport,
            self.count,
            prefs.lens_scale,
            pixels_per_point,
        )?;
        let painter = ctx
            .layer_painter(LayerId::new(Order::Tooltip, Id::unique("capture-readout")))
            .with_clip_rect(viewport);
        let guide = |a: Pos2, b: Pos2| {
            painter.line_segment([a, b], Stroke::new(3.0, Color32::BLACK));
            painter.line_segment([a, b], Stroke::new(1.0, Color32::WHITE));
        };
        if prefs.screen_crosshair {
            guide(
                Pos2::new(viewport.left(), pointer.y),
                Pos2::new(viewport.right(), pointer.y),
            );
            guide(
                Pos2::new(pointer.x, viewport.top()),
                Pos2::new(pointer.x, viewport.bottom()),
            );
        }
        if prefs.center_crosshair {
            guide(
                pointer - Vec2::new(12.0, 0.0),
                pointer + Vec2::new(12.0, 0.0),
            );
            guide(
                pointer - Vec2::new(0.0, 12.0),
                pointer + Vec2::new(0.0, 12.0),
            );
        }
        if prefs.show_magnifier {
            let texture = self.texture.get_or_insert_with(|| {
                let mut rgba = Vec::with_capacity(self.pixels.len() * 4);
                for (i, pixel) in self.pixels.iter().enumerate() {
                    // Unavailable cells have a neutral checker; sampling still returns None.
                    let missing = if (i / usize::from(self.count) + i % usize::from(self.count))
                        .is_multiple_of(2)
                    {
                        40
                    } else {
                        80
                    };
                    rgba.extend_from_slice(&pixel.unwrap_or([missing, missing, missing, 255]));
                }
                ctx.load_texture(
                    "source-magnifier",
                    egui::ColorImage::from_rgba_unmultiplied([usize::from(self.count); 2], &rgba),
                    egui::TextureOptions::NEAREST,
                )
            });
            if prefs.lens_shape == LensShape::Square {
                painter.image(
                    texture.id(),
                    layout.rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                painter.rect_stroke(
                    layout.rect,
                    0,
                    Stroke::new(2.0, Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            } else {
                let mut mesh = egui::Mesh::with_texture(texture.id());
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: layout.rect.center(),
                    uv: Pos2::new(0.5, 0.5),
                    color: Color32::WHITE,
                });
                for i in 0..=64 {
                    let angle = i as f32 * std::f32::consts::TAU / 64.0;
                    let unit = Vec2::new(angle.cos(), angle.sin());
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: layout.rect.center() + unit * layout.rect.width() / 2.0,
                        uv: Pos2::new(0.5 + unit.x / 2.0, 0.5 + unit.y / 2.0),
                        color: Color32::WHITE,
                    });
                    if i > 0 {
                        mesh.add_triangle(0, i, i + 1);
                    }
                }
                painter.add(mesh);
                painter.circle_stroke(
                    layout.rect.center(),
                    layout.rect.width() / 2.0,
                    Stroke::new(2.0, Color32::WHITE),
                );
            }
            for i in 1..self.count {
                let offset = (f32::from(i) - f32::from(self.count) / 2.0) * layout.pixel_scale;
                let half = layout.rect.width() / 2.0;
                let extent = if prefs.lens_shape == LensShape::Circle {
                    (half * half - offset * offset).max(0.0).sqrt()
                } else {
                    half
                };
                let c = layout.rect.center();
                let grid = Stroke::new(1.0 / pixels_per_point, Color32::from_black_alpha(100));
                painter.line_segment(
                    [
                        Pos2::new(c.x + offset, c.y - extent),
                        Pos2::new(c.x + offset, c.y + extent),
                    ],
                    grid,
                );
                painter.line_segment(
                    [
                        Pos2::new(c.x - extent, c.y + offset),
                        Pos2::new(c.x + extent, c.y + offset),
                    ],
                    grid,
                );
            }
            let center =
                Rect::from_center_size(layout.rect.center(), Vec2::splat(layout.pixel_scale));
            painter.rect_stroke(
                center,
                0,
                Stroke::new(3.0, Color32::BLACK),
                egui::StrokeKind::Outside,
            );
            painter.rect_stroke(
                center,
                0,
                Stroke::new(1.0, Color32::WHITE),
                egui::StrokeKind::Outside,
            );
        }
        if !self.lines.is_empty() {
            let galley = painter.layout(
                self.lines.join("\n"),
                egui::FontId::monospace(13.0),
                ctx.global_style().visuals.text_color(),
                (viewport.width() - 12.0).max(1.0),
            );
            let size = galley.size() + Vec2::splat(12.0);
            let pos = Pos2::new(
                (pointer.x + 20.0).clamp(
                    viewport.left(),
                    (viewport.right() - size.x).max(viewport.left()),
                ),
                (pointer.y - size.y - 20.0).clamp(
                    viewport.top(),
                    (viewport.bottom() - size.y).max(viewport.top()),
                ),
            );
            painter.rect_filled(
                Rect::from_min_size(pos, size),
                4,
                ctx.global_style().visuals.panel_fill,
            );
            painter.galley(
                pos + Vec2::splat(6.0),
                galley,
                ctx.global_style().visuals.text_color(),
            );
        }
        Ok(())
    }
}
