use super::geometry::PhysicalRect;
use crate::app::error::AppError;

/// Session token; only the native adapter can resolve it to a window handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(u64);
impl WindowId {
    pub fn new(token: u64) -> Result<Self, AppError> {
        if token == 0 {
            return Err(AppError::InvalidInput("window token must be nonzero"));
        }
        Ok(Self(token))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowCandidate {
    pub id: WindowId,
    pub bounds: PhysicalRect,
}
pub struct WindowState {
    pub candidate: WindowCandidate,
    pub visible: bool,
    pub minimized: bool,
    pub cloaked: bool,
    pub owned_by_app: bool,
}
pub fn eligible_windows(windows: impl IntoIterator<Item = WindowState>) -> Vec<WindowCandidate> {
    windows
        .into_iter()
        .filter(|w| w.visible && !w.minimized && !w.cloaked && !w.owned_by_app)
        .map(|w| w.candidate)
        .collect()
}
