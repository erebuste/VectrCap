use vectrcap::capture::geometry::{
    MonitorGeometry, PhysicalPoint as Point, PhysicalRect as Rect, RgbaFrame,
    compose_intersections, virtual_bounds,
};
fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
    Rect::new(x, y, w, h).unwrap()
}
#[test]
fn negative_origin_and_mixed_dpi_match_pixels() {
    let m = MonitorGeometry::new("left".into(), r(-1920, -200, 1920, 1080), 1.5).unwrap();
    let p = m.logical_to_physical(100.0, 40.0).unwrap();
    assert_eq!(p, Point { x: -1770, y: -140 });
    assert_eq!(m.physical_to_logical(p).unwrap(), (100.0, 40.0));
    assert_eq!(
        r(-1920, -200, 3840, 1280).pixel_offset(p).unwrap(),
        (150, 60)
    );
    assert!(m.logical_to_physical(f64::NAN, 0.0).is_err());
    assert!(m.logical_to_physical(f64::MAX, 0.0).is_err());
    assert!(
        Rect::from_points(Point { x: i32::MIN, y: 0 }, Point { x: i32::MAX, y: 2 })
            .unwrap()
            .rgba_len()
            .is_err()
    );
    assert!(r(i32::MAX - 2, 0, 2, 2).translated(3, 0).is_err());
}
#[test]
fn cross_monitor_screenshot_composes_intersections() {
    let frames = vec![
        RgbaFrame::new(r(-2, 0, 2, 2), [255, 0, 0, 255].repeat(4)).unwrap(),
        RgbaFrame::new(r(1, 1, 2, 2), [0, 255, 0, 255].repeat(4)).unwrap(),
    ];
    let bounds = virtual_bounds(&[r(-2, 0, 2, 2), r(1, 1, 2, 2)]).unwrap();
    assert_eq!(bounds, r(-2, 0, 5, 3));
    let out = compose_intersections(r(-1, 0, 3, 3), &frames).unwrap();
    assert_eq!(&out[0..12], &[255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&out[12..24], &[255, 0, 0, 255, 0, 0, 0, 0, 0, 255, 0, 255]);
    assert_eq!(&out[24..36], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 255]);
    assert!(RgbaFrame::new(r(0, 0, 2, 2), vec![0; 4]).is_err());
    assert!(
        virtual_bounds(&[r(i32::MIN, 0, 1, 1), r(i32::MAX - 1, 0, 1, 1)])
            .unwrap()
            .rgba_len()
            .is_err()
    );
}

#[test]
#[ignore = "interactive Windows metadata and synthetic-window-only capture smoke"]
#[allow(unsafe_code)]
fn native_monitor_window_hover_and_snapshot_smoke() {
    use std::{
        cell::Cell,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use tokio_util::sync::CancellationToken;
    use vectrcap::{
        capture::{
            preferences::RegionCapturePreferences,
            request::{CaptureSnapshot, CaptureSource, Countdown},
            selection::{SelectionInput, SelectionModel, SelectionOutcome},
        },
        platform::windows::{
            capture::{NativeCountdown, WindowsCaptureSource},
            monitors::{DpiContext, enumerate_monitors, system_drag_threshold},
        },
    };
    use windows_sys::Win32::{
        Foundation::POINT,
        UI::WindowsAndMessaging::{GetPhysicalCursorPos, SetCursorPos},
    };
    let _dpi = DpiContext::enter().unwrap();
    let monitors = enumerate_monitors().unwrap();
    assert!(!monitors.is_empty());
    for m in &monitors {
        println!(
            "MONITOR {} {:?} scale={}",
            m.id(),
            m.bounds(),
            m.scale_factor()
        );
    }
    // SAFETY: preserve and restore the physical pointer after this isolated native check.
    let mut old = POINT::default();
    assert_ne!(unsafe { GetPhysicalCursorPos(&mut old) }, 0);
    struct PointerRestore(POINT);
    impl Drop for PointerRestore {
        fn drop(&mut self) {
            unsafe {
                SetCursorPos(self.0.x, self.0.y);
            }
        }
    }
    let _pointer = PointerRestore(old);
    for index in 0..monitors.len() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready.json");
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "native_synthetic_fixture",
                "--nocapture",
            ])
            .env("VECTRCAP_REGION_SMOKE_FILE", &ready)
            .env("VECTRCAP_REGION_SMOKE_MONITOR", index.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::File::create(dir.path().join("fixture.log")).unwrap(),
            ))
            .spawn()
            .unwrap();
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut _child = ChildGuard(child);
        let start = Instant::now();
        while !ready.exists() {
            if let Some(status) = _child.0.try_wait().unwrap() {
                panic!(
                    "fixture exited {status}: {}",
                    std::fs::read_to_string(dir.path().join("fixture.log")).unwrap()
                );
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "fixture did not start"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let bounds: Rect = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        let mut source = WindowsCaptureSource::new();
        let windows = source.windows().unwrap();
        println!("Eligible windows on probe {}: {}", index, windows.len());
        let target = *windows
            .iter()
            .find(|w| w.bounds == bounds)
            .expect("synthetic child window is eligible");
        let center = Point {
            x: bounds.left() + 100,
            y: bounds.top() + 100,
        };
        let pixels = source.window(target, false).unwrap();
        let s = CaptureSnapshot::new(pixels, bounds, monitors.clone(), windows.clone()).unwrap();
        assert_eq!(s.sample(center), Some([25, 180, 70, 255]));
        let mut model = SelectionModel::new(
            &s,
            RegionCapturePreferences::default(),
            center,
            system_drag_threshold().unwrap(),
        )
        .unwrap();
        assert_eq!(
            model.handle(SelectionInput::PointerMove(center)),
            SelectionOutcome::Hover(Some(bounds))
        );
        // A real native tracking tooltip appears after the fixture's timer. Wait on
        // the production countdown, hide/flush its chrome, then acquire only the fixture.
        let hidden = Cell::new(false);
        let mut countdown = NativeCountdown::new(|| {
            hidden.set(true);
            Ok(())
        });
        countdown.wait(1, &CancellationToken::new()).unwrap();
        countdown.hide_chrome().unwrap();
        assert!(hidden.get());
        let without_cursor = source.desktop(bounds, &monitors, false).unwrap();
        assert!(
            without_cursor
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .any(|(i, p)| i % (bounds.width() as usize) >= 160
                    && i / (bounds.width() as usize) < 85
                    && *p != [25, 180, 70, 255]),
            "native delayed tooltip must be in fresh pixels"
        );
        // Place the real source cursor on synthetic content; no personal pixels are read.
        assert_ne!(unsafe { SetCursorPos(center.x, center.y) }, 0);
        std::thread::sleep(Duration::from_millis(30));
        let with_cursor = source.desktop(bounds, &monitors, true).unwrap();
        let differences: Vec<_> = with_cursor
            .as_chunks::<4>()
            .0
            .iter()
            .zip(without_cursor.as_chunks::<4>().0)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect();
        assert!(
            !differences.is_empty(),
            "requested source cursor must be present"
        );
        assert!(
            differences.iter().all(|i| {
                let x = i % bounds.width() as usize;
                let y = i / bounds.width() as usize;
                (90..160).contains(&x) && (90..160).contains(&y)
            }),
            "source cursor must be at its physical hotspot"
        );
        println!(
            "PROBE {index}: own-source pixel [25,180,70,255], hover {bounds:?}, native delayed tooltip, cursor changed {} pixels",
            differences.len()
        );
    }
}

#[test]
#[ignore = "child process fixture for native_monitor_window_hover_and_snapshot_smoke"]
#[allow(unsafe_code)]
fn native_synthetic_fixture() {
    use vectrcap::platform::windows::monitors::{DpiContext, enumerate_monitors};
    use windows_sys::{
        Win32::{
            Foundation::*,
            Graphics::Gdi::*,
            UI::{Controls::*, WindowsAndMessaging::*},
        },
        core::w,
    };
    let Ok(path) = std::env::var("VECTRCAP_REGION_SMOKE_FILE") else {
        return;
    };
    let _dpi = DpiContext::enter().unwrap();
    let index = std::env::var("VECTRCAP_REGION_SMOKE_MONITOR")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let monitor = enumerate_monitors().unwrap()[index].bounds();
    // SAFETY: this test owns its class, window, GDI brush, and message loop.
    unsafe {
        struct TooltipState {
            hwnd: HWND,
            info: TTTOOLINFOW,
        }
        unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            unsafe {
                match msg {
                    WM_PAINT => {
                        let mut ps = PAINTSTRUCT::default();
                        let dc = BeginPaint(hwnd, &mut ps);
                        let brush = CreateSolidBrush(25 | (180 << 8) | (70 << 16));
                        FillRect(dc, &ps.rcPaint, brush);
                        DeleteObject(brush);
                        EndPaint(hwnd, &ps);
                        0
                    }
                    WM_TIMER => {
                        if w == 2 {
                            DestroyWindow(hwnd);
                            return 0;
                        }
                        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TooltipState;
                        if !state.is_null() {
                            let tooltip = &*state;
                            SendMessageW(
                                tooltip.hwnd,
                                TTM_TRACKACTIVATE,
                                1,
                                &tooltip.info as *const _ as LPARAM,
                            );
                        }
                        KillTimer(hwnd, 1);
                        0
                    }
                    WM_DESTROY => {
                        PostQuitMessage(0);
                        0
                    }
                    _ => DefWindowProcW(hwnd, msg, w, l),
                }
            }
        }
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: w!("VectrCapRegionSynthetic"),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&class), 0);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("VectrCapRegionSynthetic"),
            w!("VectrCap synthetic selection test"),
            WS_POPUP,
            monitor.left() + 40,
            monitor.top() + 40,
            320,
            240,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        assert!(!hwnd.is_null());
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        UpdateWindow(hwnd);
        let bounds = r(monitor.left() + 40, monitor.top() + 40, 320, 240);
        let tooltip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            w!(""),
            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
            0,
            0,
            0,
            0,
            hwnd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        assert!(!tooltip.is_null());
        let state = Box::new(TooltipState {
            hwnd: tooltip,
            info: TTTOOLINFOW {
                // Test executables have no comctl32-v6 manifest; use the v2 layout.
                cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                hwnd,
                uId: 1,
                lpszText: w!("Synthetic delayed tooltip") as *mut _,
                ..Default::default()
            },
        });
        assert_ne!(
            SendMessageW(tooltip, TTM_ADDTOOLW, 0, &state.info as *const _ as LPARAM),
            0
        );
        let position =
            ((bounds.left() + 160) as u16 as u32) | (((bounds.top() + 30) as u16 as u32) << 16);
        SendMessageW(tooltip, TTM_TRACKPOSITION, 0, position as LPARAM);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const _ as isize);
        assert_ne!(SetTimer(hwnd, 1, 300, None), 0);
        assert_ne!(SetTimer(hwnd, 2, 15000, None), 0);
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&bounds).unwrap()).unwrap();
        std::fs::rename(tmp, path).unwrap();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
