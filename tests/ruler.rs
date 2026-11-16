use vectrcap::{
    capture::geometry::{MonitorGeometry, PhysicalPoint as Point, PhysicalRect as Rect},
    tools::{UtilitySnapshot, ruler::*},
};
fn strip(values: &[u8], split: bool) -> UtilitySnapshot {
    let b = Rect::new(0, 0, values.len() as u32, 1).unwrap();
    let monitors = if split {
        vec![
            MonitorGeometry::new("a".into(), Rect::new(0, 0, 2, 1).unwrap(), 1.0).unwrap(),
            MonitorGeometry::new("b".into(), Rect::new(2, 0, b.width() - 2, 1).unwrap(), 2.0)
                .unwrap(),
        ]
    } else {
        vec![MonitorGeometry::new("a".into(), b, 1.0).unwrap()]
    };
    UtilitySnapshot::new(
        values.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        b,
        monitors,
        0,
    )
    .unwrap()
}
#[test]
fn inclusive_bounds_match_reference_metrics() {
    let m = measure_rect(Rect::new(2887, 458, 634, 566).unwrap()).unwrap();
    assert_eq!(
        (m.left, m.top, m.right, m.bottom, m.width, m.height),
        (2887, 458, 3520, 1023, 634, 566)
    );
    assert_eq!((m.area, m.perimeter), (358844, 2400));
    assert_eq!(format!("{:.2}", m.distance), "848.48");
    assert_eq!(format!("{:.2}", m.angle_degrees.unwrap()), "41.75");
}
#[test]
fn single_pixel_has_zero_distance_and_no_angle() {
    let m = measure_rect(Rect::new(-1, -2, 1, 1).unwrap()).unwrap();
    assert_eq!(m.distance, 0.0);
    assert_eq!(m.angle_degrees, None);
}
#[test]
fn reverse_drag_normalizes_without_off_by_one() {
    let r = rect_from_endpoints(Point { x: 3520, y: 1023 }, Point { x: 2887, y: 458 }).unwrap();
    assert_eq!(r, Rect::new(2887, 458, 634, 566).unwrap());
    assert_eq!(
        rect_from_endpoints(Point { x: 0, y: 0 }, Point { x: 0, y: 0 })
            .unwrap()
            .width(),
        1
    );
}
#[test]
fn area_and_perimeter_do_not_overflow() {
    let m = measure_rect(Rect::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX).unwrap()).unwrap();
    assert_eq!(m.area, u64::from(u32::MAX).pow(2));
    assert_eq!(m.perimeter, 4 * u64::from(u32::MAX));
}
#[test]
fn run_tolerance_compares_to_anchor() {
    let s = strip(&[100, 110, 120, 130, 140], false);
    let m = measure_run(&s, Point { x: 0, y: 0 }, Axis::Horizontal, 8).unwrap();
    assert_eq!(m.end.x, 2);
    assert_eq!(
        measure_run(&s, Point { x: 0, y: 0 }, Axis::Horizontal, 0)
            .unwrap()
            .end
            .x,
        0
    );
    assert_eq!(
        measure_run(&s, Point { x: 0, y: 0 }, Axis::Horizontal, 100)
            .unwrap()
            .end
            .x,
        4
    );
    assert!(measure_run(&s, Point { x: 0, y: 0 }, Axis::Horizontal, 101).is_err());
}
#[test]
fn runs_stop_at_monitor_edges() {
    let s = strip(&[100, 100, 100, 100], true);
    let m = measure_run(&s, Point { x: 0, y: 0 }, Axis::Horizontal, 100).unwrap();
    assert_eq!((m.start.x, m.end.x), (0, 1));
}
#[test]
fn measurement_moves_by_physical_pixels() {
    let mut r = RulerState::default();
    r.add(Rect::new(-5, -5, 3, 4).unwrap()).unwrap();
    r.move_selected(1, 0, false).unwrap();
    assert_eq!(r.selected().unwrap().left(), -4);
    r.move_selected(0, -1, true).unwrap();
    assert_eq!(r.selected().unwrap().top(), -15);
}
#[test]
fn measurement_count_is_bounded() {
    let mut r = RulerState::default();
    for _ in 0..256 {
        r.add(Rect::new(0, 0, 1, 1).unwrap()).unwrap();
    }
    assert!(r.add(Rect::new(0, 0, 1, 1).unwrap()).is_err());
    assert_eq!(r.measurements().len(), 256);
    r.clear();
    assert!(r.measurements().is_empty());
}

#[test]
fn session_rejects_movement_outside_source_without_mutating_measurement() {
    use vectrcap::tools::session::*;
    let mut session = UtilitySession::new(
        UtilityKind::Ruler,
        strip(&[1, 1, 1], false),
        Point { x: 1, y: 0 },
    );
    session.handle(UtilityInput::AddRect(Rect::new(2, 0, 1, 1).unwrap()));
    let effects = session.handle(UtilityInput::Move {
        dx: 1,
        dy: 0,
        shift: false,
    });
    assert!(matches!(effects.as_slice(), [UtilityEffect::Error(_)]));
    assert_eq!(
        session.ruler.selected().unwrap(),
        Rect::new(2, 0, 1, 1).unwrap()
    );
}

#[test]
fn tolerance_uses_max_channel_difference_and_floor_threshold() {
    let b = Rect::new(0, 0, 3, 1).unwrap();
    let s = UtilitySnapshot::new(
        vec![0, 0, 0, 255, 2, 0, 0, 255, 0, 3, 0, 255],
        b,
        vec![MonitorGeometry::new("m".into(), b, 1.0).unwrap()],
        1,
    )
    .unwrap();
    assert_eq!(
        measure_run(&s, Point { x: 0, y: 0 }, Axis::Horizontal, 1)
            .unwrap()
            .end
            .x,
        1
    );
    assert_eq!(
        measure_run(&s, Point { x: 0, y: 0 }, Axis::Vertical, 100)
            .unwrap()
            .length,
        1
    );
}

#[test]
fn resizing_handles_preserves_inclusive_endpoints_and_normalizes_crossing() {
    let original = Rect::new(-10, -20, 5, 6).unwrap();
    assert_eq!(
        resize_rect(original, RectHandle::BottomRight, Point { x: 0, y: 0 }).unwrap(),
        Rect::new(-10, -20, 11, 21).unwrap()
    );
    assert_eq!(
        resize_rect(original, RectHandle::TopLeft, Point { x: 2, y: 3 }).unwrap(),
        Rect::new(-6, -15, 9, 19).unwrap()
    );
    assert_eq!(
        resize_rect(original, RectHandle::Left, Point { x: -12, y: 500 }).unwrap(),
        Rect::new(-12, -20, 7, 6).unwrap()
    );
}
