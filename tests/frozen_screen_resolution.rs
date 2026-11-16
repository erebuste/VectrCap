use vectrcap::{
    capture::geometry::PhysicalRect,
    ui::image_review::{MAX_TILE_SIDE, MAX_VISIBLE_TILES, tile_plan},
};

#[test]
fn desktop_and_multi_monitor_backgrounds_keep_every_source_pixel() {
    for (width, height) in [
        (1920, 1080),
        (3840, 2160),
        (7680, 2160),
        (11520, 2160),
        (7680, 4320),
    ] {
        let tiles = tile_plan(
            width,
            height,
            PhysicalRect::new(0, 0, width, height).unwrap(),
        )
        .unwrap();
        assert!(
            tiles.iter().all(|tile| tile.stride() == 1),
            "{width}x{height} background was downsampled"
        );
        assert_eq!(
            tiles
                .iter()
                .map(|t| u64::from(t.bounds().width()) * u64::from(t.bounds().height()))
                .sum::<u64>(),
            u64::from(width) * u64::from(height)
        );
        assert!(tiles.len() <= MAX_VISIBLE_TILES);
        assert!(tiles.iter().all(|t| {
            t.texture_size()
                .iter()
                .all(|&side| side <= MAX_TILE_SIDE as usize)
        }));
    }
}

#[test]
fn off_grid_visible_desktop_keeps_native_resolution() {
    let tiles = tile_plan(
        7680,
        4320,
        PhysicalRect::new(1000, 1000, 3840, 2160).unwrap(),
    )
    .unwrap();
    assert!(tiles.iter().all(|t| t.stride() == 1));
    assert!(
        tiles
            .iter()
            .any(|t| t.bounds().left() == 0 && t.bounds().top() == 0)
    );
}

#[test]
fn extremely_tall_review_still_has_a_bounded_texture_plan() {
    let tiles = tile_plan(
        64,
        1_000_000,
        PhysicalRect::new(0, 0, 64, 1_000_000).unwrap(),
    )
    .unwrap();
    assert!(tiles.len() <= MAX_VISIBLE_TILES);
    assert!(tiles.iter().any(|t| t.stride() > 1));
    assert!(tiles.iter().all(|t| {
        t.texture_size()
            .iter()
            .all(|&side| side <= MAX_TILE_SIDE as usize)
    }));
}

#[test]
fn frozen_background_uploads_native_pixels_at_each_display_scale() {
    use eframe::egui::{self, Pos2, Rect, Vec2};
    use std::sync::Arc;
    use vectrcap::ui::image_review::TiledImage;
    let pixels = Arc::new(image::RgbaImage::from_fn(1920, 1080, |x, y| {
        if (x + y) % 2 == 0 {
            image::Rgba([0, 0, 0, 255])
        } else {
            image::Rgba([255, 255, 255, 255])
        }
    }));
    for scale in [1.0, 1.25, 2.0] {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        let mut tiled = TiledImage::new(pixels.clone()).unwrap();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let actual_scale = ui.ctx().pixels_per_point();
            tiled
                .paint(
                    ui.painter(),
                    Rect::from_min_size(
                        Pos2::ZERO,
                        Vec2::new(1920.0 / actual_scale, 1080.0 / actual_scale),
                    ),
                    (0.0, 0.0),
                    1.0 / f64::from(actual_scale),
                )
                .unwrap();
        });
        let uploaded: usize = output
            .textures_delta
            .set
            .iter()
            .filter(|(id, _)| **id != egui::TextureId::Managed(0))
            .flat_map(|(_, deltas)| deltas.iter())
            .map(|d| d.image.size().into_iter().product::<usize>())
            .sum();
        output.textures_delta.clear();
        assert_eq!(
            uploaded,
            1920 * 1080,
            "display scale {scale} lost source pixels"
        );
    }
}
