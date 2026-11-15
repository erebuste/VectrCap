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
        let pin = self
            .pins
            .get_mut(&id)
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
        Ok(vec![PinEffect::UpdateViewport(pin.clone())])
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
    pin.state.position.x = pin.state.position.x.clamp(
        area.left(),
        (i64::from(area.right()) - i64::from(w.min(area.width()))) as i32,
    );
    pin.state.position.y = pin.state.position.y.clamp(
        area.top(),
        (i64::from(area.bottom()) - i64::from(h.min(area.height()))) as i32,
    );
}
