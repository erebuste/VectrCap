//! Cancellable source acquisition before an overlay exists. No output side effects.
use super::{
    geometry::{
        MonitorGeometry, PhysicalPoint, PhysicalRect, same_layout, validate_layout, virtual_bounds,
    },
    last_region::LastRegion,
    preferences::RegionCapturePreferences,
    windows::WindowCandidate,
};
use crate::app::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureMode {
    Region,
    Window,
    Fullscreen,
    LastRegion,
}
#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub mode: CaptureMode,
    pub delay_seconds: u8,
    pub include_cursor: bool,
    pub target: Option<WindowCandidate>,
    pub last_region: Option<LastRegion>,
    pub preferences: RegionCapturePreferences,
}
impl CaptureRequest {
    pub fn new(
        mode: CaptureMode,
        delay_seconds: u8,
        include_cursor: bool,
        preferences: RegionCapturePreferences,
    ) -> Result<Self, AppError> {
        let request = Self {
            mode,
            delay_seconds,
            include_cursor,
            target: None,
            last_region: None,
            preferences,
        };
        request.validate()?;
        Ok(request)
    }
    pub fn validate(&self) -> Result<(), AppError> {
        if self.delay_seconds > 60 {
            return Err(AppError::InvalidInput(
                "screenshot delay must be 0 to 60 seconds",
            ));
        }
        self.preferences.validate()?;
        if let Some(region) = &self.last_region {
            region.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureOverride {
    pub mode: CaptureMode,
    pub delay_seconds: Option<u8>,
    pub include_cursor: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScreenshotPreferences {
    pub delay_seconds: u8,
    pub include_cursor: bool,
    pub overrides: Vec<CaptureOverride>,
}
impl Default for ScreenshotPreferences {
    fn default() -> Self {
        // ShareX shows the cursor in screenshots by default.
        Self {
            delay_seconds: 0,
            include_cursor: true,
            overrides: Vec::new(),
        }
    }
}
impl ScreenshotPreferences {
    pub const DELAY_PRESETS: [u8; 4] = [0, 3, 5, 10];
    pub fn validate(&self) -> Result<(), AppError> {
        if self.delay_seconds > 60 || self.overrides.len() > 4 {
            return Err(AppError::InvalidInput(
                "screenshot delay or override count is out of range",
            ));
        }
        for (i, o) in self.overrides.iter().enumerate() {
            if o.delay_seconds.is_some_and(|d| d > 60)
                || self.overrides[..i].iter().any(|other| o.mode == other.mode)
            {
                return Err(AppError::InvalidInput(
                    "screenshot override is invalid or duplicated",
                ));
            }
        }
        Ok(())
    }
    pub fn request(
        &self,
        mode: CaptureMode,
        prefs: RegionCapturePreferences,
    ) -> Result<CaptureRequest, AppError> {
        self.validate()?;
        let override_ = self.overrides.iter().find(|o| o.mode == mode);
        CaptureRequest::new(
            mode,
            override_
                .and_then(|o| o.delay_seconds)
                .unwrap_or(self.delay_seconds),
            override_
                .and_then(|o| o.include_cursor)
                .unwrap_or(self.include_cursor),
            prefs,
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotHint {
    NoLastRegion,
    LayoutChanged,
}
pub struct CaptureSnapshot {
    pub(crate) mode: CaptureMode,
    pub(crate) pixels: Vec<u8>,
    pub(crate) bounds: PhysicalRect,
    pub(crate) monitors: Vec<MonitorGeometry>,
    pub(crate) windows: Vec<WindowCandidate>,
    /// ShareX hover targets in pick order (controls, client areas, windows);
    /// empty means hover uses the window bounds.
    pub(crate) hover_areas: Vec<PhysicalRect>,
    pub(crate) initial_crop: Option<PhysicalRect>,
    pub(crate) hint: Option<SnapshotHint>,
    preferences: RegionCapturePreferences,
    launch_pointer: Option<PhysicalPoint>,
}
impl std::fmt::Debug for CaptureSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSnapshot")
            .field("pixel_bytes", &self.pixels.len())
            .field("bounds", &self.bounds)
            .field("monitors", &self.monitors)
            .field("window_count", &self.windows.len())
            .field("initial_crop", &self.initial_crop)
            .field("hint", &self.hint)
            .finish_non_exhaustive()
    }
}
impl CaptureSnapshot {
    pub fn new(
        pixels: Vec<u8>,
        bounds: PhysicalRect,
        monitors: Vec<MonitorGeometry>,
        windows: Vec<WindowCandidate>,
    ) -> Result<Self, AppError> {
        validate_layout(&monitors)?;
        if pixels.len() != bounds.rgba_len()?
            || !monitors
                .iter()
                .any(|m| m.bounds().intersection(bounds).is_some())
            || windows.len() > 4096
        {
            return Err(AppError::Capture("snapshot pixels or geometry are invalid"));
        }
        Ok(Self {
            mode: CaptureMode::Region,
            pixels,
            bounds,
            monitors,
            windows,
            hover_areas: vec![],
            initial_crop: None,
            hint: None,
            preferences: RegionCapturePreferences::default(),
            launch_pointer: None,
        })
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn bounds(&self) -> PhysicalRect {
        self.bounds
    }
    pub fn monitors(&self) -> &[MonitorGeometry] {
        &self.monitors
    }
    pub fn windows(&self) -> &[WindowCandidate] {
        &self.windows
    }
    pub fn with_hover_areas(mut self, areas: Vec<PhysicalRect>) -> Self {
        self.hover_areas = areas;
        self
    }
    pub fn initial_crop(&self) -> Option<PhysicalRect> {
        self.initial_crop
    }
    pub fn hint(&self) -> Option<SnapshotHint> {
        self.hint
    }
    pub fn preferences(&self) -> &RegionCapturePreferences {
        &self.preferences
    }
    pub fn launch_pointer(&self) -> Option<PhysicalPoint> {
        self.launch_pointer
    }
    /// Gaps and out-of-source positions are unavailable, not an invented black pixel.
    pub fn sample(&self, p: PhysicalPoint) -> Option<[u8; 4]> {
        if !self.monitors.iter().any(|m| m.bounds().contains(p)) {
            return None;
        }
        let (x, y) = self.bounds.pixel_offset(p).ok()?;
        let index = ((u64::from(y) * u64::from(self.bounds.width()) + u64::from(x)) * 4) as usize;
        self.pixels.get(index..index + 4)?.try_into().ok()
    }
}
/// The native UI owns countdown presentation. hide_chrome must synchronously hide
/// and flush its presentation before returning; the overlay is created only after snapshot.
pub trait Countdown {
    fn wait(&mut self, seconds: u8, cancel: &CancellationToken) -> Result<(), AppError>;
    fn hide_chrome(&mut self) -> Result<(), AppError>;
}
pub trait CaptureSource {
    /// Utility sources must explicitly support visible references. Refuse an
    /// adapter that would silently apply screenshot exclusion policy.
    fn set_purpose(&mut self, purpose: SourcePurpose) -> Result<(), AppError> {
        match purpose {
            SourcePurpose::Screenshot => Ok(()),
            SourcePurpose::Utility => {
                Err(AppError::Capture("utility capture source is unsupported"))
            }
        }
    }
    fn monitors(&mut self) -> Result<Vec<MonitorGeometry>, AppError>;
    fn pointer(&mut self) -> Result<PhysicalPoint, AppError>;
    fn windows(&mut self) -> Result<Vec<WindowCandidate>, AppError>;
    /// ShareX "detect controls": child controls and client areas of `windows`
    /// (z-ordered), plus the windows, in hover pick order. Best effort.
    fn hover_areas(&mut self, _windows: &[WindowCandidate]) -> Vec<PhysicalRect> {
        vec![]
    }
    /// Retain the actual window source identity before a delay (not just a reusable HWND).
    /// This must not acquire pixels or start a capture session.
    fn prepare_window(&mut self, target: WindowCandidate) -> Result<(), AppError>;
    fn desktop(
        &mut self,
        bounds: PhysicalRect,
        monitors: &[MonitorGeometry],
        cursor: bool,
    ) -> Result<Vec<u8>, AppError>;
    /// Acquire only the source retained by prepare_window before any delay.
    /// Return an error if no source was prepared; never resolve a new source here.
    fn window(&mut self, target: WindowCandidate, cursor: bool) -> Result<Vec<u8>, AppError>;
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SourcePurpose {
    #[default]
    Screenshot,
    Utility,
}
pub struct CaptureService<S, C> {
    source: S,
    countdown: C,
    cancel: CancellationToken,
}
/// Compare identities and geometry without changing the frozen hover z-order.
fn same_window_layout(a: &[WindowCandidate], b: &[WindowCandidate]) -> bool {
    let a_set: HashSet<_> = a.iter().collect();
    let b_set: HashSet<_> = b.iter().collect();
    // Native inventories contain each candidate once; reject malformed duplicates.
    a.len() == b.len() && a_set.len() == a.len() && a_set == b_set
}
impl<S: CaptureSource, C: Countdown> CaptureService<S, C> {
    pub fn utility_snapshot(&mut self) -> Result<crate::tools::UtilitySnapshot, AppError> {
        self.check_cancel()?;
        self.source.set_purpose(SourcePurpose::Utility)?;
        let layout = self.source.monitors()?;
        validate_layout(&layout)?;
        let pointer = self.source.pointer()?;
        let bounds = virtual_bounds(&layout.iter().map(|m| m.bounds()).collect::<Vec<_>>())?;
        bounds.rgba_len()?;
        self.countdown.hide_chrome()?;
        self.check_cancel()?;
        if !same_layout(&layout, &self.source.monitors()?) {
            return Err(AppError::Capture(
                "display layout changed before utility capture",
            ));
        }
        let pixels = self.source.desktop(bounds, &layout, false)?;
        self.check_cancel()?;
        if !same_layout(&layout, &self.source.monitors()?) {
            return Err(AppError::Capture(
                "display layout changed during utility capture",
            ));
        }
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = GENERATION
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |g| g.checked_add(1),
            )
            .map_err(|_| AppError::Capture("utility geometry generation exhausted"))?;
        let mut snapshot = crate::tools::UtilitySnapshot::new(pixels, bounds, layout, generation)?;
        snapshot.set_launch_pointer(pointer);
        Ok(snapshot)
    }
    pub fn new(source: S, countdown: C, cancel: CancellationToken) -> Self {
        Self {
            source,
            countdown,
            cancel,
        }
    }
    pub fn into_source(self) -> S {
        self.source
    }
    fn check_cancel(&self) -> Result<(), AppError> {
        if self.cancel.is_cancelled() {
            Err(AppError::Cancelled)
        } else {
            Ok(())
        }
    }
    pub fn snapshot(&mut self, request: CaptureRequest) -> Result<CaptureSnapshot, AppError> {
        request.validate()?;
        self.source.set_purpose(SourcePurpose::Screenshot)?;
        self.check_cancel()?;
        let layout = self.source.monitors()?;
        validate_layout(&layout)?;
        let pointer = self.source.pointer()?;
        layout
            .iter()
            .find(|m| m.bounds().contains(pointer))
            .ok_or(AppError::Capture("pointer is outside the display layout"))?;
        let target = if request.mode == CaptureMode::Window {
            let target = request
                .target
                .ok_or(AppError::Capture("a fixed window target must be selected"))?;
            if !self.source.windows()?.contains(&target) {
                return Err(AppError::Capture("window target changed; select it again"));
            }
            self.source.prepare_window(target)?;
            Some(target)
        } else {
            None
        };
        let bounds = match request.mode {
            CaptureMode::Window => target.expect("window target was validated").bounds,
            // Fullscreen is the whole desktop across every monitor, as in ShareX.
            _ => virtual_bounds(&layout.iter().map(|m| m.bounds()).collect::<Vec<_>>())?,
        };
        bounds.rgba_len()?;
        self.countdown.wait(request.delay_seconds, &self.cancel)?;
        self.check_cancel()?;
        self.countdown.hide_chrome()?;
        self.check_cancel()?;
        if !same_layout(&layout, &self.source.monitors()?) {
            return Err(AppError::Capture("display layout changed; select again"));
        }
        let windows = self.source.windows()?;
        if target.is_some_and(|t| !windows.contains(&t)) {
            return Err(AppError::Capture("window target changed; select it again"));
        }
        let pixels = if let Some(target) = target {
            self.source.window(target, request.include_cursor)?
        } else {
            self.source
                .desktop(bounds, &layout, request.include_cursor)?
        };
        self.check_cancel()?;
        // Read before the freshness check below, so windows that moved invalidate them too.
        let areas = self.source.hover_areas(&windows);
        if !same_layout(&layout, &self.source.monitors()?) {
            return Err(AppError::Capture("display layout changed while capturing"));
        }
        if !same_window_layout(&windows, &self.source.windows()?) {
            return Err(AppError::Capture(
                "window layout changed while capturing; try again",
            ));
        }
        let mut snapshot = CaptureSnapshot::new(pixels, bounds, layout, windows)?;
        snapshot.hover_areas = areas;
        snapshot.mode = request.mode;
        match request.mode {
            CaptureMode::Window | CaptureMode::Fullscreen => snapshot.initial_crop = Some(bounds),
            CaptureMode::LastRegion => match request.last_region {
                Some(region) if region.matches_layout(&snapshot.monitors) => {
                    snapshot.initial_crop = Some(region.bounds())
                }
                Some(_) => snapshot.hint = Some(SnapshotHint::LayoutChanged),
                None => snapshot.hint = Some(SnapshotHint::NoLastRegion),
            },
            CaptureMode::Region => {}
        }
        snapshot.preferences = request.preferences;
        snapshot.launch_pointer = Some(pointer);
        Ok(snapshot)
    }
}
pub struct CursorImage {
    position: PhysicalPoint,
    hotspot: (u32, u32),
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}
impl CursorImage {
    pub fn new(
        position: PhysicalPoint,
        hotspot: (u32, u32),
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<Self, AppError> {
        if width > 512
            || height > 512
            || hotspot.0 >= width
            || hotspot.1 >= height
            || rgba.len() != PhysicalRect::new(0, 0, width, height)?.rgba_len()?
        {
            return Err(AppError::InvalidInput(
                "cursor dimensions or hotspot are invalid",
            ));
        }
        Ok(Self {
            position,
            hotspot,
            width,
            height,
            rgba,
        })
    }
}
pub fn composite_cursor(
    pixels: &mut [u8],
    bounds: PhysicalRect,
    cursor: &CursorImage,
) -> Result<(), AppError> {
    if pixels.len() != bounds.rgba_len()? {
        return Err(AppError::Capture("cursor destination buffer is invalid"));
    }
    let left = i64::from(cursor.position.x) - i64::from(cursor.hotspot.0);
    let top = i64::from(cursor.position.y) - i64::from(cursor.hotspot.1);
    for y in 0..cursor.height {
        for x in 0..cursor.width {
            let (dx, dy) = (
                left + i64::from(x) - i64::from(bounds.left()),
                top + i64::from(y) - i64::from(bounds.top()),
            );
            if dx < 0
                || dy < 0
                || dx >= i64::from(bounds.width())
                || dy >= i64::from(bounds.height())
            {
                continue;
            }
            let src = ((y * cursor.width + x) * 4) as usize;
            let dst = ((dy as u64 * u64::from(bounds.width()) + dx as u64) * 4) as usize;
            let alpha = u32::from(cursor.rgba[src + 3]);
            let dest_alpha = u32::from(pixels[dst + 3]);
            let out_alpha = alpha * 255 + dest_alpha * (255 - alpha);
            for channel in 0..3 {
                pixels[dst + channel] = (u32::from(cursor.rgba[src + channel]) * alpha * 255
                    + u32::from(pixels[dst + channel]) * dest_alpha * (255 - alpha)
                    + out_alpha / 2)
                    .checked_div(out_alpha)
                    .unwrap_or(0) as u8;
            }
            pixels[dst + 3] = out_alpha.div_ceil(255) as u8;
        }
    }
    Ok(())
}
