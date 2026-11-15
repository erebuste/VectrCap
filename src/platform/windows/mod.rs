pub mod appearance;
pub mod clipboard;
pub mod credentials;
pub mod file_dialog;
pub mod hotkeys;
pub mod instance;
pub mod shell_actions;
pub mod startup;
pub mod tray;

pub mod capture;
pub mod monitors;
pub mod poster;
#[allow(clippy::module_inception)] // Task 5's specified native window adapter path.
pub mod windows;

pub mod recording_events;

pub mod clipboard_sources;
pub mod destination_association;
pub mod explorer;
pub mod ipc;

pub mod drop_target;
