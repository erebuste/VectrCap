use crate::{
    app::error::AppError,
    capture::geometry::{MonitorGeometry, PhysicalRect},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputSize {
    Original,
    Fit1280x720,
    Fit1920x1080,
}
impl OutputSize {
    pub fn dimensions(self, w: u32, h: u32) -> Result<(u32, u32), AppError> {
        PhysicalRect::new(0, 0, w, h)?.rgba_len()?;
        let (mw, mh) = match self {
            Self::Original => (w, h),
            Self::Fit1280x720 => (1280, 720),
            Self::Fit1920x1080 => (1920, 1080),
        };
        let scale = 1.0_f64.min(mw as f64 / w as f64).min(mh as f64 / h as f64);
        Ok((
            (w as f64 * scale).floor().max(1.0) as u32,
            (h as f64 * scale).floor().max(1.0) as u32,
        ))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Small,
    Balanced,
    High,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioMode {
    Off,
    System,
    Microphone,
    Both,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndpointSelection {
    Default,
    Id(String),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioOptions {
    pub mode: AudioMode,
    pub system: EndpointSelection,
    pub microphone: EndpointSelection,
    pub system_gain: u8,
    pub microphone_gain: u8,
    pub aac_kbps: u32,
}
impl Default for AudioOptions {
    fn default() -> Self {
        Self {
            mode: AudioMode::Off,
            system: EndpointSelection::Default,
            microphone: EndpointSelection::Default,
            system_gain: 100,
            microphone_gain: 100,
            aac_kbps: 128,
        }
    }
}
impl AudioOptions {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.system_gain > 100
            || self.microphone_gain > 100
            || ![96, 128, 160, 192].contains(&self.aac_kbps)
        {
            return Err(AppError::InvalidInput(
                "audio gain or AAC bitrate is unsupported",
            ));
        }
        for e in [&self.system, &self.microphone] {
            if let EndpointSelection::Id(id) = e
                && (id.is_empty() || id.len() > 4096 || id.chars().any(char::is_control))
            {
                return Err(AppError::InvalidInput("invalid audio endpoint selection"));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GifOptions {
    pub fps: u32,
}
impl Default for GifOptions {
    fn default() -> Self {
        Self { fps: 15 }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mp4Options {
    pub fps: u32,
    pub quality: Quality,
    pub custom_bitrate: Option<u32>,
    pub audio: AudioOptions,
}
impl Default for Mp4Options {
    fn default() -> Self {
        Self {
            fps: 30,
            quality: Quality::Balanced,
            custom_bitrate: None,
            audio: AudioOptions::default(),
        }
    }
}
impl Mp4Options {
    pub fn bitrate(&self, w: u32, h: u32) -> u32 {
        self.custom_bitrate.unwrap_or_else(|| {
            let bpp = match self.quality {
                Quality::Small => 0.06,
                Quality::Balanced => 0.12,
                Quality::High => 0.20,
            };
            ((w as f64 * h as f64 * self.fps as f64 * bpp / 1000.0).round() * 1000.0)
                .clamp(500_000.0, 100_000_000.0) as u32
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormatOptions {
    Gif(GifOptions),
    Mp4(Mp4Options),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingOptions {
    pub monitor_id: String,
    pub rect: PhysicalRect,
    pub cursor: bool,
    pub countdown_seconds: u32,
    pub timed_stop_seconds: Option<u32>,
    pub maximum_active_seconds: u32,
    pub output_size: OutputSize,
    pub confirm_discard: bool,
    pub format: FormatOptions,
}
impl RecordingOptions {
    pub fn new(monitor_id: String, rect: PhysicalRect, mp4: bool) -> Self {
        Self {
            monitor_id,
            rect,
            cursor: true,
            countdown_seconds: 3,
            timed_stop_seconds: None,
            maximum_active_seconds: if mp4 { 600 } else { 60 },
            output_size: OutputSize::Original,
            confirm_discard: true,
            format: if mp4 {
                FormatOptions::Mp4(Mp4Options::default())
            } else {
                FormatOptions::Gif(GifOptions::default())
            },
        }
    }
    pub fn fps(&self) -> u32 {
        match &self.format {
            FormatOptions::Gif(g) => g.fps,
            FormatOptions::Mp4(m) => m.fps,
        }
    }
    pub fn active_limit_seconds(&self) -> u32 {
        self.timed_stop_seconds
            .unwrap_or(self.maximum_active_seconds)
    }
    pub fn audio(&self) -> Option<&AudioOptions> {
        match &self.format {
            FormatOptions::Mp4(m) if m.audio.mode != AudioMode::Off => Some(&m.audio),
            _ => None,
        }
    }
    pub fn dimensions(&self) -> Result<(u32, u32), AppError> {
        let (w, h) = self
            .output_size
            .dimensions(self.rect.width(), self.rect.height())?;
        let (w, h) = if matches!(self.format, FormatOptions::Mp4(_)) {
            ((w + 1) & !1, (h + 1) & !1)
        } else {
            (w, h)
        };
        PhysicalRect::new(0, 0, w, h)?.rgba_len()?;
        Ok((w, h))
    }
    pub fn validate(&self) -> Result<(), AppError> {
        if self.monitor_id.is_empty()
            || self.monitor_id.len() > 1024
            || self.monitor_id.chars().any(char::is_control)
        {
            return Err(AppError::InvalidInput("invalid recording monitor"));
        }
        let max = match &self.format {
            FormatOptions::Gif(g) => {
                if ![10, 15, 20].contains(&g.fps) {
                    return Err(AppError::InvalidInput("GIF FPS must be 10, 15 or 20"));
                }
                300
            }
            FormatOptions::Mp4(m) => {
                if ![15, 30, 60].contains(&m.fps) {
                    return Err(AppError::InvalidInput("MP4 FPS must be 15, 30 or 60"));
                }
                if m.custom_bitrate
                    .is_some_and(|b| !(500_000..=100_000_000).contains(&b))
                {
                    return Err(AppError::InvalidInput(
                        "MP4 bitrate must be 0.5 to 100 Mbps",
                    ));
                }
                m.audio.validate()?;
                3600
            }
        };
        if self.countdown_seconds > 60
            || self.maximum_active_seconds == 0
            || self.maximum_active_seconds > max
            || self
                .timed_stop_seconds
                .is_some_and(|s| s == 0 || s > self.maximum_active_seconds)
        {
            return Err(AppError::InvalidInput(
                "recording countdown or active duration is out of range",
            ));
        }
        self.dimensions()?;
        Ok(())
    }
    pub fn validate_monitor(&self, monitor: &MonitorGeometry) -> Result<(), AppError> {
        self.validate()?;
        let b = monitor.bounds();
        let r = self.rect;
        if self.monitor_id != monitor.id()
            || r.left() < b.left()
            || r.top() < b.top()
            || i64::from(r.left()) + i64::from(r.width())
                > i64::from(b.left()) + i64::from(b.width())
            || i64::from(r.top()) + i64::from(r.height())
                > i64::from(b.top()) + i64::from(b.height())
        {
            return Err(AppError::InvalidInput(
                "recording must stay inside the selected monitor",
            ));
        }
        Ok(())
    }
}
