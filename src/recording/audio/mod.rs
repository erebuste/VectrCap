pub mod devices;
pub mod mix;
pub mod resample;
pub mod timeline;
pub mod wasapi;
use crate::app::error::AppError;
use std::{collections::VecDeque, time::Duration};
pub const RATE: u32 = 48_000;
pub const MAX_AUDIO_FRAMES: usize = 12_000;
#[derive(Debug)]
pub struct AudioChunk {
    samples: Vec<i16>,
    pub timestamp: Duration,
}
impl AudioChunk {
    pub fn new(samples: Vec<i16>, timestamp: Duration) -> Result<Self, AppError> {
        if samples.is_empty()
            || !samples.len().is_multiple_of(2)
            || samples.len() / 2 > MAX_AUDIO_FRAMES
        {
            return Err(AppError::Capture(
                "audio block must be stereo PCM16 and at most 250 ms",
            ));
        }
        Ok(Self { samples, timestamp })
    }
    pub fn samples(&self) -> &[i16] {
        &self.samples
    }
    pub fn sample_frames(&self) -> usize {
        self.samples.len() / 2
    }
    pub fn duration(&self) -> Duration {
        Duration::from_nanos(self.sample_frames() as u64 * 1_000_000_000 / u64::from(RATE))
    }
}
#[derive(Default)]
pub struct AudioQueue {
    blocks: VecDeque<AudioChunk>,
    frames: usize,
}
impl AudioQueue {
    pub fn push(&mut self, chunk: AudioChunk) -> Result<(), AppError> {
        if self.frames + chunk.sample_frames() > MAX_AUDIO_FRAMES {
            return Err(AppError::Capture(
                "audio queue exceeded 250 ms; recording interrupted",
            ));
        }
        self.frames += chunk.sample_frames();
        self.blocks.push_back(chunk);
        Ok(())
    }
    pub fn pop(&mut self) -> Option<AudioChunk> {
        let c = self.blocks.pop_front()?;
        self.frames -= c.sample_frames();
        Some(c)
    }
    pub fn sample_frames(&self) -> usize {
        self.frames
    }
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.frames = 0;
    }
}
#[derive(Clone, Copy, Debug)]
pub enum SampleFormat {
    Pcm16,
    Float32,
}
/// Silent WASAPI packets are permitted to have no data pointer at all.
pub fn normalize_pcm(
    bytes: &[u8],
    frames: usize,
    format: SampleFormat,
    channels: usize,
    silent: bool,
) -> Result<Vec<f32>, AppError> {
    if ![1, 2].contains(&channels) || frames > 48_000 {
        return Err(AppError::Capture(
            "unsupported audio layout or oversized packet",
        ));
    }
    if silent {
        return Ok(vec![0.0; frames * 2]);
    }
    let width = match format {
        SampleFormat::Pcm16 => 2,
        SampleFormat::Float32 => 4,
    };
    if bytes.len() != frames * channels * width {
        return Err(AppError::Capture("invalid audio packet stride"));
    }
    let mut out = Vec::with_capacity(frames * 2);
    for frame in bytes.chunks_exact(channels * width) {
        let mut stereo = [0.0; 2];
        for ch in 0..channels {
            let b = &frame[ch * width..(ch + 1) * width];
            let v = match format {
                SampleFormat::Pcm16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                SampleFormat::Float32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            };
            if !v.is_finite() {
                return Err(AppError::Capture("nonfinite audio sample"));
            }
            stereo[ch] = v.clamp(-1.0, 1.0);
        }
        if channels == 1 {
            stereo[1] = stereo[0];
        }
        out.extend_from_slice(&stereo);
    }
    Ok(out)
}
/// Verifies device-position/QPC correlation; no default endpoint fallback.
pub struct PacketClock {
    rate: u32,
    last: Option<(u64, u64, u32)>,
}
impl PacketClock {
    pub fn new(rate: u32) -> Result<Self, AppError> {
        if !(8000..=192000).contains(&rate) {
            return Err(AppError::Capture("unsupported audio sample rate"));
        }
        Ok(Self { rate, last: None })
    }
    pub fn observe(
        &mut self,
        position: u64,
        qpc_hns: u64,
        frames: u32,
    ) -> Result<Duration, AppError> {
        if let Some((p, t, n)) = self.last {
            let delta = position
                .checked_sub(p)
                .ok_or(AppError::Capture("audio device position moved backwards"))?;
            let dt = qpc_hns
                .checked_sub(t)
                .ok_or(AppError::Capture("audio QPC timestamp moved backwards"))?;
            if delta < u64::from(n)
                || ((delta as u128 * 10_000_000 / u128::from(self.rate)).abs_diff(dt as u128))
                    > 200_000
            {
                return Err(AppError::Capture(
                    "audio discontinuity or clock drift exceeded 20 ms",
                ));
            }
        }
        self.last = Some((position, qpc_hns, frames));
        Ok(Duration::from_nanos(
            qpc_hns
                .checked_mul(100)
                .ok_or(AppError::Capture("audio timestamp overflow"))?,
        ))
    }
}
