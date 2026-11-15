//! Physical monitor geometry. Enter PMv2 before creating any capture/overlay HWND.
use crate::{
    app::error::AppError,
    capture::geometry::{MonitorGeometry, PhysicalRect, validate_layout},
};
use std::{marker::PhantomData, rc::Rc};
use windows_sys::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
    core::w,
};

/// Thread-affine scope: windows created inside inherit per-monitor-v2 awareness.
/// Keep this guard on the overlay owner thread through window creation.
pub struct DpiContext {
    previous: DPI_AWARENESS_CONTEXT,
    _thread: PhantomData<Rc<()>>,
}
impl DpiContext {
    #[allow(unsafe_code)]
    pub fn enter() -> Result<Self, AppError> {
        // SAFETY: documented pseudo-handle; no owned resource is passed to Windows.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.is_null() {
            return Err(AppError::Capture(
                "per-monitor-v2 DPI awareness is unavailable",
            ));
        }
        Ok(Self {
            previous,
            _thread: PhantomData,
        })
    }
}
impl Drop for DpiContext {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: a guard cannot cross threads; restore the context returned on entry.
        unsafe {
            SetThreadDpiAwarenessContext(self.previous);
        }
    }
}
struct Enumeration {
    monitors: Vec<MonitorGeometry>,
    error: Option<AppError>,
}
#[allow(unsafe_code)]
unsafe extern "system" fn collect_monitor(
    handle: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> i32 {
    // SAFETY: EnumDisplayMonitors invokes this synchronously with the live context below.
    let state = unsafe { &mut *(data as *mut Enumeration) };
    let result = (|| {
        if state.monitors.len() >= 64 {
            return Err(AppError::Capture("too many attached monitors"));
        }
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo) } == 0 {
            return Err(AppError::Capture("monitor information is unavailable"));
        }
        let raw = info.monitorInfo.rcMonitor;
        let bounds = rect_from_win32(raw)?;
        let end = info
            .szDevice
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(info.szDevice.len());
        let id = String::from_utf16(&info.szDevice[..end])
            .map_err(|_| AppError::Capture("monitor identity is invalid"))?;
        // GetDpiForMonitor is not DPI-aware. A hidden PMv2 probe uses GetDpiForWindow,
        // the documented DPI-aware API, on the actual monitor without rendering chrome.
        let probe = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                bounds.left(),
                bounds.top(),
                1,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if probe.is_null() {
            return Err(AppError::Capture("monitor DPI probe could not be created"));
        }
        let dpi = unsafe { GetDpiForWindow(probe) };
        unsafe {
            DestroyWindow(probe);
        }
        if dpi == 0 {
            return Err(AppError::Capture("monitor DPI is unavailable"));
        }
        MonitorGeometry::new(id, bounds, f64::from(dpi) / 96.0)
    })();
    match result {
        Ok(m) => {
            state.monitors.push(m);
            1
        }
        Err(e) => {
            state.error = Some(e);
            0
        }
    }
}
pub(crate) fn rect_from_win32(rect: RECT) -> Result<PhysicalRect, AppError> {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return Err(AppError::Capture("native frame is empty"));
    }
    PhysicalRect::new(
        rect.left,
        rect.top,
        rect.left.abs_diff(rect.right),
        rect.top.abs_diff(rect.bottom),
    )
}
#[allow(unsafe_code)]
pub fn enumerate_monitors() -> Result<Vec<MonitorGeometry>, AppError> {
    let _dpi = DpiContext::enter()?;
    let mut state = Enumeration {
        monitors: vec![],
        error: None,
    };
    // SAFETY: callback and context remain valid for this synchronous enumeration.
    let ok = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect_monitor),
            &mut state as *mut _ as LPARAM,
        )
    };
    if let Some(error) = state.error {
        return Err(error);
    }
    if ok == 0 {
        return Err(AppError::Capture("monitor enumeration failed"));
    }
    validate_layout(&state.monitors)?;
    Ok(state.monitors)
}
/// Pin placement uses work areas, excluding taskbars and reserved app bars.
#[allow(unsafe_code)]
pub fn enumerate_work_areas() -> Result<Vec<PhysicalRect>, AppError> {
    let _dpi = DpiContext::enter()?;
    struct Areas {
        rects: Vec<PhysicalRect>,
        error: Option<AppError>,
    }
    unsafe extern "system" fn collect(handle: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
        let state = unsafe { &mut *(data as *mut Areas) };
        let result = (|| {
            if state.rects.len() >= 64 {
                return Err(AppError::Capture("too many pin work areas"));
            }
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetMonitorInfoW(handle, &mut info) } == 0 {
                return Err(AppError::Capture("pin monitor work area is unavailable"));
            }
            rect_from_win32(info.rcWork)
        })();
        match result {
            Ok(rect) => {
                state.rects.push(rect);
                1
            }
            Err(error) => {
                state.error = Some(error);
                0
            }
        }
    }
    let mut state = Areas {
        rects: vec![],
        error: None,
    };
    if unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            &mut state as *mut _ as LPARAM,
        )
    } == 0
        || state.rects.is_empty()
    {
        return Err(state
            .error
            .unwrap_or(AppError::Capture("pin monitor enumeration failed")));
    }
    Ok(state.rects)
}
pub fn system_drag_threshold() -> Result<(u32, u32), AppError> {
    system_drag_threshold_at(super::windows::physical_pointer()?)
}
/// Resolve the threshold for the session's launch monitor, even if the pointer
/// has moved to another DPI monitor by the time the overlay is created.
#[allow(unsafe_code)]
pub fn system_drag_threshold_at(
    pointer: crate::capture::geometry::PhysicalPoint,
) -> Result<(u32, u32), AppError> {
    let _dpi = DpiContext::enter()?;
    let monitors = enumerate_monitors()?;
    let scale = monitors
        .iter()
        .find(|m| m.bounds().contains(pointer))
        .ok_or(AppError::Capture(
            "drag launch point is outside the monitor layout",
        ))?
        .scale_factor();
    let dpi = (scale * 96.0).round() as u32;
    // SAFETY: GetSystemMetricsForDpi takes integer indices and has no borrowed pointers.
    // SM_CX/CYDRAG describe a rectangle centered on the press position.
    let (x, y) = unsafe {
        (
            GetSystemMetricsForDpi(SM_CXDRAG, dpi),
            GetSystemMetricsForDpi(SM_CYDRAG, dpi),
        )
    };
    if x <= 0 || y <= 0 {
        return Err(AppError::Capture("system drag threshold is unavailable"));
    }
    Ok(((x as u32).div_ceil(2), (y as u32).div_ceil(2)))
}
