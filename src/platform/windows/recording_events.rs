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

/// The red line around a recording: four plain Win32 popups just outside the
/// recorded area, filled by their class brush. No egui/GL surface, so no
/// Windows 11 frame or rounded corners, nothing to paint, and clicks pass
/// through. Excluded from capture too (they are outside the area anyway).
/// Owned by the GUI thread, whose message loop serves them; dropped = gone.
pub struct RecordingBoundary(Vec<usize>);
impl RecordingBoundary {
    #[allow(unsafe_code)]
    pub fn show(area: PhysicalRect) -> Result<Self, AppError> {
        use windows_sys::Win32::{
            Graphics::{Dwm::*, Gdi::CreateSolidBrush},
            System::LibraryLoader::GetModuleHandleW,
        };
        const T: i32 = 2;
        let class: Vec<u16> = "VectrCapRecordingBoundary\0".encode_utf16().collect();
        let (l, t, r, b) = (area.left(), area.top(), area.right(), area.bottom());
        let (w, h) = (area.width() as i32, area.height() as i32);
        let mut strips = Self(Vec::with_capacity(4));
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            static REGISTERED: std::sync::Once = std::sync::Once::new();
            REGISTERED.call_once(|| {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(DefWindowProcW),
                    hInstance: instance,
                    hbrBackground: CreateSolidBrush(0x0040_40E8), // RGB(232, 64, 64)
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&wc);
            });
            for (x, y, sw, sh) in [
                (l - T, t - T, w + 2 * T, T),
                (l - T, b, w + 2 * T, T),
                (l - T, t, T, h),
                (r, t, T, h),
            ] {
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_NOACTIVATE
                        | WS_EX_LAYERED
                        | WS_EX_TRANSPARENT,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP,
                    x,
                    y,
                    sw,
                    sh,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    return Err(AppError::Capture("recording boundary could not be created"));
                }
                strips.0.push(hwnd as usize);
                // Layered for click-through; a layered window needs an opacity to be drawn.
                SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
                let square = DWMWCP_DONOTROUND;
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                    &square as *const _ as *const _,
                    std::mem::size_of_val(&square) as u32,
                );
                let _ = super::windows::set_capture_affinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        Ok(strips)
    }
}
impl Drop for RecordingBoundary {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        for &hwnd in &self.0 {
            // SAFETY: our own windows, destroyed on the thread that created them.
            unsafe { DestroyWindow(hwnd as _) };
        }
    }
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
