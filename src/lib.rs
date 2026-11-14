//! VectrCap domain library. `main.rs` (added with the tray app) only starts the
//! Windows app; everything testable lives here.

// File ownership/deletion safety is built on Windows handle semantics
// (share modes, directory-relative opens, delete-by-handle); there is no
// weaker fallback for other platforms.
#[cfg(not(windows))]
compile_error!("VectrCap targets Windows only");

pub mod app;
pub mod capture;
pub mod persistence;
pub mod platform;
pub mod ui;
pub mod vectr;
