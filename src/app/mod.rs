//! Application-level shared types: errors, artifacts/jobs, and destinations.

pub mod controller;
pub mod desktop;
pub mod destination_manager;
pub mod destinations;
pub mod error;
pub mod events;
pub mod image_finish;
pub mod notifications;
pub mod types;

pub mod local_actions;
pub mod shortcuts;

pub mod batches;
pub mod cli;
pub mod destination_install;
pub mod ingress;
pub mod upload_sources;
