//! Sample-index alignment with a bounded correction window. QPC time, not the
//! number of callbacks, determines where each normalized block belongs.
use super::{
    AudioChunk,
    devices::{SourceKind, open_selected},
    mix::mix_stereo,
    wasapi::AudioSourceHandle,
};
use crate::{
    app::error::AppError,
    capture::source::SharedCaptureState,
    recording::{RecordingEncoder, options::AudioOptions},
};
use std::{collections::VecDeque, time::Duration};
pub const SOURCE_PENDING_FRAMES: usize = 6000; // 125 ms here + 125 ms on capture owner.
pub struct AlignedSource {
    samples: VecDeque<f32>,
    cursor: u64,
    corrections: u64,
}
impl AlignedSource {
    pub fn new(cursor: u64) -> Self {
        Self {
            samples: VecDeque::new(),
            cursor,
            corrections: 0,
        }
    }
    pub fn push(&mut self, at: u64, samples: &[f32]) -> Result<(), AppError> {
        if !samples.len().is_multiple_of(2) || samples.iter().any(|s| !s.is_finite()) {
            return Err(AppError::Capture("invalid normalized source samples"));
        }
        let expected = self.cursor + self.samples.len() as u64 / 2;
        let difference = at.abs_diff(expected);
        // Allow startup latency up to 100 ms, then at most 20 ms per packet.
        let tolerance = if expected == 0 { 4800 } else { 960 };
        if difference > tolerance {
            return Err(AppError::Capture(
                "audio source exceeded alignment tolerance",
            ));
        }
        let skip = expected.saturating_sub(at).min(samples.len() as u64 / 2) as usize;
        let gap = at.saturating_sub(expected) as usize;
        if self.samples.len() / 2 + gap + samples.len() / 2 - skip > SOURCE_PENDING_FRAMES {
            return Err(AppError::Capture("aligned source exceeded 125 ms queue"));
        }
        self.corrections = self.corrections.saturating_add(difference);
        // More than 20 ms of correction per second indicates sustained bad timing.
        if self.corrections > 4800 + self.cursor / 50 {
            return Err(AppError::Capture(
                "sustained source clock correction exceeded budget",
            ));
        }
        self.samples.extend(std::iter::repeat_n(0.0, gap * 2));
        self.samples.extend(&samples[skip * 2..]);
        Ok(())
    }
    pub fn available(&self) -> usize {
        self.samples.len() / 2
    }
    pub fn take(&mut self, frames: usize) -> Result<Vec<f32>, AppError> {
        if frames > self.available() {
            return Err(AppError::Capture(
                "selected audio source has not reached the common timeline",
            ));
        }
        self.cursor += frames as u64;
        Ok(self.samples.drain(..frames * 2).collect())
    }
    pub fn clear_at(&mut self, frame: u64) {
        self.samples.clear();
        self.cursor = frame;
        self.corrections = 0;
    }
}
pub struct PhysicalAudio {
    sources: Vec<(SourceKind, AudioSourceHandle, AlignedSource)>,
    options: AudioOptions,
    pub frames: u64,
}
impl PhysicalAudio {
    pub fn open(options: &AudioOptions) -> Result<Self, AppError> {
        let sources = open_selected(Some(options), |kind, e| {
            Ok((
                kind,
                AudioSourceHandle::start(kind, e.clone())?,
                AlignedSource::new(0),
            ))
        })?;
        Ok(Self {
            sources,
            options: options.clone(),
            frames: 0,
        })
    }
    pub fn resume(&mut self) -> Result<(), AppError> {
        for (_, s, b) in &mut self.sources {
            b.clear_at(self.frames);
            s.resume()?;
        }
        Ok(())
    }
    pub fn pause(&mut self) -> Result<(), AppError> {
        for (_, s, _) in &self.sources {
            s.pause()?;
        }
        Ok(())
    }
    pub fn pump(
        &mut self,
        state: &SharedCaptureState,
        encoder: &mut dyn RecordingEncoder,
        end: Duration,
    ) -> Result<(), AppError> {
        for (_, source, aligned) in &mut self.sources {
            while let Some(block) = source.pop()? {
                let mapped = state
                    .lock()
                    .map_err(|_| AppError::Capture("capture state poisoned"))?
                    .map_qpc(block.qpc);
                if let Some(t) = mapped {
                    let at = (t.as_nanos() * 48000 / 1_000_000_000) as u64;
                    aligned.push(at, &block.stereo)?;
                }
            }
        }
        let target = (end.as_nanos() * 48000 / 1_000_000_000) as u64;
        let available = self
            .sources
            .iter()
            .map(|(_, _, b)| b.available())
            .min()
            .unwrap_or(0);
        let count = available.min(target.saturating_sub(self.frames) as usize);
        let mut remaining = count;
        while remaining > 0 {
            let n = remaining.min(480);
            let mut system = None;
            let mut mic = None;
            for (kind, _, b) in &mut self.sources {
                let data = b.take(n)?;
                match kind {
                    SourceKind::System => system = Some(data),
                    SourceKind::Microphone => mic = Some(data),
                }
            }
            let mixed = mix_stereo(system.as_deref(), mic.as_deref(), &self.options)?;
            encoder.push_audio(&AudioChunk::new(
                mixed,
                Duration::from_nanos(self.frames * 1_000_000_000 / 48000),
            )?)?;
            self.frames += n as u64;
            remaining -= n;
        }
        if target.saturating_sub(self.frames) > 12000 {
            return Err(AppError::Capture(
                "selected audio source stalled for over 250 ms",
            ));
        }
        Ok(())
    }
    pub fn end_time(&self) -> Duration {
        Duration::from_nanos(self.frames * 1_000_000_000 / 48000)
    }
}
