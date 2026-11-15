use std::{cell::RefCell, rc::Rc};
use tokio_util::sync::CancellationToken;
use vectrcap::{
    app::error::AppError,
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint as Point, PhysicalRect as Rect},
        last_region::{FinishOutcome, LastRegion},
        preferences::RegionCapturePreferences,
        request::{
            CaptureMode, CaptureRequest, CaptureService, CaptureSource, Countdown, CursorImage,
            ScreenshotPreferences, SnapshotHint, composite_cursor,
        },
        windows::{WindowCandidate, WindowId},
    },
};
fn rect() -> Rect {
    Rect::new(-10, 0, 20, 10).unwrap()
}
fn monitors() -> Vec<MonitorGeometry> {
    vec![MonitorGeometry::new("m".into(), rect(), 1.5).unwrap()]
}
type Events = Rc<RefCell<Vec<&'static str>>>;
struct Clock {
    events: Events,
    cancel: bool,
}
impl Countdown for Clock {
    fn wait(&mut self, seconds: u8, cancel: &CancellationToken) -> Result<(), AppError> {
        assert!(seconds <= 60);
        self.events.borrow_mut().push("wait");
        if self.cancel {
            cancel.cancel();
        }
        Ok(())
    }
    fn hide_chrome(&mut self) -> Result<(), AppError> {
        self.events.borrow_mut().push("hide");
        Ok(())
    }
}
struct Source {
    events: Events,
    changed_target: bool,
    layouts: usize,
    changed_layout: bool,
}
impl CaptureSource for Source {
    fn prepare_window(&mut self, _target: WindowCandidate) -> Result<(), AppError> {
        self.events.borrow_mut().push("prepare_window");
        Ok(())
    }
    fn monitors(&mut self) -> Result<Vec<MonitorGeometry>, AppError> {
        self.layouts += 1;
        if self.changed_layout && self.layouts > 1 {
            Ok(vec![MonitorGeometry::new("new".into(), rect(), 1.0)?])
        } else {
            Ok(monitors())
        }
    }
    fn pointer(&mut self) -> Result<Point, AppError> {
        Ok(Point { x: 0, y: 1 })
    }
    fn windows(&mut self) -> Result<Vec<WindowCandidate>, AppError> {
        Ok(vec![WindowCandidate {
            id: WindowId::new(
                if self.changed_target && self.events.borrow().contains(&"wait") {
                    2
                } else {
                    1
                },
            )?,
            bounds: rect(),
        }])
    }
    fn desktop(
        &mut self,
        _bounds: Rect,
        _monitors: &[MonitorGeometry],
        cursor: bool,
    ) -> Result<Vec<u8>, AppError> {
        self.events
            .borrow_mut()
            .push(if cursor { "desktop_cursor" } else { "desktop" });
        Ok(vec![7; 800])
    }
    fn window(&mut self, _target: WindowCandidate, _cursor: bool) -> Result<Vec<u8>, AppError> {
        if !self.events.borrow().contains(&"prepare_window") {
            return Err(AppError::Capture(
                "target source was not fixed before countdown",
            ));
        }
        self.events.borrow_mut().push("window");
        Ok(vec![9; 800])
    }
}
fn request(mode: CaptureMode) -> CaptureRequest {
    CaptureRequest::new(mode, 0, false, RegionCapturePreferences::default()).unwrap()
}
fn run(
    req: CaptureRequest,
    cancel: bool,
    changed_target: bool,
    changed_layout: bool,
) -> (
    Result<vectrcap::capture::request::CaptureSnapshot, AppError>,
    Events,
) {
    let events = Rc::new(RefCell::new(vec![]));
    let source = Source {
        events: events.clone(),
        changed_target,
        layouts: 0,
        changed_layout,
    };
    let clock = Clock {
        events: events.clone(),
        cancel,
    };
    (
        CaptureService::new(source, clock, CancellationToken::new()).snapshot(req),
        events,
    )
}
#[test]
fn last_region_uses_fresh_pixels_and_no_old_annotations() {
    let mut req = request(CaptureMode::LastRegion);
    req.last_region = Some(LastRegion::new(Rect::new(-5, 1, 4, 3).unwrap(), &monitors()).unwrap());
    let (s, _) = run(req, false, false, false);
    let s = s.unwrap();
    assert_eq!(s.initial_crop(), Some(Rect::new(-5, 1, 4, 3).unwrap()));
    assert_eq!(s.pixels(), &vec![7; 800]);
    let value = serde_json::to_value(LastRegion::new(rect(), &monitors()).unwrap()).unwrap();
    assert!(value.get("pixels").is_none());
    assert!(value.get("annotations").is_none());
    assert!(value.get("window").is_none());
}
#[test]
fn missing_last_region_opens_selection() {
    let (s, _) = run(request(CaptureMode::LastRegion), false, false, false);
    let s = s.unwrap();
    assert_eq!(s.initial_crop(), None);
    assert_eq!(s.hint(), Some(SnapshotHint::NoLastRegion));
}
#[test]
fn changed_monitor_layout_requires_reselection() {
    let mut req = request(CaptureMode::LastRegion);
    req.last_region = Some(
        LastRegion::new(
            rect(),
            &[MonitorGeometry::new("m".into(), rect(), 1.0).unwrap()],
        )
        .unwrap(),
    );
    let (s, _) = run(req, false, false, false);
    let s = s.unwrap();
    assert_eq!(s.initial_crop(), None);
    assert_eq!(s.hint(), Some(SnapshotHint::LayoutChanged));
    assert!(
        run(request(CaptureMode::Region), false, false, true)
            .0
            .is_err()
    );
}
#[test]
fn cancelled_capture_keeps_previous_region() {
    let old = Some(LastRegion::new(rect(), &monitors()).unwrap());
    assert_eq!(
        LastRegion::after_finish(
            old.clone(),
            CaptureMode::Region,
            FinishOutcome::Cancelled,
            &monitors()
        )
        .unwrap(),
        old
    );
    assert_eq!(
        LastRegion::after_finish(
            old.clone(),
            CaptureMode::Region,
            FinishOutcome::ClipboardEdit,
            &monitors()
        )
        .unwrap(),
        old
    );
    assert_eq!(
        LastRegion::after_finish(
            old.clone(),
            CaptureMode::Window,
            FinishOutcome::Confirmed(Rect::new(0, 0, 1, 1).unwrap()),
            &monitors()
        )
        .unwrap(),
        old
    );
    let next = LastRegion::after_finish(
        old,
        CaptureMode::Region,
        FinishOutcome::Confirmed(Rect::new(0, 0, 1, 1).unwrap()),
        &monitors(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(next.bounds(), Rect::new(0, 0, 1, 1).unwrap());
}
#[test]
fn delay_is_cancellable_and_capture_happens_after_countdown() {
    for delay in [0, 3, 5, 10, 60] {
        let mut req = request(CaptureMode::Region);
        req.delay_seconds = delay;
        let (s, events) = run(req, false, false, false);
        s.unwrap();
        assert_eq!(*events.borrow(), vec!["wait", "hide", "desktop"]);
    }
    assert!(
        CaptureRequest::new(
            CaptureMode::Region,
            61,
            false,
            RegionCapturePreferences::default()
        )
        .is_err()
    );
    let (s, events) = run(request(CaptureMode::Region), true, false, false);
    assert_eq!(s.unwrap_err(), AppError::Cancelled);
    assert_eq!(*events.borrow(), vec!["wait"]);
}
#[test]
fn delayed_window_target_never_changes_silently() {
    let mut req = request(CaptureMode::Window);
    req.target = Some(WindowCandidate {
        id: WindowId::new(1).unwrap(),
        bounds: rect(),
    });
    req.delay_seconds = 3;
    let (s, events) = run(req.clone(), false, true, false);
    assert!(s.is_err());
    assert!(!events.borrow().contains(&"window"));
    assert!(!events.borrow().contains(&"desktop"));
    assert_eq!(*events.borrow(), vec!["prepare_window", "wait", "hide"]);
    let (s, events) = run(req, false, false, false);
    let s = s.unwrap();
    assert_eq!(s.pixels(), &vec![9; 800]);
    assert!(events.borrow().contains(&"window"));
    assert!(!events.borrow().contains(&"desktop"));
}
#[test]
fn screenshot_cursor_defaults_off_independent_of_recording() {
    let p = ScreenshotPreferences::default();
    assert!(!p.include_cursor);
    assert_eq!(p.delay_seconds, 0);
    assert_eq!(ScreenshotPreferences::DELAY_PRESETS, [0, 3, 5, 10]);
    let mut p = p;
    p.overrides
        .push(vectrcap::capture::request::CaptureOverride {
            mode: CaptureMode::Region,
            delay_seconds: Some(5),
            include_cursor: Some(true),
        });
    let req = p
        .request(CaptureMode::Region, RegionCapturePreferences::default())
        .unwrap();
    assert!(req.include_cursor);
    assert_eq!(req.delay_seconds, 5);
    assert!(
        !p.request(CaptureMode::Fullscreen, RegionCapturePreferences::default())
            .unwrap()
            .include_cursor
    );
    assert!(vectrcap::recording::options::RecordingOptions::new("m".into(), rect(), false).cursor);
}
#[test]
fn cursor_hotspot_matches_physical_crop() {
    let mut pixels = vec![0; 4 * 3 * 4];
    let cursor = CursorImage::new(
        Point { x: -9, y: 1 },
        (1, 1),
        2,
        2,
        [255, 0, 0, 255].repeat(4),
    )
    .unwrap();
    composite_cursor(&mut pixels, Rect::new(-10, 0, 4, 3).unwrap(), &cursor).unwrap();
    assert_eq!(&pixels[..8], &[255, 0, 0, 255, 255, 0, 0, 255]);
    assert_eq!(&pixels[16..24], &[255, 0, 0, 255, 255, 0, 0, 255]);
    assert_eq!(&pixels[8..16], &[0; 8]);
    assert!(CursorImage::new(Point { x: 0, y: 0 }, (2, 0), 2, 2, vec![0; 16]).is_err());
}

#[test]
fn last_region_with_an_edge_in_a_monitor_gap_roundtrips() {
    let layout = vec![
        MonitorGeometry::new("a".into(), Rect::new(-100, 0, 50, 100).unwrap(), 1.5).unwrap(),
        MonitorGeometry::new("b".into(), Rect::new(50, 0, 100, 100).unwrap(), 1.0).unwrap(),
    ];
    let region = LastRegion::new(Rect::new(-75, 10, 50, 50).unwrap(), &layout).unwrap();
    let loaded: LastRegion = serde_json::from_slice(&serde_json::to_vec(&region).unwrap()).unwrap();
    assert!(loaded.matches_layout(&layout));
    assert_eq!(loaded, region);
}
