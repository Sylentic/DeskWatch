//! DeskWatch bridge library: config, page model, MQTT and data sources.
//! The `deskwatch-bridge` binary in `main.rs` wires these together.

/// The version shown to people (setup page, dashboard snapshot). A release
/// build shows the plain version. The `:dev` image is built with
/// `DESKWATCH_DEV_BUILD` set (the commit, cut to 7 characters), which gives `1.0.5-dev (abc1234)`.
/// Version checks and the user agent keep the plain `CARGO_PKG_VERSION`.
pub fn build_label() -> &'static str {
    static LABEL: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| match option_env!("DESKWATCH_DEV_BUILD").map(str::trim) {
            Some("") | None => env!("CARGO_PKG_VERSION").to_string(),
            Some(commit) => format!(
                "{}-dev ({})",
                env!("CARGO_PKG_VERSION"),
                commit.get(..7).unwrap_or(commit)
            ),
        });
    &LABEL
}

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
pub mod notices;
pub mod probe;
pub mod prometheus;
pub mod setup;
pub mod source;
pub mod store;
pub mod supervisor;
pub mod update;
