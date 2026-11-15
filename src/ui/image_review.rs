//! Bounded visible texture tiles. Physical offsets use f64 and are subtracted
//! before f32 painting, keeping long-image coordinates independent of UI scale.
use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, PhysicalRect},
};
use eframe::egui::{self, Painter, Pos2, Rect, Vec2};
use image::RgbaImage;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub const MAX_TILE_SIDE: u32 = 1024;
pub const MAX_VISIBLE_TILES: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageTile {
    bounds: PhysicalRect,
    stride: u32,
}
impl ImageTile {
    pub fn bounds(&self) -> PhysicalRect {
        self.bounds
    }
    pub fn stride(&self) -> u32 {
        self.stride
    }
    pub fn texture_size(&self) -> [usize; 2] {
        [
            self.bounds.width().div_ceil(self.stride) as usize,
            self.bounds.height().div_ceil(self.stride) as usize,
        ]
    }
}
pub fn tile_plan(
    width: u32,
    height: u32,
    visible: PhysicalRect,
) -> Result<Vec<ImageTile>, AppError> {
    let source = PhysicalRect::new(0, 0, width, height)?;
    source.rgba_len()?;
    let Some(visible) = source.intersection(visible) else {
        return Ok(vec![]);
    };
    let mut stride = 1u32;
    while visible.width().div_ceil(stride) > MAX_TILE_SIDE
        || visible.height().div_ceil(stride) > MAX_TILE_SIDE
    {
        stride = stride
            .checked_mul(2)
            .ok_or(AppError::InvalidInput("image tile stride overflows"))?;
    }
    let span = MAX_TILE_SIDE
        .checked_mul(stride)
        .ok_or(AppError::InvalidInput("image tile span overflows"))?;
    let mut tiles = vec![];
    for row in visible.top() as u32 / span..=(visible.bottom() as u32 - 1) / span {
        for col in visible.left() as u32 / span..=(visible.right() as u32 - 1) / span {
            let (x, y) = (col * span, row * span);
            tiles.push(ImageTile {
                bounds: PhysicalRect::new(
                    x as i32,
                    y as i32,
                    span.min(width - x),
                    span.min(height - y),
                )?,
                stride,
            });
            if tiles.len() > MAX_VISIBLE_TILES {
                return Err(AppError::InvalidInput("image view exceeds tile budget"));
            }
        }
    }
    Ok(tiles)
}

pub struct TiledImage {
    pixels: Arc<RgbaImage>,
    textures: HashMap<ImageTile, egui::TextureHandle>,
}
impl TiledImage {
    pub fn new(pixels: Arc<RgbaImage>) -> Result<Self, AppError> {
        PhysicalRect::new(0, 0, pixels.width(), pixels.height())?.rgba_len()?;
        Ok(Self {
            pixels,
            textures: HashMap::new(),
        })
    }
    pub fn pixels(&self) -> &Arc<RgbaImage> {
        &self.pixels
    }
    pub fn resident_tile_count(&self) -> usize {
        self.textures.len()
    }
    pub fn paint(
        &mut self,
        painter: &Painter,
        viewport: Rect,
        offset: (f64, f64),
        zoom: f64,
    ) -> Result<(), AppError> {
        if !viewport.is_finite()
            || viewport.width() <= 0.0
            || viewport.height() <= 0.0
            || !zoom.is_finite()
            || !(0.000001..=32.0).contains(&zoom)
            || !offset.0.is_finite()
            || !offset.1.is_finite()
            || offset.0 < 0.0
            || offset.1 < 0.0
            || offset.0 >= f64::from(self.pixels.width())
            || offset.1 >= f64::from(self.pixels.height())
        {
            return Err(AppError::InvalidInput("image view geometry is invalid"));
        }
        let left = offset.0.floor() as u32;
        let top = offset.1.floor() as u32;
        let width = ((f64::from(viewport.width()) / zoom + offset.0.fract()).ceil() as u32)
            .max(1)
            .min(self.pixels.width() - left);
        let height = ((f64::from(viewport.height()) / zoom + offset.1.fract()).ceil() as u32)
            .max(1)
            .min(self.pixels.height() - top);
        let visible = PhysicalRect::new(left as i32, top as i32, width, height)?;
        let tiles = tile_plan(self.pixels.width(), self.pixels.height(), visible)?;
        let retained: HashSet<_> = tiles.iter().copied().collect();
        self.textures.retain(|tile, _| retained.contains(tile));
        let painter = painter.with_clip_rect(viewport.intersect(painter.clip_rect()));
        for tile in tiles {
            let texture = self.textures.entry(tile).or_insert_with(|| {
                let size = tile.texture_size();
                let mut rgba = Vec::with_capacity(size[0] * size[1] * 4);
                for y in 0..size[1] {
                    for x in 0..size[0] {
                        let sx = tile.bounds.left() as u32 + x as u32 * tile.stride;
                        let sy = tile.bounds.top() as u32 + y as u32 * tile.stride;
                        rgba.extend_from_slice(&self.pixels.get_pixel(sx, sy).0);
                    }
                }
                painter.ctx().load_texture(
                    "review-tile",
                    egui::ColorImage::from_rgba_unmultiplied(size, &rgba),
                    egui::TextureOptions::NEAREST,
                )
            });
            let min = viewport.min
                + Vec2::new(
                    ((f64::from(tile.bounds.left()) - offset.0) * zoom) as f32,
                    ((f64::from(tile.bounds.top()) - offset.1) * zoom) as f32,
                );
            let size = Vec2::new(
                (f64::from(tile.bounds.width()) * zoom) as f32,
                (f64::from(tile.bounds.height()) * zoom) as f32,
            );
            let texture_size = tile.texture_size();
            painter.image(
                texture.id(),
                Rect::from_min_size(min, size),
                Rect::from_min_max(
                    Pos2::ZERO,
                    Pos2::new(
                        tile.bounds.width() as f32 / (texture_size[0] as f32 * tile.stride as f32),
                        tile.bounds.height() as f32 / (texture_size[1] as f32 * tile.stride as f32),
                    ),
                ),
                egui::Color32::WHITE,
            );
        }
        Ok(())
    }
}

/// Shared scroll/zoom presentation for both editing and decoded output review.
pub struct ImageViewport {
    tiled: Option<TiledImage>,
    zoom: f64,
    offset: (f64, f64),
}
impl Default for ImageViewport {
    fn default() -> Self {
        Self {
            tiled: None,
            zoom: 1.0,
            offset: (0.0, 0.0),
        }
    }
}
pub(crate) struct ImageCanvas {
    pub rect: egui::Rect,
    pub response: egui::Response,
    pub offset: (f64, f64),
    pub zoom: f64,
}

impl ImageViewport {
    pub(crate) fn canvas(&mut self, ui: &mut egui::Ui, dimensions: (u32, u32)) -> ImageCanvas {
        let min_zoom = (1.0 / f64::from(dimensions.0.max(dimensions.1))).max(0.000001);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Slider::new(&mut self.zoom, min_zoom..=32.0)
                    .logarithmic(true)
                    .text("Zoom"),
            );
            if ui.button("100%").clicked() {
                self.zoom = 1.0;
            }
            if ui.button("Fit image").clicked() {
                self.zoom = (f64::from(ui.available_width()) / f64::from(dimensions.0))
                    .min(
                        f64::from((ui.available_height() - 60.0).max(1.0))
                            / f64::from(dimensions.1),
                    )
                    .clamp(min_zoom, 32.0);
                self.offset = (0.0, 0.0);
            }
        });
        let size = Vec2::new(
            ui.available_width().max(1.0),
            (ui.available_height() - 60.0).max(80.0),
        );
        let max_x = (f64::from(dimensions.0) - f64::from(size.x) / self.zoom).max(0.0);
        let max_y = (f64::from(dimensions.1) - f64::from(size.y) / self.zoom).max(0.0);
        self.offset.0 = self.offset.0.clamp(0.0, max_x);
        self.offset.1 = self.offset.1.clamp(0.0, max_y);
        ui.add_enabled_ui(max_x > 0.0, |ui| {
            ui.add(
                egui::Slider::new(&mut self.offset.0, 0.0..=max_x).text("Horizontal position (px)"),
            );
        });
        ui.add_enabled_ui(max_y > 0.0, |ui| {
            ui.add(
                egui::Slider::new(&mut self.offset.1, 0.0..=max_y).text("Vertical position (px)"),
            );
        });
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta);
            self.offset.0 = (self.offset.0 - f64::from(scroll.x) / self.zoom).clamp(0.0, max_x);
            self.offset.1 = (self.offset.1 - f64::from(scroll.y) / self.zoom).clamp(0.0, max_y);
        }
        ImageCanvas {
            rect,
            response,
            offset: self.offset,
            zoom: self.zoom,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        pixels: &Arc<RgbaImage>,
    ) -> Result<(egui::Response, Option<PhysicalPoint>), AppError> {
        if self
            .tiled
            .as_ref()
            .is_none_or(|t| !Arc::ptr_eq(t.pixels(), pixels))
        {
            self.tiled = Some(TiledImage::new(pixels.clone())?);
        }
        let canvas = self.canvas(ui, pixels.dimensions());
        let rect = canvas.rect;
        let response = canvas.response;
        ui.painter()
            .rect_filled(rect, 0, egui::Color32::from_gray(40));
        self.tiled
            .as_mut()
            .expect("tile owner is initialized")
            .paint(ui.painter(), rect, self.offset, self.zoom)?;
        let point = response
            .interact_pointer_pos()
            .or_else(|| response.hover_pos())
            .filter(|p| rect.contains(*p))
            .and_then(|p| {
                let x = (self.offset.0 + f64::from(p.x - rect.left()) / self.zoom).floor();
                let y = (self.offset.1 + f64::from(p.y - rect.top()) / self.zoom).floor();
                (x >= 0.0
                    && y >= 0.0
                    && x < f64::from(pixels.width())
                    && y < f64::from(pixels.height()))
                .then_some(PhysicalPoint {
                    x: x as i32,
                    y: y as i32,
                })
            });
        Ok((response, point))
    }
}

/// Reusable native egui review surface. The owner receives typed finish events;
/// archive, clipboard, Pin and upload effects stay outside the image widgets.
pub struct ImageReview {
    editor: super::capture_overlay::CaptureOverlay,
    viewport: ImageViewport,
    open: bool,
}
pub enum ImageReviewEvent {
    Finish(super::image_export::ImageFinish),
    Cancel,
}
impl ImageReview {
    pub fn new(document: crate::editor::EditorDocument) -> Self {
        Self {
            editor: super::capture_overlay::CaptureOverlay::new(document),
            viewport: Default::default(),
            open: true,
        }
    }
    pub fn from_image(pixels: Arc<RgbaImage>) -> Result<Self, AppError> {
        Ok(Self::new(crate::editor::EditorDocument::from_image(
            pixels,
        )?))
    }
    pub fn into_editor(self) -> super::capture_overlay::CaptureOverlay {
        self.editor
    }
    pub fn document(&self) -> &crate::editor::EditorDocument {
        self.editor.document()
    }
    pub fn editor_mut(&mut self) -> &mut super::capture_overlay::CaptureOverlay {
        &mut self.editor
    }
    pub fn apply(&mut self, command: crate::editor::EditorCommand) -> Result<(), AppError> {
        self.editor.apply_command(command)
    }
    pub fn finish_failed(&mut self, error: AppError) {
        self.editor.finish_failed(error);
    }
    pub fn show(&mut self, ctx: &egui::Context) -> Option<ImageReviewEvent> {
        self.show_with_shortcuts(ctx, &Default::default())
    }
    pub fn show_with_shortcuts(
        &mut self,
        ctx: &egui::Context,
        saved: &crate::app::shortcuts::ShortcutSettings,
    ) -> Option<ImageReviewEvent> {
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut event = None;
        egui::Window::new("Image review")
            .id(egui::Id::unique(("image-review", self.document().id())))
            .open(&mut open)
            .default_size([900.0, 760.0])
            .show(ctx, |ui| {
                event = self
                    .editor
                    .show_review(ui, saved, &mut self.viewport)
                    .map(|event| match event {
                        super::capture_overlay::CaptureOverlayEvent::Finish(finish) => {
                            ImageReviewEvent::Finish(finish)
                        }
                        super::capture_overlay::CaptureOverlayEvent::RecordingSelected(_)
                        | super::capture_overlay::CaptureOverlayEvent::Cancel => {
                            ImageReviewEvent::Cancel
                        }
                    });
            });
        if !open || matches!(event, Some(ImageReviewEvent::Cancel)) {
            self.open = false;
            return Some(ImageReviewEvent::Cancel);
        }
        event
    }
}

#[cfg(test)]
mod editing_tests {
    use super::*;
    use crate::ui::{
        annotation_toolbar::AnnotationTool,
        capture_overlay::{CaptureOverlay, OverlayTransform},
    };
    #[test]
    fn zoomed_scrolled_review_uses_the_real_annotation_event_loop() {
        let pixels = Arc::new(RgbaImage::from_pixel(
            80,
            4000,
            image::Rgba([200, 100, 50, 255]),
        ));
        let mut editor =
            CaptureOverlay::new(crate::editor::EditorDocument::from_image(pixels.clone()).unwrap());
        editor.set_tool(AnnotationTool::Redaction).unwrap();
        let mut viewport = ImageViewport {
            zoom: 2.0,
            offset: (0.0, 3000.0),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 900.0),
            )),
            ..Default::default()
        };
        let mut canvas_rect = egui::Rect::NOTHING;
        let mut output = ctx.run_ui(raw.clone(), |ui| {
            canvas_rect = viewport.canvas(ui, (80, 4000)).rect;
        });
        output.textures_delta.clear();
        // First establish floating chrome before targeting a point below it.
        let mut output = ctx.run_ui(raw.clone(), |ui| {
            assert!(
                editor
                    .show_review(ui, &Default::default(), &mut viewport)
                    .is_none()
            );
        });
        output.textures_delta.clear();
        let transform = OverlayTransform::for_review(
            editor.document().bounds(),
            canvas_rect,
            viewport.offset,
            viewport.zoom,
        )
        .unwrap();
        let start = PhysicalPoint { x: 10, y: 3300 };
        let end = PhysicalPoint { x: 30, y: 3320 };
        for (point, pressed) in [(start, true), (end, false)] {
            let pos = transform.logical(point);
            let mut input = raw.clone();
            input.events = vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ];
            let mut output = ctx.run_ui(input, |ui| {
                assert!(
                    editor
                        .show_review(ui, &Default::default(), &mut viewport)
                        .is_none()
                );
            });
            output.textures_delta.clear();
        }
        let doc = editor.document();
        assert_eq!(doc.annotations().len(), 1);
        let crate::editor::Annotation::Redaction { bounds, .. } = doc.annotations()[0] else {
            panic!("expected completed redaction");
        };
        assert_eq!(bounds, PhysicalRect::new(10, 3300, 20, 20).unwrap());
        assert!(Arc::ptr_eq(doc.source(), &pixels));
        assert_eq!(doc.screen_location(), None);
        let exported = crate::editor::render_export(doc).unwrap();
        assert_eq!(exported.get_pixel(15, 3305).0, [255, 60, 60, 255]);
        assert_eq!(exported.get_pixel(0, 0).0, [200, 100, 50, 255]);
    }
    #[test]
    fn review_transform_preserves_large_offsets_and_checks_input() {
        let bounds = PhysicalRect::new(0, 0, 37, 200000).unwrap();
        let view = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 500.0));
        for zoom in [0.0025, 1.0, 2.0, 32.0] {
            let t = OverlayTransform::for_review(bounds, view, (0.0, 199500.0), zoom).unwrap();
            let point = PhysicalPoint { x: 7, y: 199501 };
            let center = t.logical(point) + egui::Vec2::splat((0.5 * zoom) as f32);
            assert_eq!(t.physical(center), Some(point));
        }
        assert!(OverlayTransform::for_review(bounds, view, (f64::NAN, 0.0), 1.0).is_err());
        assert!(OverlayTransform::for_review(bounds, view, (0.0, 200000.0), 1.0).is_err());
        assert!(OverlayTransform::for_review(bounds, view, (0.0, 0.0), 0.0).is_err());
    }
}
