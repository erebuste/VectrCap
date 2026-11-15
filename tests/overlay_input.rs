use image::{Rgba, RgbaImage};
use std::sync::Arc;
use vectrcap::{
    app::shortcuts::{KeyChord, KeyCode, ShortcutAction, ShortcutFocus, ShortcutSettings},
    capture::geometry::PhysicalPoint,
    editor::EditorDocument,
    ui::annotation_toolbar::AnnotationTool,
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

fn chord(ctrl: bool, vk: u32) -> KeyChord {
    KeyChord {
        ctrl,
        alt: false,
        shift: false,
        win: false,
        key: KeyCode::from_vk(vk).unwrap(),
    }
}

#[test]
fn remapped_tool_uses_saved_chord() {
    let mut saved = ShortcutSettings::default();
    let mut bindings = saved.effective();
    let arrow = bindings
        .iter_mut()
        .find(|b| b.action == ShortcutAction::ToolArrow)
        .unwrap();
    arrow.chord = Some(chord(true, 0x4B));
    arrow.enabled = true;
    saved.bindings = bindings;
    saved.validate().unwrap();
    let saved: ShortcutSettings =
        serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    let mut overlay = overlay();
    assert!(
        !overlay
            .handle_chord(&saved, chord(false, 0x41), ShortcutFocus::Normal, false)
            .unwrap()
    );
    assert!(
        overlay
            .handle_chord(&saved, chord(true, 0x4B), ShortcutFocus::Normal, false)
            .unwrap()
    );
    assert_eq!(overlay.tool(), AnnotationTool::Arrow);
    assert!(
        !overlay
            .handle_chord(&saved, chord(false, 0x52), ShortcutFocus::Normal, true)
            .unwrap()
    );
    assert_eq!(overlay.tool(), AnnotationTool::Arrow);
}

#[test]
fn cleared_tool_shortcut_does_not_dispatch() {
    let mut saved = ShortcutSettings::default();
    saved.bindings = saved.effective();
    for action in [ShortcutAction::ToolArrow, ShortcutAction::ToolRegion] {
        let binding = saved
            .bindings
            .iter_mut()
            .find(|b| b.action == action)
            .unwrap();
        binding.chord = None;
        binding.enabled = false;
    }
    let saved: ShortcutSettings =
        serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    let mut overlay = overlay();
    assert!(
        overlay
            .handle_chord(&saved, chord(false, 0x52), ShortcutFocus::Normal, false)
            .unwrap()
    );
    for key in [0x41, 0x09] {
        assert!(
            !overlay
                .handle_chord(&saved, chord(false, key), ShortcutFocus::Normal, false)
                .unwrap()
        );
        assert_eq!(overlay.tool(), AnnotationTool::Rectangle);
    }
}

#[test]
fn tool_remap_never_steals_text_input() {
    let saved = ShortcutSettings::default();
    let mut overlay = overlay();
    for focus in [ShortcutFocus::TextEntry, ShortcutFocus::ShortcutEntry] {
        assert!(
            !overlay
                .handle_chord(&saved, chord(false, 0x52), focus, false)
                .unwrap()
        );
    }
    overlay
        .begin_text(
            PhysicalPoint { x: 2, y: 2 },
            "Typing".into(),
            16.0,
            [0, 0, 0, 255],
        )
        .unwrap();
    assert!(
        !overlay
            .handle_chord(&saved, chord(false, 0x52), ShortcutFocus::Normal, false)
            .unwrap()
    );
    overlay.handle(OverlayInput::Enter).unwrap();
    assert!(
        !overlay
            .handle_chord(&saved, chord(true, 0x5A), ShortcutFocus::TextEntry, false)
            .unwrap()
    );
    assert_eq!(overlay.document().annotations().len(), 1);
    assert!(
        overlay
            .handle_chord(&saved, chord(true, 0x5A), ShortcutFocus::Normal, false)
            .unwrap()
    );
    assert!(overlay.document().annotations().is_empty());
    assert!(
        overlay
            .handle_chord(&saved, chord(true, 0x59), ShortcutFocus::Normal, false)
            .unwrap()
    );
    assert_eq!(overlay.document().annotations().len(), 1);
}

#[test]
fn region_default_migrates_without_overriding_existing_assignments() {
    let mut old = ShortcutSettings { bindings: vec![] };
    let region = old
        .effective()
        .into_iter()
        .find(|b| b.action == ShortcutAction::ToolRegion)
        .unwrap();
    assert!(region.enabled);
    assert_eq!(region.chord, Some(chord(false, 0x09)));
    let mut custom =
        vectrcap::app::shortcuts::ShortcutBinding::unassigned(ShortcutAction::ToolArrow);
    custom.chord = Some(chord(false, 0x09));
    custom.enabled = true;
    old.bindings.push(custom);
    let mut overlay = overlay();
    assert!(
        overlay
            .handle_chord(&old, chord(false, 0x09), ShortcutFocus::Normal, false)
            .unwrap()
    );
    assert_eq!(overlay.tool(), AnnotationTool::Arrow);
    let region = old
        .effective()
        .into_iter()
        .find(|b| b.action == ShortcutAction::ToolRegion)
        .unwrap();
    assert!(!region.enabled);
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
