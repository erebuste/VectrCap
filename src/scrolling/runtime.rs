//! Native readback and all pixel matching run off the controller/GUI threads.
use super::{session::*, stable::CandidateQueue, *};
use crate::{
    capture::geometry::PhysicalRect,
    platform::windows::{
        monitors::DpiContext,
        windows::{ScrollingTarget, window_rect},
    },
};
use image::RgbaImage;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::*,
    window::Window,
};
/// Native lock/suspend/display messages invalidate even an already queued frame.
static INTERRUPT_EPOCH: AtomicU64 = AtomicU64::new(0);
pub fn invalidate_candidates() {
    INTERRUPT_EPOCH.fetch_add(1, Ordering::SeqCst);
}
static WATCHED_TARGET: AtomicUsize = AtomicUsize::new(0);
static TARGET_EVENT_EPOCH: AtomicU64 = AtomicU64::new(0);
static TARGET_HOOKS_AVAILABLE: AtomicBool = AtomicBool::new(false);
/// Pure filter for GUI-thread WinEvent callbacks. Child-object movement is ignored.
pub fn target_event_invalidates(event: u32, hwnd: usize, object: i32, watched: usize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    watched != 0
        && (event == EVENT_SYSTEM_FOREGROUND
            || (hwnd == watched
                && object == OBJID_WINDOW
                && matches!(event, EVENT_OBJECT_LOCATIONCHANGE | EVENT_OBJECT_DESTROY)))
}
#[allow(unsafe_code)]
unsafe extern "system" fn target_event(
    _hook: windows_sys::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    hwnd: windows_sys::Win32::Foundation::HWND,
    object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if target_event_invalidates(
        event,
        hwnd as usize,
        object,
        WATCHED_TARGET.load(Ordering::SeqCst),
    ) {
        TARGET_EVENT_EPOCH.fetch_add(1, Ordering::SeqCst);
    }
}
/// Attached and dropped on the GUI message-pump thread, which delivers out-of-context hooks.
pub struct TargetEventHooks {
    hooks: Vec<windows_sys::Win32::UI::Accessibility::HWINEVENTHOOK>,
}
impl TargetEventHooks {
    #[allow(unsafe_code)]
    pub fn attach() -> Result<Self, AppError> {
        use windows_sys::Win32::UI::{Accessibility::SetWinEventHook, WindowsAndMessaging::*};
        let mut guard = Self { hooks: vec![] };
        for event in [
            EVENT_SYSTEM_FOREGROUND,
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_DESTROY,
        ] {
            let hook = unsafe {
                SetWinEventHook(
                    event,
                    event,
                    std::ptr::null_mut(),
                    Some(target_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            if hook.is_null() {
                return Err(AppError::Capture(
                    "scroll target event observation unavailable",
                ));
            }
            guard.hooks.push(hook);
        }
        TARGET_HOOKS_AVAILABLE.store(true, Ordering::SeqCst);
        Ok(guard)
    }
}
impl Drop for TargetEventHooks {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        TARGET_HOOKS_AVAILABLE.store(false, Ordering::SeqCst);
        for hook in self.hooks.drain(..) {
            unsafe {
                windows_sys::Win32::UI::Accessibility::UnhookWinEvent(hook);
            }
        }
    }
}
struct WatchedTarget(usize);
impl WatchedTarget {
    fn new(hwnd: usize) -> Result<Self, AppError> {
        if !TARGET_HOOKS_AVAILABLE.load(Ordering::SeqCst) {
            return Err(AppError::Capture(
                "scroll target event observation unavailable",
            ));
        }
        WATCHED_TARGET
            .compare_exchange(0, hwnd, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| AppError::InUse("previous scrolling source is still shutting down"))?;
        Ok(Self(hwnd))
    }
}
impl Drop for WatchedTarget {
    fn drop(&mut self) {
        let _ = WATCHED_TARGET.compare_exchange(self.0, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}
#[derive(Clone)]
pub struct ScrollUpdate {
    pub state: ScrollState,
    pub options: ScrollOptions,
    pub accepted_segments: u32,
    pub dimensions: Option<(u32, u32)>,
    pub partial_reason: Option<&'static str>,
    pub countdown: Duration,
}
pub struct RuntimeEvent {
    pub session: uuid::Uuid,
    pub update: ScrollUpdate,
    pub review: Option<ScrollReview>,
    pub error: Option<AppError>,
}
impl RuntimeEvent {
    /// A Review status alone is telemetry; only the owned pixels finish review handoff.
    pub fn is_terminal(&self) -> bool {
        self.review.is_some() || self.update.state == ScrollState::Cancelled
    }
}
#[derive(Clone, Copy)]
pub enum ScrollControl {
    Pause,
    Resume,
    Stop,
    Cancel,
    Interrupt(&'static str),
}
pub struct ScrollingRuntime {
    tx: mpsc::SyncSender<ScrollControl>,
    cancel: CancellationToken,
    thread: Option<thread::JoinHandle<()>>,
}
struct Candidate {
    image: RgbaImage,
    identity: TargetIdentity,
    now: Duration,
}
struct Queue {
    frames: CandidateQueue<Candidate>,
    paused: bool,
    last: Option<Duration>,
    target_epoch: u64,
    invalid: Option<TargetIdentity>,
    failure: Option<AppError>,
}
struct Flags {
    target: Arc<ScrollingTarget>,
    queue: Arc<Mutex<Queue>>,
    origin: Instant,
    epoch: u64,
    raw: PhysicalRect,
}
struct Handler {
    flags: Flags,
}
struct PreparedItem {
    item: windows::Graphics::Capture::GraphicsCaptureItem,
    window: Window,
}
impl From<PreparedItem> for GraphicsCaptureItemType {
    fn from(p: PreparedItem) -> Self {
        Self::Window((p.item, p.window))
    }
}
struct NativeGuard(Option<CaptureControl<Handler, AppError>>);
impl NativeGuard {
    fn stop(&mut self) -> Result<(), AppError> {
        self.0.take().map_or(Ok(()), |c| {
            c.stop()
                .map_err(|_| AppError::Capture("scroll source shutdown failed"))
        })
    }
}
impl Drop for NativeGuard {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("scroll source shutdown failed: {error}");
        }
    }
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = AppError;
    fn new(ctx: Context<Flags>) -> Result<Self, AppError> {
        Ok(Self { flags: ctx.flags })
    }
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _control: InternalCaptureControl,
    ) -> Result<(), AppError> {
        if let Err(e) = self.accept(frame)
            && let Ok(mut q) = self.flags.queue.lock()
        {
            q.frames.clear();
            q.failure = Some(e);
            q.paused = true;
        }
        Ok(())
    }
    fn on_closed(&mut self) -> Result<(), AppError> {
        if let Ok(mut q) = self.flags.queue.lock() {
            q.frames.clear();
            q.failure = Some(AppError::Capture("scroll target closed"));
            q.paused = true;
        }
        Ok(())
    }
}
impl Handler {
    fn accept(&mut self, frame: &mut Frame) -> Result<(), AppError> {
        let _dpi = DpiContext::enter()?;
        let mut q = self
            .flags
            .queue
            .lock()
            .map_err(|_| AppError::Capture("scroll candidate queue unavailable"))?;
        if q.paused {
            return Ok(());
        }
        let now = self.flags.origin.elapsed();
        if q.last
            .is_some_and(|last| now.saturating_sub(last) < Duration::from_millis(100))
        {
            return Ok(());
        }
        q.last = Some(now);
        if INTERRUPT_EPOCH.load(Ordering::SeqCst) != self.flags.epoch {
            return Err(AppError::Capture(
                "desktop interrupted during scrolling capture",
            ));
        }
        let identity = self.flags.target.identity()?;
        if !identity.focused || TARGET_EVENT_EPOCH.load(Ordering::SeqCst) != q.target_epoch {
            let mut identity = identity;
            identity.focused = false;
            q.frames.clear();
            q.invalid = Some(identity);
            q.paused = true;
            return Ok(());
        }
        let bounds = identity.window_rect;
        let raw = self.flags.raw;
        let viewport = identity.viewport;
        let origin = if (frame.width(), frame.height()) == (bounds.width(), bounds.height()) {
            bounds
        } else if (frame.width(), frame.height()) == (raw.width(), raw.height()) {
            raw
        } else {
            return Err(AppError::Capture("scroll source dimensions changed"));
        };
        if !origin.contains_rect(viewport) {
            return Err(AppError::Capture("scroll viewport left original source"));
        }
        q.frames.reserve_latest();
        let x = origin.left().abs_diff(viewport.left());
        let y = origin.top().abs_diff(viewport.top());
        let mut buffer = frame
            .buffer_crop(x, y, x + viewport.width(), y + viewport.height())
            .map_err(|_| AppError::Capture("scroll window readback failed"))?;
        let pitch = buffer.row_pitch() as usize;
        let row = viewport.width() as usize * 4;
        let bytes = buffer.as_raw_buffer();
        let length = viewport.rgba_len()?;
        if pitch < row
            || bytes.len()
                < pitch
                    .checked_mul(viewport.height() as usize - 1)
                    .and_then(|n| n.checked_add(row))
                    .ok_or(AppError::Capture("scroll stride overflow"))?
        {
            return Err(AppError::Capture("scroll source row layout invalid"));
        }
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(length)
            .map_err(|_| AppError::Capture("scroll candidate allocation failed"))?;
        for y in 0..viewport.height() as usize {
            pixels.extend_from_slice(&bytes[y * pitch..y * pitch + row]);
        }
        let after = self.flags.target.identity()?;
        if identity != after
            || INTERRUPT_EPOCH.load(Ordering::SeqCst) != self.flags.epoch
            || TARGET_EVENT_EPOCH.load(Ordering::SeqCst) != q.target_epoch
        {
            let mut after = after;
            after.focused = false;
            q.frames.clear();
            q.invalid = Some(after);
            q.paused = true;
            return Ok(());
        }
        let image = RgbaImage::from_raw(viewport.width(), viewport.height(), pixels)
            .ok_or(AppError::Capture("scroll frame layout invalid"))?;
        q.frames.push(Candidate {
            image,
            identity,
            now,
        });
        Ok(())
    }
}
impl ScrollingRuntime {
    pub fn start(
        id: uuid::Uuid,
        target: ScrollingTarget,
        events: tokio::sync::mpsc::Sender<RuntimeEvent>,
    ) -> Result<Self, AppError> {
        let (tx, rx) = mpsc::sync_channel(8);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let thread = thread::Builder::new()
            .name("vectrcap-scrolling".into())
            .spawn(move || {
                let options = target.options();
                let mut session = ScrollingSession::default();
                let origin = Instant::now();
                let deadline = Duration::from_secs(3);
                let mut error = None;
                let target = Arc::new(target);
                let result = (|| -> Result<(), AppError> {
                    let _dpi = DpiContext::enter()?;
                    let _apartment = crate::platform::windows::capture::Apartment::enter()?;
                    let native = target.native()?;
                    let _watch = WatchedTarget::new(native.hwnd)?;
                    // Convert HWND once, before countdown; the item retains this window object.
                    let item: GraphicsCaptureItemType =
                        Window::from_raw_hwnd(native.hwnd as *mut _)
                            .try_into()
                            .map_err(|_| {
                                AppError::Capture("scroll target capture item unavailable")
                            })?;
                    let GraphicsCaptureItemType::Window((item, window)) = item else {
                        return Err(AppError::Capture("scroll source is not a window"));
                    };
                    let item = PreparedItem { item, window };
                    let raw = window_rect(native)?;
                    let mut identity = target.identity()?;
                    identity.focused = true;
                    session.handle(ScrollInput::Select {
                        options: options.clone(),
                        identity,
                        now: Duration::ZERO,
                    });
                    let queue = Arc::new(Mutex::new(Queue {
                        frames: CandidateQueue::default(),
                        paused: true,
                        last: None,
                        target_epoch: TARGET_EVENT_EPOCH.load(Ordering::SeqCst),
                        invalid: None,
                        failure: None,
                    }));
                    let epoch = INTERRUPT_EPOCH.load(Ordering::SeqCst);
                    let mut capture = NativeGuard(None);
                    let mut item = Some(item);
                    let mut resume_requested = false;
                    let mut target_epoch = TARGET_EVENT_EPOCH.load(Ordering::SeqCst);
                    loop {
                        if token.is_cancelled() {
                            session.handle(ScrollInput::Cancel);
                        }
                        let control = match rx.recv_timeout(Duration::from_millis(25)) {
                            Ok(c) => Some(c),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(mpsc::RecvTimeoutError::Disconnected) => {
                                Some(ScrollControl::Cancel)
                            }
                        };
                        if let Some(c) = control {
                            match c {
                                ScrollControl::Resume => resume_requested = true,
                                ScrollControl::Pause => {
                                    session.handle(ScrollInput::Pause);
                                }
                                ScrollControl::Stop => {
                                    session.handle(ScrollInput::Stop);
                                }
                                ScrollControl::Cancel => {
                                    session.handle(ScrollInput::Cancel);
                                }
                                ScrollControl::Interrupt(reason) => {
                                    session.handle(ScrollInput::Interrupt(reason));
                                }
                            }
                        }
                        if INTERRUPT_EPOCH.load(Ordering::SeqCst) != epoch {
                            session.handle(ScrollInput::Interrupt(
                                "Desktop locked, suspended or display changed",
                            ));
                        }
                        let countdown = session.state() == ScrollState::Countdown;
                        session.handle(ScrollInput::Tick(origin.elapsed()));
                        if countdown && session.state() == ScrollState::Capturing {
                            target_epoch = TARGET_EVENT_EPOCH.load(Ordering::SeqCst);
                        }
                        if session.state() == ScrollState::Capturing
                            && TARGET_EVENT_EPOCH.load(Ordering::SeqCst) != target_epoch
                        {
                            session.handle(ScrollInput::Pause);
                        }
                        if matches!(
                            session.state(),
                            ScrollState::Review | ScrollState::Cancelled
                        ) {
                            break;
                        }
                        if matches!(
                            session.state(),
                            ScrollState::Capturing | ScrollState::Paused
                        ) {
                            match target.identity() {
                                Ok(identity) => {
                                    session.handle(ScrollInput::Observe(identity.clone()));
                                    if resume_requested
                                        && session.state() == ScrollState::Paused
                                        && identity.focused
                                    {
                                        session.handle(ScrollInput::Resume(identity));
                                        target_epoch = TARGET_EVENT_EPOCH.load(Ordering::SeqCst);
                                        resume_requested = false;
                                    }
                                }
                                Err(_) => {
                                    session.handle(ScrollInput::Interrupt(
                                        "Original target or display became unavailable",
                                    ));
                                }
                            }
                        }
                        if matches!(
                            session.state(),
                            ScrollState::Review | ScrollState::Cancelled
                        ) {
                            break;
                        }
                        {
                            let mut q = queue.lock().map_err(|_| {
                                AppError::Capture("scroll candidate queue unavailable")
                            })?;
                            let active = session.state() == ScrollState::Capturing;
                            if q.paused != !active {
                                q.frames.clear();
                                q.last = None;
                                if active {
                                    q.invalid = None;
                                }
                            }
                            q.target_epoch = target_epoch;
                            q.paused = !active;
                            if let Some(id) = q.invalid.take() {
                                session.handle(ScrollInput::Observe(id));
                            }
                            if let Some(e) = q.failure.take() {
                                error = Some(e);
                                session.handle(ScrollInput::Interrupt(
                                    "Native scrolling source interrupted",
                                ));
                            }
                            if session.state() != ScrollState::Capturing {
                                q.paused = true;
                                q.frames.clear();
                            }
                        }
                        if session.state() == ScrollState::Capturing && capture.0.is_none() {
                            let settings = Settings::new(
                                item.take()
                                    .ok_or(AppError::Capture("scroll source already consumed"))?,
                                CursorCaptureSettings::WithoutCursor,
                                DrawBorderSettings::Default,
                                SecondaryWindowSettings::Exclude,
                                MinimumUpdateIntervalSettings::Default,
                                DirtyRegionSettings::Default,
                                ColorFormat::Rgba8,
                                Flags {
                                    target: target.clone(),
                                    queue: queue.clone(),
                                    origin,
                                    epoch,
                                    raw,
                                },
                            );
                            capture.0 =
                                Some(Handler::start_free_threaded(settings).map_err(|_| {
                                    AppError::Capture("scroll capture could not start")
                                })?);
                        }
                        if let Some(c) = &capture.0
                            && c.is_finished()
                        {
                            session
                                .handle(ScrollInput::Interrupt("Native scrolling source stopped"));
                        }
                        {
                            // Keep the queue locked through matching so an in-flight
                            // candidate plus queued candidates never exceeds two.
                            let mut q = queue.lock().map_err(|_| {
                                AppError::Capture("scroll candidate queue unavailable")
                            })?;
                            if let Some(candidate) = q.frames.pop()
                                && session.state() == ScrollState::Capturing
                            {
                                if target.identity().ok().as_ref() == Some(&candidate.identity)
                                    && INTERRUPT_EPOCH.load(Ordering::SeqCst) == epoch
                                    && TARGET_EVENT_EPOCH.load(Ordering::SeqCst) == target_epoch
                                {
                                    session.handle(ScrollInput::Frame {
                                        image: candidate.image,
                                        identity: candidate.identity,
                                        now: candidate.now,
                                    });
                                } else {
                                    session.handle(ScrollInput::Pause);
                                }
                            }
                        }
                        if matches!(
                            session.state(),
                            ScrollState::Review | ScrollState::Cancelled
                        ) {
                            break;
                        }
                        let update = ScrollUpdate {
                            state: session.state(),
                            options: options.clone(),
                            accepted_segments: session.accepted_segments(),
                            dimensions: session.dimensions(),
                            partial_reason: session.partial_reason(),
                            countdown: deadline.saturating_sub(origin.elapsed()),
                        };
                        let _ = events.try_send(RuntimeEvent {
                            session: id,
                            update,
                            review: None,
                            error: None,
                        });
                    }
                    capture.stop()?;
                    Ok(())
                })();
                if let Err(e) = result {
                    error = Some(e);
                    session.handle(ScrollInput::Interrupt("Scrolling capture interrupted"));
                }
                let update = ScrollUpdate {
                    state: session.state(),
                    options,
                    accepted_segments: session.accepted_segments(),
                    dimensions: session.dimensions(),
                    partial_reason: session.partial_reason(),
                    countdown: Duration::ZERO,
                };
                let _ = events.blocking_send(RuntimeEvent {
                    session: id,
                    update,
                    review: session.take_review(),
                    error,
                });
            })
            .map_err(|e| AppError::io("starting scrolling worker", &e))?;
        Ok(Self {
            tx,
            cancel,
            thread: Some(thread),
        })
    }
    pub fn control(&self, c: ScrollControl) -> Result<(), AppError> {
        if matches!(c, ScrollControl::Cancel) {
            self.cancel.cancel();
            return Ok(());
        }
        self.tx
            .try_send(c)
            .map_err(|_| AppError::InUse("scroll controls full or worker stopped"))
    }
    pub fn join(mut self) {
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for ScrollingRuntime {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
