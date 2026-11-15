use super::state::{PinId, PinOptions, PinState, PinnedImage};
use crate::{
    app::error::AppError,
    capture::geometry::{PhysicalPoint, PhysicalRect},
};
use image::RgbaImage;
use std::{collections::HashMap, sync::Arc};

pub const MAX_PINS: usize = 16;
pub const MAX_PINNED_PIXELS: u64 = 128_000_000;

#[derive(Clone)]
pub struct PinSnapshot {
    pub id: PinId,
    pub image: PinnedImage,
    pub state: PinState,
    pub visible: bool,
}
impl PinSnapshot {
    pub fn display_size(&self) -> (u32, u32) {
        self.state.display_size(&self.image)
    }
}
pub enum PinInput {
    Move { dx: i32, dy: i32 },
    Zoom(i32),
    Opacity(i32),
    ToggleMinimized,
    Reset,
    Copy,
    Options(PinOptions),
    Close,
}
pub enum PinEffect {
    UpdateViewport(PinSnapshot),
    CloseViewport(PinId),
    CopyImage(Arc<RgbaImage>),
}
#[derive(Default)]
pub struct PinManager {
    pins: HashMap<PinId, PinSnapshot>,
    pixels: u64,
    work_areas: Vec<PhysicalRect>,
    pointer: Option<PhysicalPoint>,
    capture_depth: u32,
}
impl PinManager {
    pub fn get(&self, id: PinId) -> Option<&PinSnapshot> {
        self.pins.get(&id)
    }
    pub fn snapshots(&self) -> Vec<PinSnapshot> {
        self.pins.values().cloned().collect()
    }
    pub fn check_capacity(&self, image: &PinnedImage) -> Result<(), AppError> {
        if image.pixels().width().max(image.pixels().height()) > 32760 {
            return Err(AppError::InvalidInput(
                "image is too long for a pin window; save or review it instead",
            ));
        }
        if self.pins.len() >= MAX_PINS {
            return Err(AppError::InUse(
                "at most 16 pin windows may be open; close a pin first",
            ));
        }
        if image.pixel_count() > MAX_PINNED_PIXELS.saturating_sub(self.pixels) {
            return Err(AppError::InUse(
                "pins exceed the 128-megapixel total budget; close a pin first",
            ));
        }
        Ok(())
    }
    /// Validate both pixel capacity and a reachable display before persistence.
    pub fn validate_open(
        &self,
        image: &PinnedImage,
        placement: Option<PhysicalPoint>,
    ) -> Result<(), AppError> {
        self.check_capacity(image)?;
        self.area_for(placement.or(self.pointer))?;
        Ok(())
    }
    pub fn open(
        &mut self,
        image: PinnedImage,
        placement: Option<PhysicalPoint>,
    ) -> Result<PinId, AppError> {
        self.check_capacity(&image)?;
        let area = self.area_for(placement.or(self.pointer))?;
        let position = placement.unwrap_or_else(|| PhysicalPoint {
            x: (i64::from(area.left())
                + (i64::from(area.width()) - i64::from(image.pixels().width())) / 2)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            y: (i64::from(area.top())
                + (i64::from(area.height()) - i64::from(image.pixels().height())) / 2)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        });
        let id = PinId::new();
        let mut pin = PinSnapshot {
            id,
            image,
            state: PinState::new(position),
            visible: self.capture_depth == 0,
        };
        clamp_to_area(&mut pin, area);
        self.pixels += pin.image.pixel_count();
        self.pins.insert(id, pin);
        Ok(id)
    }
    pub fn handle(&mut self, id: PinId, input: PinInput) -> Result<Vec<PinEffect>, AppError> {
        if matches!(input, PinInput::Close) {
            if let Some(pin) = self.pins.remove(&id) {
                self.pixels -= pin.image.pixel_count();
            }
            return Ok(vec![PinEffect::CloseViewport(id)]);
        }
        let mut pin = self
            .pins
            .get(&id)
            .cloned()
            .ok_or(AppError::InvalidInput("pin is no longer open"))?;
        match input {
            PinInput::Move { dx, dy } => {
                pin.state.position = PhysicalPoint {
                    x: pin
                        .state
                        .position
                        .x
                        .checked_add(dx)
                        .ok_or(AppError::InvalidInput("pin movement overflows"))?,
                    y: pin
                        .state
                        .position
                        .y
                        .checked_add(dy)
                        .ok_or(AppError::InvalidInput("pin movement overflows"))?,
                };
            }
            PinInput::Zoom(steps) => {
                pin.state.zoom =
                    (i64::from(pin.state.zoom) + i64::from(steps) * 10).clamp(20, 500) as u16
            }
            PinInput::Opacity(steps) => {
                pin.state.opacity =
                    (i64::from(pin.state.opacity) + i64::from(steps) * 5).clamp(10, 100) as u8
            }
            PinInput::ToggleMinimized => pin.state.minimized = !pin.state.minimized,
            PinInput::Reset => {
                pin.state.zoom = 100;
                pin.state.opacity = 100;
                pin.state.minimized = false;
            }
            PinInput::Options(options) => pin.state.options = options,
            PinInput::Copy => return Ok(vec![PinEffect::CopyImage(pin.image.pixels().clone())]),
            PinInput::Close => unreachable!(),
        }
        if pin.display_size().0.max(pin.display_size().1) > 32760 {
            return Err(AppError::InvalidInput(
                "pin zoom exceeds the native viewport size limit",
            ));
        }
        let area = self.area_for(Some(pin.state.position))?;
        clamp_to_area(&mut pin, area);
        self.pins.insert(id, pin.clone());
        Ok(vec![PinEffect::UpdateViewport(pin)])
    }
    pub fn close_all(&mut self) -> Vec<PinEffect> {
        self.pixels = 0;
        self.pins
            .drain()
            .map(|(id, _)| PinEffect::CloseViewport(id))
            .collect()
    }
    fn area_for(&self, point: Option<PhysicalPoint>) -> Result<PhysicalRect, AppError> {
        self.work_areas
            .iter()
            .copied()
            .find(|area| point.is_some_and(|p| area.contains(p)))
            .or_else(|| self.work_areas.first().copied())
            .ok_or(AppError::Capture(
                "no monitor work area is available for pins",
            ))
    }
    pub fn update_work_areas(
        &mut self,
        areas: Vec<PhysicalRect>,
        pointer: PhysicalPoint,
    ) -> Result<Vec<PinEffect>, AppError> {
        if areas.is_empty() || areas.len() > 64 {
            return Err(AppError::InvalidInput(
                "pin work area count is out of range",
            ));
        }
        self.work_areas = areas;
        self.pointer = Some(pointer);
        let mut effects = vec![];
        for pin in self.pins.values_mut() {
            let area = self
                .work_areas
                .iter()
                .copied()
                .find(|a| a.contains(pin.state.position))
                .unwrap_or(self.work_areas[0]);
            let previous = pin.state.position;
            clamp_to_area(pin, area);
            if previous != pin.state.position {
                effects.push(PinEffect::UpdateViewport(pin.clone()));
            }
        }
        Ok(effects)
    }
    pub fn suspend_for_capture(&mut self) -> Vec<PinEffect> {
        self.capture_depth = self.capture_depth.saturating_add(1);
        self.visibility(false)
    }
    pub fn restore_after_capture(&mut self) -> Vec<PinEffect> {
        self.capture_depth = self.capture_depth.saturating_sub(1);
        self.visibility(self.capture_depth == 0)
    }
    fn visibility(&mut self, visible: bool) -> Vec<PinEffect> {
        self.pins
            .values_mut()
            .filter_map(|pin| {
                if pin.visible == visible {
                    return None;
                }
                pin.visible = visible;
                Some(PinEffect::UpdateViewport(pin.clone()))
            })
            .collect()
    }
}
fn clamp_to_area(pin: &mut PinSnapshot, area: PhysicalRect) {
    let (w, h) = pin.display_size();
    let (w, h) = (w.max(48), h.max(48));
    pin.state.position.x = pin.state.position.x.clamp(
        area.left(),
        (i64::from(area.right()) - i64::from(w.min(area.width()))) as i32,
    );
    pin.state.position.y = pin.state.position.y.clamp(
        area.top(),
        (i64::from(area.bottom()) - i64::from(h.min(area.height()))) as i32,
    );
}

/// Injectable native boundary. Implementations must verify process AND window
/// identity before every operation, because HWND values can be reused.
pub trait PinWindowSystem {
    fn is_owned(&self, id: PinId, handle: usize) -> bool;
    fn exclude_from_capture(&mut self, handle: usize) -> bool;
    fn is_visible(&self, handle: usize) -> bool;
    fn set_visible(&mut self, handle: usize, visible: bool) -> Result<(), AppError>;
    fn flush(&mut self) -> Result<(), AppError>;
}
struct NativePin {
    handle: usize,
    desired_visible: bool,
    exclusion_verified: bool,
}
pub struct PinWindowRegistry<S> {
    system: S,
    windows: HashMap<PinId, NativePin>,
    depth: u32,
}
impl<S: PinWindowSystem> PinWindowRegistry<S> {
    pub fn new(system: S) -> Self {
        Self {
            system,
            windows: HashMap::new(),
            depth: 0,
        }
    }
    pub fn suspended(&self) -> bool {
        self.depth > 0
    }
    pub fn unregister(&mut self, id: PinId) {
        self.windows.remove(&id);
    }
    /// Hide and synchronize before forgetting a HWND. Egui closes deferred
    /// viewports later; removing a still-visible handle here could otherwise
    /// let a simultaneous snapshot capture that final frame.
    pub fn retire(&mut self, id: PinId) -> Result<(), AppError> {
        if let Some(pin) = self.windows.get(&id)
            && self.system.is_owned(id, pin.handle)
            && self.system.is_visible(pin.handle)
        {
            self.system.set_visible(pin.handle, false)?;
            self.system.flush()?;
        }
        self.windows.remove(&id);
        Ok(())
    }
    pub fn register(&mut self, id: PinId, handle: usize) -> Result<bool, AppError> {
        if !self.system.is_owned(id, handle) {
            return Err(AppError::Capture("pin window identity changed"));
        }
        if let Some(old) = self.windows.get(&id)
            && old.handle == handle
        {
            return Ok(old.exclusion_verified);
        }
        if !self.windows.contains_key(&id) && self.windows.len() >= MAX_PINS * 2 {
            return Err(AppError::InUse("too many native pin windows"));
        }
        let exclusion_verified = self.system.exclude_from_capture(handle);
        // New viewports start hidden and cannot reveal themselves during a
        // concurrent recording/snapshot, even if affinity is unavailable.
        if self.suspended() {
            self.system.set_visible(handle, false)?;
        }
        self.windows.insert(
            id,
            NativePin {
                handle,
                desired_visible: true,
                exclusion_verified,
            },
        );
        Ok(exclusion_verified)
    }
    pub fn set_visible(&mut self, id: PinId, visible: bool) -> Result<(), AppError> {
        let suspended = self.suspended();
        let pin = self
            .windows
            .get_mut(&id)
            .ok_or(AppError::Capture("pin HWND is not registered"))?;
        if !self.system.is_owned(id, pin.handle) {
            return Err(AppError::Capture("pin window identity changed"));
        }
        pin.desired_visible = visible;
        if self.system.is_visible(pin.handle) != (visible && !suspended) {
            self.system.set_visible(pin.handle, visible && !suspended)?;
        }
        Ok(())
    }
    pub fn suspend_for_capture(&mut self) -> Result<(), AppError> {
        let next = self
            .depth
            .checked_add(1)
            .ok_or(AppError::Capture("pin capture nesting overflows"))?;
        if self.suspended() {
            self.depth = next;
            return Ok(());
        }
        self.windows
            .retain(|id, pin| self.system.is_owned(*id, pin.handle));
        let mut hidden = vec![];
        let result = (|| {
            for pin in self.windows.values() {
                let visible = self.system.is_visible(pin.handle);
                if visible {
                    self.system.set_visible(pin.handle, false)?;
                }
                hidden.push((pin.handle, visible));
            }
            if !hidden.is_empty() {
                self.system.flush()?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            for (handle, visible) in hidden {
                if let Err(restore) = self.system.set_visible(handle, visible) {
                    eprintln!("pin visibility rollback failed: {restore}");
                }
            }
            return Err(error);
        }
        self.depth = next;
        Ok(())
    }
    pub fn restore_after_capture(&mut self) -> Result<(), AppError> {
        self.depth = self.depth.saturating_sub(1);
        if self.suspended() {
            return Ok(());
        }
        let mut error = None;
        for (id, pin) in &self.windows {
            if self.system.is_owned(*id, pin.handle)
                && let Err(e) = self.system.set_visible(pin.handle, pin.desired_visible)
            {
                error.get_or_insert(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
