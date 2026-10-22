//! DeskWatch bridge library: config, page model, MQTT and data sources.
//! The `deskwatch-bridge` binary in `main.rs` wires these together.

pub mod access;
pub mod alertmanager;
pub mod alerts;
pub mod auth;
pub mod azure_devops;
pub mod bitbucket;
pub mod ci;
pub mod composer;
pub mod config;
pub mod demo;
pub mod fleet;
pub mod fonts;
pub mod gitea;
pub mod github;
pub mod gitlab;
pub mod health;
pub mod hooks;
pub mod icons;
pub mod kiosk;
pub mod model;
pub mod mqtt;
pub mod probe;
pub mod prometheus;
pub mod setup;
pub mod source;
pub mod stats;
pub mod store;
pub mod supervisor;
pub mod update;
