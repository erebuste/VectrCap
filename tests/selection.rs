use vectrcap::capture::{
    geometry::{MonitorGeometry, PhysicalPoint as Point, PhysicalRect as Rect},
    preferences::RegionCapturePreferences,
    request::CaptureSnapshot,
    selection::{
        InputContext, SelectionInput as Input, SelectionModel, SelectionOutcome as Outcome,
    },
    windows::{WindowCandidate, WindowId, WindowState, eligible_windows},
};

fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
    Rect::new(x, y, w, h).unwrap()
}
fn candidate(id: u64, bounds: Rect) -> WindowCandidate {
    WindowCandidate {
        id: WindowId::new(id).unwrap(),
        bounds,
    }
}
fn snapshot() -> CaptureSnapshot {
    let bounds = rect(-100, 0, 300, 200);
    CaptureSnapshot::new(
        vec![0; 300 * 200 * 4],
        bounds,
        vec![
            MonitorGeometry::new("left".into(), rect(-100, 0, 100, 200), 1.5).unwrap(),
            MonitorGeometry::new("right".into(), rect(0, 0, 200, 200), 1.0).unwrap(),
        ],
        vec![
            candidate(1, rect(-50, 10, 150, 100)),
            candidate(2, rect(-80, 0, 250, 150)),
        ],
    )
    .unwrap()
}
fn model() -> SelectionModel {
    SelectionModel::new(
        &snapshot(),
        RegionCapturePreferences::default(),
        Point { x: 0, y: 20 },
        (4, 4),
    )
    .unwrap()
}
fn click(m: &mut SelectionModel, p: Point) -> Outcome {
    m.handle(Input::PrimaryDown {
        point: p,
        context: InputContext::Region,
    });
    m.handle(Input::PrimaryUp(p))
}

#[test]
fn frontmost_window_wins_hover() {
    assert_eq!(
        model().handle(Input::PointerMove(Point { x: 0, y: 20 })),
        Outcome::Hover(Some(rect(-50, 10, 150, 100)))
    );
}
#[test]
fn click_selects_window_but_drag_stays_custom() {
    let mut m = model();
    assert_eq!(
        click(&mut m, Point { x: 0, y: 20 }),
        Outcome::Selected(rect(-50, 10, 150, 100))
    );
    m.handle(Input::Clear);
    m.handle(Input::PrimaryDown {
        point: Point { x: 90, y: 90 },
        context: InputContext::Region,
    });
    m.handle(Input::PointerMove(Point { x: -40, y: 20 }));
    assert_eq!(
        m.handle(Input::PrimaryUp(Point { x: -40, y: 20 })),
        Outcome::Selected(rect(-40, 20, 130, 70))
    );
    m.handle(Input::Clear);
    assert_eq!(click(&mut m, Point { x: 190, y: 190 }), Outcome::Unchanged);
}
#[test]
fn hover_does_not_change_committed_crop() {
    let mut m = model();
    click(&mut m, Point { x: 0, y: 20 });
    assert_eq!(
        m.handle(Input::PointerMove(Point { x: 190, y: 190 })),
        Outcome::Unchanged
    );
    assert_eq!(m.crop(), Some(rect(-50, 10, 150, 100)));
    m.handle(Input::Clear);
    assert_eq!(
        m.handle(Input::PrimaryDown {
            point: Point { x: 0, y: 20 },
            context: InputContext::Drawing
        }),
        Outcome::Unchanged
    );
    assert_eq!(
        m.handle(Input::PrimaryUp(Point { x: 0, y: 20 })),
        Outcome::Unchanged
    );
    assert_eq!(m.crop(), None);
}
#[test]
fn excluded_windows_never_win() {
    let states: Vec<_> = (1..=6)
        .map(|id| WindowState {
            candidate: candidate(id, rect(0, 0, 30, 30)),
            visible: id != 1,
            minimized: id == 2,
            cloaked: id == 3,
            owned_by_app: id == 4,
        })
        .collect();
    let eligible = eligible_windows(states);
    assert_eq!(
        eligible.iter().map(|w| w.id).collect::<Vec<_>>(),
        vec![WindowId::new(5).unwrap(), WindowId::new(6).unwrap()]
    );
    let s = CaptureSnapshot::new(
        vec![0; 400],
        rect(0, 0, 10, 10),
        vec![MonitorGeometry::new("m".into(), rect(0, 0, 10, 10), 1.0).unwrap()],
        vec![candidate(9, rect(-20, -20, 25, 25))],
    )
    .unwrap();
    let mut m = SelectionModel::new(
        &s,
        RegionCapturePreferences::default(),
        Point { x: 1, y: 1 },
        (4, 4),
    )
    .unwrap();
    assert_eq!(
        click(&mut m, Point { x: 1, y: 1 }),
        Outcome::Selected(rect(0, 0, 5, 5))
    );
}
#[test]
fn resize_keeps_bounds_valid() {
    let mut m = model();
    click(&mut m, Point { x: 0, y: 20 });
    assert!(matches!(
        m.handle(Input::ResizeCrop(rect(-150, -10, 400, 250))),
        Outcome::Changed(_)
    ));
    assert_eq!(m.crop(), Some(rect(-100, 0, 300, 200)));
    assert!(matches!(
        m.handle(Input::MoveCrop {
            dx: i32::MAX,
            dy: i32::MIN
        }),
        Outcome::Rejected(_)
    ));
    assert_eq!(m.crop(), Some(rect(-100, 0, 300, 200)));
    m.handle(Input::Clear);
    m.handle(Input::PrimaryDown {
        point: Point { x: i32::MIN, y: 0 },
        context: InputContext::Region,
    });
    assert_eq!(
        m.handle(Input::PrimaryUp(Point {
            x: i32::MAX,
            y: 100
        })),
        Outcome::Unchanged
    );
}
#[test]
fn display_change_invalidates_session() {
    let mut m = model();
    click(&mut m, Point { x: 0, y: 20 });
    assert_eq!(m.handle(Input::LayoutChanged(vec![])), Outcome::Cancelled);
    assert_eq!(m.crop(), None);
    assert_eq!(click(&mut m, Point { x: 0, y: 20 }), Outcome::Cancelled);
}

#[test]
fn text_and_toolbar_context_cannot_move_committed_crop() {
    let mut m = model();
    click(&mut m, Point { x: 0, y: 20 });
    for context in [
        InputContext::Text,
        InputContext::Drawing,
        InputContext::Chrome,
    ] {
        m.handle(Input::Context(context));
        assert_eq!(
            m.handle(Input::MoveCrop { dx: 1, dy: 1 }),
            Outcome::Unchanged
        );
        assert_eq!(
            m.handle(Input::ResizeCrop(rect(0, 0, 10, 10))),
            Outcome::Unchanged
        );
        assert_eq!(m.crop(), Some(rect(-50, 10, 150, 100)));
    }
}

#[test]
fn monitor_gaps_are_unavailable_and_cannot_start_selection() {
    let bounds = rect(-100, 0, 300, 200);
    let s = CaptureSnapshot::new(
        vec![0; 300 * 200 * 4],
        bounds,
        vec![
            MonitorGeometry::new("a".into(), rect(-100, 0, 50, 200), 1.5).unwrap(),
            MonitorGeometry::new("b".into(), rect(50, 0, 150, 200), 1.0).unwrap(),
        ],
        vec![candidate(1, bounds)],
    )
    .unwrap();
    assert_eq!(s.sample(Point { x: 0, y: 20 }), None);
    let mut m = SelectionModel::new(
        &s,
        RegionCapturePreferences::default(),
        Point { x: 100, y: 20 },
        (4, 4),
    )
    .unwrap();
    assert_eq!(click(&mut m, Point { x: 0, y: 20 }), Outcome::Unchanged);
    assert_eq!(m.crop(), None);
}

#[test]
fn pointer_move_resize_and_escape_preserve_committed_crop() {
    let mut m = model();
    click(&mut m, Point { x: 0, y: 20 });
    m.handle(Input::PrimaryDown {
        point: Point { x: 0, y: 50 },
        context: InputContext::Region,
    });
    assert_eq!(
        m.handle(Input::PrimaryUp(Point { x: 10, y: 60 })),
        Outcome::Changed(rect(-40, 20, 150, 100))
    );
    m.handle(Input::PrimaryDown {
        point: Point { x: -40, y: 50 },
        context: InputContext::Region,
    });
    assert_eq!(
        m.handle(Input::PrimaryUp(Point { x: -80, y: 50 })),
        Outcome::Changed(rect(-80, 20, 190, 100))
    );
    m.handle(Input::PrimaryDown {
        point: Point { x: 0, y: 50 },
        context: InputContext::Region,
    });
    m.handle(Input::PointerMove(Point { x: 20, y: 60 }));
    assert_eq!(
        m.preview(Point { x: 20, y: 60 }).unwrap(),
        Some(rect(-60, 30, 190, 100))
    );
    assert_eq!(m.handle(Input::Escape), Outcome::EditCancelled);
    assert_eq!(m.crop(), Some(rect(-80, 20, 190, 100)));
}
