use image::RgbaImage;
use std::sync::Arc;
use vectrcap::{
    capture::geometry::{PhysicalPoint, PhysicalRect},
    pin::{PinEffect, PinInput, PinManager, PinnedImage},
};

fn image(w: u32, h: u32) -> PinnedImage {
    PinnedImage::new(Arc::new(RgbaImage::new(w, h)), None).unwrap()
}
fn manager() -> PinManager {
    let mut manager = PinManager::default();
    manager
        .update_work_areas(
            vec![PhysicalRect::new(-100, -100, 1920, 1080).unwrap()],
            PhysicalPoint { x: 0, y: 0 },
        )
        .unwrap();
    manager
}
#[test]
fn pins_keep_independent_display_state() {
    let mut m = manager();
    let a = m
        .open(image(200, 100), Some(PhysicalPoint { x: 20, y: 30 }))
        .unwrap();
    let b = m.open(image(100, 200), None).unwrap();
    m.handle(a, PinInput::Zoom(1)).unwrap();
    m.handle(a, PinInput::Opacity(-1)).unwrap();
    m.handle(a, PinInput::Move { dx: 1, dy: -10 }).unwrap();
    let s = m.get(a).unwrap();
    assert_eq!(
        (s.state.zoom_percent(), s.state.opacity_percent()),
        (110, 95)
    );
    assert_eq!(s.state.position(), PhysicalPoint { x: 21, y: 20 });
    assert_eq!(
        (
            m.get(b).unwrap().state.zoom_percent(),
            m.get(b).unwrap().state.opacity_percent()
        ),
        (100, 100)
    );
    m.handle(a, PinInput::Reset).unwrap();
    assert_eq!(
        (
            m.get(a).unwrap().state.zoom_percent(),
            m.get(a).unwrap().state.opacity_percent()
        ),
        (100, 100)
    );
}
#[test]
fn zoom_and_opacity_clamp_to_spec_bounds() {
    let mut m = manager();
    let id = m.open(image(40, 20), None).unwrap();
    for _ in 0..200 {
        m.handle(id, PinInput::Zoom(-1)).unwrap();
        m.handle(id, PinInput::Opacity(-1)).unwrap();
    }
    assert_eq!(
        (
            m.get(id).unwrap().state.zoom_percent(),
            m.get(id).unwrap().state.opacity_percent()
        ),
        (20, 10)
    );
    for _ in 0..200 {
        m.handle(id, PinInput::Zoom(1)).unwrap();
        m.handle(id, PinInput::Opacity(1)).unwrap();
    }
    assert_eq!(
        (
            m.get(id).unwrap().state.zoom_percent(),
            m.get(id).unwrap().state.opacity_percent()
        ),
        (500, 100)
    );
    assert_eq!(m.get(id).unwrap().display_size(), (200, 100));
}
#[test]
fn copy_uses_full_resolution_final_pixels() {
    let mut m = manager();
    let source = image(320, 160);
    let pixels = source.pixels().clone();
    let id = m.open(source, None).unwrap();
    m.handle(id, PinInput::Zoom(20)).unwrap();
    m.handle(id, PinInput::Opacity(-10)).unwrap();
    m.handle(id, PinInput::ToggleMinimized).unwrap();
    let effects = m.handle(id, PinInput::Copy).unwrap();
    let [PinEffect::CopyImage(actual)] = effects.as_slice() else {
        panic!("only clipboard effect expected")
    };
    assert!(Arc::ptr_eq(actual, &pixels));
}
#[test]
fn close_never_deletes_or_uploads() {
    let mut m = manager();
    let source = image(20, 10);
    let pixels = source.pixels().clone();
    let id = m.open(source, None).unwrap();
    assert!(
        matches!(m.handle(id, PinInput::Close).unwrap().as_slice(), [PinEffect::CloseViewport(closed)] if *closed == id)
    );
    assert!(m.get(id).is_none());
    assert_eq!(Arc::strong_count(&pixels), 1);
    assert!(m.close_all().is_empty());
}
#[test]
fn preview_restores_expanded_size() {
    let mut m = manager();
    let id = m.open(image(400, 200), None).unwrap();
    m.handle(id, PinInput::Zoom(5)).unwrap();
    assert_eq!(m.get(id).unwrap().display_size(), (600, 300));
    m.handle(id, PinInput::ToggleMinimized).unwrap();
    assert_eq!(m.get(id).unwrap().display_size(), (96, 48));
    m.handle(id, PinInput::ToggleMinimized).unwrap();
    assert_eq!(m.get(id).unwrap().display_size(), (600, 300));
}
#[test]
fn monitor_removal_repositions_pin() {
    let mut m = manager();
    let id = m
        .open(image(200, 100), Some(PhysicalPoint { x: 1000, y: 500 }))
        .unwrap();
    let remaining = PhysicalRect::new(-1200, 0, 1000, 800).unwrap();
    m.update_work_areas(vec![remaining], PhysicalPoint { x: -500, y: 100 })
        .unwrap();
    assert!(remaining.contains(m.get(id).unwrap().state.position()));
    assert_eq!(m.get(id).unwrap().image.pixels().dimensions(), (200, 100));
    assert!(
        m.update_work_areas(vec![], PhysicalPoint { x: 0, y: 0 })
            .is_err()
    );
}
#[test]
fn pin_limits_refuse_without_evicting() {
    let mut m = manager();
    let ids: Vec<_> = (0..16)
        .map(|_| m.open(image(1, 1), None).unwrap())
        .collect();
    assert!(m.open(image(1, 1), None).is_err());
    assert!(ids.iter().all(|id| m.get(*id).is_some()));
    assert_eq!(m.close_all().len(), 16);
    // Reuse immutable sources to exercise the exact aggregate boundary without
    // allocating three separate 256-MB buffers.
    let big = image(8000, 8000);
    let a = m.open(big.clone(), None).unwrap();
    let b = m.open(big.clone(), None).unwrap();
    assert!(m.open(image(1, 1), None).is_err());
    m.handle(a, PinInput::Close).unwrap();
    assert!(m.get(b).is_some());
    assert!(m.open(big, None).is_ok());
    assert!(PinnedImage::new(Arc::new(RgbaImage::new(0, 1)), None).is_err());
}

#[test]
fn pin_viewport_is_borderless_and_uses_physical_dpi_geometry() {
    let mut m = manager();
    let id = m
        .open(image(400, 200), Some(PhysicalPoint { x: 40, y: 60 }))
        .unwrap();
    let builder = vectrcap::ui::pin_window::viewport_builder(m.get(id).unwrap(), 2.0).unwrap();
    assert_eq!(builder.decorations, Some(false));
    assert_eq!(builder.resizable, Some(false));
    assert_eq!(builder.inner_size, Some(eframe::egui::vec2(200.0, 100.0)));
    assert_eq!(builder.position, Some(eframe::egui::pos2(20.0, 30.0)));
    assert_eq!(
        builder.window_level,
        Some(eframe::egui::WindowLevel::AlwaysOnTop)
    );
    assert!(vectrcap::ui::pin_window::viewport_builder(m.get(id).unwrap(), f32::NAN).is_err());
}
#[test]
fn pin_keyboard_controls_are_focus_scoped_and_preserve_text_input() {
    use eframe::egui::{Context, Event, Key, Modifiers, RawInput};
    let ctx = Context::default();
    for (key, modifiers, expected) in [
        (Key::Plus, Modifiers::NONE, 0),
        (Key::Minus, Modifiers::CTRL, 1),
        (Key::ArrowRight, Modifiers::SHIFT, 2),
        (Key::C, Modifiers::CTRL, 3),
        (Key::Escape, Modifiers::NONE, 4),
    ] {
        let raw = || RawInput {
            events: vec![
                Event::ModifiersChanged(modifiers),
                Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw(), |_ui| {
            assert!(vectrcap::ui::pin_window::keyboard_inputs(&ctx, false, false).is_empty());
            assert!(vectrcap::ui::pin_window::keyboard_inputs(&ctx, true, true).is_empty());
            let inputs = vectrcap::ui::pin_window::keyboard_inputs(&ctx, true, false);
            assert!(ctx.input(|i| {
                !i.events
                    .iter()
                    .any(|e| matches!(e, Event::Key { pressed: true, .. }))
            }));
            assert!(matches!(
                (expected, inputs.as_slice()),
                (0, [PinInput::Zoom(1)])
                    | (1, [PinInput::Opacity(-1)])
                    | (2, [PinInput::Move { dx: 10, dy: 0 }])
                    | (3, [PinInput::Copy])
                    | (4, [PinInput::Close])
            ));
        });
        output.textures_delta.clear();
        let mut output = ctx.run_ui(
            RawInput {
                events: vec![Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            },
            |_| {},
        );
        output.textures_delta.clear();
    }
}
#[test]
fn pin_display_preview_is_bounded_and_source_is_unchanged() {
    let pixels = Arc::new(RgbaImage::new(4096, 2048));
    let image = PinnedImage::new(pixels.clone(), None).unwrap();
    assert_eq!(image.display_pixels().dimensions(), (2048, 1024));
    assert!(Arc::ptr_eq(image.pixels(), &pixels));
    let mut m = manager();
    let too_wide = PinnedImage::new(Arc::new(RgbaImage::new(40_000, 1)), None).unwrap();
    assert!(m.open(too_wide, None).is_err());
}

fn pin_frame(
    ctx: &eframe::egui::Context,
    window: &mut vectrcap::ui::pin_window::PinWindow,
    pin: &vectrcap::pin::PinSnapshot,
    time: f64,
    events: Vec<eframe::egui::Event>,
) -> Vec<PinInput> {
    pin_frame_with_pointer(ctx, window, pin, time, events, None)
}
fn pin_frame_with_pointer(
    ctx: &eframe::egui::Context,
    window: &mut vectrcap::ui::pin_window::PinWindow,
    pin: &vectrcap::pin::PinSnapshot,
    time: f64,
    events: Vec<eframe::egui::Event>,
    pointer: Option<PhysicalPoint>,
) -> Vec<PinInput> {
    let mut inputs = vec![];
    let mut output = ctx.run_ui(
        eframe::egui::RawInput {
            screen_rect: Some(eframe::egui::Rect::from_min_size(
                eframe::egui::Pos2::ZERO,
                eframe::egui::vec2(400.0, 300.0),
            )),
            time: Some(time),
            events,
            ..Default::default()
        },
        |ui| {
            inputs = if let Some(pointer) = pointer {
                window.show_at_pointer(ui, pin, pointer)
            } else {
                window.show(ui, pin)
            };
        },
    );
    output.textures_delta.clear();
    inputs
}

#[test]
fn dragging_uses_screen_pixels_without_feedback_from_window_movement() {
    use eframe::egui::{Context, Event, Modifiers, PointerButton, pos2};
    let ctx = Context::default();
    let mut m = manager();
    let id = m.open(image(400, 300), None).unwrap();
    let pin = m.get(id).unwrap();
    let mut window = vectrcap::ui::pin_window::PinWindow::default();
    let start = PhysicalPoint { x: 1000, y: 1000 };
    pin_frame_with_pointer(
        &ctx,
        &mut window,
        pin,
        0.0,
        vec![Event::PointerMoved(pos2(250.0, 150.0))],
        Some(start),
    );
    pin_frame_with_pointer(
        &ctx,
        &mut window,
        pin,
        1.0,
        vec![Event::PointerButton {
            pos: pos2(250.0, 150.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }],
        Some(start),
    );
    let moved = PhysicalPoint { x: 1025, y: 1015 };
    let inputs = pin_frame_with_pointer(
        &ctx,
        &mut window,
        pin,
        1.1,
        vec![Event::PointerMoved(pos2(275.0, 165.0))],
        Some(moved),
    );
    assert!(matches!(
        inputs.as_slice(),
        [PinInput::Move { dx: 25, dy: 15 }]
    ));
    // Moving the HWND changes local cursor coordinates without moving the
    // physical cursor. That correction must never drag the pin back again.
    let inputs = pin_frame_with_pointer(
        &ctx,
        &mut window,
        pin,
        1.2,
        vec![Event::PointerMoved(pos2(250.0, 150.0))],
        Some(moved),
    );
    assert!(
        !inputs
            .iter()
            .any(|input| matches!(input, PinInput::Move { .. }))
    );
}
#[test]
fn a_dragged_pin_holds_its_new_place_until_the_app_catches_up() {
    use eframe::egui::{Context, Event, Modifiers, PointerButton, pos2};
    let ctx = Context::default();
    let mut m = manager();
    let id = m.open(image(400, 300), None).unwrap();
    let pin = m.get(id).unwrap().clone();
    let origin = pin.state.position();
    let mut window = vectrcap::ui::pin_window::PinWindow::default();
    let start = PhysicalPoint { x: 1000, y: 1000 };
    let press = |pressed| {
        vec![Event::PointerButton {
            pos: pos2(250.0, 150.0),
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }]
    };
    let hover = vec![Event::PointerMoved(pos2(250.0, 150.0))];
    pin_frame_with_pointer(&ctx, &mut window, &pin, 0.0, hover, Some(start));
    pin_frame_with_pointer(&ctx, &mut window, &pin, 1.0, press(true), Some(start));
    let moved = PhysicalPoint { x: 1025, y: 1015 };
    let drag = vec![Event::PointerMoved(pos2(275.0, 165.0))];
    pin_frame_with_pointer(&ctx, &mut window, &pin, 1.1, drag, Some(moved));
    let ahead = PhysicalPoint {
        x: origin.x + 25,
        y: origin.y + 15,
    };
    assert_eq!(window.held(), Some((ahead, 100)));
    pin_frame_with_pointer(&ctx, &mut window, &pin, 1.2, press(false), Some(moved));
    assert_eq!(window.held(), Some((ahead, 100)), "app state still behind");
    m.handle(id, PinInput::Move { dx: 25, dy: 15 }).unwrap();
    let caught_up = m.get(id).unwrap().clone();
    pin_frame_with_pointer(&ctx, &mut window, &caught_up, 1.3, vec![], Some(moved));
    assert_eq!(window.held(), None);
}
#[test]
fn set_zoom_is_clamped_to_the_zoom_range() {
    let mut m = manager();
    let id = m.open(image(40, 30), None).unwrap();
    m.handle(id, PinInput::SetZoom(1000)).unwrap();
    assert_eq!(m.get(id).unwrap().state.zoom_percent(), 500);
    m.handle(id, PinInput::SetZoom(5)).unwrap();
    assert_eq!(m.get(id).unwrap().state.zoom_percent(), 20);
}
#[test]
fn the_corner_grip_scales_by_the_larger_stretch() {
    use vectrcap::ui::pin_window::grip_zoom;
    assert_eq!(grip_zoom(100, (400, 300), (40, 0)), 110);
    assert_eq!(grip_zoom(100, (400, 300), (0, 60)), 120);
    assert_eq!(grip_zoom(100, (400, 300), (-1000, -1000)), 20);
    assert_eq!(grip_zoom(400, (1600, 1200), (2000, 0)), 500);
}
#[test]
fn pin_pointer_controls_work_with_options_open() {
    use eframe::egui::{Context, Event, Modifiers, PointerButton, pos2};
    let ctx = Context::default();
    let mut m = manager();
    let id = m.open(image(400, 300), None).unwrap();
    let pin = m.get(id).unwrap();
    let mut window = vectrcap::ui::pin_window::PinWindow::default();
    window.options_open = true;
    pin_frame(
        &ctx,
        &mut window,
        pin,
        0.0,
        vec![Event::PointerMoved(pos2(250.0, 150.0))],
    );
    for (index, button) in [PointerButton::Middle, PointerButton::Secondary]
        .into_iter()
        .enumerate()
    {
        let at = 1.0 + index as f64;
        pin_frame(
            &ctx,
            &mut window,
            pin,
            at,
            vec![Event::PointerButton {
                pos: pos2(250.0, 150.0),
                button,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
        );
        let inputs = pin_frame(
            &ctx,
            &mut window,
            pin,
            at + 0.1,
            vec![Event::PointerButton {
                pos: pos2(250.0, 150.0),
                button,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(matches!(
            (button, inputs.as_slice()),
            (PointerButton::Middle, [PinInput::Reset])
                | (PointerButton::Secondary, [PinInput::Close])
        ));
    }
}
#[test]
fn pin_wheel_drag_double_click_and_texture_release_are_headless() {
    use eframe::egui::{Context, Event, Modifiers, MouseWheelUnit, PointerButton, pos2, vec2};
    let ctx = Context::default();
    let mut m = manager();
    let source = image(400, 300);
    let weak = Arc::downgrade(source.pixels());
    let id = m.open(source, None).unwrap();
    let mut window = vectrcap::ui::pin_window::PinWindow::default();
    let pin = m.get(id).unwrap().clone();
    pin_frame(
        &ctx,
        &mut window,
        &pin,
        0.0,
        vec![Event::PointerMoved(pos2(250.0, 150.0))],
    );
    for (time, modifiers, delta) in [(1.0, Modifiers::NONE, 1.0), (2.0, Modifiers::CTRL, -1.0)] {
        let inputs = pin_frame(
            &ctx,
            &mut window,
            &pin,
            time,
            vec![Event::MouseWheel {
                source: eframe::egui::MouseWheelSource::Unknown,
                phase: eframe::egui::TouchPhase::Move,
                unit: MouseWheelUnit::Line,
                delta: vec2(0.0, delta),
                modifiers,
            }],
        );
        assert!(matches!(
            inputs.as_slice(),
            [PinInput::Zoom(1)] | [PinInput::Opacity(-1)]
        ));
    }
    let button = |pressed| Event::PointerButton {
        pos: pos2(250.0, 150.0),
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    pin_frame(&ctx, &mut window, &pin, 3.0, vec![button(true)]);
    pin_frame(&ctx, &mut window, &pin, 3.05, vec![button(false)]);
    pin_frame(&ctx, &mut window, &pin, 3.1, vec![button(true)]);
    assert!(matches!(
        pin_frame(&ctx, &mut window, &pin, 3.15, vec![button(false)]).as_slice(),
        [PinInput::ToggleMinimized]
    ));
    pin_frame(&ctx, &mut window, &pin, 4.0, vec![button(true)]);
    assert!(
        pin_frame(
            &ctx,
            &mut window,
            &pin,
            4.1,
            vec![Event::PointerMoved(pos2(275.0, 165.0))]
        )
        .iter()
        .any(|input| matches!(input, PinInput::Move { dx, dy } if *dx > 0 && *dy > 0))
    );
    drop(pin);
    m.close_all();
    assert!(
        weak.upgrade().is_none(),
        "CPU source released after closing"
    );
    drop(window);
    let mut output = ctx.run_ui(Default::default(), |_| {});
    assert!(
        !output.textures_delta.free.is_empty(),
        "GPU texture handle released"
    );
    output.textures_delta.clear();
}

#[test]
fn repeated_pin_open_close_releases_image_and_texture_ownership() {
    let ctx = eframe::egui::Context::default();
    let mut m = manager();
    for cycle in 0..32 {
        let source = image(20, 10);
        let weak = Arc::downgrade(source.pixels());
        let id = m.open(source, None).unwrap();
        let mut window = vectrcap::ui::pin_window::PinWindow::default();
        pin_frame(
            &ctx,
            &mut window,
            m.get(id).unwrap(),
            f64::from(cycle),
            vec![],
        );
        m.close_all();
        assert!(weak.upgrade().is_none());
        drop(window);
        let mut output = ctx.run_ui(Default::default(), |_| {});
        assert!(!output.textures_delta.free.is_empty());
        output.textures_delta.clear();
    }
}
