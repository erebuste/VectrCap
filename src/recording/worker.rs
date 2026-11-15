use super::frame::CapturedFrame;
use super::{
    audio::{AudioChunk, mix::mix_stereo},
    native::qpc_now,
    options::*,
    *,
};
use crate::{app::error::AppError, capture::source::NativeCapture};
use std::collections::VecDeque;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
/// Full queues drop the oldest unencoded frame; timestamps retain elapsed time.
#[derive(Default)]
pub struct FrameQueue {
    frames: VecDeque<CapturedFrame>,
    dropped: u64,
}
impl FrameQueue {
    pub fn reserve_latest(&mut self) {
        if self.frames.len() == 3 {
            self.frames.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
    }
    pub fn push(&mut self, frame: CapturedFrame) {
        if self.frames.len() == 3 {
            self.frames.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.frames.push_back(frame);
    }
    pub fn pop(&mut self) -> Option<CapturedFrame> {
        self.frames.pop_front()
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingControl {
    Pause,
    Resume,
    Stop,
    Cancel,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingEvent {
    Started,
    Paused(Duration),
    Resumed(Duration),
    Finalizing,
    Interrupted(AppError),
}
pub struct RecordingHandle {
    controls: mpsc::SyncSender<RecordingControl>,
    cancel: CancellationToken,
    thread: Option<JoinHandle<Result<EncodedRecording, AppError>>>,
}
impl RecordingHandle {
    pub fn control(&self, c: RecordingControl) -> Result<(), AppError> {
        if c == RecordingControl::Cancel {
            self.cancel.cancel();
            return Ok(());
        }
        match self.controls.try_send(c) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) if c == RecordingControl::Stop => Ok(()),
            Err(_) => Err(AppError::InUse("recording control queue is full or closed")),
        }
    }
    pub fn finish(mut self) -> Result<EncodedRecording, AppError> {
        self.thread
            .take()
            .ok_or(AppError::Encoder("recording already joined"))?
            .join()
            .map_err(|_| AppError::Encoder("recording owner thread panicked"))?
    }
}
impl Drop for RecordingHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
pub struct RecordingWorker;
#[derive(Clone)]
pub struct ProbeSettings {
    pub window_title: String,
    pub output_root: PathBuf,
    pub synthetic_audio: bool,
    pub encoder_delay: Duration,
    pub media_time: Arc<AtomicU64>,
}
impl RecordingWorker {
    pub fn start(
        options: RecordingOptions,
        cancel: CancellationToken,
        events: mpsc::SyncSender<RecordingEvent>,
    ) -> Result<RecordingHandle, AppError> {
        Self::launch(options, cancel, events, None)
    }
    /// Uses an exact-title, same-process generated window. Synthetic audio never opens devices.
    pub fn start_probe(
        options: RecordingOptions,
        cancel: CancellationToken,
        events: mpsc::SyncSender<RecordingEvent>,
        probe: ProbeSettings,
    ) -> Result<RecordingHandle, AppError> {
        Self::launch(options, cancel, events, Some(probe))
    }
    fn launch(
        options: RecordingOptions,
        cancel: CancellationToken,
        events: mpsc::SyncSender<RecordingEvent>,
        probe: Option<ProbeSettings>,
    ) -> Result<RecordingHandle, AppError> {
        options.validate()?;
        if probe.as_ref().is_some_and(|p| !p.synthetic_audio) && options.audio().is_some() {
            return Err(AppError::InvalidInput(
                "proof audio requires explicit synthetic mode",
            ));
        }
        let (tx, rx) = mpsc::sync_channel(8);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let token = cancel.clone();
        let thread = thread::Builder::new()
            .name("vectrcap-recorder".into())
            .spawn(move || {
                let root = probe
                    .as_ref()
                    .map_or_else(std::env::temp_dir, |p| p.output_root.clone());
                let encoder: Result<Box<dyn RecordingEncoder>, AppError> = match &options.format {
                    FormatOptions::Gif(_) => gif::GifEncoder::new(&root, options.clone())
                        .map(|e| Box::new(e) as Box<dyn RecordingEncoder>),
                    FormatOptions::Mp4(_) => mp4::Mp4Encoder::new(&root, options.clone())
                        .map(|e| Box::new(e) as Box<dyn RecordingEncoder>),
                };
                let encoder = match encoder {
                    Ok(e) => {
                        let _ = ready_tx.send(Ok(()));
                        e
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.clone()));
                        return Err(e);
                    }
                };
                run(options, token, events, rx, probe, encoder)
            })
            .map_err(|e| AppError::io("starting recording owner thread", &e))?;
        let mut handle = RecordingHandle {
            controls: tx,
            cancel,
            thread: Some(thread),
        };
        if let Err(e) = ready_rx
            .recv()
            .map_err(|_| AppError::Encoder("encoder initialization interrupted"))?
        {
            handle.cancel.cancel();
            if let Some(t) = handle.thread.take() {
                let _ = t.join();
            }
            return Err(e);
        }
        Ok(handle)
    }
}
fn event(tx: &mpsc::SyncSender<RecordingEvent>, e: RecordingEvent) -> Result<(), AppError> {
    tx.try_send(e)
        .map_err(|_| AppError::Capture("recording event consumer unavailable or too slow"))
}
fn run(
    options: RecordingOptions,
    cancel: CancellationToken,
    events: mpsc::SyncSender<RecordingEvent>,
    controls: mpsc::Receiver<RecordingControl>,
    probe: Option<ProbeSettings>,
    mut encoder: Box<dyn RecordingEncoder>,
) -> Result<EncodedRecording, AppError> {
    let countdown = Instant::now();
    while countdown.elapsed() < Duration::from_secs(u64::from(options.countdown_seconds)) {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        if matches!(
            controls.try_recv(),
            Ok(RecordingControl::Stop | RecordingControl::Cancel)
        ) {
            return Err(AppError::Cancelled);
        }
        thread::sleep(Duration::from_millis(10));
    }
    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }
    // Native audio startup is gated below until the capture/mixer integration is proven.
    // This is an explicit error, never a silent audio-Off substitution.
    if options.audio().is_some() && probe.is_none() {
        return Err(AppError::Capture(
            "physical audio timeline integration is not yet qualified; choose Off or run an explicit device proof",
        ));
    }
    let mut capture = if let Some(p) = &probe {
        NativeCapture::generated_window(&p.window_title, options.clone())?
    } else {
        NativeCapture::monitor(options.clone())?
    };
    let startup = Instant::now();
    let mut started = false;
    let mut audio_frames = 0u64;
    let mut last_video = None;
    let mut interruption = None;
    let limit = Duration::from_secs(u64::from(options.active_limit_seconds()));
    let end = loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        if let Err(e) = capture.check() {
            interruption = Some(e);
            break capture
                .state
                .lock()
                .map_err(|_| AppError::Capture("capture state poisoned"))?
                .elapsed(qpc_now()?)?
                .min(limit);
        }
        let mut requested_stop = false;
        while let Ok(command) = controls.try_recv() {
            match command {
                RecordingControl::Cancel => return Err(AppError::Cancelled),
                RecordingControl::Stop => requested_stop = true,
                RecordingControl::Pause => {
                    let t = capture
                        .state
                        .lock()
                        .map_err(|_| AppError::Capture("capture state poisoned"))?
                        .pause(qpc_now()?)?
                        .min(limit);
                    drain_video(&capture, &mut *encoder, &mut last_video, Duration::ZERO)?;
                    feed_synthetic(&mut *encoder, options.audio(), &mut audio_frames, t)?;
                    event(&events, RecordingEvent::Paused(t))?;
                }
                RecordingControl::Resume => {
                    let t = capture
                        .state
                        .lock()
                        .map_err(|_| AppError::Capture("capture state poisoned"))?
                        .resume(qpc_now()?)?;
                    event(&events, RecordingEvent::Resumed(t))?;
                }
            }
        }
        drain_video(
            &capture,
            &mut *encoder,
            &mut last_video,
            probe.as_ref().map_or(Duration::ZERO, |p| p.encoder_delay),
        )?;
        let (t, has_origin) = {
            let mut state = capture
                .state
                .lock()
                .map_err(|_| AppError::Capture("capture state poisoned"))?;
            (state.elapsed(qpc_now()?)?.min(limit), state.clock.is_some())
        };
        if let Some(p) = &probe {
            p.media_time.store(t.as_nanos() as u64, Ordering::Release);
        }
        if has_origin && !started {
            event(&events, RecordingEvent::Started)?;
            started = true;
        }
        if !started && startup.elapsed() > Duration::from_secs(5) {
            return Err(AppError::Capture(
                "no native frame arrived within five seconds",
            ));
        }
        feed_synthetic(&mut *encoder, options.audio(), &mut audio_frames, t)?;
        if requested_stop || t >= limit {
            break t;
        }
        thread::sleep(Duration::from_millis(2));
    };
    capture
        .state
        .lock()
        .map_err(|_| AppError::Capture("capture state poisoned"))?
        .pause(qpc_now()?)?;
    capture.stop()?;
    drain_video(&capture, &mut *encoder, &mut last_video, Duration::ZERO)?;
    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }
    if let Some(e) = interruption {
        event(&events, RecordingEvent::Interrupted(e.clone()))?;
        return Err(e);
    }
    event(&events, RecordingEvent::Finalizing)?;
    encoder.end_at(end)?;
    let output = encoder.finish()?;
    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }
    Ok(output)
}
fn drain_video(
    capture: &NativeCapture,
    encoder: &mut dyn RecordingEncoder,
    last: &mut Option<Duration>,
    delay: Duration,
) -> Result<(), AppError> {
    // Drain only the initial bounded batch; a producer cannot keep this loop alive forever.
    for _ in 0..3 {
        let frame = capture
            .state
            .lock()
            .map_err(|_| AppError::Capture("capture state poisoned"))?
            .queue
            .pop();
        let Some(mut frame) = frame else {
            break;
        };
        if last.is_none() {
            frame.timestamp = Duration::ZERO;
        }
        if last.is_some_and(|t| frame.timestamp <= t) {
            continue;
        }
        encoder.push_video(&frame)?;
        *last = Some(frame.timestamp);
        if !delay.is_zero() {
            thread::sleep(delay);
        }
    }
    Ok(())
}
fn feed_synthetic(
    encoder: &mut dyn RecordingEncoder,
    options: Option<&AudioOptions>,
    frames: &mut u64,
    end: Duration,
) -> Result<(), AppError> {
    let Some(o) = options else { return Ok(()) };
    let target = (end.as_nanos() * 48_000 / 1_000_000_000) as u64;
    while *frames < target {
        let n = (target - *frames).min(480) as usize;
        let mut system = Vec::with_capacity(n * 2);
        let mut mic = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = (*frames + i as u64) as f32 / 48000.0;
            let marker = if (t.fract()) < 0.1 { 0.4 } else { 0.0 };
            let a = (t * 440.0 * std::f32::consts::TAU).sin() * marker;
            let b = (t * 660.0 * std::f32::consts::TAU).sin() * marker;
            system.extend_from_slice(&[a, a]);
            mic.extend_from_slice(&[b, b]);
        }
        let samples = mix_stereo(
            matches!(o.mode, AudioMode::System | AudioMode::Both).then_some(system.as_slice()),
            matches!(o.mode, AudioMode::Microphone | AudioMode::Both).then_some(mic.as_slice()),
            o,
        )?;
        let timestamp = Duration::from_nanos(*frames * 1_000_000_000 / 48000);
        encoder.push_audio(&AudioChunk::new(samples, timestamp)?)?;
        *frames += n as u64;
    }
    Ok(())
}
