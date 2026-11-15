//! Native still acquisition. Desktop regions use GDI intersections; explicit Window
//! uses that window's WGC source, never a substituted desktop image. Preparing
//! WGC initializes one process-lifetime MTA lease for repeated capture sessions.
use super::{
    monitors::{DpiContext, enumerate_monitors},
    windows::{Windows, physical_pointer, window_rect},
};
use crate::{
    app::error::AppError,
    capture::{
        geometry::{MonitorGeometry, PhysicalPoint, PhysicalRect, allocate_rgba, validate_layout},
        request::{CaptureSource, Countdown},
        windows::WindowCandidate,
    },
};
use std::{
    sync::mpsc::{self, SyncSender},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};
use windows_sys::Win32::{
    Graphics::{Dwm::DwmFlush, Gdi::*},
    UI::WindowsAndMessaging::*,
};
#[derive(Default)]
pub struct WindowsCaptureSource {
    windows: Windows,
    prepared: Option<(WindowCandidate, PreparedWindowItem)>,
    // Declared after the WinRT item so it drops after all interfaces are released.
    apartment: Option<Apartment>,
}
// Keep only the Send window variant. The library's enum also contains a
// thread-affine picker HWND variant, so the enum itself must not cross threads.
struct PreparedWindowItem {
    item: windows::Graphics::Capture::GraphicsCaptureItem,
    window: Window,
}
impl From<PreparedWindowItem> for GraphicsCaptureItemType {
    fn from(value: PreparedWindowItem) -> Self {
        Self::Window((value.item, value.window))
    }
}
struct Apartment {
    owned: bool,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
struct ProcessMta(usize);
impl Drop for ProcessMta {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: this owns the cookie returned by CoIncrementMTAUsage. The API
        // allows releasing on any thread; a static retains it until process exit.
        unsafe {
            windows_sys::Win32::System::Com::CoDecrementMTAUsage(self.0 as _);
        }
    }
}
/// WGC activation factories are process-cached. Keep the implicit MTA alive across
/// source destruction/recreation, including the gaps between capture workers.
/// This owns one COM usage lease, never a window, frame, or capture session.
#[allow(unsafe_code)]
pub fn initialize_capture_runtime() -> Result<(), AppError> {
    static MTA: std::sync::Mutex<Option<ProcessMta>> = std::sync::Mutex::new(None);
    ensure_runtime_lease(&MTA, || {
        let mut cookie = std::ptr::null_mut();
        // SAFETY: a writable local cookie output; the successful lease is owned above.
        if unsafe { windows_sys::Win32::System::Com::CoIncrementMTAUsage(&mut cookie) } < 0 {
            return Err(AppError::Capture("capture MTA initialization failed"));
        }
        Ok(ProcessMta(cookie as usize))
    })
}
fn ensure_runtime_lease<T>(
    lease: &std::sync::Mutex<Option<T>>,
    initialize: impl FnOnce() -> Result<T, AppError>,
) -> Result<(), AppError> {
    let mut lease = lease
        .lock()
        .map_err(|_| AppError::Capture("capture runtime initialization lock is poisoned"))?;
    // Serialize acquisition as well as publication: racing retries must not
    // increment COM usage twice. Errors leave no lease and may be retried.
    if lease.is_none() {
        *lease = Some(initialize()?);
    }
    Ok(())
}

impl Apartment {
    #[allow(unsafe_code)]
    fn enter() -> Result<Self, AppError> {
        initialize_capture_runtime()?;
        // windows-sys 0.61 has no WinRT module; use the already-pinned WGC projection.
        use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
        // SAFETY: initializes this calling thread only. An existing STA is retained.
        let owned = match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => true,
            Err(error) if error.code().0 == 0x80010106u32 as i32 => false,
            Err(_) => return Err(AppError::Capture("capture runtime initialization failed")),
        };
        Ok(Self {
            owned,
            _thread: std::marker::PhantomData,
        })
    }
}
impl Drop for Apartment {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: !Send keeps the balanced uninitialization on the entering thread.
        if self.owned {
            unsafe {
                windows::Win32::System::WinRT::RoUninitialize();
            }
        }
    }
}
impl WindowsCaptureSource {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Dedicated acquisition worker entry. All native adapters are created and
/// dropped on this thread; cancellation never produces an output artifact.
pub fn acquire_screenshot(
    mut request: crate::capture::request::CaptureRequest,
    cancel: CancellationToken,
) -> Result<crate::capture::request::CaptureSnapshot, AppError> {
    use crate::capture::request::{CaptureMode, CaptureService};
    let pins = super::windows::PinCaptureGuard::begin()?;
    let mut source = WindowsCaptureSource::new();
    if request.mode == CaptureMode::Window {
        let pointer = source.pointer()?;
        request.target = Some(
            source
                .windows()?
                .into_iter()
                .find(|w| w.bounds.contains(pointer))
                .ok_or(AppError::Capture("no eligible window under the pointer"))?,
        );
    }
    let result = CaptureService::new(source, NativeCountdown::new(hide_app_chrome), cancel)
        .snapshot(request);
    pins.restore()?;
    result
}

#[allow(unsafe_code)]
fn hide_app_chrome() -> Result<(), AppError> {
    unsafe extern "system" fn hide(hwnd: windows_sys::Win32::Foundation::HWND, _: isize) -> i32 {
        let mut pid = 0;
        // SAFETY: EnumWindows lends each handle; Win32 tolerates destroyed windows.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == std::process::id() && IsWindowVisible(hwnd) != 0 {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
        1
    }
    // SAFETY: synchronous callback; no pointers or retained native handles.
    if unsafe { EnumWindows(Some(hide), 0) } == 0 {
        return Err(AppError::Capture("capture chrome could not be hidden"));
    }
    Ok(())
}
impl CaptureSource for WindowsCaptureSource {
    fn prepare_window(&mut self, target: WindowCandidate) -> Result<(), AppError> {
        let _dpi = DpiContext::enter()?;
        let native = self.windows.resolve(target)?;
        if self.apartment.is_none() {
            self.apartment = Some(Apartment::enter()?);
        }
        // The retained GraphicsCaptureItem refers to the original window object.
        // A recycled HWND after the countdown cannot retarget it to a new window.
        let item: GraphicsCaptureItemType = Window::from_raw_hwnd(native.hwnd as *mut _)
            .try_into()
            .map_err(|_| AppError::Capture("selected window source is unavailable"))?;
        let GraphicsCaptureItemType::Window((item, window)) = item else {
            return Err(AppError::Capture("selected source is not a window"));
        };
        self.prepared = Some((target, PreparedWindowItem { item, window }));
        Ok(())
    }
    fn monitors(&mut self) -> Result<Vec<MonitorGeometry>, AppError> {
        enumerate_monitors()
    }
    fn pointer(&mut self) -> Result<PhysicalPoint, AppError> {
        physical_pointer()
    }
    fn windows(&mut self) -> Result<Vec<WindowCandidate>, AppError> {
        self.windows.enumerate()
    }
    fn desktop(
        &mut self,
        bounds: PhysicalRect,
        monitors: &[MonitorGeometry],
        cursor: bool,
    ) -> Result<Vec<u8>, AppError> {
        let _dpi = DpiContext::enter()?;
        validate_layout(monitors)?;
        let mut result = allocate_rgba(bounds)?;
        let mut any = false;
        for monitor in monitors {
            if let Some(part) = bounds.intersection(monitor.bounds()) {
                any = true;
                let pixels = capture_region(part, cursor)?;
                let dx = bounds.left().abs_diff(part.left()) as usize;
                let dy = bounds.top().abs_diff(part.top()) as usize;
                for row in 0..part.height() as usize {
                    let dst = ((dy + row) * bounds.width() as usize + dx) * 4;
                    let src = row * part.width() as usize * 4;
                    let len = part.width() as usize * 4;
                    result[dst..dst + len].copy_from_slice(&pixels[src..src + len]);
                }
            }
        }
        if !any {
            return Err(AppError::Capture(
                "capture source contains only monitor gaps",
            ));
        }
        Ok(result)
    }
    fn window(&mut self, target: WindowCandidate, cursor: bool) -> Result<Vec<u8>, AppError> {
        let (prepared, item) = self
            .prepared
            .take()
            .ok_or(AppError::Capture("window source was not prepared"))?;
        let _dpi = DpiContext::enter()?;
        if prepared != target {
            return Err(AppError::Capture(
                "prepared window target changed; select again",
            ));
        }
        let native = self.windows.resolve(target)?;
        let raw = window_rect(native)?;
        target.bounds.rgba_len()?;
        raw.rgba_len()?;
        let (send, receive) = mpsc::sync_channel(1);
        let settings = Settings::new(
            item,
            if cursor {
                CursorCaptureSettings::WithCursor
            } else {
                CursorCaptureSettings::WithoutCursor
            },
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Exclude,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            StillFlags {
                send,
                visible: target.bounds,
                raw,
            },
        );
        let control = StillHandler::start_free_threaded(settings)
            .map_err(|_| AppError::Capture("window source capture could not start"))?;
        let result = receive
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| AppError::Capture("window source did not supply a frame"));
        let stopped = control
            .stop()
            .map_err(|_| AppError::Capture("window source shutdown failed"));
        stopped?;
        self.windows.resolve(target)?;
        result?
    }
}
struct StillFlags {
    send: SyncSender<Result<Vec<u8>, AppError>>,
    visible: PhysicalRect,
    raw: PhysicalRect,
}
struct StillHandler {
    flags: StillFlags,
}
impl GraphicsCaptureApiHandler for StillHandler {
    type Flags = StillFlags;
    type Error = AppError;
    fn new(context: Context<StillFlags>) -> Result<Self, AppError> {
        Ok(Self {
            flags: context.flags,
        })
    }
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), AppError> {
        let result = self.copy_frame(frame);
        let _ = self.flags.send.try_send(result);
        control.stop();
        Ok(())
    }
    fn on_closed(&mut self) -> Result<(), AppError> {
        let _ = self
            .flags
            .send
            .try_send(Err(AppError::Capture("window source closed")));
        Ok(())
    }
}
impl StillHandler {
    fn copy_frame(&self, frame: &mut Frame) -> Result<Vec<u8>, AppError> {
        let visible = self.flags.visible;
        let raw = self.flags.raw;
        // WGC's visible-frame and full-window representations are accepted only
        // when they map exactly; unexpected sizes require reselection.
        let source = if (frame.width(), frame.height()) == (visible.width(), visible.height()) {
            PhysicalRect::new(0, 0, visible.width(), visible.height())?
        } else if (frame.width(), frame.height()) == (raw.width(), raw.height())
            && raw.contains_rect(visible)
        {
            PhysicalRect::new(
                visible.left() - raw.left(),
                visible.top() - raw.top(),
                visible.width(),
                visible.height(),
            )?
        } else {
            return Err(AppError::Capture(
                "window source geometry changed or is unsupported",
            ));
        };
        let mut buffer = frame
            .buffer_crop(
                source.left() as u32,
                source.top() as u32,
                source.right() as u32,
                source.bottom() as u32,
            )
            .map_err(|_| AppError::Capture("window pixels could not be read"))?;
        let pitch = buffer.row_pitch() as usize;
        let bytes = buffer.as_raw_buffer();
        let row_bytes = visible.width() as usize * 4;
        if pitch < row_bytes || bytes.len() < pitch * (visible.height() as usize - 1) + row_bytes {
            return Err(AppError::Capture("window source row layout is invalid"));
        }
        let mut out = allocate_rgba(visible)?;
        for row in 0..visible.height() as usize {
            out[row * row_bytes..(row + 1) * row_bytes]
                .copy_from_slice(&bytes[row * pitch..row * pitch + row_bytes]);
        }
        Ok(out)
    }
}

struct Surface {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
}
impl Surface {
    #[allow(unsafe_code)]
    fn new(bounds: PhysicalRect) -> Result<Self, AppError> {
        bounds.rgba_len()?;
        let mut surface = Self {
            screen: std::ptr::null_mut(),
            memory: std::ptr::null_mut(),
            bitmap: std::ptr::null_mut(),
            previous: std::ptr::null_mut(),
            bits: std::ptr::null_mut(),
        };
        // SAFETY: each successful allocation is installed immediately in this RAII
        // owner. DIB size is checked before Windows or Rust allocates its pixels.
        unsafe {
            surface.screen = GetDC(std::ptr::null_mut());
            if surface.screen.is_null() {
                return Err(AppError::Capture("desktop context is unavailable"));
            }
            surface.memory = CreateCompatibleDC(surface.screen);
            if surface.memory.is_null() {
                return Err(AppError::Capture("capture context allocation failed"));
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: bounds.width() as i32,
                    biHeight: -(bounds.height() as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            surface.bitmap = CreateDIBSection(
                surface.screen,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if surface.bitmap.is_null() || bits.is_null() {
                return Err(AppError::Capture("capture bitmap allocation failed"));
            }
            surface.bits = bits.cast();
            surface.previous = SelectObject(surface.memory, surface.bitmap);
            if surface.previous.is_null() || surface.previous as isize == -1 {
                surface.previous = std::ptr::null_mut();
                return Err(AppError::Capture("capture bitmap selection failed"));
            }
        }
        Ok(surface)
    }
}
impl Drop for Surface {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: release in reverse dependency order, including partial construction.
        unsafe {
            if !self.previous.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(std::ptr::null_mut(), self.screen);
            }
        }
    }
}
#[allow(unsafe_code)]
fn capture_region(bounds: PhysicalRect, cursor: bool) -> Result<Vec<u8>, AppError> {
    let surface = Surface::new(bounds)?;
    let mut out = allocate_rgba(bounds)?;
    // SAFETY: both contexts and selected DIB outlive all drawing and readback.
    unsafe {
        if BitBlt(
            surface.memory,
            0,
            0,
            bounds.width() as i32,
            bounds.height() as i32,
            surface.screen,
            bounds.left(),
            bounds.top(),
            SRCCOPY | CAPTUREBLT,
        ) == 0
        {
            return Err(AppError::Capture("desktop pixel acquisition failed"));
        }
        if cursor {
            draw_cursor(surface.memory, bounds)?;
        }
        if GdiFlush() == 0 {
            return Err(AppError::Capture("capture pixel readback failed"));
        }
        let bytes = std::slice::from_raw_parts(surface.bits, out.len());
        for (rgba, bgra) in out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(bytes.as_chunks::<4>().0)
        {
            rgba.copy_from_slice(&[bgra[2], bgra[1], bgra[0], 255]);
        }
    }
    Ok(out)
}
struct Icon {
    handle: HICON,
    mask: HBITMAP,
    color: HBITMAP,
}
impl Drop for Icon {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: GetIconInfo returns owned bitmaps; CopyIcon returns an owned icon.
        unsafe {
            if !self.mask.is_null() {
                DeleteObject(self.mask);
            }
            if !self.color.is_null() {
                DeleteObject(self.color);
            }
            if !self.handle.is_null() {
                DestroyIcon(self.handle);
            }
        }
    }
}
#[allow(unsafe_code)]
fn draw_cursor(dc: HDC, bounds: PhysicalRect) -> Result<(), AppError> {
    // SAFETY: stack outputs are writable, icon/bitmaps have a scoped owner, and
    // DrawIconEx clips negative source-relative coordinates to the selected DIB.
    unsafe {
        let mut cursor = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if GetCursorInfo(&mut cursor) == 0 {
            return Err(AppError::Capture("source cursor is unavailable"));
        }
        if cursor.flags & CURSOR_SHOWING == 0 {
            return Ok(());
        }
        let mut owned = Icon {
            handle: CopyIcon(cursor.hCursor),
            mask: std::ptr::null_mut(),
            color: std::ptr::null_mut(),
        };
        if owned.handle.is_null() {
            return Err(AppError::Capture("source cursor could not be copied"));
        }
        let mut info = ICONINFO::default();
        if GetIconInfo(owned.handle, &mut info) == 0 {
            return Err(AppError::Capture("source cursor hotspot is unavailable"));
        }
        owned.mask = info.hbmMask;
        owned.color = info.hbmColor;
        let x =
            i64::from(cursor.ptScreenPos.x) - i64::from(info.xHotspot) - i64::from(bounds.left());
        let y =
            i64::from(cursor.ptScreenPos.y) - i64::from(info.yHotspot) - i64::from(bounds.top());
        let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
            return Ok(());
        };
        if DrawIconEx(
            dc,
            x,
            y,
            owned.handle,
            0,
            0,
            0,
            std::ptr::null_mut(),
            DI_NORMAL,
        ) == 0
        {
            return Err(AppError::Capture("source cursor could not be drawn"));
        }
    }
    Ok(())
}

/// Run on a capture worker, never the shell's UI thread. The callback hides all
/// caller-owned chrome synchronously; DwmFlush then waits for its presentation.
pub struct NativeCountdown<F> {
    hide: F,
}
impl<F: FnMut() -> Result<(), AppError>> NativeCountdown<F> {
    pub fn new(hide: F) -> Self {
        Self { hide }
    }
}
impl<F: FnMut() -> Result<(), AppError>> Countdown for NativeCountdown<F> {
    fn wait(&mut self, seconds: u8, cancel: &CancellationToken) -> Result<(), AppError> {
        if seconds > 60 {
            return Err(AppError::InvalidInput(
                "screenshot delay exceeds 60 seconds",
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(u64::from(seconds));
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Cancelled);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
    #[allow(unsafe_code)]
    fn hide_chrome(&mut self) -> Result<(), AppError> {
        (self.hide)()?;
        // SAFETY: DwmFlush takes no pointers and flushes the compositor presentation.
        if unsafe { DwmFlush() } < 0 {
            return Err(AppError::Capture(
                "countdown presentation could not be hidden",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn capture_runtime_retries_failure_and_retains_success() {
        let lease = Default::default();
        let attempts = AtomicUsize::new(0);
        let failure = AppError::Capture("injected transient MTA failure");
        assert_eq!(
            ensure_runtime_lease(&lease, || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<usize, _>(failure.clone())
            }),
            Err(failure)
        );
        assert_eq!(
            ensure_runtime_lease(&lease, || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(42usize)
            }),
            Ok(())
        );
        assert_eq!(
            ensure_runtime_lease(&lease, || {
                panic!("a successful lifetime lease must not be acquired again")
            }),
            Ok(())
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn capture_runtime_concurrent_retries_acquire_one_successful_lease() {
        let lease = Default::default();
        assert!(
            ensure_runtime_lease(&lease, || Err::<usize, _>(AppError::Capture(
                "transient MTA failure"
            )))
            .is_err()
        );
        let attempts = AtomicUsize::new(0);
        let start = Barrier::new(8);
        std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        ensure_runtime_lease(&lease, || {
                            attempts.fetch_add(1, Ordering::SeqCst);
                            std::thread::yield_now();
                            Ok(42usize)
                        })
                    })
                })
                .collect();
            for thread in threads {
                assert_eq!(thread.join().unwrap(), Ok(()));
            }
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
