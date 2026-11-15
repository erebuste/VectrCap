//! Screen-anchored annotation state and deterministic, chrome-free CPU rendering.
pub mod commands;
pub mod document;
pub mod export;
pub mod render;
pub mod shapes;
pub mod text;

pub use commands::EditorCommand;
pub use document::{Annotation, EditorDocument, Style};
pub use render::{render_export, sample_source_color};
