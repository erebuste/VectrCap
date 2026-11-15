use image::{Rgba, RgbaImage};
use std::sync::Arc;
use vectrcap::{
    capture::geometry::PhysicalPoint,
    editor::EditorDocument,
    ui::capture_overlay::{CaptureOverlay, OverlayAction, OverlayInput},
};

fn overlay() -> CaptureOverlay {
    CaptureOverlay::new(
        EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(
            32,
            32,
            Rgba([100, 120, 140, 255]),
        )))
        .unwrap(),
    )
}
#[test]
fn text_enter_does_not_finish() {
    let mut overlay = overlay();
    overlay
        .begin_text(
            PhysicalPoint { x: 2, y: 2 },
            "Hi".into(),
            16.0,
            [0, 0, 0, 255],
        )
        .unwrap();
    assert_eq!(overlay.handle(OverlayInput::Enter).unwrap(), None);
    assert_eq!(overlay.document().annotations().len(), 1);
    assert!(!overlay.finish_visible());
    assert_eq!(
        overlay.handle(OverlayInput::Enter).unwrap(),
        Some(OverlayAction::Continue)
    );
    assert!(overlay.finish_visible());
}
#[test]
fn escape_unwinds_active_edit_first() {
    let mut overlay = overlay();
    overlay
        .begin_text(
            PhysicalPoint { x: 2, y: 2 },
            "Discard".into(),
            16.0,
            [0, 0, 0, 255],
        )
        .unwrap();
    assert_eq!(overlay.handle(OverlayInput::Escape).unwrap(), None);
    assert!(!overlay.is_cancelled());
    assert!(overlay.document().annotations().is_empty());
    assert_eq!(
        overlay.handle(OverlayInput::Escape).unwrap(),
        Some(OverlayAction::Cancel)
    );
    assert!(overlay.is_cancelled());
}
#[test]
fn cancel_has_no_disk_or_network_effect() {
    let mut overlay = overlay();
    // The session owns only memory; its typed outcome cannot execute artifact IO.
    assert_eq!(
        overlay.handle(OverlayInput::Escape).unwrap(),
        Some(OverlayAction::Cancel)
    );
    assert!(overlay.document().annotations().is_empty());
    assert!(
        overlay
            .handle(OverlayInput::Finish(OverlayAction::Upload))
            .is_err()
    );
}
