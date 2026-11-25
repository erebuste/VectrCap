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
    pub fn pop_before(&mut self, cutoff: Option<Duration>) -> Option<CapturedFrame> {
        if cutoff.is_some_and(|t| self.frames.front().is_some_and(|f| f.timestamp >= t)) {
            return None;
        }
        self.frames.pop_front()
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
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
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
        Self::launch(options, cancel, events, None, None)
    }
    /// Uses an exact-title, same-process generated window. Synthetic audio never opens devices.
    pub fn start_probe(
        options: RecordingOptions,
        cancel: CancellationToken,
        events: mpsc::SyncSender<RecordingEvent>,
        probe: ProbeSettings,
    ) -> Result<RecordingHandle, AppError> {
        Self::launch(options, cancel, events, Some(probe), None)
    }
    pub fn start_resolved(
        options: RecordingOptions,
        cancel: CancellationToken,
        events: mpsc::SyncSender<RecordingEvent>,
        sources: Vec<super::session::ResolvedSource>,
    ) -> Result<RecordingHandle, AppError> {
        Self::launch(options, cancel, events, None, Some(sources))
    }
    fn launch(
        options: RecordingOptions,
        cancel: CancellationToken,
        events: mpsc::SyncSender<RecordingEvent>,
        probe: Option<ProbeSettings>,
        sources: Option<Vec<super::session::ResolvedSource>>,
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
                run(options, token, events, rx, probe, sources, encoder)
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
    sources: Option<Vec<super::session::ResolvedSource>>,
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
    // Open exactly the selected sources. WASAPI binds Default once and verifies
    // its identity throughout the session; no fallback or source substitution.
    let mut physical_audio = if probe.is_none() {
        options
            .audio()
            .map(|o| match &sources {
                Some(s) => audio::timeline::PhysicalAudio::open_resolved(o, s),
                None => audio::timeline::PhysicalAudio::open(o),
            })
            .transpose()?
    } else {
        None
    };
    if let Some(audio) = &mut physical_audio {
        audio.resume()?;
    }
    crate::persistence::ensure_free_space(&std::env::temp_dir(), 16 * 1024 * 1024)?;
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
    let mut space_check = Instant::now();
    let mut end = loop {
        if cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        if space_check.elapsed() >= Duration::from_secs(1) {
            crate::persistence::ensure_free_space(&std::env::temp_dir(), 16 * 1024 * 1024)?;
            space_check = Instant::now();
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
                    if let Some(audio) = &mut physical_audio {
                        if let Err(e) = audio.pause() {
                            interruption.get_or_insert(e);
                            requested_stop = true;
                        }
                        if let Err(e) = audio.pump(&capture.state, &mut *encoder, t) {
                            interruption.get_or_insert(e);
                            requested_stop = true;
                        }
                    }
                    // A drain failure here must not discard an already-stored interruption
                    // cause by propagating a different error before it is ever reported.
                    if let Err(e) = drain_video(
                        &capture,
                        &mut *encoder,
                        &mut last_video,
                        Duration::ZERO,
                        Some(physical_audio.as_ref().map_or(t, |a| a.end_time().min(t))),
                    ) {
                        interruption.get_or_insert(e);
                        requested_stop = true;
                    }
                    if probe.is_some() {
                        feed_synthetic(&mut *encoder, options.audio(), &mut audio_frames, t)?;
                    }
                    event(&events, RecordingEvent::Paused(t))?;
                }
                RecordingControl::Resume => {
                    let paused = capture
                        .state
                        .lock()
                        .map_err(|_| AppError::Capture("capture state poisoned"))?
                        .is_paused();
                    let t = capture
                        .state
                        .lock()
                        .map_err(|_| AppError::Capture("capture state poisoned"))?
                        .resume(qpc_now()?)?;
                    if paused
                        && let Some(audio) = &mut physical_audio
                        && let Err(e) = audio.resume()
                    {
                        interruption = Some(e);
                        requested_stop = true;
                    }
                    event(&events, RecordingEvent::Resumed(t))?;
                }
            }
        }
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
        if let Some(audio) = &mut physical_audio {
            if let Err(e) = audio.pump(&capture.state, &mut *encoder, t) {
                interruption = Some(e);
                requested_stop = true;
            }
        } else {
            feed_synthetic(&mut *encoder, options.audio(), &mut audio_frames, t)?;
        }
        // Cut off at the media time read above, not just the limit: after a resume,
        // WGC presentation stamps can map a few ms ahead of the clock, and a frame
        // past `t` would land after the final endpoint ("invalid MP4 endpoint").
        drain_video(
            &capture,
            &mut *encoder,
            &mut last_video,
            probe.as_ref().map_or(Duration::ZERO, |p| p.encoder_delay),
            Some(physical_audio.as_ref().map_or(t, |a| a.end_time().min(t))),
        )?;
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
    if let Some(audio) = &mut physical_audio {
        if let Err(e) = audio
            .pause()
            .and_then(|()| audio.pump(&capture.state, &mut *encoder, end))
        {
            interruption.get_or_insert(e);
        }
        end = end.min(audio.end_time());
    }
    if let Err(e) = capture.stop() {
        interruption.get_or_insert(e);
    }
    // Same reasoning as the Pause arm above: a drain failure here must not discard
    // an already-stored interruption cause before the Interrupted event is sent.
    if let Err(e) = drain_video(
        &capture,
        &mut *encoder,
        &mut last_video,
        Duration::ZERO,
        Some(end),
    ) {
        interruption.get_or_insert(e);
    }
    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }
    if let Some(e) = interruption {
        event(&events, RecordingEvent::Interrupted(e.clone()))?;
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
    cutoff: Option<Duration>,
) -> Result<(), AppError> {
    // Drain only the initial bounded batch; a producer cannot keep this loop alive forever.
    for _ in 0..3 {
        let frame = capture
            .state
            .lock()
            .map_err(|_| AppError::Capture("capture state poisoned"))?
            .queue
            .pop_before(cutoff);
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
