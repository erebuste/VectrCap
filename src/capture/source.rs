//! WGC owns one native pool frame. The callback crops on the GPU before copying
//! into a capacity-three queue. No uncropped CPU image is retained.
#![allow(unsafe_code)]
use super::geometry::{MonitorGeometry, PhysicalRect};
use crate::{
    app::error::AppError,
    recording::{
        clock::MediaClock,
        frame::CapturedFrame,
        options::{FormatOptions, RecordingOptions},
        worker::FrameQueue,
    },
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use windows::Win32::Graphics::Gdi::*;
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::*,
    window::Window,
};

pub struct CaptureState {
    pub queue: FrameQueue,
    pub failure: Option<AppError>,
    pub clock: Option<MediaClock>,
    segment_start: Duration,
    segment_active: Duration,
    segment_end: Option<Duration>,
    paused: bool,
    last_frame: Option<Duration>,
}
impl Default for CaptureState {
    fn default() -> Self {
        Self {
            queue: FrameQueue::default(),
            failure: None,
            clock: None,
            segment_start: Duration::ZERO,
            segment_active: Duration::ZERO,
            segment_end: None,
            paused: false,
            last_frame: None,
        }
    }
}
impl CaptureState {
    pub fn elapsed(&mut self, now: Duration) -> Result<Duration, AppError> {
        self.clock
            .as_mut()
            .map_or(Ok(Duration::ZERO), |c| c.elapsed(now))
    }
    pub fn pause(&mut self, now: Duration) -> Result<Duration, AppError> {
        if !self.paused {
            self.segment_end = Some(now);
        }
        self.paused = true;
        self.clock
            .as_mut()
            .map_or(Ok(Duration::ZERO), |c| c.pause(now))
    }
    pub fn resume(&mut self, now: Duration) -> Result<Duration, AppError> {
        if !self.paused {
            return self.elapsed(now);
        }
        let t = self
            .clock
            .as_mut()
            .map_or(Ok(Duration::ZERO), |c| c.resume(now))?;
        self.segment_start = now;
        self.segment_active = t;
        self.segment_end = None;
        self.paused = false;
        Ok(t)
    }
    pub fn is_paused(&self) -> bool {
        self.paused
    }
    pub fn map_qpc(&self, qpc: Duration) -> Option<Duration> {
        if self.paused || qpc < self.segment_start {
            None
        } else {
            self.segment_active.checked_add(qpc - self.segment_start)
        }
    }
    /// Queued packets from the active side of a pause boundary remain eligible
    /// while acquisition is stopped and the ordered transition drains them.
    pub fn map_audio_qpc(&self, qpc: Duration) -> Option<Duration> {
        if self.clock.is_none()
            || qpc < self.segment_start
            || self.segment_end.is_some_and(|end| qpc >= end)
        {
            None
        } else {
            self.segment_active.checked_add(qpc - self.segment_start)
        }
    }
}
pub type SharedCaptureState = Arc<Mutex<CaptureState>>;
pub struct CaptureFlags {
    state: SharedCaptureState,
    options: RecordingOptions,
    source_rect: PhysicalRect,
    source_size: Option<(u32, u32)>,
}
pub struct Handler {
    flags: CaptureFlags,
}
impl GraphicsCaptureApiHandler for Handler {
    type Flags = CaptureFlags;
    type Error = AppError;
    fn new(ctx: Context<CaptureFlags>) -> Result<Self, AppError> {
        Ok(Self { flags: ctx.flags })
    }
    fn on_frame_arrived(
        &mut self,
        f: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), AppError> {
        let result = self.accept(f);
        if let Err(e) = result {
            if let Ok(mut s) = self.flags.state.lock() {
                s.failure = Some(e);
            }
            control.stop();
        }
        Ok(())
    }
    fn on_closed(&mut self) -> Result<(), AppError> {
        if let Ok(mut s) = self.flags.state.lock() {
            s.failure = Some(AppError::Capture("selected capture source closed"));
        }
        Ok(())
    }
}
impl Handler {
    fn accept(&mut self, f: &mut Frame) -> Result<(), AppError> {
        let mut s = self
            .flags
            .state
            .lock()
            .map_err(|_| AppError::Capture("capture state poisoned"))?;
        if s.paused {
            return Ok(());
        }
        let size = (f.width(), f.height());
        if self.flags.source_size.is_some_and(|old| old != size) {
            return Err(AppError::Capture("capture source geometry changed"));
        }
        self.flags.source_size = Some(size);
        let r = self.flags.source_rect;
        if r.left() < 0
            || r.top() < 0
            || r.left() as u64 + u64::from(r.width()) > u64::from(size.0)
            || r.top() as u64 + u64::from(r.height()) > u64::from(size.1)
        {
            return Err(AppError::Capture("selected crop exceeds native source"));
        }
        let ticks = f
            .timestamp()
            .map_err(|_| AppError::Capture("missing WGC QPC timestamp"))?
            .Duration;
        let qpc = Duration::from_nanos(
            u64::try_from(ticks)
                .ok()
                .and_then(|t| t.checked_mul(100))
                .ok_or(AppError::Capture("invalid WGC timestamp"))?,
        );
        if s.clock.is_none() {
            s.clock = Some(MediaClock::new(qpc));
            s.segment_start = qpc;
            s.segment_active = Duration::ZERO;
        }
        let Some(t) = s.map_qpc(qpc) else {
            return Ok(());
        };
        let interval = Duration::from_nanos(1_000_000_000 / u64::from(self.flags.options.fps()));
        if s.last_frame
            .is_some_and(|old| t <= old || t - old < interval)
        {
            return Ok(());
        }
        if t >= Duration::from_secs(u64::from(self.flags.options.active_limit_seconds())) {
            return Ok(());
        }
        // Reserve before allocating the next owned frame, so slow encoders cannot
        // cause even a transient fourth queued pixel buffer.
        s.queue.reserve_latest();
        let mut b = f
            .buffer_crop(
                r.left() as u32,
                r.top() as u32,
                r.left() as u32 + r.width(),
                r.top() as u32 + r.height(),
            )
            .map_err(|_| AppError::Capture("native crop readback failed"))?;
        let stride = b.row_pitch() as usize;
        let frame = CapturedFrame::crop(
            b.as_raw_buffer(),
            (r.width(), r.height()),
            stride,
            PhysicalRect::new(0, 0, r.width(), r.height())?,
            self.flags.options.output_size,
            matches!(self.flags.options.format, FormatOptions::Mp4(_)),
            t,
        )?;
        s.queue.push(frame);
        s.last_frame = Some(t);
        Ok(())
    }
}
pub struct NativeCapture {
    pins: Option<crate::platform::windows::windows::PinCaptureGuard>,
    control: Option<CaptureControl<Handler, AppError>>,
    pub state: SharedCaptureState,
}
impl NativeCapture {
    pub fn monitor(options: RecordingOptions) -> Result<Self, AppError> {
        let monitor = Monitor::enumerate()
            .map_err(|_| AppError::Capture("monitor enumeration failed"))?
            .into_iter()
            .find(|m| m.device_name().ok().as_deref() == Some(&options.monitor_id))
            .ok_or(AppError::Capture("selected monitor is unavailable"))?;
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            if !GetMonitorInfoW(HMONITOR(monitor.as_raw_hmonitor()), &mut info).as_bool() {
                return Err(AppError::Capture("monitor geometry unavailable"));
            }
        }
        let b = info.rcMonitor;
        let bounds = PhysicalRect::new(
            b.left,
            b.top,
            (b.right - b.left) as u32,
            (b.bottom - b.top) as u32,
        )?;
        options.validate_monitor(&MonitorGeometry::new(
            options.monitor_id.clone(),
            bounds,
            1.0,
        )?)?;
        let r = options.rect;
        let local = PhysicalRect::new(r.left() - b.left, r.top() - b.top, r.width(), r.height())?;
        Self::start(monitor, options, local)
    }
    /// Safe proof entry: can capture only an exact-title window owned by this process.
    /// Coordinates are relative to the generated window, never the desktop.
    pub fn generated_window(title: &str, options: RecordingOptions) -> Result<Self, AppError> {
        options.validate()?;
        let window = Window::from_name(title)
            .map_err(|_| AppError::Capture("generated proof window unavailable"))?;
        if window
            .process_id()
            .map_err(|_| AppError::Capture("cannot verify proof window owner"))?
            != std::process::id()
        {
            return Err(AppError::Capture(
                "proof window is not owned by this process",
            ));
        }
        let r = options.rect;
        Self::start(window, options, r)
    }
    fn start<T: TryInto<GraphicsCaptureItemType> + Send + 'static>(
        item: T,
        options: RecordingOptions,
        source_rect: PhysicalRect,
    ) -> Result<Self, AppError> {
        let pins = crate::platform::windows::windows::PinCaptureGuard::begin()?;
        let state = Arc::new(Mutex::new(CaptureState::default()));
        let cursor = if options.cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        };
        let settings = Settings::new(
            item,
            cursor,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Exclude,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            CaptureFlags {
                state: state.clone(),
                options,
                source_rect,
                source_size: None,
            },
        );
        let control = Handler::start_free_threaded(settings)
            .map_err(|_| AppError::Capture("Windows Graphics Capture could not start"))?;
        Ok(Self {
            pins: Some(pins),
            control: Some(control),
            state,
        })
    }
    pub fn check(&self) -> Result<(), AppError> {
        let s = self
            .state
            .lock()
            .map_err(|_| AppError::Capture("capture state poisoned"))?;
        if let Some(e) = &s.failure {
            return Err(e.clone());
        }
        if self.control.as_ref().is_some_and(|c| c.is_finished()) {
            return Err(AppError::Capture("native capture stopped unexpectedly"));
        }
        Ok(())
    }
    pub fn stop(&mut self) -> Result<(), AppError> {
        let result = self.control.take().map_or(Ok(()), |c| {
            c.stop()
                .map_err(|_| AppError::Capture("native capture shutdown failed"))
        });
        let restore = self.pins.take().map_or(Ok(()), |pins| pins.restore());
        result.and(restore)
    }
}
impl Drop for NativeCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
