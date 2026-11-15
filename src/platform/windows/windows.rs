//! Session-owned identities; HWNDs, process IDs and live state never enter saved regions.
use super::monitors::{DpiContext, rect_from_win32};
use crate::{
    app::error::AppError,
    capture::{
        geometry::{PhysicalPoint, PhysicalRect},
        windows::{WindowCandidate, WindowId},
    },
};
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
};
use windows_sys::Win32::{Foundation::*, Graphics::Dwm::*, UI::WindowsAndMessaging::*};
const MAX_WINDOWS: usize = 4096;
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy)]
pub(crate) struct NativeWindow {
    pub hwnd: usize,
    pub pid: u32,
    pub tid: u32,
    pub bounds: PhysicalRect,
}
#[derive(Default)]
pub struct Windows {
    entries: HashMap<WindowId, NativeWindow>,
}
#[allow(unsafe_code)]
fn read_window(hwnd: HWND) -> Option<NativeWindow> {
    // SAFETY: Win32 validates these borrowed HWNDs, which may disappear at any time.
    // Fail closed on DWM errors; never fall back to invisible resize margins.
    unsafe {
        if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
            return None;
        }
        let mut pid = 0;
        let tid = GetWindowThreadProcessId(hwnd, &mut pid);
        if tid == 0 || pid == 0 || pid == std::process::id() {
            return None;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        ) < 0
            || cloaked != 0
        {
            return None;
        }
        let mut frame = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            &mut frame as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        ) < 0
        {
            return None;
        }
        Some(NativeWindow {
            hwnd: hwnd as usize,
            pid,
            tid,
            bounds: rect_from_win32(frame).ok()?,
        })
    }
}
struct Inventory {
    handles: HashSet<usize>,
    overflow: bool,
}
#[allow(unsafe_code)]
unsafe extern "system" fn collect_window(hwnd: HWND, data: LPARAM) -> i32 {
    // SAFETY: this pointer belongs to the synchronous EnumWindows call below.
    let inventory = unsafe { &mut *(data as *mut Inventory) };
    if inventory.handles.len() >= MAX_WINDOWS {
        inventory.overflow = true;
        return 0;
    }
    inventory.handles.insert(hwnd as usize);
    1
}
impl Windows {
    #[allow(unsafe_code)]
    pub fn enumerate(&mut self) -> Result<Vec<WindowCandidate>, AppError> {
        let _dpi = DpiContext::enter()?;
        let mut inventory = Inventory {
            handles: HashSet::new(),
            overflow: false,
        };
        // SAFETY: context remains live until all synchronous callbacks return.
        if unsafe { EnumWindows(Some(collect_window), &mut inventory as *mut _ as LPARAM) } == 0
            || inventory.overflow
        {
            return Err(AppError::Capture(
                "window enumeration failed or exceeded its limit",
            ));
        }
        let mut next = HashMap::new();
        let mut candidates = vec![];
        let mut visited = HashSet::new();
        // Enumerate inventory first, then walk documented z-order links with a hard bound
        // and cycle detection, so destruction/reordering can never cause an infinite loop.
        let mut hwnd = unsafe { GetTopWindow(std::ptr::null_mut()) };
        while !hwnd.is_null() {
            if visited.len() >= MAX_WINDOWS || !visited.insert(hwnd as usize) {
                return Err(AppError::Capture(
                    "window z-order changed during enumeration",
                ));
            }
            if inventory.handles.contains(&(hwnd as usize))
                && let Some(window) = read_window(hwnd)
            {
                let existing = self
                    .entries
                    .iter()
                    .find(|(_, old)| {
                        old.hwnd == window.hwnd && old.pid == window.pid && old.tid == window.tid
                    })
                    .map(|(id, _)| *id);
                let id = match existing {
                    Some(id) => id,
                    None => WindowId::new(
                        NEXT_TOKEN
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                                n.checked_add(1)
                            })
                            .map_err(|_| AppError::Capture("window token space is exhausted"))?,
                    )?,
                };
                candidates.push(WindowCandidate {
                    id,
                    bounds: window.bounds,
                });
                next.insert(id, window);
            }
            // SAFETY: GetWindow accepts a transient HWND and returns null when it vanished.
            hwnd = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
        }
        self.entries = next;
        Ok(candidates)
    }
    pub(crate) fn resolve(&self, target: WindowCandidate) -> Result<NativeWindow, AppError> {
        let _dpi = DpiContext::enter()?;
        let saved = self.entries.get(&target.id).ok_or(AppError::Capture(
            "window token is not owned by this capture source",
        ))?;
        let current = read_window(saved.hwnd as HWND)
            .ok_or(AppError::Capture("selected window is unavailable"))?;
        if current.pid != saved.pid || current.tid != saved.tid || current.bounds != target.bounds {
            return Err(AppError::Capture(
                "selected window changed; select it again",
            ));
        }
        Ok(current)
    }
}
#[allow(unsafe_code)]
pub fn physical_pointer() -> Result<PhysicalPoint, AppError> {
    let mut p = POINT::default();
    // SAFETY: writable stack point; this API explicitly returns physical screen pixels.
    if unsafe { GetPhysicalCursorPos(&mut p) } == 0 {
        return Err(AppError::Capture(
            "physical pointer position is unavailable",
        ));
    }
    Ok(PhysicalPoint { x: p.x, y: p.y })
}
#[allow(unsafe_code)]
pub(crate) fn window_rect(window: NativeWindow) -> Result<PhysicalRect, AppError> {
    let mut r = RECT::default();
    // SAFETY: Win32 validates the transient HWND; DpiContext is held by the caller.
    if unsafe { GetWindowRect(window.hwnd as HWND, &mut r) } == 0 {
        return Err(AppError::Capture("selected window frame is unavailable"));
    }
    rect_from_win32(r)
}
