//! Synchronous Media Foundation sink writer: no callback, no app-internal sender,
//! and MF_SINK_WRITER_DISABLE_THROTTLING explicitly false.
#![allow(unsafe_code)]
use super::{native::*, options::FormatOptions, *};
use crate::app::error::AppError;
use std::{path::Path, time::Duration};
use windows::{Win32::Media::MediaFoundation::*, core::HSTRING};
pub struct Mp4Encoder {
    writer: Option<IMFSinkWriter>,
    pending: Option<(Vec<u8>, Duration)>,
    video: u32,
    audio: Option<u32>,
    audio_end: Duration,
    end: Duration,
    options: RecordingOptions,
    output: Option<OutputFile>,
    // COM resources above must be released before shutting down MF/COM.
    _runtime: Runtime,
}
impl Mp4Encoder {
    pub fn new(root: &Path, options: RecordingOptions) -> Result<Self, AppError> {
        options.validate()?;
        let FormatOptions::Mp4(ref m) = options.format else {
            return Err(AppError::InvalidInput("MP4 encoder requires MP4 options"));
        };
        let runtime = Runtime::new()?;
        let output = OutputFile::new(root, "mp4")?;
        let (w, h) = options.dimensions()?;
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 3).map_err(err)?;
            let attrs = attrs.ok_or(AppError::Encoder("missing sink attributes"))?;
            attrs
                .SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 0)
                .map_err(err)?;
            attrs.SetUINT32(&MF_LOW_LATENCY, 1).map_err(err)?;
            let writer =
                MFCreateSinkWriterFromURL(&HSTRING::from(output.partial.as_os_str()), None, &attrs)
                    .map_err(|_| AppError::Encoder("MP4 sink unavailable"))?;
            let out = video_type(w, h, m.fps, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, m.bitrate(w, h))
                .map_err(err)?;
            let video = writer
                .AddStream(&out)
                .map_err(|_| AppError::Encoder("H.264 output settings unsupported"))?;
            // RGB32 is converted by the native color converter; positive stride is bottom-up.
            let input = video_type(w, h, m.fps, &MFVideoFormat_RGB32)?;
            writer.SetInputMediaType(video, &input, None).map_err(|_| {
                AppError::Encoder(
                    "H.264 input dimensions/FPS unsupported; choose supported settings",
                )
            })?;
            let audio = if options.audio().is_some() {
                let a = pcm_type(48000, false)?;
                a.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).map_err(err)?;
                a.SetUINT32(
                    &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                    m.audio.aac_kbps * 1000 / 8,
                )
                .map_err(err)?;
                a.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 1).map_err(err)?;
                a.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0).map_err(err)?;
                let stream = writer
                    .AddStream(&a)
                    .map_err(|_| AppError::Encoder("AAC output settings unsupported"))?;
                writer
                    .SetInputMediaType(stream, &pcm_type(48000, false)?, None)
                    .map_err(|_| AppError::Encoder("AAC PCM input unsupported"))?;
                Some(stream)
            } else {
                None
            };
            writer
                .BeginWriting()
                .map_err(|_| AppError::Encoder("native codec rejected recording settings"))?;
            Ok(Self {
                writer: Some(writer),
                pending: None,
                video,
                audio,
                audio_end: Duration::ZERO,
                end: Duration::ZERO,
                options,
                output: Some(output),
                _runtime: runtime,
            })
        }
    }
    fn flush_to(&mut self, end: Duration) -> Result<(), AppError> {
        if let Some((data, start)) = self.pending.take() {
            let duration = end
                .checked_sub(start)
                .filter(|t| !t.is_zero())
                .ok_or(AppError::Encoder("invalid video sample duration"))?;
            let s = sample(&data, start, duration)?;
            unsafe {
                self.writer
                    .as_ref()
                    .ok_or(AppError::Encoder("MP4 already finalized"))?
                    .WriteSample(self.video, &s)
                    .map_err(|_| AppError::Encoder("native H.264 sample write failed"))?;
            }
        }
        Ok(())
    }
}
unsafe fn video_type(
    w: u32,
    h: u32,
    fps: u32,
    subtype: &windows::core::GUID,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let t = MFCreateMediaType().map_err(err)?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(err)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype).map_err(err)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(err)?;
        for (k, v) in [
            (MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64),
            (MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1),
            (MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1),
        ] {
            t.SetUINT64(&k, v).map_err(err)?;
        }
        Ok(t)
    }
}
impl RecordingEncoder for Mp4Encoder {
    fn push_video(&mut self, f: &CapturedFrame) -> Result<(), AppError> {
        if (f.width(), f.height()) != self.options.dimensions()? {
            return Err(AppError::Encoder("MP4 frame dimensions changed"));
        }
        if let Some((_, t)) = &self.pending {
            if f.timestamp <= *t {
                return Err(AppError::Encoder("MP4 timestamps are not increasing"));
            }
        } else if !f.timestamp.is_zero() {
            return Err(AppError::Encoder("first MP4 frame must start at zero"));
        }
        self.flush_to(f.timestamp)?;
        let mut bgra = vec![0; f.rgba().len()];
        let stride = f.width() as usize * 4;
        for (y, row) in f.rgba().chunks_exact(stride).enumerate() {
            for (x, p) in row.as_chunks::<4>().0.iter().enumerate() {
                let i = (f.height() as usize - 1 - y) * stride + x * 4;
                bgra[i..i + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        self.pending = Some((bgra, f.timestamp));
        self.end =
            f.timestamp + Duration::from_nanos(1_000_000_000 / u64::from(self.options.fps()));
        Ok(())
    }
    fn push_audio(&mut self, a: &AudioChunk) -> Result<(), AppError> {
        let stream = self
            .audio
            .ok_or(AppError::Encoder("audio is disabled for this recording"))?;
        if a.timestamp.abs_diff(self.audio_end) > Duration::from_micros(22) {
            return Err(AppError::Encoder(
                "audio is not contiguous on the common timeline",
            ));
        }
        let bytes: Vec<u8> = a.samples().iter().flat_map(|s| s.to_le_bytes()).collect();
        let s = sample(&bytes, a.timestamp, a.duration())?;
        unsafe {
            self.writer
                .as_ref()
                .ok_or(AppError::Encoder("MP4 already finalized"))?
                .WriteSample(stream, &s)
                .map_err(|_| AppError::Encoder("native AAC sample write failed"))?;
        }
        self.audio_end = a.timestamp + a.duration();
        Ok(())
    }
    fn end_at(&mut self, t: Duration) -> Result<(), AppError> {
        if self.pending.as_ref().is_none_or(|(_, p)| t <= *p) {
            return Err(AppError::Encoder("invalid MP4 endpoint"));
        }
        if self.audio.is_some() && t.abs_diff(self.audio_end) > Duration::from_millis(21) {
            return Err(AppError::Encoder("audio/video endpoints diverged"));
        }
        self.end = t;
        Ok(())
    }
    fn finish(mut self: Box<Self>) -> Result<EncodedRecording, AppError> {
        if self.pending.is_none() {
            return Err(AppError::Encoder("no usable MP4 frame"));
        }
        if self.audio.is_some() && self.audio_end.is_zero() {
            return Err(AppError::Encoder("selected audio produced no samples"));
        }
        self.flush_to(self.end)?;
        let writer = self
            .writer
            .take()
            .ok_or(AppError::Encoder("MP4 already finalized"))?;
        unsafe {
            writer
                .Finalize()
                .map_err(|_| AppError::Encoder("native MP4 finalization failed"))?;
        }
        drop(writer);
        self.output
            .take()
            .ok_or(AppError::Encoder("missing MP4 output"))?
            .publish(self.options.clone(), self.end)
    }
}
