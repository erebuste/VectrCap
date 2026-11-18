//! Typed Windows interruption policy; unlock/resume notifications do not restart acquisition.
use crate::{
    app::error::AppError, capture::geometry::PhysicalRect, recording::session::Interruption,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
pub fn interruption_for_message(message: u32, wparam: usize) -> Option<Interruption> {
    match message {
        WM_DISPLAYCHANGE => Some(Interruption::DisplayChanged),
        WM_POWERBROADCAST if wparam == PBT_APMSUSPEND as usize => Some(Interruption::Sleep),
        WM_WTSSESSION_CHANGE if matches!(wparam, n if n==WTS_SESSION_LOCK as usize||n==WTS_SESSION_LOGOFF as usize) => {
            Some(Interruption::Locked)
        }
        _ => None,
    }
}
/// Called on the GUI thread, after eframe has created the named viewport.
/// Reapply physical placement independently of egui's DPI/chrome zoom. All
/// recording chrome is geometrically outside the capture even without affinity.
#[allow(unsafe_code)]
pub fn place_overlay(title: &str, bounds: PhysicalRect) -> Result<(), AppError> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
        if hwnd.is_null() {
            return Ok(());
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != std::process::id() {
            return Err(AppError::Capture("recording overlay identity changed"));
        }
        // Best effort only; strict geometric exclusion remains the fallback.
        let _ = super::windows::set_capture_affinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        if SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            bounds.left(),
            bounds.top(),
            bounds.width() as i32,
            bounds.height() as i32,
            SWP_NOACTIVATE,
        ) == 0
        {
            return Err(AppError::Capture(
                "recording controls could not be placed outside the selected area",
            ));
        }
    }
    Ok(())
}

/// Clicks on this window of ours no longer take focus from the app behind it.
#[allow(unsafe_code)]
pub fn never_activate(title: &str) {
    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
        let mut pid = 0;
        if hwnd.is_null()
            || GetWindowThreadProcessId(hwnd, &mut pid) == 0
            || pid != std::process::id()
        {
            return;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if style & WS_EX_NOACTIVATE as isize == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE as isize);
        }
    }
}

/// Hands the foreground to `hwnd`. Allowed while one of our windows (the
/// selection overlay) holds it; best effort otherwise.
#[allow(unsafe_code)]
pub fn focus_window(hwnd: usize) {
    unsafe {
        SetForegroundWindow(hwnd as _);
    }
}

#[allow(unsafe_code)]
pub fn hide_selection(title: &str) -> Result<(), AppError> {
    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
        if !hwnd.is_null() {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != std::process::id() {
                return Err(AppError::Capture("selection window identity changed"));
            }
            ShowWindow(hwnd, SW_HIDE);
            if IsWindowVisible(hwnd) != 0 {
                return Err(AppError::Capture("recording selection could not be hidden"));
            }
            if windows_sys::Win32::Graphics::Dwm::DwmFlush() < 0 {
                return Err(AppError::Capture(
                    "recording selection compositor synchronization failed",
                ));
            }
        }
    }
    Ok(())
}
