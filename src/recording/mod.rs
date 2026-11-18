//! Native recording, selected-area sessions and finalized-output ownership.
pub mod audio;
pub mod clock;
pub mod frame;
pub mod gif;
pub mod mp4;
pub(crate) mod native;
pub mod options;
pub mod worker;
use crate::app::error::AppError;
use audio::AudioChunk;
use frame::CapturedFrame;
use options::RecordingOptions;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub trait RecordingEncoder {
    fn push_video(&mut self, frame: &CapturedFrame) -> Result<(), AppError>;
    fn push_audio(&mut self, chunk: &AudioChunk) -> Result<(), AppError>;
    /// Sets the exact final active endpoint, including an unchanged last frame.
    fn end_at(&mut self, time: Duration) -> Result<(), AppError>;
    fn finish(self: Box<Self>) -> Result<EncodedRecording, AppError>;
}
#[derive(Debug)]
pub struct EncodedRecording {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub active_duration: Duration,
    pub options: RecordingOptions,
    // Temporary output remains owned until explicitly saved/staged by the caller.
    pub(crate) _directory: tempfile::TempDir,
}
pub(crate) struct OutputFile {
    dir: tempfile::TempDir,
    pub partial: PathBuf,
    extension: &'static str,
}
impl OutputFile {
    fn new(root: &Path, extension: &'static str) -> Result<Self, AppError> {
        let dir = tempfile::Builder::new()
            .prefix("vectrcap-recording-")
            .tempdir_in(root)
            .map_err(|e| AppError::io("creating private recording directory", &e))?;
        let partial = dir.path().join(format!("partial.{extension}"));
        Ok(Self {
            dir,
            partial,
            extension,
        })
    }
    fn publish(
        self,
        options: RecordingOptions,
        duration: Duration,
    ) -> Result<EncodedRecording, AppError> {
        if duration.is_zero() {
            return Err(AppError::Encoder("no usable recording duration"));
        }
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.partial)
            .map_err(|e| AppError::io("opening finalized recording", &e))?;
        if f.metadata()
            .map_err(|e| AppError::io("checking finalized recording", &e))?
            .len()
            == 0
        {
            return Err(AppError::Encoder("empty finalized recording"));
        }
        f.sync_all()
            .map_err(|e| AppError::io("flushing finalized recording", &e))?;
        drop(f);
        let path = self
            .dir
            .path()
            .join(format!("recording.{}", self.extension));
        std::fs::rename(&self.partial, &path)
            .map_err(|e| AppError::io("publishing finalized recording", &e))?;
        let (width, height) = options.dimensions()?;
        Ok(EncodedRecording {
            path,
            width,
            height,
            active_duration: duration,
            options,
            _directory: self.dir,
        })
    }
}

pub mod preferences;
pub mod review;
pub mod runtime;
pub mod session;
