//! Bridge configuration, loaded from a TOML file.
//!
//! Every field has a sensible default, so a minimal config only needs the MQTT
//! host. Secrets never live in this file: the MQTT password comes from the
//! `DESKWATCH_MQTT_PASSWORD` environment variable (see the systemd unit).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::RotationEntry;

/// Environment variable that holds the MQTT password.
pub const MQTT_PASSWORD_ENV: &str = "DESKWATCH_MQTT_PASSWORD";

/// Config path used when none is given on the command line or in `DESKWATCH_CONFIG`.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/deskwatch/bridge.toml";

/// Top-level config file layout.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub mqtt: MqttConfig,
    #[serde(default)]
    pub server: ServerConfig,
    /// Idle pages in the order they rotate. See `[[rotation]]` in the example config.
    #[serde(default = "default_rotation")]
    pub rotation: Vec<RotationEntry>,
}

/// Connection to the Mosquitto broker.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    /// Optional broker username. The password is read from `DESKWATCH_MQTT_PASSWORD`.
    pub username: Option<String>,
    /// First part of every topic, `deskpanel` in the schema.
    pub topic_prefix: String,
    /// Seconds between MQTT keep-alive pings.
    pub keep_alive_s: u64,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 1883,
            client_id: "deskwatch-bridge".into(),
            username: None,
            topic_prefix: "deskpanel".into(),
            keep_alive_s: 30,
        }
    }
}

/// The local `server` source: stats about the machine the bridge runs on.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Name shown on the stats page. Defaults to the system hostname.
    pub display_name: Option<String>,
    /// Seconds between stats samples and publishes (5 s in the schema).
    pub interval_s: u64,
    /// Network interface to report. Defaults to the busiest non-loopback one.
    pub net_iface: Option<String>,
    /// Filesystem used for `disk_pct`.
    pub disk_path: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            display_name: None,
            interval_s: 5,
            net_iface: None,
            disk_path: PathBuf::from("/"),
        }
    }
}

/// Rotation used when the config has no `[[rotation]]` blocks: the stats page only.
fn default_rotation() -> Vec<RotationEntry> {
    vec![RotationEntry {
        page: "stats".into(),
        dwell_s: 20,
        skip_when_empty: false,
    }]
}

impl Config {
    /// Parse a config from TOML text.
    pub fn from_toml(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text).context("invalid config")?;
        anyhow::ensure!(
            config.server.interval_s > 0,
            "server.interval_s must be above 0"
        );
        anyhow::ensure!(
            !config.rotation.is_empty(),
            "rotation needs at least one page"
        );
        Ok(config)
    }

    /// Read and parse a config file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        Self::from_toml(&text).with_context(|| format!("in config file {}", path.display()))
    }

    /// Pick the config path: first CLI argument, then `DESKWATCH_CONFIG`, then the default.
    pub fn path_from_env() -> PathBuf {
        std::env::args_os()
            .nth(1)
            .or_else(|| std::env::var_os("DESKWATCH_CONFIG"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_uses_defaults() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config.mqtt.host, "localhost");
        assert_eq!(config.mqtt.topic_prefix, "deskpanel");
        assert_eq!(config.server.interval_s, 5);
        assert_eq!(config.rotation.len(), 1);
        assert_eq!(config.rotation[0].page, "stats");
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.toml");
        let config = Config::from_toml(text).unwrap();
        assert_eq!(config.rotation.len(), 3);
        assert!(config.rotation[1].skip_when_empty);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // Catches typos such as `intervall_s` instead of silently using a default.
        assert!(Config::from_toml("[server]\nintervall_s = 5\n").is_err());
    }

    #[test]
    fn zero_interval_is_rejected() {
        assert!(Config::from_toml("[server]\ninterval_s = 0\n").is_err());
    }
}
