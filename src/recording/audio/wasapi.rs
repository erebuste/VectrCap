//! Native WASAPI device acquisition is explicitly selected, never a default side effect.
#![allow(unsafe_code)]
use super::{PacketClock, SampleFormat, devices::SourceKind, normalize_pcm, resample::Resampler};
use crate::{
    app::error::AppError,
    recording::{native::*, options::EndpointSelection},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::*,
        Media::{Audio::*, KernelStreaming::*, Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT},
        System::{Com::*, Threading::*},
    },
    core::{HSTRING, PCWSTR},
};

pub struct SourceBlock {
    pub stereo: Vec<f32>,
    pub qpc: Duration,
}
#[derive(Default)]
struct Buffer {
    blocks: VecDeque<SourceBlock>,
    frames: usize,
    error: Option<AppError>,
}
enum Command {
    Pause(mpsc::SyncSender<Result<(), AppError>>),
    Resume(mpsc::SyncSender<Result<(), AppError>>),
    Stop,
}
/// The COM client, resampler, event and every native packet live on one owning
/// thread. Only copied, normalized bounded blocks cross the thread boundary.
pub struct AudioSourceHandle {
    tx: mpsc::SyncSender<Command>,
    buffer: Arc<Mutex<Buffer>>,
    thread: Option<JoinHandle<()>>,
}
impl AudioSourceHandle {
    pub fn start(kind: SourceKind, endpoint: EndpointSelection) -> Result<Self, AppError> {
        let (tx, rx) = mpsc::sync_channel(2);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        let shared = buffer.clone();
        let thread = thread::Builder::new()
            .name("vectrcap-wasapi".into())
            .spawn(move || {
                let result = run(kind, endpoint, rx, &shared, ready_tx);
                if let Err(e) = result
                    && let Ok(mut b) = shared.lock()
                {
                    b.error = Some(e);
                }
            })
            .map_err(|e| AppError::io("starting audio owner thread", &e))?;
        let mut handle = Self {
            tx,
            buffer,
            thread: Some(thread),
        };
        if let Err(e) = ready_rx
            .recv()
            .map_err(|_| AppError::Capture("audio owner stopped during initialization"))?
        {
            handle.stop();
            return Err(e);
        }
        Ok(handle)
    }
    pub fn pop(&self) -> Result<Option<SourceBlock>, AppError> {
        let mut b = self
            .buffer
            .lock()
            .map_err(|_| AppError::Capture("audio buffer poisoned"))?;
        if let Some(e) = &b.error {
            return Err(e.clone());
        }
        let block = b.blocks.pop_front();
        if let Some(block) = &block {
            b.frames -= block.stereo.len() / 2;
        }
        Ok(block)
    }
    pub fn pause(&self) -> Result<(), AppError> {
        self.transition(true)
    }
    pub fn resume(&self) -> Result<(), AppError> {
        self.transition(false)
    }
    fn transition(&self, pause: bool) -> Result<(), AppError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(if pause {
                Command::Pause(tx)
            } else {
                Command::Resume(tx)
            })
            .map_err(|_| AppError::Capture("audio worker stopped"))?;
        rx.recv()
            .map_err(|_| AppError::Capture("audio transition interrupted"))?
    }
    pub fn stop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for AudioSourceHandle {
    fn drop(&mut self) {
        self.stop();
    }
}
struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct MixFormat(*mut WAVEFORMATEX);
impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}
struct Client {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    event: Event,
    device: IMMDevice,
    enumerator: IMMDeviceEnumerator,
    endpoint_id: String,
    default: bool,
    kind: SourceKind,
    rate: u32,
    channels: usize,
    format: SampleFormat,
    align: usize,
    _runtime: Runtime,
}
impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
impl Client {
    fn open(kind: SourceKind, selection: &EndpointSelection) -> Result<Self, AppError> {
        let runtime = Runtime::new()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)
                    .map_err(|_| AppError::Capture("audio endpoint enumeration unavailable"))?;
            let flow = if kind == SourceKind::System {
                eRender
            } else {
                eCapture
            };
            let device = match selection {
                EndpointSelection::Default => enumerator.GetDefaultAudioEndpoint(flow, eConsole),
                EndpointSelection::Id(id) => enumerator.GetDevice(&HSTRING::from(id)),
            }
            .map_err(|_| AppError::Capture("selected audio endpoint unavailable"))?;
            let endpoint_id = device_id(&device)?;
            let client: IAudioClient = device
                .Activate(CLSCTX_INPROC_SERVER, None)
                .map_err(|_| AppError::Capture("selected audio endpoint refused activation"))?;
            let format = MixFormat(
                client
                    .GetMixFormat()
                    .map_err(|_| AppError::Capture("audio mix format unavailable"))?,
            );
            if format.0.is_null() {
                return Err(AppError::Capture("null audio mix format"));
            }
            let f = std::ptr::read_unaligned(format.0);
            let channels = usize::from(f.nChannels);
            let rate = f.nSamplesPerSec;
            PacketClock::new(rate)?;
            let encoding = if f.wFormatTag == 1 {
                SampleFormat::Pcm16
            } else if f.wFormatTag == 3 {
                SampleFormat::Float32
            } else if f.wFormatTag == 0xfffe && f.cbSize >= 22 {
                let ext = std::ptr::read_unaligned(format.0.cast::<WAVEFORMATEXTENSIBLE>());
                let expected_mask = if channels == 1 { 4 } else { 3 };
                if ext.dwChannelMask != expected_mask
                    || ext.Samples.wValidBitsPerSample != f.wBitsPerSample
                {
                    return Err(AppError::Capture(
                        "unsupported audio channel mask or valid-bit layout",
                    ));
                }
                let subtype = ext.SubFormat;
                if subtype == KSDATAFORMAT_SUBTYPE_PCM {
                    SampleFormat::Pcm16
                } else if subtype == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                    SampleFormat::Float32
                } else {
                    return Err(AppError::Capture("unsupported extensible audio subtype"));
                }
            } else {
                return Err(AppError::Capture("unsupported audio format"));
            };
            let bits = match encoding {
                SampleFormat::Pcm16 => 16,
                SampleFormat::Float32 => 32,
            };
            let align = channels * bits / 8;
            if ![1, 2].contains(&channels)
                || usize::from(f.wBitsPerSample) != bits
                || usize::from(f.nBlockAlign) != align
                || u64::from(f.nAvgBytesPerSec) != u64::from(rate) * align as u64
            {
                return Err(AppError::Capture(
                    "unsupported audio stride or layout; select mono/stereo PCM16/float32",
                ));
            }
            let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | if kind == SourceKind::System {
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                } else {
                    0
                };
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    flags,
                    1_000_000,
                    0,
                    format.0,
                    None,
                )
                .map_err(|_| {
                    AppError::Capture("WASAPI shared event stream initialization failed")
                })?;
            if client.GetBufferSize().map_err(err)? as u64 > u64::from(rate) / 4 {
                return Err(AppError::Capture("native audio buffer exceeds 250 ms"));
            }
            let event = Event(CreateEventW(None, false, false, PCWSTR::null()).map_err(err)?);
            client.SetEventHandle(event.0).map_err(err)?;
            let capture: IAudioCaptureClient = client.GetService().map_err(err)?;
            Ok(Self {
                capture,
                client,
                event,
                device,
                enumerator,
                endpoint_id,
                default: matches!(selection, EndpointSelection::Default),
                kind,
                rate,
                channels,
                format: encoding,
                align,
                _runtime: runtime,
            })
        }
    }
    fn verify_device(&self) -> Result<(), AppError> {
        unsafe {
            if self
                .device
                .GetState()
                .map_err(|_| AppError::Capture("selected audio endpoint invalidated"))?
                != DEVICE_STATE_ACTIVE
            {
                return Err(AppError::Capture(
                    "selected audio endpoint is no longer active",
                ));
            }
            if self.default {
                let flow = if self.kind == SourceKind::System {
                    eRender
                } else {
                    eCapture
                };
                let current = self
                    .enumerator
                    .GetDefaultAudioEndpoint(flow, eConsole)
                    .map_err(|_| {
                        AppError::Capture("selected default audio endpoint disappeared")
                    })?;
                if device_id(&current)? != self.endpoint_id {
                    return Err(AppError::Capture(
                        "selected default audio endpoint changed; explicit reselection required",
                    ));
                }
            }
        }
        Ok(())
    }
    fn packet(&self, clock: &mut PacketClock) -> Result<Option<(Vec<f32>, Duration)>, AppError> {
        unsafe {
            if self
                .capture
                .GetNextPacketSize()
                .map_err(|_| AppError::Capture("audio endpoint packet query failed"))?
                == 0
            {
                return Ok(None);
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut position = 0;
            let mut qpc = 0;
            self.capture
                .GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut position),
                    Some(&mut qpc),
                )
                .map_err(|_| AppError::Capture("audio packet acquisition failed"))?;
            // Every successful GetBuffer is paired with ReleaseBuffer even when validation fails.
            let result = (|| {
                if flags
                    & (AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32
                        | AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32)
                    != 0
                {
                    return Err(AppError::Capture(
                        "WASAPI reported discontinuity or invalid timestamp",
                    ));
                }
                if flags & !(AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || frames > self.rate / 4 {
                    return Err(AppError::Capture(
                        "unsupported audio flags or oversized packet",
                    ));
                }
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                let bytes = if silent {
                    &[][..]
                } else {
                    if data.is_null() {
                        return Err(AppError::Capture("null nonsilent WASAPI packet"));
                    }
                    std::slice::from_raw_parts(data, frames as usize * self.align)
                };
                let time = clock.observe(position, qpc, frames)?;
                Ok((
                    normalize_pcm(bytes, frames as usize, self.format, self.channels, silent)?,
                    time,
                ))
            })();
            let release = self
                .capture
                .ReleaseBuffer(frames)
                .map_err(|_| AppError::Capture("WASAPI packet release failed"));
            release?;
            result.map(Some)
        }
    }
}
unsafe fn device_id(d: &IMMDevice) -> Result<String, AppError> {
    unsafe {
        let id = d.GetId().map_err(err)?;
        let result = id
            .to_string()
            .map_err(|_| AppError::Capture("invalid endpoint identifier"));
        CoTaskMemFree(Some(id.0.cast()));
        result
    }
}
fn run(
    kind: SourceKind,
    endpoint: EndpointSelection,
    rx: mpsc::Receiver<Command>,
    shared: &Arc<Mutex<Buffer>>,
    ready: mpsc::SyncSender<Result<(), AppError>>,
) -> Result<(), AppError> {
    let setup = (|| {
        let c = Client::open(kind, &endpoint)?;
        let r = Resampler::new(c.rate)?;
        Ok::<_, AppError>((c, r))
    })();
    let (client, mut resampler) = match setup {
        Ok(c) => {
            let _ = ready.send(Ok(()));
            c
        }
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }
    };
    let mut paused = true;
    let mut clock = PacketClock::new(client.rate)?;
    let mut output_end = Duration::ZERO;
    loop {
        while let Ok(command) = rx.try_recv() {
            match command {
                Command::Stop => return Ok(()),
                Command::Pause(ack) => {
                    let result = (|| {
                        if !paused {
                            unsafe {
                                client.client.Stop().map_err(err)?;
                            }
                            collect(&client, &mut clock, &mut resampler, shared, &mut output_end)?;
                            let tail = resampler.drain()?;
                            enqueue(shared, tail, output_end, &mut output_end)?;
                            unsafe {
                                client.client.Reset().map_err(err)?;
                            }
                            paused = true;
                            resampler.reset()?;
                        }
                        Ok(())
                    })();
                    let _ = ack.send(result.clone());
                    result?;
                }
                Command::Resume(ack) => {
                    let result = (|| {
                        if paused {
                            client.verify_device()?;
                            clock = PacketClock::new(client.rate)?;
                            {
                                let mut b = shared
                                    .lock()
                                    .map_err(|_| AppError::Capture("audio buffer poisoned"))?;
                                b.blocks.clear();
                                b.frames = 0;
                            }
                            output_end = qpc_now()?;
                            unsafe {
                                client.client.Start().map_err(|_| {
                                    AppError::Capture("selected audio stream failed to start")
                                })?;
                            }
                            paused = false;
                        }
                        Ok(())
                    })();
                    let _ = ack.send(result.clone());
                    result?;
                }
            }
        }
        // A short event wait bounds command latency; it does not poll audio packets.
        let wait = unsafe { WaitForSingleObject(client.event.0, 10) };
        if wait == WAIT_FAILED {
            return Err(AppError::Capture("audio event wait failed"));
        }
        if paused {
            continue;
        }
        client.verify_device()?;
        if wait == WAIT_OBJECT_0 {
            collect(&client, &mut clock, &mut resampler, shared, &mut output_end)?;
        }
        // Shared-mode loopback sends no packets when its render endpoint is idle.
        // Verify endpoint identity/state, query an empty packet buffer, then advance
        // explicit silence. Input devices never use this idle-render exception.
        if kind == SourceKind::System
            && wait == WAIT_TIMEOUT
            && unsafe { client.capture.GetNextPacketSize().map_err(err)? } == 0
        {
            let now = qpc_now()?;
            let missing = now.saturating_sub(output_end);
            if missing > Duration::from_millis(20) {
                let frames = ((missing - Duration::from_millis(10)).as_nanos() * 48000
                    / 1_000_000_000)
                    .min(4800) as usize;
                enqueue(shared, vec![0.0; frames * 2], output_end, &mut output_end)?;
            }
        }
    }
}
fn enqueue(
    shared: &Arc<Mutex<Buffer>>,
    stereo: Vec<f32>,
    qpc: Duration,
    end: &mut Duration,
) -> Result<(), AppError> {
    if stereo.is_empty() {
        return Ok(());
    }
    let mut b = shared
        .lock()
        .map_err(|_| AppError::Capture("audio buffer poisoned"))?;
    let frames = stereo.len() / 2;
    if b.frames + frames > super::timeline::SOURCE_PENDING_FRAMES {
        return Err(AppError::Capture(
            "selected audio capture queue exceeded 125 ms; recording interrupted",
        ));
    }
    *end = qpc + Duration::from_nanos(frames as u64 * 1_000_000_000 / 48000);
    b.frames += frames;
    b.blocks.push_back(SourceBlock { stereo, qpc });
    Ok(())
}
fn collect(
    client: &Client,
    clock: &mut PacketClock,
    resampler: &mut Resampler,
    shared: &Arc<Mutex<Buffer>>,
    end: &mut Duration,
) -> Result<(), AppError> {
    while let Some((pcm, qpc)) = client.packet(clock)? {
        let start = qpc
            .checked_sub(resampler.pending_duration())
            .ok_or(AppError::Capture("resampler timestamp underflow"))?;
        let stereo = resampler.process(&pcm)?;
        enqueue(shared, stereo, start, end)?;
    }
    Ok(())
}
