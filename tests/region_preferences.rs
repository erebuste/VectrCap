use vectrcap::{
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint as Point, PhysicalRect as Rect},
        preferences::{
            CaptureMouseAction as Action, LensShape, MouseButton, RegionCapturePreferences,
            SizePreset,
        },
        request::CaptureSnapshot,
        selection::{
            InputContext, SelectionInput as Input, SelectionModel, SelectionOutcome as Outcome,
        },
    },
    persistence::settings::Settings,
};
fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
    Rect::new(x, y, w, h).unwrap()
}
fn model(p: RegionCapturePreferences) -> SelectionModel {
    let s = CaptureSnapshot::new(
        vec![0; 1600 * 800 * 4],
        r(-800, 0, 1600, 800),
        vec![
            MonitorGeometry::new("a".into(), r(-800, 0, 800, 800), 1.5).unwrap(),
            MonitorGeometry::new("b".into(), r(0, 0, 800, 800), 1.0).unwrap(),
        ],
        vec![],
    )
    .unwrap();
    SelectionModel::new(&s, p, Point { x: -400, y: 50 }, (4, 4)).unwrap()
}
fn drag(m: &mut SelectionModel, from: Point, to: Point) -> Outcome {
    m.handle(Input::PrimaryDown {
        point: from,
        context: InputContext::Region,
    });
    m.handle(Input::PointerMove(to));
    m.handle(Input::PrimaryUp(to))
}
#[test]
fn region_defaults_match_confirmed_choices() {
    let p = RegionCapturePreferences::default();
    assert!(p.window_detection && p.show_position_size && p.show_magnifier);
    assert_eq!(p.dim_percent, 20);
    assert_eq!(
        (p.lens_shape, p.lens_source_count, p.lens_scale),
        (LensShape::Circle, 15, 20)
    );
    assert!(
        !p.show_rgb
            && !p.show_hex
            && !p.center_crosshair
            && !p.screen_crosshair
            && !p.fixed_size
            && !p.active_monitor_only
    );
    assert_eq!(
        p.fixed_dimensions,
        SizePreset {
            width: 640,
            height: 480
        }
    );
    assert_eq!(
        p.presets,
        vec![
            SizePreset {
                width: 640,
                height: 480
            },
            SizePreset {
                width: 1280,
                height: 720
            },
            SizePreset {
                width: 1920,
                height: 1080
            }
        ]
    );
    assert_eq!(p.mouse.right, Action::CancelActiveOrCapture);
    assert_eq!(p.mouse.middle, Action::ToggleSelectionDrawing);
    assert_eq!(p.mouse.button4, Action::None);
    assert_eq!(p.mouse.button5, Action::None);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut s = Settings {
        region: p,
        ..Default::default()
    };
    s.region.dim_percent = 33;
    s.save(&path).unwrap();
    assert_eq!(Settings::load(&path).unwrap().region.dim_percent, 33);
}
#[test]
fn lens_count_and_zoom_are_bounded() {
    for count in [8, 10, 36, 255] {
        let p = RegionCapturePreferences {
            lens_source_count: count,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }
    for count in [9, 15, 35] {
        for scale in [4, 24] {
            assert!(
                RegionCapturePreferences {
                    lens_source_count: count,
                    lens_scale: scale,
                    ..Default::default()
                }
                .validate()
                .is_ok()
            );
        }
    }
    for scale in [0, 3, 25] {
        assert!(
            RegionCapturePreferences {
                lens_scale: scale,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    let mut s = Settings::default();
    s.region.dim_percent = 81;
    assert!(s.validate().is_err());
    s.region.dim_percent = 80;
    s.region.presets = vec![
        SizePreset {
            width: 1,
            height: 1
        };
        17
    ];
    assert!(s.validate().is_err());
    assert!(
        RegionCapturePreferences {
            fixed_dimensions: SizePreset {
                width: 8001,
                height: 8000
            },
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}
#[test]
fn disabled_window_detection_keeps_manual_drag() {
    let mut m = model(RegionCapturePreferences {
        window_detection: false,
        ..Default::default()
    });
    assert_eq!(
        drag(&mut m, Point { x: -100, y: 10 }, Point { x: 100, y: 110 }),
        Outcome::Selected(r(-100, 10, 200, 100))
    );
}
#[test]
fn fixed_size_rejects_out_of_bounds_without_resizing() {
    let mut m = model(RegionCapturePreferences {
        fixed_size: true,
        ..Default::default()
    });
    assert!(matches!(
        drag(&mut m, Point { x: 700, y: 700 }, Point { x: 790, y: 790 }),
        Outcome::Rejected(_)
    ));
    assert_eq!(m.crop(), None);
    assert_eq!(
        drag(&mut m, Point { x: -400, y: 20 }, Point { x: 100, y: 100 }),
        Outcome::Selected(r(-400, 20, 640, 480))
    );
    assert!(matches!(
        m.handle(Input::ResizeCrop(r(-400, 20, 100, 100))),
        Outcome::Rejected(_)
    ));
    assert_eq!(m.crop(), Some(r(-400, 20, 640, 480)));
}
#[test]
fn alt_presets_only_apply_to_region_tool() {
    let mut m = model(RegionCapturePreferences::default());
    m.handle(Input::Alt(true));
    assert_eq!(
        drag(&mut m, Point { x: -700, y: 10 }, Point { x: -50, y: 500 }),
        Outcome::Selected(r(-700, 10, 640, 480))
    );
    m.handle(Input::Clear);
    m.handle(Input::PrimaryDown {
        point: Point { x: -700, y: 10 },
        context: InputContext::Text,
    });
    assert_eq!(
        m.handle(Input::PrimaryUp(Point { x: 0, y: 600 })),
        Outcome::Unchanged
    );
    assert_eq!(m.crop(), None);
    m.handle(Input::PrimaryDown {
        point: Point { x: -700, y: 10 },
        context: InputContext::Drawing,
    });
    assert_eq!(
        m.handle(Input::PrimaryUp(Point { x: 0, y: 600 })),
        Outcome::Unchanged
    );
}
#[test]
fn active_monitor_is_fixed_at_session_start() {
    let mut p = RegionCapturePreferences {
        active_monitor_only: true,
        ..Default::default()
    };
    let mut m = model(p.clone());
    p.active_monitor_only = false;
    assert_eq!(
        drag(&mut m, Point { x: -100, y: 10 }, Point { x: 500, y: 100 }),
        Outcome::Selected(r(-100, 10, 100, 90))
    );
    assert_eq!(
        m.handle(Input::Auxiliary(MouseButton::Button4)),
        Outcome::Unchanged
    );
    assert_eq!(m.permitted_bounds(), r(-800, 0, 800, 800));
}
#[test]
fn auxiliary_mouse_actions_do_not_publish() {
    let mut p = RegionCapturePreferences::default();
    p.mouse.button4 = Action::SelectFullscreen;
    p.mouse.button5 = Action::SelectActiveMonitor;
    let mut m = model(p);
    assert_eq!(
        m.handle(Input::Auxiliary(MouseButton::Button4)),
        Outcome::Selected(r(-800, 0, 1600, 800))
    );
    assert_eq!(
        m.handle(Input::Auxiliary(MouseButton::Button5)),
        Outcome::Selected(r(-800, 0, 800, 800))
    );
    m.handle(Input::Context(InputContext::Text));
    assert_eq!(
        m.handle(Input::Auxiliary(MouseButton::Button4)),
        Outcome::Unchanged
    );
}
#[test]
fn escape_cannot_be_lost_by_mouse_remapping() {
    let mut p = RegionCapturePreferences::default();
    p.mouse.right = Action::None;
    let mut m = model(p);
    m.handle(Input::PrimaryDown {
        point: Point { x: -100, y: 10 },
        context: InputContext::Region,
    });
    m.handle(Input::PointerMove(Point { x: 100, y: 110 }));
    assert_eq!(m.handle(Input::Escape), Outcome::EditCancelled);
    assert_eq!(m.crop(), None);
    m.handle(Input::Context(InputContext::Text));
    assert_eq!(m.handle(Input::Escape), Outcome::EditCancelled);
    assert_eq!(m.handle(Input::Escape), Outcome::Cancelled);
}

#[test]
fn screenshot_and_last_region_settings_roundtrip_and_validate() {
    use vectrcap::capture::{last_region::LastRegion, request::CaptureMode};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut s = Settings::default();
    s.screenshot.delay_seconds = 10;
    s.screenshot.include_cursor = true;
    s.last_region = Some(
        LastRegion::new(
            r(-100, 0, 100, 100),
            &[MonitorGeometry::new("m".into(), r(-800, 0, 800, 800), 1.5).unwrap()],
        )
        .unwrap(),
    );
    s.save(&path).unwrap();
    let loaded = Settings::load(&path).unwrap();
    assert_eq!(loaded.screenshot, s.screenshot);
    assert_eq!(loaded.last_region, s.last_region);
    let req = loaded.capture_request(CaptureMode::LastRegion).unwrap();
    assert!(req.include_cursor);
    assert_eq!(req.delay_seconds, 10);
    assert_eq!(req.last_region, loaded.last_region);
    s.screenshot.delay_seconds = 61;
    assert!(s.validate().is_err());
    let mut json = serde_json::to_value(Settings::default()).unwrap();
    json["region"]["mouse"]["right"] = serde_json::json!("RunCommand");
    assert!(serde_json::from_value::<Settings>(json).is_err());
    let mut json = serde_json::to_value(loaded).unwrap();
    json["last_region"]["bounds"]["width"] = serde_json::json!(0);
    assert!(serde_json::from_value::<Settings>(json).is_err());
}
