use super::options::*;
use crate::{
    app::error::AppError, capture::geometry::PhysicalRect, persistence::settings::RecordingLimits,
};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingPreferences {
    pub gif: GifOptions,
    pub mp4: Mp4Options,
    pub cursor: bool,
    pub countdown_seconds: u32,
    pub output_size: OutputSize,
    pub confirm_discard: bool,
    pub gif_timed_stop: Option<u32>,
    pub mp4_timed_stop: Option<u32>,
}
impl Default for RecordingPreferences {
    fn default() -> Self {
        Self {
            gif: Default::default(),
            mp4: Default::default(),
            cursor: true,
            countdown_seconds: 3,
            output_size: OutputSize::Original,
            confirm_discard: true,
            gif_timed_stop: None,
            mp4_timed_stop: None,
        }
    }
}
impl RecordingPreferences {
    pub fn options(
        &self,
        mp4: bool,
        monitor_id: String,
        rect: PhysicalRect,
        limits: RecordingLimits,
    ) -> RecordingOptions {
        let mut o = RecordingOptions::new(monitor_id, rect, mp4);
        o.format = if mp4 {
            FormatOptions::Mp4(self.mp4.clone())
        } else {
            FormatOptions::Gif(self.gif.clone())
        };
        o.cursor = self.cursor;
        o.countdown_seconds = self.countdown_seconds;
        o.output_size = self.output_size;
        o.confirm_discard = self.confirm_discard;
        o.maximum_active_seconds = if mp4 {
            limits.mp4_max_minutes * 60
        } else {
            limits.gif_max_seconds
        };
        o.timed_stop_seconds = if mp4 {
            self.mp4_timed_stop
        } else {
            self.gif_timed_stop
        };
        o
    }
    pub fn validate(&self) -> Result<(), AppError> {
        self.validate_limits(RecordingLimits::default())
    }
    pub fn validate_limits(&self, limits: RecordingLimits) -> Result<(), AppError> {
        limits.validate()?;
        for mp4 in [false, true] {
            self.options(
                mp4,
                "validation".into(),
                PhysicalRect::new(0, 0, 640, 360)?,
                limits,
            )
            .validate()?;
        }
        Ok(())
    }
}
