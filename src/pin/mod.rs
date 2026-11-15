//! Session-only reference images. Presentation never changes finalized pixels.
pub mod manager;
pub mod source;
pub mod state;

pub use manager::{PinEffect, PinInput, PinManager, PinSnapshot};
pub use state::{PinId, PinOptions, PinnedImage};
