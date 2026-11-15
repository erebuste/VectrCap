use image::{Rgba, RgbaImage};
use std::sync::Arc;
use vectrcap::{
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint, PhysicalRect},
        request::CaptureSnapshot,
    },
    editor::{Annotation, EditorCommand, EditorDocument, Style, render_export},
};

fn rect(x: i32, y: i32, w: u32, h: u32) -> PhysicalRect {
    PhysicalRect::new(x, y, w, h).unwrap()
}
fn document() -> EditorDocument {
    let bounds = rect(-20, -10, 100, 80);
    let pixels = RgbaImage::from_fn(100, 80, |x, y| Rgba([x as u8, y as u8, 100, 255]));
    EditorDocument::new(
        CaptureSnapshot::new(
            pixels.into_raw(),
            bounds,
            vec![MonitorGeometry::new("fixture".into(), bounds, 1.0).unwrap()],
            vec![],
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn image_local_annotations_export_at_correct_offsets() {
    let mut doc = EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(
        80,
        400,
        Rgba([200, 150, 100, 255]),
    )))
    .unwrap();
    doc.apply(EditorCommand::Insert(
        Annotation::spotlight(rect(8, 290, 25, 40), 0.6).unwrap(),
    ))
    .unwrap();
    doc.apply(EditorCommand::Insert(
        Annotation::highlight(rect(10, 302, 10, 10), [255, 255, 0], 0.35).unwrap(),
    ))
    .unwrap();
    doc.apply(EditorCommand::Insert(
        Annotation::redaction(rect(15, 310, 7, 9), [4, 5, 6]).unwrap(),
    ))
    .unwrap();
    doc.apply(EditorCommand::Crop(Some(rect(10, 300, 30, 40))))
        .unwrap();
    assert_eq!(doc.screen_location(), None);
    let full = vectrcap::editor::render::render_preview(&doc).unwrap();
    let exported = render_export(&doc).unwrap();
    assert_eq!(exported.dimensions(), (30, 40));
    assert_eq!(exported.get_pixel(5, 10).0, [4, 5, 6, 255]);
    for y in 0..40 {
        for x in 0..30 {
            assert_eq!(full.get_pixel(x + 10, y + 300), exported.get_pixel(x, y));
        }
    }
    assert_eq!(full.get_pixel(0, 0).0, [80, 60, 40, 255]);
    assert_eq!(full.get_pixel(9, 299).0, [200, 150, 100, 255]);
}

#[test]
fn long_image_review_uses_bounded_tiles() {
    use eframe::egui::{Context, Pos2, Rect as UiRect, Vec2};
    use vectrcap::ui::image_review::TiledImage;
    let source = Arc::new(RgbaImage::from_pixel(37, 200_000, Rgba([20, 40, 60, 255])));
    let mut tiled = TiledImage::new(source).unwrap();
    let ctx = Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        tiled
            .paint(
                ui.painter(),
                UiRect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 500.0)),
                (0.0, 0.0),
                0.0025,
            )
            .unwrap();
    });
    let sizes: Vec<_> = output
        .textures_delta
        .set
        .iter()
        .filter(|(id, _)| **id != eframe::egui::TextureId::Managed(0))
        .flat_map(|(_, deltas)| deltas.iter().map(|d| d.image.size()))
        .collect();
    output.textures_delta.clear();
    assert!(tiled.resident_tile_count() > 0 && tiled.resident_tile_count() <= 16);
    assert!(!sizes.is_empty());
    assert!(
        sizes.iter().all(|s| s.iter().all(|&axis| axis <= 1024)),
        "oversized tile {sizes:?}"
    );
    let mut output = ctx.run_ui(Default::default(), |ui| {
        tiled
            .paint(
                ui.painter(),
                UiRect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 500.0)),
                (0.0, 199_500.0),
                1.0,
            )
            .unwrap();
    });
    let sizes: Vec<_> = output
        .textures_delta
        .set
        .iter()
        .filter(|(id, _)| **id != eframe::egui::TextureId::Managed(0))
        .flat_map(|(_, deltas)| deltas.iter().map(|d| d.image.size()))
        .collect();
    output.textures_delta.clear();
    assert!(tiled.resident_tile_count() > 0 && tiled.resident_tile_count() <= 16);
    assert!(!sizes.is_empty());
    assert!(sizes.iter().all(|s| s.iter().all(|&axis| axis <= 1024)));
}
fn redaction() -> Annotation {
    Annotation::redaction(rect(0, 0, 12, 12), [7, 8, 9]).unwrap()
}

#[test]
fn annotate_before_select_equals_select_before_annotate() {
    let mut a = document();
    let mut b = document();
    assert!(a.crop().is_none());
    let mark = Annotation::rectangle(
        rect(-5, -5, 40, 30),
        Style::new([200, 0, 0, 255], 2.0, Some([0, 150, 0, 255])).unwrap(),
    )
    .unwrap();
    a.apply(EditorCommand::Insert(mark.clone())).unwrap();
    a.apply(EditorCommand::Crop(Some(rect(0, 0, 40, 30))))
        .unwrap();
    b.apply(EditorCommand::Crop(Some(rect(0, 0, 40, 30))))
        .unwrap();
    b.apply(EditorCommand::Insert(mark)).unwrap();
    assert_eq!(render_export(&a).unwrap(), render_export(&b).unwrap());
}
#[test]
fn crop_clips_without_moving_annotations() {
    let mut d = document();
    let mark = redaction();
    let id = mark.id();
    d.apply(EditorCommand::Insert(mark)).unwrap();
    d.apply(EditorCommand::Crop(Some(rect(5, 5, 25, 20))))
        .unwrap();
    let out = render_export(&d).unwrap();
    assert_eq!(out.dimensions(), (25, 20));
    assert_eq!(out.get_pixel(0, 0).0, [7, 8, 9, 255]);
    assert_eq!(out.get_pixel(7, 7).0, [32, 22, 100, 255]);
    assert_eq!(d.annotations()[0].id(), id);
    d.apply(EditorCommand::Crop(Some(rect(-10, -10, 40, 30))))
        .unwrap();
    assert_eq!(
        render_export(&d).unwrap().get_pixel(10, 10).0,
        [7, 8, 9, 255]
    );
}
#[test]
fn redaction_replaces_every_underlying_pixel() {
    let mut d = EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(
        20,
        20,
        Rgba([210, 150, 70, 0]),
    )))
    .unwrap();
    d.apply(EditorCommand::Insert(redaction())).unwrap();
    let out = render_export(&d).unwrap();
    for y in 0..12 {
        for x in 0..12 {
            assert_eq!(out.get_pixel(x, y).0, [7, 8, 9, 255]);
        }
    }
}
#[test]
fn unicode_text_and_freehand_export() {
    let mut d =
        EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(200, 80, Rgba([255; 4]))))
            .unwrap();
    d.apply(EditorCommand::Insert(
        Annotation::text(
            PhysicalPoint { x: 4, y: 3 },
            "HéλЖ!".into(),
            24.0,
            [0, 0, 0, 255],
        )
        .unwrap(),
    ))
    .unwrap();
    d.apply(EditorCommand::Insert(
        Annotation::freehand(
            vec![
                PhysicalPoint { x: 5, y: 60 },
                PhysicalPoint { x: 50, y: 60 },
            ]
            .into(),
            [250, 0, 0, 255],
            4.0,
        )
        .unwrap(),
    ))
    .unwrap();
    let out = render_export(&d).unwrap();
    assert!(out.enumerate_pixels().any(|(_, y, p)| y < 40 && p[0] < 100));
    assert_eq!(out.get_pixel(25, 60).0, [250, 0, 0, 255]);
    // Unsupported scalars must fail visibly instead of silently exporting empty glyphs.
    let missing = Annotation::text(
        PhysicalPoint { x: 0, y: 0 },
        "\u{10ffff}".into(),
        20.0,
        [0, 0, 0, 255],
    );
    assert!(missing.is_err());
}
#[test]
fn undo_redo_restores_document() {
    let mut d = document();
    d.apply(EditorCommand::Crop(Some(rect(0, 0, 20, 20))))
        .unwrap();
    let before = render_export(&d).unwrap();
    d.apply(EditorCommand::Insert(redaction())).unwrap();
    let after = render_export(&d).unwrap();
    let revision = d.revision();
    assert_ne!(before, after);
    assert!(d.undo().unwrap());
    assert_eq!(render_export(&d).unwrap(), before);
    assert!(d.revision() > revision);
    assert!(d.redo().unwrap());
    assert_eq!(render_export(&d).unwrap(), after);
    let mark = d.annotations()[0].clone();
    d.apply(EditorCommand::Erase(vec![mark.id()])).unwrap();
    assert!(d.annotations().is_empty());
    d.undo().unwrap();
    assert_eq!(d.annotations()[0].id(), mark.id());
}
#[test]
fn toolbar_pixels_are_absent() {
    let mut d = document();
    d.apply(EditorCommand::Crop(Some(rect(-20, -10, 100, 80))))
        .unwrap();
    let overlay = vectrcap::ui::capture_overlay::CaptureOverlay::new(d);
    assert_eq!(
        render_export(overlay.document()).unwrap().get_pixel(0, 0).0,
        [0, 0, 100, 255]
    );
    assert!(
        render_export(overlay.document())
            .unwrap()
            .pixels()
            .all(|p| *p != Rgba([255, 0, 255, 255]))
    );
}
