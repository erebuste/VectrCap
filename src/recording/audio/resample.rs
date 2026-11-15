//! Windows Audio Resampler DSP, synchronous pull after each bounded input.
#![allow(unsafe_code)]
use super::{MAX_AUDIO_FRAMES, PacketClock};
use crate::{app::error::AppError, recording::native::*};
use std::{mem::ManuallyDrop, time::Duration};
use windows::Win32::{Media::MediaFoundation::*, System::Com::*};
pub struct Resampler {
    transform: IMFTransform,
    rate: u32,
    input_frames: u64,
    output_frames: u64,
    _runtime: Runtime,
}
impl Resampler {
    pub fn new(rate: u32) -> Result<Self, AppError> {
        PacketClock::new(rate)?;
        let runtime = Runtime::new()?;
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CResamplerMediaObject, None, CLSCTX_INPROC_SERVER)
                    .map_err(err)?;
            transform
                .SetInputType(0, &pcm_type(rate, true)?, 0)
                .map_err(err)?;
            transform
                .SetOutputType(0, &pcm_type(48000, true)?, 0)
                .map_err(err)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(err)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(err)?;
            Ok(Self {
                transform,
                rate,
                input_frames: 0,
                output_frames: 0,
                _runtime: runtime,
            })
        }
    }
    pub fn process(&mut self, stereo: &[f32]) -> Result<Vec<f32>, AppError> {
        if stereo.is_empty()
            || !stereo.len().is_multiple_of(2)
            || stereo.len() / 2 > self.rate as usize / 4
            || stereo.iter().any(|v| !v.is_finite())
        {
            return Err(AppError::Capture("invalid or oversized resampler input"));
        }
        let time = Duration::from_nanos(self.input_frames * 1_000_000_000 / u64::from(self.rate));
        let duration =
            Duration::from_nanos(stereo.len() as u64 / 2 * 1_000_000_000 / u64::from(self.rate));
        let bytes: Vec<u8> = stereo.iter().flat_map(|s| s.to_le_bytes()).collect();
        let s = sample(&bytes, time, duration)?;
        unsafe {
            self.transform.ProcessInput(0, &s, 0).map_err(err)?;
        }
        self.input_frames += stereo.len() as u64 / 2;
        self.pull()
    }
    pub fn drain(&mut self) -> Result<Vec<f32>, AppError> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(err)?;
        }
        self.pull()
    }
    pub fn reset(&mut self) -> Result<(), AppError> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .map_err(err)?;
        }
        self.input_frames = 0;
        self.output_frames = 0;
        Ok(())
    }
    pub fn pending_duration(&self) -> Duration {
        Duration::from_nanos(self.input_frames * 1_000_000_000 / u64::from(self.rate))
            .saturating_sub(Duration::from_nanos(
                self.output_frames * 1_000_000_000 / 48000,
            ))
    }
    fn pull(&mut self) -> Result<Vec<f32>, AppError> {
        let mut out = Vec::new();
        unsafe {
            // A fixed output allocation and a total 250 ms ceiling also bound unexpected MFT output.
            loop {
                let buffer = MFCreateMemoryBuffer((MAX_AUDIO_FRAMES * 8) as u32).map_err(err)?;
                let s = MFCreateSample().map_err(err)?;
                s.AddBuffer(&buffer).map_err(err)?;
                let mut data = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(s)),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut data, &mut status);
                ManuallyDrop::drop(&mut data[0].pEvents);
                ManuallyDrop::drop(&mut data[0].pSample);
                if let Err(e) = result {
                    if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                        break;
                    }
                    return Err(err(e));
                }
                let len = buffer.GetCurrentLength().map_err(err)? as usize;
                if len == 0 || !len.is_multiple_of(8) || out.len() + len / 4 > MAX_AUDIO_FRAMES * 2
                {
                    return Err(AppError::Capture("resampler output exceeded bounded block"));
                }
                let mut ptr = std::ptr::null_mut();
                buffer.Lock(&mut ptr, None, None).map_err(err)?;
                let bytes = std::slice::from_raw_parts(ptr, len);
                let values: Vec<f32> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                buffer.Unlock().map_err(err)?;
                if values.iter().any(|v| !v.is_finite()) {
                    return Err(AppError::Capture("nonfinite resampler output"));
                }
                out.extend(values);
            }
        }
        self.output_frames += out.len() as u64 / 2;
        Ok(out)
    }
}
