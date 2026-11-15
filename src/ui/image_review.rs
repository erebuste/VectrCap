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
impl ImageViewport {
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
        let min_zoom = (1.0 / f64::from(pixels.width().max(pixels.height()))).max(0.000001);
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
                self.zoom = (f64::from(ui.available_width()) / f64::from(pixels.width()))
                    .min(
                        f64::from((ui.available_height() - 60.0).max(1.0))
                            / f64::from(pixels.height()),
                    )
                    .clamp(min_zoom, 32.0);
                self.offset = (0.0, 0.0);
            }
        });
        let size = Vec2::new(
            ui.available_width().max(1.0),
            (ui.available_height() - 60.0).max(80.0),
        );
        let max_x = (f64::from(pixels.width()) - f64::from(size.x) / self.zoom).max(0.0);
        let max_y = (f64::from(pixels.height()) - f64::from(size.y) / self.zoom).max(0.0);
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
    document: crate::editor::EditorDocument,
    output: super::image_export::ImageExportPane,
    open: bool,
    error: Option<AppError>,
}
pub enum ImageReviewEvent {
    Finish(super::image_export::ImageFinish),
    Cancel,
}
impl ImageReview {
    pub fn new(document: crate::editor::EditorDocument) -> Self {
        Self {
            document,
            output: Default::default(),
            open: true,
            error: None,
        }
    }
    pub fn from_image(pixels: Arc<RgbaImage>) -> Result<Self, AppError> {
        Ok(Self::new(crate::editor::EditorDocument::from_image(
            pixels,
        )?))
    }
    pub fn document(&self) -> &crate::editor::EditorDocument {
        &self.document
    }
    pub fn apply(&mut self, command: crate::editor::EditorCommand) -> Result<(), AppError> {
        self.document
            .apply(command)
            .inspect_err(|error| self.error = Some(error.clone()))?;
        self.error = None;
        Ok(())
    }
    /// A failed downstream save/copy must return the useful editable document
    /// and selected output settings to the user, not discard this review.
    pub fn finish_failed(&mut self, error: AppError) {
        self.output.set_finishing(false);
        self.error = Some(error);
    }
    pub fn show(&mut self, ctx: &egui::Context) -> Option<ImageReviewEvent> {
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut event = None;
        egui::Window::new("Image review")
            .id(egui::Id::unique(("image-review", self.document.id())))
            .open(&mut open)
            .default_size([900.0, 760.0])
            .vscroll(true)
            .show(ctx, |ui| {
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                }
                if ui.button("Cancel review").clicked() {
                    event = Some(ImageReviewEvent::Cancel);
                }
                if let Some(finish) = self.output.draw(ui, &self.document) {
                    event = Some(ImageReviewEvent::Finish(finish));
                }
            });
        if !open || matches!(event, Some(ImageReviewEvent::Cancel)) {
            self.open = false;
            return Some(ImageReviewEvent::Cancel);
        }
        event
    }
}
