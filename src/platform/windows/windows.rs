//! Session-owned identities; HWNDs, process IDs and live state never enter saved regions.
use super::monitors::{DpiContext, rect_from_win32};
use crate::{
    app::error::AppError,
    capture::{
        geometry::{PhysicalPoint, PhysicalRect},
        windows::{HoverArea, WindowCandidate, WindowId},
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
#[derive(Default, Clone)]
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
const MAX_CONTROLS: usize = 4096;
#[allow(unsafe_code)]
unsafe extern "system" fn collect_child(hwnd: HWND, data: LPARAM) -> i32 {
    // SAFETY: this pointer belongs to the synchronous EnumChildWindows call below.
    let children = unsafe { &mut *(data as *mut Vec<usize>) };
    children.push(hwnd as usize);
    (children.len() < MAX_CONTROLS) as i32
}
#[allow(unsafe_code)]
fn client_rect(hwnd: HWND) -> Option<PhysicalRect> {
    let mut r = RECT::default();
    let mut origin = POINT::default();
    // SAFETY: writable stack values; Win32 validates the transient HWND.
    unsafe {
        if GetClientRect(hwnd, &mut r) == 0
            || windows_sys::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut origin) == 0
        {
            return None;
        }
    }
    rect_from_win32(RECT {
        left: origin.x,
        top: origin.y,
        right: origin.x + r.right,
        bottom: origin.y + r.bottom,
    })
    .ok()
}
/// The window's title bar strip: from its top edge down to the bottom of the
/// caption buttons, which DWM reports even for app-drawn title bars.
#[allow(unsafe_code)]
fn caption_band(hwnd: HWND, frame: PhysicalRect) -> Option<PhysicalRect> {
    let mut buttons = RECT::default();
    let mut window = RECT::default();
    // SAFETY: writable stack values; Win32 validates the transient HWND.
    unsafe {
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_BUTTON_BOUNDS as u32,
            &mut buttons as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        ) < 0
            || GetWindowRect(hwnd, &mut window) == 0
        {
            return None;
        }
    }
    // Caption button bounds are relative to the window rect.
    let bottom = window.top + buttons.bottom;
    let height = bottom - frame.top();
    if buttons.bottom <= buttons.top || height <= 0 || height as u32 >= frame.height() / 2 {
        return None;
    }
    PhysicalRect::new(frame.left(), frame.top(), frame.width(), height as u32).ok()
}
/// ShareX skips a client area that is the whole window bar a thin border
/// (custom title bars); otherwise it would always win over the window.
fn same_as_frame(client: PhysicalRect, frame: PhysicalRect) -> bool {
    const TOLERANCE: i32 = 8;
    client.left() - frame.left() <= TOLERANCE
        && client.top() - frame.top() <= TOLERANCE
        && frame.right() - client.right() <= TOLERANCE
        && frame.bottom() - client.bottom() <= TOLERANCE
}
impl Windows {
    /// ShareX's window list with "detect controls": for each window in z-order,
    /// its visible child controls (smallest first, clipped to the window), its
    /// client area when it has a real title bar, then the window itself.
    #[allow(unsafe_code)]
    pub(crate) fn hover_areas(&self, candidates: &[WindowCandidate]) -> Vec<HoverArea> {
        let Ok(_dpi) = DpiContext::enter() else {
            return vec![];
        };
        let mut areas = vec![];
        for candidate in candidates {
            let frame = candidate.bounds;
            // Same fail-closed identity check as capture: skip a window whose
            // handle vanished, was recycled or moved since enumeration.
            let Ok(window) = self.resolve(*candidate) else {
                continue;
            };
            if areas.len() >= MAX_CONTROLS * 4 {
                break;
            }
            let hwnd = window.hwnd as HWND;
            let mut children: Vec<usize> = vec![];
            // SAFETY: the callback only touches `children`, which outlives the call.
            unsafe {
                EnumChildWindows(
                    hwnd,
                    Some(collect_child),
                    &mut children as *mut _ as LPARAM,
                )
            };
            let mut controls: Vec<PhysicalRect> = children
                .into_iter()
                .filter_map(|child| {
                    let child = child as HWND;
                    let mut r = RECT::default();
                    // SAFETY: Win32 validates the transient child HWND.
                    if unsafe { IsWindowVisible(child) == 0 || GetWindowRect(child, &mut r) == 0 } {
                        return None;
                    }
                    rect_from_win32(r).ok()?.intersection(frame)
                })
                .filter(|r| *r != frame)
                .collect();
            controls.sort_by_key(|r| u64::from(r.width()) * u64::from(r.height()));
            controls.dedup();
            // The title bar strip selects the whole window, even where an app
            // (Explorer's tabs) draws its own controls into it.
            if let Some(caption) = caption_band(hwnd, frame) {
                areas.push(HoverArea {
                    hit: caption,
                    select: frame,
                });
            }
            areas.extend(controls.into_iter().map(HoverArea::itself));
            if let Some(client) = client_rect(hwnd).and_then(|c| c.intersection(frame))
                && !same_as_frame(client, frame)
            {
                areas.push(HoverArea::itself(client));
            }
            areas.push(HoverArea::itself(frame));
        }
        areas
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
/// Region-capture arrow keys move the real cursor, so hover and the magnifier follow.
#[allow(unsafe_code)]
pub fn nudge_pointer(dx: i32, dy: i32) -> Result<(), AppError> {
    let p = physical_pointer()?;
    // SAFETY: plain value arguments; Windows clips the cursor to the desktop.
    if unsafe { SetPhysicalCursorPos(p.x.saturating_add(dx), p.y.saturating_add(dy)) } == 0 {
        return Err(AppError::Capture("pointer could not be moved"));
    }
    Ok(())
}
#[allow(unsafe_code)]
fn is_shell_window(hwnd: HWND) -> bool {
    let mut class = [0u16; 32];
    // SAFETY: writable fixed buffer; Win32 validates the transient HWND.
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
    matches!(
        class.as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW"
    )
}
impl Windows {
    /// ShareX's "active window": the foreground window. When that is the taskbar
    /// or desktop (e.g. launched from the tray), the topmost app window instead.
    /// `candidates` must come from the latest `enumerate`, in z-order.
    #[allow(unsafe_code)]
    pub(crate) fn active(&self, candidates: &[WindowCandidate]) -> Option<WindowCandidate> {
        let usable = |c: &&WindowCandidate| {
            self.entries
                .get(&c.id)
                .is_some_and(|w| !is_shell_window(w.hwnd as HWND))
        };
        // SAFETY: no arguments; the result is only compared, never dereferenced.
        let foreground = unsafe { GetAncestor(GetForegroundWindow(), GA_ROOT) } as usize;
        candidates
            .iter()
            .filter(usable)
            .find(|c| {
                self.entries
                    .get(&c.id)
                    .is_some_and(|w| w.hwnd == foreground)
            })
            .or_else(|| candidates.iter().find(usable))
            .copied()
    }
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

struct NativePinSystem;
impl crate::pin::manager::PinWindowSystem for NativePinSystem {
    #[allow(unsafe_code)]
    fn is_owned(&self, id: crate::pin::PinId, handle: usize) -> bool {
        let hwnd = handle as HWND;
        let mut pid = 0;
        let mut title = [0u16; 128];
        unsafe {
            if GetWindowThreadProcessId(hwnd, &mut pid) == 0 || pid != std::process::id() {
                return false;
            }
            let len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
            len > 0
                && String::from_utf16(&title[..len as usize]).ok().as_deref()
                    == Some(&crate::ui::pin_window::viewport_title(id))
        }
    }
    #[allow(unsafe_code)]
    fn exclude_from_capture(&mut self, handle: usize) -> bool {
        unsafe {
            let mut enabled = 0;
            let mut affinity = 0;
            DwmIsCompositionEnabled(&mut enabled) >= 0
                && enabled != 0
                && set_capture_affinity(handle as HWND, WDA_EXCLUDEFROMCAPTURE)
                && GetWindowDisplayAffinity(handle as HWND, &mut affinity) != 0
                && affinity == WDA_EXCLUDEFROMCAPTURE
        }
    }
    #[allow(unsafe_code)]
    fn is_visible(&self, handle: usize) -> bool {
        unsafe { IsWindowVisible(handle as HWND) != 0 }
    }
    #[allow(unsafe_code)]
    fn set_visible(&mut self, handle: usize, visible: bool) -> Result<(), AppError> {
        unsafe {
            ShowWindow(
                handle as HWND,
                if visible { SW_SHOWNOACTIVATE } else { SW_HIDE },
            );
            if (IsWindowVisible(handle as HWND) != 0) != visible {
                return Err(AppError::Capture("pin visibility change failed"));
            }
        }
        Ok(())
    }
    #[allow(unsafe_code)]
    fn flush(&mut self) -> Result<(), AppError> {
        if unsafe { DwmFlush() } < 0 {
            return Err(AppError::Capture(
                "pin hiding could not be synchronized with the desktop",
            ));
        }
        Ok(())
    }
}
fn native_pins()
-> &'static std::sync::Mutex<crate::pin::manager::PinWindowRegistry<NativePinSystem>> {
    static PINS: std::sync::OnceLock<
        std::sync::Mutex<crate::pin::manager::PinWindowRegistry<NativePinSystem>>,
    > = std::sync::OnceLock::new();
    PINS.get_or_init(|| {
        std::sync::Mutex::new(crate::pin::manager::PinWindowRegistry::new(NativePinSystem))
    })
}
pub fn pins_suspended() -> bool {
    native_pins().lock().map_or(true, |pins| pins.suspended())
}

// Track only affinity changes owned by VectrCap. GetWindowDisplayAffinity is
// documented to fail on ordinary non-layered windows, so it cannot inventory
// the otherwise-visible app windows for a utility snapshot.
const AFFINITY_PROPERTY: windows_sys::core::PCWSTR =
    windows_sys::core::w!("VectrCap.CaptureAffinity");
fn app_affinities() -> &'static std::sync::Mutex<HashMap<usize, (u32, u32)>> {
    static AFFINITIES: std::sync::OnceLock<std::sync::Mutex<HashMap<usize, (u32, u32)>>> =
        std::sync::OnceLock::new();
    AFFINITIES.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}
#[allow(unsafe_code)]
fn affinity_identity(hwnd: HWND, tid: u32) -> bool {
    let mut pid = 0;
    // Window properties disappear when the HWND is destroyed, preventing a
    // recycled handle on the same GUI thread from inheriting our policy.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid) == tid
            && pid == std::process::id()
            && GetPropW(hwnd, AFFINITY_PROPERTY) as usize == 1
    }
}
#[allow(unsafe_code)]
pub(crate) fn set_capture_affinity(hwnd: HWND, affinity: u32) -> bool {
    let Ok(mut states) = app_affinities().lock() else {
        return false;
    };
    let mut pid = 0;
    unsafe {
        let tid = GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != std::process::id() || tid == 0 {
            return false;
        }
        states.retain(|&handle, (tid, _)| affinity_identity(handle as HWND, *tid));
        // Publish ownership before changing affinity, so every successful native
        // change is restorable even when GetWindowDisplayAffinity is unsupported.
        if SetPropW(hwnd, AFFINITY_PROPERTY, 1usize as HANDLE) == 0 {
            return false;
        }
        if SetWindowDisplayAffinity(hwnd, affinity) == 0 {
            return false;
        }
        states.insert(hwnd as usize, (tid, affinity));
        true
    }
}
/// Include visible reference/app windows while retaining screenshot exclusion.
/// The pin registry lock serializes with pin registration and visibility.
#[allow(unsafe_code)]
pub(crate) fn with_utility_capture<T>(
    capture: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    let pins = native_pins()
        .lock()
        .map_err(|_| AppError::Capture("native pin registry unavailable"))?;
    if pins.suspended() {
        return Err(AppError::InUse(
            "another acquisition owns reference windows",
        ));
    }
    let mut states = app_affinities()
        .lock()
        .map_err(|_| AppError::Capture("window capture policy unavailable"))?;
    states.retain(|&handle, (tid, _)| affinity_identity(handle as HWND, *tid));
    struct Affinities(Vec<(HWND, u32, u32)>);
    impl Drop for Affinities {
        fn drop(&mut self) {
            for &(hwnd, tid, affinity) in &self.0 {
                // Restoration is secondary to the captured result. Future app
                // screenshot/recording capture also synchronously hides pins.
                unsafe {
                    if affinity_identity(hwnd, tid) && SetWindowDisplayAffinity(hwnd, affinity) == 0
                    {
                        eprintln!("restoring utility source window affinity failed");
                    }
                }
            }
        }
    }
    let mut restore = Affinities(vec![]);
    for (&handle, &(tid, affinity)) in states.iter() {
        let hwnd = handle as HWND;
        if affinity != WDA_NONE && unsafe { IsWindowVisible(hwnd) } != 0 {
            restore.0.push((hwnd, tid, affinity));
            if unsafe { SetWindowDisplayAffinity(hwnd, WDA_NONE) } == 0 {
                return Err(AppError::Capture(
                    "visible utility references could not be included",
                ));
            }
        }
    }
    if unsafe { DwmFlush() } < 0 {
        return Err(AppError::Capture(
            "utility source presentation could not be synchronized",
        ));
    }
    // The guard restores on success, failure and unwinding, never masking the
    // primary capture result with a secondary affinity-restoration error.
    let result = capture();
    drop(restore);
    drop(states);
    drop(pins);
    result
}
pub fn unregister_pin_window(id: crate::pin::PinId) {
    if let Ok(mut pins) = native_pins().lock()
        && let Err(error) = pins.retire(id)
    {
        eprintln!("retiring reference window failed: {error}");
    }
}
#[allow(unsafe_code)]
fn find_pin(id: crate::pin::PinId) -> Option<HWND> {
    let title: Vec<u16> = crate::ui::pin_window::viewport_title(id)
        .encode_utf16()
        .chain([0])
        .collect();
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    (!hwnd.is_null()).then_some(hwnd)
}
#[allow(unsafe_code)]
pub fn pin_window_scale(id: crate::pin::PinId) -> Option<f32> {
    find_pin(id).and_then(|hwnd| {
        let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
        (dpi > 0).then_some(dpi as f32 / 96.0)
    })
}
/// Registry mutex serializes visibility with capture startup. Viewports are
/// created hidden; only this boundary may show them. Position/size use PMv2
/// physical pixels, independently of egui's user interface zoom.
#[allow(unsafe_code)]
pub fn sync_pin_window(
    id: crate::pin::PinId,
    position: PhysicalPoint,
    size: (u32, u32),
    visible: bool,
    topmost: bool,
) -> Result<bool, AppError> {
    PhysicalRect::new(position.x, position.y, size.0, size.1)?;
    if size.0.max(size.1) > 32760 {
        return Err(AppError::InvalidInput(
            "pin viewport dimensions exceed native limits",
        ));
    }
    let Some(hwnd) = find_pin(id) else {
        return Ok(false);
    };
    let _dpi = DpiContext::enter()?;
    let mut pins = native_pins()
        .lock()
        .map_err(|_| AppError::Capture("native pin registry unavailable"))?;
    pins.register(id, hwnd as usize)?;
    let mut current = RECT::default();
    unsafe {
        if GetWindowRect(hwnd, &mut current) == 0 {
            return Err(AppError::Capture("pin geometry is unavailable"));
        }
        let changed = current.left != position.x
            || current.top != position.y
            || i64::from(current.right) - i64::from(current.left) != i64::from(size.0)
            || i64::from(current.bottom) - i64::from(current.top) != i64::from(size.1);
        if changed
            && SetWindowPos(
                hwnd,
                if topmost {
                    HWND_TOPMOST
                } else {
                    HWND_NOTOPMOST
                },
                position.x,
                position.y,
                size.0 as i32,
                size.1 as i32,
                SWP_NOACTIVATE,
            ) == 0
        {
            return Err(AppError::Capture("pin placement failed"));
        }
    }
    pins.set_visible(id, visible)?;
    Ok(true)
}

/// Holds synchronous hiding through every native snapshot/recording exit path,
/// including acquisition failure, cancellation and unwinding. Affinity is
/// verified at registration, but hiding also covers GDI/legacy capture paths.
pub struct PinCaptureGuard {
    active: bool,
}
impl PinCaptureGuard {
    pub fn begin() -> Result<Self, AppError> {
        native_pins()
            .lock()
            .map_err(|_| AppError::Capture("native pin registry unavailable"))?
            .suspend_for_capture()?;
        Ok(Self { active: true })
    }
    pub fn restore(mut self) -> Result<(), AppError> {
        self.active = false;
        native_pins()
            .lock()
            .map_err(|_| AppError::Capture("native pin registry unavailable"))?
            .restore_after_capture()
    }
}
impl Drop for PinCaptureGuard {
    fn drop(&mut self) {
        if self.active {
            let result = native_pins()
                .lock()
                .map_err(|_| AppError::Capture("native pin registry unavailable"))
                .and_then(|mut pins| pins.restore_after_capture());
            if let Err(error) = result {
                eprintln!("restoring reference windows after capture failed: {error}");
            }
        }
    }
}

/// Original HWND/PID/TID and display layout; never resolves a replacement target.
pub struct ScrollingTarget {
    windows: Windows,
    candidate: WindowCandidate,
    viewport: PhysicalRect,
    monitor: crate::capture::geometry::MonitorGeometry,
}
impl ScrollingTarget {
    pub fn new(
        windows: Windows,
        candidate: WindowCandidate,
        viewport: PhysicalRect,
        monitor: crate::capture::geometry::MonitorGeometry,
    ) -> Result<Self, AppError> {
        if !candidate.bounds.contains_rect(viewport) || !monitor.bounds().contains_rect(viewport) {
            return Err(AppError::Capture(
                "scroll viewport must stay inside the original window and one monitor",
            ));
        }
        windows.resolve(candidate)?;
        Ok(Self {
            windows,
            candidate,
            viewport,
            monitor,
        })
    }
    pub(crate) fn native(&self) -> Result<NativeWindow, AppError> {
        self.windows.resolve(self.candidate)
    }
    #[allow(unsafe_code)]
    pub fn identity(&self) -> Result<crate::scrolling::TargetIdentity, AppError> {
        let native = self.native()?;
        let monitors = super::monitors::enumerate_monitors()?;
        if !monitors.iter().any(|m| m == &self.monitor) {
            return Err(AppError::Capture("scroll display geometry changed"));
        }
        Ok(crate::scrolling::TargetIdentity {
            target: self.candidate.id,
            monitor_id: self.monitor.id().to_owned(),
            window_rect: self.candidate.bounds,
            viewport: self.viewport,
            focused: unsafe { GetForegroundWindow() as usize == native.hwnd },
        })
    }
    pub fn options(&self) -> crate::scrolling::ScrollOptions {
        crate::scrolling::ScrollOptions {
            target: self.candidate.id,
            monitor_id: self.monitor.id().to_owned(),
            rect: self.viewport,
            maximum_duration: std::time::Duration::from_secs(600),
            maximum_segments: 1000,
        }
    }
}
