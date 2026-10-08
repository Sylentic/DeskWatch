//! Bridge configuration, loaded from a TOML file.
//!
//! Every field has a sensible default, so a minimal config only needs the MQTT
//! host. Secrets never live in this file: it only names credential files
//! (`token_file`, `webhook_secret_file`, `password_file`), which systemd
//! loads with `LoadCredential=` (see the sample unit).
//!
//! Data sources are arrays, so there can be several of each:
//! `[[source.gitea]]` blocks, `[[source.github]]` blocks and so on.

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::RotationEntry;
use crate::source::{Interrupt, valid_name};

/// Environment variable that holds the MQTT password, when `mqtt.password_file`
/// is not set.
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
    #[serde(default)]
    pub http: HttpConfig,
    /// Data sources, one array per adapter type.
    #[serde(default)]
    pub source: Sources,
    /// Alerts raised on `<topic_prefix>/alert` by Home Assistant or scripts.
    #[serde(default)]
    pub alerts: AlertsConfig,
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
    /// Optional broker username.
    pub username: Option<String>,
    /// Credential holding the broker password. Without it the password comes
    /// from `DESKWATCH_MQTT_PASSWORD`.
    pub password_file: Option<String>,
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
            password_file: None,
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

/// The shared HTTP listener for webhooks.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    /// Address webhook routes listen on. Only opened when a source needs it.
    pub listen: SocketAddr,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 8787)),
        }
    }
}

/// The `[alerts]` table: alerts that arrive on `<topic_prefix>/alert`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlertsConfig {
    /// Subscribe to the alert topic at all.
    pub enabled: bool,
    /// Most alerts kept at once; the least urgent, oldest one is dropped first.
    pub max_active: usize,
    /// When set, only these alert ids may be `critical` (level 0, above
    /// running jobs); any other critical alert is shown as a warning.
    pub critical_ids: Option<Vec<String>>,
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_active: 10,
            critical_ids: None,
        }
    }
}

impl AlertsConfig {
    /// May the alert with this id be critical?
    pub fn allows_critical(&self, id: &str) -> bool {
        self.critical_ids
            .as_ref()
            .is_none_or(|ids| ids.iter().any(|allowed| allowed == id))
    }
}

/// All configured sources, by adapter type.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    pub gitea: Vec<GiteaConfig>,
    pub github: Vec<GithubConfig>,
}

/// One `[[source.gitea]]` block: Actions webhooks, job step polling and open PRs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GiteaConfig {
    /// Instance name, unique among Gitea sources. Used in the webhook URL
    /// (`/webhook/gitea/<name>`) and in logs.
    pub name: String,
    /// Gitea web address, used for API polling, such as `https://gitea.example.com`.
    pub base_url: String,
    /// Credential with the webhook secret, the same value as in Gitea's
    /// webhook settings.
    pub webhook_secret_file: String,
    /// Credential with a read-only API token (`read:repository`). Without it
    /// the bridge polls anonymously, which only sees public repositories.
    pub token_file: Option<String>,
    /// Repositories (`owner/name`) whose open PRs are counted by polling.
    /// PR webhooks from other repositories still count until the bridge restarts.
    #[serde(default)]
    pub repos: Vec<String>,
    /// Seconds between open PR polls (the safety net behind PR webhooks).
    #[serde(default = "default_gitea_poll_s")]
    pub poll_s: u64,
    /// Seconds between step progress polls while a job runs.
    #[serde(default = "default_gitea_job_poll_s")]
    pub job_poll_s: u64,
    /// May running jobs and finished runs take over the screen? `true`,
    /// `false`, or a list of repositories that may.
    #[serde(default = "interrupt_all")]
    pub interrupt: Interrupt,
    /// Short panel labels for repositories, such as `{ "team/service-a" = "work A" }`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
}

/// One `[[source.github]]` block: open PRs and Actions runs from github.com,
/// GitHub Enterprise Server or GHE.com, by polling.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    /// Instance name, unique among GitHub sources. Used in logs and fact keys.
    pub name: String,
    /// API root: `https://api.github.com` (default), `https://<host>/api/v3`
    /// for Enterprise Server, or `https://api.<subdomain>.ghe.com`.
    #[serde(default = "default_github_base_url")]
    pub base_url: String,
    /// Credential with a read-only fine-grained token (Metadata, Pull
    /// requests and Actions: read).
    pub token_file: String,
    /// Repositories (`owner/name`) to watch.
    pub repos: Vec<String>,
    /// Seconds between polls of open PRs and recent runs.
    #[serde(default = "default_github_poll_s")]
    pub poll_s: u64,
    /// Seconds between step progress polls while a run is in progress.
    #[serde(default = "default_github_job_poll_s")]
    pub job_poll_s: u64,
    /// May running jobs and finished runs take over the screen? `true`,
    /// `false`, or a list of repositories that may.
    #[serde(default = "interrupt_all")]
    pub interrupt: Interrupt,
    /// Flash "New PR" when a poll finds a new, non-draft PR.
    #[serde(default = "yes")]
    pub notify_new_prs: bool,
    /// Short panel labels for repositories, such as `{ "team/service-a" = "work A" }`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
}

fn default_github_base_url() -> String {
    crate::github::api::GITHUB_COM.into()
}

fn default_github_poll_s() -> u64 {
    60
}

fn default_github_job_poll_s() -> u64 {
    5
}

fn yes() -> bool {
    true
}

fn interrupt_all() -> Interrupt {
    Interrupt::All(true)
}

fn default_gitea_poll_s() -> u64 {
    60
}

fn default_gitea_job_poll_s() -> u64 {
    5
}

/// Rotation used when the config has no `[[rotation]]` blocks: the stats page only.
fn default_rotation() -> Vec<RotationEntry> {
    vec![RotationEntry {
        page: "stats".into(),
        dwell_s: 20,
        skip_when_empty: false,
    }]
}

/// `owner/name` with no empty part. The name goes into API URLs.
fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
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
        let mut names = HashSet::new();
        for gitea in &config.source.gitea {
            let name = &gitea.name;
            anyhow::ensure!(
                valid_name(name),
                "source.gitea name {name:?} may only use letters, digits, - and _"
            );
            anyhow::ensure!(
                names.insert(name),
                "two source.gitea blocks are named {name:?}"
            );
            anyhow::ensure!(
                gitea.poll_s > 0 && gitea.job_poll_s > 0,
                "source.gitea {name}: poll_s and job_poll_s must be above 0"
            );
            anyhow::ensure!(
                gitea.repos.iter().all(|r| r.split('/').count() == 2),
                "source.gitea {name}: repos entries must look like owner/name"
            );
        }
        let mut names = HashSet::new();
        for github in &config.source.github {
            let name = &github.name;
            anyhow::ensure!(
                valid_name(name),
                "source.github name {name:?} may only use letters, digits, - and _"
            );
            anyhow::ensure!(
                names.insert(name),
                "two source.github blocks are named {name:?}"
            );
            anyhow::ensure!(
                github.base_url.starts_with("https://") || github.base_url.starts_with("http://"),
                "source.github {name}: base_url must start with https://"
            );
            anyhow::ensure!(
                github.poll_s > 0 && github.job_poll_s > 0,
                "source.github {name}: poll_s and job_poll_s must be above 0"
            );
            anyhow::ensure!(
                !github.repos.is_empty(),
                "source.github {name}: repos needs at least one owner/name"
            );
            anyhow::ensure!(
                github.repos.iter().all(|r| valid_repo(r)),
                "source.github {name}: repos entries must look like owner/name"
            );
        }
        Ok(config)
    }

    /// Read and parse a config file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        Self::from_toml(&text).with_context(|| format!("in config file {}", path.display()))
    }

    /// The config path given on the command line or in `DESKWATCH_CONFIG`,
    /// if any.
    pub fn path_given(arg: Option<PathBuf>) -> Option<PathBuf> {
        arg.or_else(|| std::env::var_os("DESKWATCH_CONFIG").map(PathBuf::from))
    }

    /// Pick the config path: the CLI argument, then `DESKWATCH_CONFIG`, then the default.
    pub fn path(arg: Option<PathBuf>) -> PathBuf {
        Self::path_given(arg).unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
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
        assert!(config.source.gitea.is_empty());
        assert_eq!(config.http.listen.port(), 8787);
        assert!(config.alerts.enabled);
        assert_eq!(config.alerts.max_active, 10);
        assert!(config.alerts.allows_critical("anything"));
    }

    #[test]
    fn critical_ids_limit_critical_alerts() {
        let config = Config::from_toml("[alerts]\ncritical_ids = [\"leak\"]\n").unwrap();
        assert!(config.alerts.allows_critical("leak"));
        assert!(!config.alerts.allows_critical("door"));
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.toml");
        let config = Config::from_toml(text).unwrap();
        assert_eq!(config.rotation.len(), 4);
        assert!(config.rotation[1].skip_when_empty);
        let gitea = &config.source.gitea[0];
        assert_eq!(gitea.name, "home");
        assert_eq!(gitea.job_poll_s, 5);
        assert_eq!(gitea.interrupt, Interrupt::All(true));
        assert!(
            config.source.github.is_empty(),
            "GitHub blocks are commented out"
        );
    }

    #[test]
    fn example_github_blocks_parse_when_uncommented() {
        let text = include_str!("../config.example.toml");
        let start = text.find("# [[source.github]]").unwrap();
        let end = text.find("# Idle pages").unwrap();
        let github: String = text[start..end]
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l.trim_start_matches('#')))
            .collect::<Vec<_>>()
            .join("\n");
        let config = Config::from_toml(&github).unwrap();
        assert_eq!(config.source.github.len(), 2);
        assert_eq!(config.source.github[1].interrupt, Interrupt::All(false));
    }

    #[test]
    fn gitea_defaults_and_checks() {
        let block = |extra: &str| {
            format!(
                "[[source.gitea]]\nname = \"home\"\nbase_url = \"http://gitea.local\"\n\
                 webhook_secret_file = \"gitea-webhook-secret\"\n{extra}"
            )
        };
        let config = Config::from_toml(&block("")).unwrap();
        let gitea = &config.source.gitea[0];
        assert_eq!(gitea.poll_s, 60);
        assert!(gitea.repos.is_empty());
        assert!(gitea.token_file.is_none());

        let list = Config::from_toml(&block(
            "interrupt = [\"me/a\"]\nalias = { \"me/a\" = \"A\" }",
        ))
        .unwrap();
        assert!(list.source.gitea[0].interrupt.allows("me/a"));
        assert_eq!(list.source.gitea[0].alias["me/a"], "A");

        assert!(Config::from_toml(&block("repos = [\"just-a-name\"]")).is_err());
        let twice = format!("{}{}", block(""), block(""));
        assert!(Config::from_toml(&twice).is_err(), "duplicate names");
        let bad_name = block("").replace("\"home\"", "\"a/b\"");
        assert!(Config::from_toml(&bad_name).is_err());
        // The pre-1.0 `[gitea]` table is gone; it must not be silently ignored.
        assert!(Config::from_toml("[gitea]\nbase_url = \"x\"\n").is_err());
    }

    #[test]
    fn github_defaults_and_checks() {
        let block = |extra: &str| {
            format!(
                "[[source.github]]\nname = \"personal\"\ntoken_file = \"github-token\"\n\
                 repos = [\"me/a\"]\n{extra}"
            )
        };
        let config = Config::from_toml(&block("")).unwrap();
        let github = &config.source.github[0];
        assert_eq!(github.base_url, "https://api.github.com");
        assert_eq!((github.poll_s, github.job_poll_s), (60, 5));
        assert!(github.notify_new_prs);
        assert_eq!(github.interrupt, Interrupt::All(true));

        let ghes = Config::from_toml(&block(
            "base_url = \"https://ghe.example.com/api/v3\"\ninterrupt = false",
        ))
        .unwrap();
        assert_eq!(
            ghes.source.github[0].base_url,
            "https://ghe.example.com/api/v3"
        );

        // The same name may be used once per adapter type.
        let gitea = "[[source.gitea]]\nname = \"personal\"\nbase_url = \"http://g\"\n\
                     webhook_secret_file = \"s\"\n";
        assert!(Config::from_toml(&format!("{gitea}{}", block(""))).is_ok());

        let twice = format!("{}{}", block(""), block(""));
        assert!(Config::from_toml(&twice).is_err(), "duplicate names");
        let no_repos = block("").replace("repos = [\"me/a\"]", "repos = []");
        assert!(Config::from_toml(&no_repos).is_err());
        let bad_repo = block("").replace("me/a", "me/a/../b");
        assert!(Config::from_toml(&bad_repo).is_err());
        assert!(Config::from_toml(&block("base_url = \"ghe.example.com\"")).is_err());
        let no_token = block("").replace("token_file = \"github-token\"\n", "");
        assert!(
            Config::from_toml(&no_token).is_err(),
            "token_file is required"
        );
        assert!(
            Config::from_toml(&block("token = \"ghp_x\"")).is_err(),
            "no inline tokens"
        );
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
