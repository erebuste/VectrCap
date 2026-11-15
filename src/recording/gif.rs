use super::{options::FormatOptions, *};
use crate::app::error::AppError;
use std::{fs::File, path::Path, time::Duration};
pub fn centiseconds(t: Duration) -> Result<u64, AppError> {
    u64::try_from((t.as_nanos() + 5_000_000) / 10_000_000)
        .map_err(|_| AppError::Encoder("GIF timestamp overflow"))
}
pub struct GifEncoder {
    writer: Option<::gif::Encoder<File>>,
    pending: Option<::gif::Frame<'static>>,
    pending_time: Duration,
    end: Duration,
    options: RecordingOptions,
    output: OutputFile,
}
impl GifEncoder {
    pub fn new(root: &Path, options: RecordingOptions) -> Result<Self, AppError> {
        options.validate()?;
        if !matches!(options.format, FormatOptions::Gif(_)) {
            return Err(AppError::InvalidInput("GIF encoder requires GIF options"));
        }
        let (w, h) = options.dimensions()?;
        let w = u16::try_from(w).map_err(|_| AppError::Encoder("GIF width unsupported"))?;
        let h = u16::try_from(h).map_err(|_| AppError::Encoder("GIF height unsupported"))?;
        let output = OutputFile::new(root, "gif")?;
        let file =
            File::create_new(&output.partial).map_err(|e| AppError::io("creating GIF", &e))?;
        let mut writer = ::gif::Encoder::new(file, w, h, &[])
            .map_err(|_| AppError::Encoder("creating GIF encoder"))?;
        writer
            .set_repeat(::gif::Repeat::Infinite)
            .map_err(|_| AppError::Encoder("setting GIF repeat"))?;
        Ok(Self {
            writer: Some(writer),
            pending: None,
            pending_time: Duration::ZERO,
            end: Duration::ZERO,
            options,
            output,
        })
    }
    fn flush_to(&mut self, t: Duration) -> Result<(), AppError> {
        let Some(mut frame) = self.pending.take() else {
            return Ok(());
        };
        let mut delay = centiseconds(t)?
            .checked_sub(centiseconds(self.pending_time)?)
            .ok_or(AppError::Encoder("GIF timestamps moved backwards"))?;
        if delay == 0 {
            return Err(AppError::Encoder(
                "GIF frame interval below one centisecond",
            ));
        }
        while delay > 0 {
            let n = delay.min(u16::MAX as u64);
            frame.delay = n as u16;
            self.writer
                .as_mut()
                .ok_or(AppError::Encoder("GIF already finalized"))?
                .write_frame(&frame)
                .map_err(|_| AppError::Encoder("writing GIF frame"))?;
            delay -= n;
        }
        Ok(())
    }
}
impl RecordingEncoder for GifEncoder {
    fn push_video(&mut self, f: &CapturedFrame) -> Result<(), AppError> {
        if (f.width(), f.height()) != self.options.dimensions()? {
            return Err(AppError::Encoder("GIF frame dimensions changed"));
        }
        if self.pending.is_some() && f.timestamp <= self.pending_time {
            return Err(AppError::Encoder("GIF timestamps are not increasing"));
        }
        if self.pending.is_none() && !f.timestamp.is_zero() {
            return Err(AppError::Encoder("first GIF frame must start at zero"));
        }
        self.flush_to(f.timestamp)?;
        let mut pixels = f.rgba().to_vec();
        self.pending = Some(::gif::Frame::from_rgba_speed(
            f.width() as u16,
            f.height() as u16,
            &mut pixels,
            10,
        ));
        self.pending_time = f.timestamp;
        self.end =
            f.timestamp + Duration::from_nanos(1_000_000_000 / u64::from(self.options.fps()));
        Ok(())
    }
    fn push_audio(&mut self, _: &AudioChunk) -> Result<(), AppError> {
        Err(AppError::Encoder("GIF does not accept audio"))
    }
    fn end_at(&mut self, t: Duration) -> Result<(), AppError> {
        if self.pending.is_none() || t <= self.pending_time {
            return Err(AppError::Encoder("invalid GIF endpoint"));
        }
        self.end = t;
        Ok(())
    }
    fn finish(mut self: Box<Self>) -> Result<EncodedRecording, AppError> {
        if self.pending.is_none() {
            return Err(AppError::Encoder("no usable GIF frame"));
        }
        self.flush_to(self.end)?;
        let writer = self
            .writer
            .take()
            .ok_or(AppError::Encoder("GIF already finalized"))?;
        let file = writer
            .into_inner()
            .map_err(|_| AppError::Encoder("GIF finalization failed"))?;
        file.sync_all()
            .map_err(|e| AppError::io("flushing GIF", &e))?;
        drop(file);
        self.output.publish(self.options, self.end)
    }
}
