//! Thread-affine COM/MF ownership and checked native sample construction.
#![allow(unsafe_code)]
use crate::app::error::AppError;
use std::{marker::PhantomData, rc::Rc, time::Duration};
use windows::Win32::{
    Media::MediaFoundation::*,
    System::{Com::*, Performance::*},
};
pub(crate) fn err(_: windows::core::Error) -> AppError {
    AppError::Encoder("native media operation failed; selected format/device requires correction")
}
pub(crate) struct Runtime {
    _thread: PhantomData<Rc<()>>,
}
impl Runtime {
    pub fn new() -> Result<Self, AppError> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(err)?;
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(err(e));
            }
        }
        Ok(Self {
            _thread: PhantomData,
        })
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}
pub fn qpc_now() -> Result<Duration, AppError> {
    unsafe {
        let mut ticks = 0;
        let mut frequency = 0;
        QueryPerformanceCounter(&mut ticks).map_err(err)?;
        QueryPerformanceFrequency(&mut frequency).map_err(err)?;
        if ticks < 0 || frequency <= 0 {
            return Err(AppError::Capture("invalid performance counter"));
        }
        let nanos = (ticks as u128) * 1_000_000_000 / (frequency as u128);
        Ok(Duration::new(
            (nanos / 1_000_000_000) as u64,
            (nanos % 1_000_000_000) as u32,
        ))
    }
}
pub(crate) fn hns(t: Duration) -> Result<i64, AppError> {
    i64::try_from(t.as_nanos() / 100).map_err(|_| AppError::Encoder("native timestamp overflow"))
}
pub(crate) fn sample(
    data: &[u8],
    time: Duration,
    duration: Duration,
) -> Result<IMFSample, AppError> {
    unsafe {
        let size = u32::try_from(data.len()).map_err(|_| AppError::Encoder("sample too large"))?;
        let buffer = MFCreateMemoryBuffer(size).map_err(err)?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None).map_err(err)?;
        // Lock succeeded for a buffer of exactly size bytes. No fallible operation before Unlock.
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.Unlock().map_err(err)?;
        buffer.SetCurrentLength(size).map_err(err)?;
        let sample = MFCreateSample().map_err(err)?;
        sample.AddBuffer(&buffer).map_err(err)?;
        sample.SetSampleTime(hns(time)?).map_err(err)?;
        sample.SetSampleDuration(hns(duration)?).map_err(err)?;
        Ok(sample)
    }
}
pub(crate) fn pcm_type(rate: u32, float: bool) -> Result<IMFMediaType, AppError> {
    unsafe {
        let t = MFCreateMediaType().map_err(err)?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(err)?;
        t.SetGUID(
            &MF_MT_SUBTYPE,
            if float {
                &MFAudioFormat_Float
            } else {
                &MFAudioFormat_PCM
            },
        )
        .map_err(err)?;
        let bytes = if float { 4 } else { 2 };
        for (k, v) in [
            (MF_MT_AUDIO_NUM_CHANNELS, 2),
            (MF_MT_AUDIO_SAMPLES_PER_SECOND, rate),
            (MF_MT_AUDIO_BITS_PER_SAMPLE, bytes * 8),
            (MF_MT_AUDIO_BLOCK_ALIGNMENT, bytes * 2),
            (MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * bytes * 2),
            (MF_MT_AUDIO_CHANNEL_MASK, 3),
        ] {
            t.SetUINT32(&k, v).map_err(err)?;
        }
        Ok(t)
    }
}
