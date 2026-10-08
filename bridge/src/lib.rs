//! DeskWatch bridge library: config, page model, MQTT and data sources.
//! The `deskwatch-bridge` binary in `main.rs` wires these together.

pub mod ci;
pub mod composer;
pub mod config;
pub mod gitea;
pub mod github;
pub mod hooks;
pub mod model;
pub mod mqtt;
pub mod source;
pub mod stats;
