use eframe::egui::{Pos2, Rect as UiRect, Vec2};
use image::{Rgba, RgbaImage};
use std::sync::Arc;
use vectrcap::{
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint as Point, PhysicalRect as Rect},
        preferences::{LensShape, RegionCapturePreferences},
        request::CaptureSnapshot,
        selection::{InputContext, SelectionInput, SelectionModel, SelectionOutcome},
    },
    editor::{Annotation, EditorCommand, EditorDocument, render_export},
    ui::magnifier::{CaptureReadout, MagnifierLayout},
};

fn document() -> EditorDocument {
    EditorDocument::from_image(Arc::new(RgbaImage::from_fn(40, 40, |x, y| {
        Rgba([x as u8, y as u8, 171, 255])
    })))
    .unwrap()
}

#[test]
fn capture_readout_ignores_dim_and_ui_chrome() {
    let mut doc = document();
    let bounds = Rect::new(0, 0, 40, 40).unwrap();
    doc.apply(EditorCommand::Insert(
        Annotation::redaction(bounds, [0, 0, 0]).unwrap(),
    ))
    .unwrap();
    let prefs = RegionCapturePreferences {
        dim_percent: 80,
        show_rgb: true,
        show_hex: true,
        ..Default::default()
    };
    let readout = CaptureReadout::sample(&doc, Point { x: 20, y: 20 }, &prefs).unwrap();
    assert_eq!(readout.center(), Some([20, 20, 171, 255]));
    assert_eq!(
        readout.lines(),
        [
            "X: 20  Y: 20",
            "Width: 40  Height: 40",
            "RGB: 20, 20, 171",
            "HEX: #1414AB"
        ]
    );
    assert_eq!(
        render_export(&doc).unwrap().get_pixel(20, 20).0,
        [0, 0, 0, 255]
    );
}

#[test]
fn rgb_hex_flags_are_read_only_and_default_off() {
    let doc = document();
    let prefs = RegionCapturePreferences::default();
    let readout = CaptureReadout::sample(&doc, Point { x: 5, y: 6 }, &prefs).unwrap();
    assert_eq!(readout.lines(), ["X: 5  Y: 6", "Width: 40  Height: 40"]);
    let prefs = RegionCapturePreferences {
        show_rgb: true,
        show_hex: true,
        show_coordinates: false,
        show_dimensions: false,
        ..prefs
    };
    assert_eq!(
        CaptureReadout::sample(&doc, Point { x: -1, y: 6 }, &prefs)
            .unwrap()
            .lines(),
        ["RGB: Unavailable", "HEX: Unavailable"]
    );
    assert_eq!(doc.revision(), 0);
    assert!(doc.annotations().is_empty());
}

#[test]
fn circle_square_lenses_keep_exact_center_pixel() {
    let doc = document();
    for shape in [LensShape::Circle, LensShape::Square] {
        let prefs = RegionCapturePreferences {
            lens_shape: shape,
            ..Default::default()
        };
        let readout = CaptureReadout::sample(&doc, Point { x: 20, y: 21 }, &prefs).unwrap();
        assert_eq!(readout.grid_size(), 15);
        assert_eq!(readout.pixel(7, 7), Some([20, 21, 171, 255]));
        assert_eq!(readout.pixel(6, 7), Some([19, 21, 171, 255]));
        assert_eq!(readout.pixel(7, 8), Some([20, 22, 171, 255]));
        assert_eq!(readout.pixel(15, 0), None);
    }
}

#[test]
fn crosshair_options_never_enter_export() {
    let doc = document();
    let before = render_export(&doc).unwrap();
    let prefs = RegionCapturePreferences {
        center_crosshair: true,
        screen_crosshair: true,
        ..Default::default()
    };
    let mut readout = CaptureReadout::sample(&doc, Point { x: 1, y: 1 }, &prefs).unwrap();
    let ctx = eframe::egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        let viewport = UiRect::from_min_size(Pos2::ZERO, Vec2::splat(500.0));
        readout
            .paint(ui.ctx(), Pos2::new(1.0, 1.0), viewport, &prefs)
            .unwrap();
    });
    output.textures_delta.clear();
    assert_eq!(render_export(&doc).unwrap(), before);
    assert_eq!(doc.revision(), 0);
}

#[test]
fn source_gaps_and_negative_origins_remain_unavailable() {
    let bounds = Rect::new(-30, -10, 40, 20).unwrap();
    let snapshot = CaptureSnapshot::new(
        vec![220; 40 * 20 * 4],
        bounds,
        vec![
            MonitorGeometry::new("left".into(), Rect::new(-30, -10, 10, 20).unwrap(), 2.0).unwrap(),
            MonitorGeometry::new("right".into(), Rect::new(0, -10, 10, 20).unwrap(), 1.0).unwrap(),
        ],
        vec![],
    )
    .unwrap();
    let doc = EditorDocument::new(snapshot).unwrap();
    let prefs = RegionCapturePreferences {
        show_rgb: true,
        ..Default::default()
    };
    assert_eq!(
        CaptureReadout::sample(&doc, Point { x: -25, y: -5 }, &prefs)
            .unwrap()
            .center(),
        Some([220; 4])
    );
    assert_eq!(
        CaptureReadout::sample(&doc, Point { x: -15, y: -5 }, &prefs)
            .unwrap()
            .center(),
        None
    );
}

#[test]
fn click_with_color_info_still_selects_or_draws() {
    let bounds = Rect::new(0, 0, 40, 40).unwrap();
    let snapshot = CaptureSnapshot::new(
        vec![255; 40 * 40 * 4],
        bounds,
        vec![MonitorGeometry::new("monitor".into(), bounds, 1.0).unwrap()],
        vec![],
    )
    .unwrap();
    let prefs = RegionCapturePreferences {
        show_rgb: true,
        show_hex: true,
        ..Default::default()
    };
    let mut selection =
        SelectionModel::new(&snapshot, prefs.clone(), Point { x: 1, y: 1 }, (4, 4)).unwrap();
    let doc = EditorDocument::new(snapshot).unwrap();
    for context in [InputContext::Drawing, InputContext::Region] {
        selection.handle(SelectionInput::PrimaryDown {
            point: Point { x: 2, y: 2 },
            context,
        });
        CaptureReadout::sample(&doc, Point { x: 20, y: 20 }, &prefs).unwrap();
        selection.handle(SelectionInput::PointerMove(Point { x: 20, y: 20 }));
        let result = selection.handle(SelectionInput::PrimaryUp(Point { x: 20, y: 20 }));
        if context == InputContext::Drawing {
            assert_eq!(result, SelectionOutcome::Unchanged);
        } else {
            assert_eq!(
                result,
                SelectionOutcome::Selected(Rect::new(2, 2, 18, 18).unwrap())
            );
        }
    }
    assert!(doc.annotations().is_empty());
}

#[test]
fn magnifier_fits_screen_without_changing_sample() {
    let doc = document();
    let prefs = RegionCapturePreferences::default();
    let readout = CaptureReadout::sample(&doc, Point { x: 0, y: 0 }, &prefs).unwrap();
    assert_eq!(readout.pixel(7, 7), Some([0, 0, 171, 255]));
    assert_eq!(readout.pixel(6, 7), None);
    let viewport = UiRect::from_min_size(Pos2::new(-120.0, 30.0), Vec2::new(110.0, 90.0));
    let layout = MagnifierLayout::fit(Pos2::new(-11.0, 119.0), viewport, 15, 20).unwrap();
    assert!(viewport.contains_rect(layout.rect));
    assert!(layout.pixel_scale > 0.0 && layout.pixel_scale <= 6.0);
    assert_eq!(readout.center(), Some([0, 0, 171, 255]));
    assert!(MagnifierLayout::fit(Pos2::ZERO, viewport, 14, 20).is_err());
    assert!(MagnifierLayout::fit(Pos2::ZERO, viewport, 15, 0).is_err());
}
