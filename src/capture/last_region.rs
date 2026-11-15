//! Geometry-only last region. Only an explicit successful region finish may replace it.
use super::{
    geometry::{MonitorGeometry, PhysicalRect, validate_layout, virtual_bounds},
    request::CaptureMode,
};
use crate::app::error::AppError;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LastRegionData", into = "LastRegionData")]
pub struct LastRegion {
    bounds: PhysicalRect,
    monitors: Vec<MonitorFingerprint>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitorFingerprint {
    id: String,
    bounds: PhysicalRect,
    scale_bits: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LastRegionData {
    bounds: PhysicalRect,
    monitors: Vec<MonitorFingerprint>,
}
#[derive(Debug, Clone, Copy)]
pub enum FinishOutcome {
    Confirmed(PhysicalRect),
    Cancelled,
    ClipboardEdit,
}
impl LastRegion {
    pub fn new(bounds: PhysicalRect, monitors: &[MonitorGeometry]) -> Result<Self, AppError> {
        validate_layout(monitors)?;
        bounds.rgba_len()?;
        if !virtual_bounds(&monitors.iter().map(|m| m.bounds()).collect::<Vec<_>>())?
            .contains_rect(bounds)
        {
            return Err(AppError::InvalidInput(
                "saved region exceeds the display layout",
            ));
        }
        let mut participating: Vec<_> = monitors
            .iter()
            .filter(|m| m.bounds().intersection(bounds).is_some())
            .map(|m| MonitorFingerprint {
                id: m.id().into(),
                bounds: m.bounds(),
                scale_bits: m.scale_factor().to_bits(),
            })
            .collect();
        if participating.is_empty() {
            return Err(AppError::InvalidInput(
                "saved region contains no monitor pixels",
            ));
        }
        participating.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Self {
            bounds,
            monitors: participating,
        })
    }
    pub fn bounds(&self) -> PhysicalRect {
        self.bounds
    }
    pub fn matches_layout(&self, monitors: &[MonitorGeometry]) -> bool {
        Self::new(self.bounds, monitors).is_ok_and(|current| current == *self)
    }
    pub fn after_finish(
        previous: Option<Self>,
        mode: CaptureMode,
        outcome: FinishOutcome,
        monitors: &[MonitorGeometry],
    ) -> Result<Option<Self>, AppError> {
        match (mode, outcome) {
            (CaptureMode::Region | CaptureMode::LastRegion, FinishOutcome::Confirmed(bounds)) => {
                Ok(Some(Self::new(bounds, monitors)?))
            }
            _ => Ok(previous),
        }
    }
    pub fn validate(&self) -> Result<(), AppError> {
        let monitors = self
            .monitors
            .iter()
            .map(|m| MonitorGeometry::new(m.id.clone(), m.bounds, f64::from_bits(m.scale_bits)))
            .collect::<Result<Vec<_>, _>>()?;
        validate_layout(&monitors)?;
        self.bounds.rgba_len()?;
        // A valid crop edge may lie in a gap beyond a participating monitor.
        // Only the fresh full layout can check the virtual-desktop envelope.
        for (index, monitor) in monitors.iter().enumerate() {
            if monitor.bounds().intersection(self.bounds).is_none()
                || (index > 0 && monitors[index - 1].id() >= monitor.id())
            {
                return Err(AppError::InvalidInput(
                    "saved region fingerprint is invalid",
                ));
            }
        }
        Ok(())
    }
}
impl TryFrom<LastRegionData> for LastRegion {
    type Error = AppError;
    fn try_from(data: LastRegionData) -> Result<Self, AppError> {
        let region = Self {
            bounds: data.bounds,
            monitors: data.monitors,
        };
        region.validate()?;
        Ok(region)
    }
}
impl From<LastRegion> for LastRegionData {
    fn from(r: LastRegion) -> Self {
        Self {
            bounds: r.bounds,
            monitors: r.monitors,
        }
    }
}
