use eframe::egui::{Event, Key, Modifiers, Pos2, Rect, Vec2};
use vectrcap::{
    app::shortcuts::{KeyChord, KeyCode, ShortcutAction, ShortcutFocus, ShortcutSettings},
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint, PhysicalRect},
        request::CaptureSnapshot,
        windows::WindowCandidate,
    },
    editor::{Annotation, render_export},
    ui::{annotation_toolbar::AnnotationTool as Tool, capture_overlay::*},
};

fn point(x: i32, y: i32) -> PhysicalPoint {
    PhysicalPoint { x, y }
}
fn session() -> CaptureOverlay {
    let bounds = PhysicalRect::new(-100, -50, 300, 200).unwrap();
    let snapshot = CaptureSnapshot::new(
        [100, 120, 140, 255].repeat(300 * 200),
        bounds,
        vec![MonitorGeometry::new("fixture".into(), bounds, 1.0).unwrap()],
        vec![WindowCandidate {
            id: vectrcap::capture::windows::WindowId::new(1).unwrap(),
            bounds: PhysicalRect::new(-80, -30, 100, 80).unwrap(),
        }],
    )
    .unwrap();
    CaptureOverlay::from_snapshot(snapshot, point(0, 0), (4, 4)).unwrap()
}
fn drag(overlay: &mut CaptureOverlay, a: PhysicalPoint, b: PhysicalPoint) {
    overlay.pointer_down(a).unwrap();
    overlay.pointer_move(b, false).unwrap();
    overlay.pointer_up(b, false).unwrap();
}
fn undo(overlay: &mut CaptureOverlay) {
    overlay
        .handle_chord(
            &ShortcutSettings::default(),
            KeyChord {
                ctrl: true,
                alt: false,
                shift: false,
                win: false,
                key: KeyCode::from_vk(0x5A).unwrap(),
            },
            ShortcutFocus::Normal,
            false,
        )
        .unwrap();
}

#[test]
fn completed_pointer_gesture_is_one_command_and_escape_discards_preview() {
    let mut overlay = session();
    overlay.set_tool(Tool::Freehand).unwrap();
    overlay.pointer_down(point(-50, 0)).unwrap();
    for x in -49..20 {
        overlay.pointer_move(point(x, x / 2), false).unwrap();
    }
    assert!(overlay.document().annotations().is_empty());
    overlay.pointer_up(point(20, 10), false).unwrap();
    assert_eq!(overlay.document().annotations().len(), 1);
    assert_eq!(overlay.document().revision(), 1);
    overlay.pointer_down(point(0, 20)).unwrap();
    overlay.pointer_move(point(20, 30), false).unwrap();
    assert_eq!(overlay.handle(OverlayInput::Escape).unwrap(), None);
    assert!(!overlay.is_cancelled());
    overlay.pointer_up(point(20, 30), false).unwrap();
    assert_eq!(overlay.document().annotations().len(), 1);
    undo(&mut overlay);
    assert!(overlay.document().annotations().is_empty());
}

#[test]
fn hover_custom_crop_and_history_keep_selection_model_in_sync() {
    let mut overlay = session();
    drag(&mut overlay, point(-60, -10), point(-60, -10));
    let hover = PhysicalRect::new(-80, -30, 100, 80).unwrap();
    assert_eq!(overlay.document().crop(), Some(hover));
    overlay.clear_crop().unwrap();
    drag(&mut overlay, point(30, 30), point(100, 100));
    let custom = PhysicalRect::new(30, 30, 70, 70).unwrap();
    assert_eq!(overlay.document().crop(), Some(custom));
    drag(&mut overlay, point(60, 60), point(70, 70));
    assert_eq!(
        overlay.document().crop(),
        Some(custom.translated(10, 10).unwrap())
    );
    undo(&mut overlay);
    assert_eq!(overlay.document().crop(), Some(custom));
    drag(&mut overlay, point(60, 60), point(65, 60));
    assert_eq!(
        overlay.document().crop(),
        Some(custom.translated(5, 0).unwrap())
    );
}

#[test]
fn all_drawing_tools_use_existing_annotations_and_export_before_selection() {
    for tool in [
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Line,
        Tool::Arrow,
        Tool::Freehand,
        Tool::Step,
        Tool::Highlight,
        Tool::Spotlight,
        Tool::Redaction,
    ] {
        let mut overlay = session();
        overlay.set_tool(tool).unwrap();
        drag(&mut overlay, point(-40, -10), point(10, 30));
        assert_eq!(overlay.document().annotations().len(), 1, "{tool:?}");
        overlay.set_tool(Tool::Region).unwrap();
        drag(&mut overlay, point(30, 30), point(100, 100));
        assert_eq!(
            render_export(overlay.document()).unwrap().dimensions(),
            (70, 70)
        );
        assert_eq!(overlay.document().annotations().len(), 1);
    }
}

#[test]
fn eraser_and_movement_preserve_source_ids_and_one_step_history() {
    let mut overlay = session();
    overlay.set_tool(Tool::Redaction).unwrap();
    drag(&mut overlay, point(-40, 0), point(-10, 20));
    let first = overlay.document().annotations()[0].clone();
    overlay.set_tool(Tool::Select).unwrap();
    drag(&mut overlay, point(-30, 10), point(-20, 15));
    assert_eq!(overlay.document().annotations()[0].id(), first.id());
    assert_ne!(overlay.document().annotations()[0], first);
    undo(&mut overlay);
    assert_eq!(overlay.document().annotations()[0], first);
    overlay.set_tool(Tool::AnnotationEraser).unwrap();
    drag(&mut overlay, point(-50, 10), point(10, 10));
    assert!(overlay.document().annotations().is_empty());
    undo(&mut overlay);
    assert_eq!(overlay.document().annotations(), &[first]);
    assert_eq!(
        overlay.document().source().get_pixel(70, 60).0,
        [100, 120, 140, 255]
    );
}

#[test]
fn smart_eraser_requires_explicit_frozen_source_sample() {
    let mut overlay = session();
    overlay.set_tool(Tool::SmartEraser).unwrap();
    assert!(overlay.pointer_down(point(0, 0)).is_err());
    assert!(overlay.document().annotations().is_empty());
    overlay.sample_smart_eraser(point(-50, 0)).unwrap();
    drag(&mut overlay, point(0, 0), point(30, 10));
    assert!(matches!(
        &overlay.document().annotations()[0],
        Annotation::SmartEraseStroke {
            color: [100, 120, 140],
            ..
        }
    ));
}

#[test]
fn native_keys_honor_saved_mapping_and_physical_pixels_at_200_percent() {
    let mut overlay = session();
    let transform = OverlayTransform::new(
        overlay.document().bounds(),
        Rect::from_min_size(Pos2::ZERO, Vec2::new(150.0, 100.0)),
        2.0,
    )
    .unwrap();
    assert_eq!(transform.physical(Pos2::new(50.0, 25.0)), Some(point(0, 0)));
    let mut saved = ShortcutSettings::default();
    saved
        .bindings
        .iter_mut()
        .find(|b| b.action == ShortcutAction::ToolArrow)
        .unwrap()
        .chord = Some(KeyChord {
        ctrl: true,
        alt: false,
        shift: false,
        win: false,
        key: KeyCode::from_vk(0x4B).unwrap(),
    });
    let event = Event::Key {
        key: Key::K,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::CTRL,
    };
    assert!(
        overlay
            .handle_key_event(&event, &saved, ShortcutFocus::Normal, false)
            .unwrap()
    );
    assert_eq!(overlay.tool(), Tool::Arrow);
    assert!(
        !overlay
            .handle_key_event(&event, &saved, ShortcutFocus::TextEntry, false)
            .unwrap()
    );
    overlay.set_tool(Tool::Region).unwrap();
    drag(&mut overlay, point(30, 30), point(100, 100));
    let crop = overlay.document().crop().unwrap();
    let right = Event::Key {
        key: Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    overlay
        .handle_key_event(&right, &saved, ShortcutFocus::Normal, false)
        .unwrap();
    assert_eq!(
        overlay.document().crop(),
        Some(crop.translated(1, 0).unwrap())
    );
    let resize = Event::Key {
        key: Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::SHIFT,
    };
    overlay
        .handle_key_event(&resize, &saved, ShortcutFocus::Normal, false)
        .unwrap();
    assert_eq!(overlay.document().crop().unwrap().width(), crop.width() + 1);
}

#[test]
fn failed_gesture_retains_edit_until_escape_and_cannot_finish() {
    let mut overlay = session();
    overlay.set_tool(Tool::Line).unwrap();
    overlay.pointer_down(point(0, 0)).unwrap();
    assert!(overlay.pointer_up(point(0, 0), false).is_err());
    assert!(overlay.has_active_edit());
    assert!(
        overlay
            .handle(OverlayInput::Finish(OverlayAction::Copy))
            .is_err()
    );
    assert_eq!(overlay.handle(OverlayInput::Escape).unwrap(), None);
    assert!(!overlay.has_active_edit());
    assert!(!overlay.is_cancelled());
}

#[test]
fn real_surface_routes_pointer_keys_and_text_without_implicit_output() {
    let mut overlay = CaptureOverlay::new(
        vectrcap::editor::EditorDocument::from_image(std::sync::Arc::new(image::RgbaImage::new(
            1200, 800,
        )))
        .unwrap(),
    );
    let ctx = eframe::egui::Context::default();
    let saved = ShortcutSettings::default();
    let frame = |overlay: &mut CaptureOverlay, events: Vec<Event>| {
        let mut event = None;
        let mut output = ctx.run_ui(
            eframe::egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
                events,
                ..Default::default()
            },
            |ui| {
                event = overlay.show(ui, &saved);
            },
        );
        output.textures_delta.clear();
        event
    };
    frame(&mut overlay, vec![]);
    let key = |key| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    frame(&mut overlay, vec![key(Key::R)]);
    assert_eq!(overlay.tool(), Tool::Rectangle);
    // Image area below the floating toolbar. Native input reaches the same
    // completed-gesture boundary as the physical-coordinate fixtures above.
    let p = Pos2::new(15.0, 700.0);
    let q = Pos2::new(80.0, 750.0);
    let button = |pos, pressed| Event::PointerButton {
        pos,
        button: eframe::egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    frame(&mut overlay, vec![Event::PointerMoved(p), button(p, true)]);
    frame(&mut overlay, vec![Event::PointerMoved(q), button(q, false)]);
    assert_eq!(overlay.document().annotations().len(), 1);
    assert!(matches!(
        frame(&mut overlay, vec![key(Key::Escape)]),
        Some(CaptureOverlayEvent::Cancel)
    ));
    assert!(overlay.is_cancelled());
}

#[test]
fn native_tab_respects_chrome_focus_and_enter_commits_updated_unicode_draft() {
    let mut overlay = CaptureOverlay::new(
        vectrcap::editor::EditorDocument::from_image(std::sync::Arc::new(image::RgbaImage::new(
            1200, 800,
        )))
        .unwrap(),
    );
    let ctx = eframe::egui::Context::default();
    let saved = ShortcutSettings::default();
    let tab = Event::Key {
        key: Key::Tab,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    overlay.set_tool(Tool::Arrow).unwrap();
    assert!(
        !overlay
            .handle_key_event(&tab, &saved, ShortcutFocus::Normal, true)
            .unwrap()
    );
    assert_eq!(overlay.tool(), Tool::Arrow);
    assert!(
        overlay
            .handle_key_event(&tab, &saved, ShortcutFocus::Normal, false)
            .unwrap()
    );
    assert_eq!(overlay.tool(), Tool::Region);
    overlay
        .begin_text(point(10, 400), "Café".into(), 24.0, [0, 0, 0, 255])
        .unwrap();
    let mut draw = |events| {
        let mut event = None;
        let mut output = ctx.run_ui(
            eframe::egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
                events,
                ..Default::default()
            },
            |ui| {
                event = overlay.show(ui, &saved);
            },
        );
        output.textures_delta.clear();
        assert!(event.is_none());
    };
    draw(vec![]);
    draw(vec![Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    assert!(!overlay.finish_visible());
    assert!(!overlay.has_active_edit());
    assert!(
        matches!(&overlay.document().annotations()[0],Annotation::Text {content,..} if &**content=="Café")
    );
}

#[test]
fn oversize_stroke_is_rejected_without_partial_history_or_losing_edit() {
    let mut overlay = session();
    overlay.set_tool(Tool::Freehand).unwrap();
    overlay.pointer_down(point(0, 0)).unwrap();
    for n in 1..20_000 {
        overlay.pointer_move(point(n % 2, 0), false).unwrap();
    }
    assert!(overlay.pointer_move(point(2, 0), false).is_err());
    assert!(overlay.pointer_up(point(3, 0), false).is_err());
    assert!(overlay.document().annotations().is_empty());
    assert!(overlay.error().is_some());
    assert!(overlay.has_active_edit());
    overlay.handle(OverlayInput::Escape).unwrap();
    assert!(!overlay.has_active_edit());
    assert!(!overlay.is_cancelled());
    assert_eq!(overlay.document().revision(), 0);
}

#[test]
fn translated_annotations_check_all_coordinates_and_preserve_original_on_failure() {
    let a = Annotation::text(point(i32::MAX, 0), "safe".into(), 24.0, [0, 0, 0, 255]).unwrap();
    assert!(a.translated(1, 0).is_err());
    assert!(matches!(
        a,
        Annotation::Text {
            position: PhysicalPoint { x: i32::MAX, y: 0 },
            ..
        }
    ));
    let mut overlay = session();
    overlay.set_tool(Tool::Step).unwrap();
    drag(&mut overlay, point(0, 0), point(0, 0));
    let original = overlay.document().annotations()[0].clone();
    let moved = original.translated(2, 3).unwrap();
    assert_eq!(moved.id(), original.id());
    assert!(matches!(moved, Annotation::Step { number: 1, .. }));
}

#[test]
fn native_overlay_does_not_clamp_a_virtual_desktop_to_one_monitor() {
    let overlay = session();
    let builder = overlay.viewport_builder(2.0).unwrap();
    assert_eq!(builder.clamp_size_to_monitor_size, Some(false));
    assert_eq!(builder.inner_size, Some(Vec2::new(150.0, 100.0)));
    assert_eq!(builder.position, Some(Pos2::new(-50.0, -25.0)));
    assert!(overlay.viewport_builder(f32::NAN).is_err());
}

#[test]
fn canvas_tab_keeps_subsequent_tool_shortcuts_in_the_canvas_context() {
    let mut overlay = session();
    let ctx = eframe::egui::Context::default();
    let saved = ShortcutSettings::default();
    for key in [None, Some(Key::R), Some(Key::Tab), Some(Key::F)] {
        let events = key
            .map(|key| Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            })
            .into_iter()
            .collect();
        let mut output = ctx.run_ui(
            eframe::egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
                events,
                ..Default::default()
            },
            |ui| {
                assert!(overlay.show(ui, &saved).is_none());
            },
        );
        output.textures_delta.clear();
    }
    assert_eq!(overlay.tool(), Tool::Freehand);
}
