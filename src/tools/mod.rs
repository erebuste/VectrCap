//! Memory-only desktop utilities. These types cannot produce capture artifacts.
pub mod color_result;
pub mod ruler;
pub mod sampler;
pub mod session;

use crate::{
    app::error::AppError,
    capture::{
        geometry::{MonitorGeometry, PhysicalRect},
        request::CaptureSnapshot,
    },
    editor::EditorDocument,
};

/// One owned RGBA buffer, with monitor validity independent of presentation scale.
/// The editor's immutable source adapter supplies the shared Task 6 sampling path;
/// no editor mutation or output action is exposed by this wrapper.
pub struct UtilitySnapshot {
    document: EditorDocument,
    monitors: Vec<MonitorGeometry>,
    geometry_generation: u64,
    launch_pointer: Option<crate::capture::geometry::PhysicalPoint>,
}
impl UtilitySnapshot {
    pub fn new(
        pixels: Vec<u8>,
        bounds: PhysicalRect,
        monitors: Vec<MonitorGeometry>,
        geometry_generation: u64,
    ) -> Result<Self, AppError> {
        if monitors.iter().any(|m| !bounds.contains_rect(m.bounds())) {
            return Err(AppError::Capture("utility monitor exceeds source bounds"));
        }
        let snapshot = CaptureSnapshot::new(pixels, bounds, monitors.clone(), vec![])?;
        Ok(Self {
            document: EditorDocument::new(snapshot)?,
            monitors,
            geometry_generation,
            launch_pointer: None,
        })
    }
    pub fn pixels(&self) -> &[u8] {
        self.document.source().as_raw()
    }
    pub fn bounds(&self) -> PhysicalRect {
        self.document.bounds()
    }
    pub fn monitors(&self) -> &[MonitorGeometry] {
        &self.monitors
    }
    pub fn geometry_generation(&self) -> u64 {
        self.geometry_generation
    }
    pub fn launch_pointer(&self) -> Option<crate::capture::geometry::PhysicalPoint> {
        self.launch_pointer
    }
    pub(crate) fn set_launch_pointer(&mut self, point: crate::capture::geometry::PhysicalPoint) {
        self.launch_pointer = Some(point);
    }
    pub(crate) fn document(&self) -> &EditorDocument {
        &self.document
    }
}
