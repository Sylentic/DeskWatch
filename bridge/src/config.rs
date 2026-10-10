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
use serde::{Deserialize, Serialize};

use crate::gitea::runners::RunnerScope;
use crate::model::RotationEntry;
use crate::source::{Interrupt, valid_name};

/// Environment variable that holds the MQTT password, when `mqtt.password_file`
/// is not set.
pub const MQTT_PASSWORD_ENV: &str = "DESKWATCH_MQTT_PASSWORD";

/// Config path used when none is given on the command line or in `DESKWATCH_CONFIG`
/// (Linux and other Unix systems).
pub const DEFAULT_CONFIG_PATH: &str = "/etc/deskwatch/bridge.toml";

/// The folder Windows installs keep their files in: `%ProgramData%\DeskWatch`
/// (normally `C:\ProgramData\DeskWatch`).
pub fn windows_data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("DeskWatch")
}

/// The config path when none is given: `/etc/deskwatch/bridge.toml`, or
/// `%ProgramData%\DeskWatch\bridge.toml` on Windows.
pub fn default_config_path() -> PathBuf {
    if cfg!(windows) {
        windows_data_dir().join("bridge.toml")
    } else {
        PathBuf::from(DEFAULT_CONFIG_PATH)
    }
}

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
    /// The browser dashboard for a big screen, served on `[http]`. Off by default.
    #[serde(default)]
    pub kiosk: KioskConfig,
    /// Idle pages in the order they rotate. See `[[rotation]]` in the example config.
    #[serde(default = "default_rotation")]
    pub rotation: Vec<RotationEntry>,
}

/// Connection to the Mosquitto broker.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttConfig {
    /// Connect to the broker at all. Set to false for a bridge that only
    /// serves the kiosk page (docs/docker.md): no connection attempts, no
    /// warnings, and no ESP panel, button or MQTT alerts.
    pub enabled: bool,
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
    /// Connect with TLS (usually port 8883). Off by default.
    pub tls: bool,
    /// PEM file with the CA that signed the broker certificate, for a private
    /// CA. Without it the system's trusted roots are used. Public, so a plain
    /// path is fine.
    pub ca_file: Option<PathBuf>,
    /// PEM client certificate, only when the broker asks for one
    /// (`require_certificate true`). Needs `client_key_file` too.
    pub client_cert_file: Option<PathBuf>,
    /// Credential holding the PEM private key for `client_cert_file`.
    pub client_key_file: Option<String>,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "localhost".into(),
            port: 1883,
            client_id: "deskwatch-bridge".into(),
            username: None,
            password_file: None,
            topic_prefix: "deskpanel".into(),
            keep_alive_s: 30,
            tls: false,
            ca_file: None,
            client_cert_file: None,
            client_key_file: None,
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

/// The `[kiosk]` table: the dashboard page for a Raspberry Pi or other big
/// screen. It is read-only; the layout is a grid of widgets listed in
/// `[[kiosk.panel]]` blocks, left to right and top to bottom.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KioskConfig {
    /// Serve the page and its data on the `[http]` listener.
    pub enabled: bool,
    /// Grid size. Every widget takes `span` cells of it.
    pub columns: u8,
    pub rows: u8,
    /// Credential holding a token that the data endpoints require, passed as
    /// `?token=...` on the page address. Set it whenever `http.listen` is
    /// reachable from other machines.
    pub token_file: Option<String>,
    /// Networks (`192.168.0.0/16`, `fd00::/8`, a single address) whose
    /// clients need no token. Judged by the TCP peer address only, so behind
    /// a reverse proxy or Docker port mapping read docs/kiosk.md first.
    /// Empty (the default) trusts nobody.
    pub trusted_networks: Vec<String>,
    /// Most live-update WebSockets open at once. More get `503` and the page
    /// falls back to polling. A page needs one per open tab or screen.
    pub max_ws_clients: u16,
    /// Colour theme: `day` (the default look), `night` (dimmer) or `auto`
    /// (night between `night_from` and `night_to`, by the browser's clock).
    pub theme: Theme,
    /// Start and end of the night, as `HH:MM`, used when `theme = "auto"`.
    /// The window may cross midnight (22:00 to 07:00).
    pub night_from: String,
    pub night_to: String,
    /// Move the whole page a few pixels every `pixel_shift_minutes`, so a
    /// static layout does not burn into an OLED or plasma screen.
    pub pixel_shift: bool,
    pub pixel_shift_minutes: u16,
    /// The widgets. Empty means the built-in layout.
    pub panel: Vec<PanelConfig>,
}

/// Colour theme of the kiosk page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Day,
    Night,
    Auto,
}

impl Default for KioskConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            columns: 4,
            rows: 3,
            token_file: None,
            trusted_networks: Vec::new(),
            max_ws_clients: 16,
            theme: Theme::Day,
            night_from: "22:00".into(),
            night_to: "07:00".into(),
            pixel_shift: false,
            pixel_shift_minutes: 5,
            panel: Vec::new(),
        }
    }
}

impl KioskConfig {
    /// Largest grid side. Beyond this the cells are too small to read.
    const MAX_GRID: u8 = 12;

    fn validate(&self) -> Result<()> {
        let max = Self::MAX_GRID;
        anyhow::ensure!(
            (1..=max).contains(&self.columns) && (1..=max).contains(&self.rows),
            "kiosk.columns and kiosk.rows must be between 1 and {max}"
        );
        anyhow::ensure!(
            self.max_ws_clients >= 1,
            "kiosk.max_ws_clients must be at least 1"
        );
        crate::kiosk::net::parse_all(&self.trusted_networks)
            .map_err(|e| anyhow::anyhow!("kiosk.trusted_networks: {e}"))?;
        for (name, value) in [
            ("night_from", &self.night_from),
            ("night_to", &self.night_to),
        ] {
            anyhow::ensure!(
                valid_time(value),
                "kiosk.{name} must be a time as HH:MM, for example \"22:00\" (got {value:?})"
            );
        }
        anyhow::ensure!(
            (1..=120).contains(&self.pixel_shift_minutes),
            "kiosk.pixel_shift_minutes must be between 1 and 120"
        );
        for (i, panel) in self.panel.iter().enumerate() {
            let n = i + 1;
            let [cols, rows] = panel.span;
            anyhow::ensure!(
                (1..=self.columns).contains(&cols) && (1..=self.rows).contains(&rows),
                "kiosk.panel {n}: span must fit the {}x{} grid and be at least 1x1",
                self.columns,
                self.rows
            );
            anyhow::ensure!(
                panel.host.is_none() || panel.widget == Widget::Stats,
                "kiosk.panel {n}: host only applies to the stats widget"
            );
            let filterable = matches!(panel.widget, Widget::Jobs | Widget::Prs | Widget::Pipelines);
            anyhow::ensure!(
                filterable || (panel.source.is_none() && panel.repo.is_none()),
                "kiosk.panel {n}: source and repo only apply to the jobs, prs and pipelines widgets"
            );
            if let Some(source) = &panel.source {
                anyhow::ensure!(
                    SOURCE_KINDS.contains(&source.as_str()),
                    "kiosk.panel {n}: source must be one of {} (got {source:?})",
                    SOURCE_KINDS.join(", ")
                );
            }
            anyhow::ensure!(
                panel.repo.as_deref().is_none_or(|r| !r.trim().is_empty()),
                "kiosk.panel {n}: repo must not be empty"
            );
            anyhow::ensure!(
                panel.rows != Some(0),
                "kiosk.panel {n}: rows must be above 0"
            );
        }
        Ok(())
    }

    /// The widgets to show: the configured ones, or the default layout for a
    /// 4x3 grid.
    pub fn panels(&self) -> Vec<PanelConfig> {
        if self.panel.is_empty() {
            default_panels()
        } else {
            self.panel.clone()
        }
    }
}

/// The CI systems a `source` filter can name, as the kiosk page tags them.
const SOURCE_KINDS: [&str; 3] = ["gitea", "github", "azure_devops"];

/// Is this `HH:MM` on a 24 hour clock?
fn valid_time(text: &str) -> bool {
    text.split_once(':').is_some_and(|(h, m)| {
        h.len() == 2
            && m.len() == 2
            && h.parse::<u8>().is_ok_and(|h| h < 24)
            && m.parse::<u8>().is_ok_and(|m| m < 60)
    })
}

/// What a kiosk widget shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Widget {
    /// One host in detail: CPU, RAM, disk, temperature, network, sparkline.
    Stats,
    /// A compact table of every host.
    Hosts,
    /// Jobs that are running now, with progress.
    Jobs,
    /// Open pull requests per repository.
    Prs,
    /// The latest run of every pipeline.
    Pipelines,
    /// Active alerts.
    Alerts,
    /// Containers and hosts that are down.
    Containers,
    /// Health of every source.
    Health,
    /// Runners and agents: idle, busy or offline. Not in the default layout.
    Runners,
}

/// One `[[kiosk.panel]]` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    pub widget: Widget,
    /// Host shown by a `stats` widget. Defaults to the first host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Replaces the widget's heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Most list rows (or running jobs) to show. Default: as many as fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,
    /// Only what comes from this CI system: `gitea`, `github` or
    /// `azure_devops`. For the `jobs`, `prs` and `pipelines` widgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Only this repository or project, matched without regard to case. A
    /// trailing `*` matches a prefix (`acme/*`). Same widgets as `source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Grid cells taken: `[columns, rows]`.
    #[serde(default = "default_span")]
    pub span: [u8; 2],
}

fn default_span() -> [u8; 2] {
    [1, 1]
}

fn panel(widget: Widget, span: [u8; 2]) -> PanelConfig {
    PanelConfig {
        widget,
        host: None,
        title: None,
        rows: None,
        source: None,
        repo: None,
        span,
    }
}

/// The layout used when `[[kiosk.panel]]` is not set, for the default 4x3 grid.
pub fn default_panels() -> Vec<PanelConfig> {
    vec![
        panel(Widget::Stats, [1, 1]),
        panel(Widget::Hosts, [1, 1]),
        panel(Widget::Jobs, [2, 1]),
        panel(Widget::Prs, [1, 2]),
        panel(Widget::Pipelines, [2, 2]),
        panel(Widget::Alerts, [1, 1]),
        panel(Widget::Health, [1, 1]),
    ]
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
    pub azure_devops: Vec<AzureDevopsConfig>,
    pub prometheus: Vec<PrometheusConfig>,
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
    /// Runner lists to show on the kiosk `runners` widget: `"user"`,
    /// `"org:<name>"`, `"repo:<owner>/<name>"` or `"admin"` (all runners,
    /// needs an administrator token). Empty means runners are not polled.
    /// Do not list overlapping scopes, a runner would show twice.
    #[serde(default)]
    pub runners: Vec<String>,
    /// Seconds between runner polls.
    #[serde(default = "default_gitea_runner_poll_s")]
    pub runner_poll_s: u64,
    /// May running jobs and finished runs take over the screen? `true`,
    /// `false`, or a list of repositories that may.
    #[serde(default = "interrupt_all")]
    pub interrupt: Interrupt,
    /// Short panel labels for repositories, such as `{ "team/service-a" = "work A" }`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
}

impl GiteaConfig {
    /// The parsed `runners` entries.
    pub fn runner_scopes(&self) -> Result<Vec<RunnerScope>> {
        self.runners
            .iter()
            .map(|text| {
                RunnerScope::parse(text)
                    .with_context(|| format!("source.gitea {}: runners", self.name))
            })
            .collect()
    }
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
    /// Self-hosted runner lists for the kiosk `runners` widget:
    /// `"repo:<owner>/<name>"` or `"org:<name>"`. Needs extra token rights
    /// (docs/runners.md). Empty means runners are not polled. Do not list
    /// overlapping scopes, a runner would show twice.
    #[serde(default)]
    pub runners: Vec<String>,
    /// Seconds between runner polls.
    #[serde(default = "default_github_runner_poll_s")]
    pub runner_poll_s: u64,
    /// Short panel labels for repositories, such as `{ "team/service-a" = "work A" }`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
}

impl GithubConfig {
    /// The parsed `runners` entries. GitHub only lists runners per
    /// repository and per organisation.
    pub fn runner_scopes(&self) -> Result<Vec<RunnerScope>> {
        self.runners
            .iter()
            .map(|text| {
                let scope = RunnerScope::parse(text)
                    .with_context(|| format!("source.github {}: runners", self.name))?;
                anyhow::ensure!(
                    matches!(scope, RunnerScope::Org(_) | RunnerScope::Repo(_)),
                    "source.github {}: runners entry {text:?} must be repo:<owner>/<name> or org:<name>",
                    self.name
                );
                Ok(scope)
            })
            .collect()
    }
}

/// One `[[source.azure_devops]]` block: pipeline runs and open PRs from Azure
/// DevOps Services, by polling.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AzureDevopsConfig {
    /// Instance name, unique among Azure DevOps sources. Used in logs and fact keys.
    pub name: String,
    /// The organisation: the first part of `https://dev.azure.com/<organization>`.
    pub organization: String,
    /// Service address. Only changes for tests.
    #[serde(default = "default_azure_devops_base_url")]
    pub base_url: String,
    /// Projects of the organisation to watch.
    pub projects: Vec<String>,
    /// Credential with a read-only personal access token (Build: Read,
    /// Code: Read).
    pub token_file: String,
    /// Seconds between polls of builds and open PRs.
    #[serde(default = "default_github_poll_s")]
    pub poll_s: u64,
    /// Seconds between step progress polls while a build is in progress.
    #[serde(default = "default_github_job_poll_s")]
    pub job_poll_s: u64,
    /// May running builds and finished runs take over the screen? `true`,
    /// `false`, or a list of projects that may. Off by default: work
    /// pipelines then show in the pipelines page and the badges only.
    #[serde(default = "interrupt_none")]
    pub interrupt: Interrupt,
    /// Count open PRs. Needs the Code (Read) scope.
    #[serde(default = "yes")]
    pub pull_requests: bool,
    /// Flash "New PR" when a poll finds a new, non-draft PR.
    #[serde(default = "yes")]
    pub notify_new_prs: bool,
    /// Pipelines whose name contains one of these words (any case) are
    /// deploys; the rest are builds.
    #[serde(default = "default_deploy_words")]
    pub deploy_words: Vec<String>,
    /// Short panel labels: for a project by its name, for the PRs of a
    /// repository by `project/repository`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
}

/// One `[[source.prometheus]]` block: host stats from node_exporter and
/// container health from cAdvisor, read through the Prometheus HTTP API.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrometheusConfig {
    /// Instance name, unique among Prometheus sources. Used in logs.
    pub name: String,
    /// Prometheus address, such as `http://prometheus.example.lan:9090`.
    pub url: String,
    /// Optional credential with a bearer token, for a Prometheus behind a
    /// reverse proxy. Without it the bridge sends no credentials.
    pub token_file: Option<String>,
    /// The machines to show, each on its own `stats:<name>` page. An entry is
    /// the node_exporter `instance` label (`"node-exporter:9100"`), or a table
    /// with `instance`, `name`, `cadvisor` and `expect_containers`.
    pub hosts: Vec<HostConfig>,
    /// Seconds between queries.
    #[serde(default = "default_prometheus_interval_s")]
    pub interval_s: u64,
    /// Mount point whose usage is shown as `disk_pct`.
    #[serde(default = "default_disk_mountpoint")]
    pub disk_mountpoint: String,
    /// Network device to report. Defaults to the busiest one that is not
    /// loopback, a bridge or a container interface.
    pub net_device: Option<String>,
    /// Replacement PromQL for a stats field, by field name (see
    /// `prometheus::QUERY_KEYS`). Each query must return one value per `instance`.
    #[serde(default)]
    pub queries: BTreeMap<String, String>,
}

fn default_prometheus_interval_s() -> u64 {
    5
}

fn default_disk_mountpoint() -> String {
    "/".into()
}

/// One machine of a Prometheus source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "HostSpec")]
pub struct HostConfig {
    /// The `instance` label of its node_exporter target.
    pub instance: String,
    /// Page name (`stats:<name>`) and the host name shown on the panel.
    /// Defaults to the instance without its port.
    pub name: String,
    /// The `instance` label of the cAdvisor target on this machine.
    pub cadvisor: Option<String>,
    /// Containers that must be running. A stopped container vanishes from
    /// cAdvisor, so only listed names can be reported as down.
    pub expect_containers: Vec<String>,
}

/// The two ways to write a host in the config.
#[derive(Deserialize)]
#[serde(untagged)]
enum HostSpec {
    Instance(String),
    Table(HostTable),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostTable {
    instance: String,
    name: Option<String>,
    cadvisor: Option<String>,
    #[serde(default)]
    expect_containers: Vec<String>,
}

impl From<HostSpec> for HostConfig {
    fn from(spec: HostSpec) -> Self {
        let table = match spec {
            HostSpec::Instance(instance) => HostTable {
                instance,
                name: None,
                cadvisor: None,
                expect_containers: Vec::new(),
            },
            HostSpec::Table(table) => table,
        };
        let name = table
            .name
            .unwrap_or_else(|| default_host_name(&table.instance));
        Self {
            instance: table.instance,
            name,
            cadvisor: table.cadvisor,
            expect_containers: table.expect_containers,
        }
    }
}

/// `node-exporter:9100` becomes `node-exporter`; anything that cannot be in
/// a page name turns into `-`.
fn default_host_name(instance: &str) -> String {
    let host = instance.rsplit_once(':').map_or(instance, |(host, _)| host);
    host.chars()
        .map(|c| if valid_name(&c.to_string()) { c } else { '-' })
        .collect()
}

fn default_github_base_url() -> String {
    crate::github::api::GITHUB_COM.into()
}

fn default_azure_devops_base_url() -> String {
    crate::azure_devops::api::AZURE_DEVOPS_COM.into()
}

fn interrupt_none() -> Interrupt {
    Interrupt::All(false)
}

fn default_deploy_words() -> Vec<String> {
    vec!["deploy".into(), "terraform".into()]
}

fn default_github_poll_s() -> u64 {
    60
}

fn default_github_job_poll_s() -> u64 {
    5
}

fn default_github_runner_poll_s() -> u64 {
    300
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

fn default_gitea_runner_poll_s() -> u64 {
    30
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
/// Organisation and project names: not empty, no slash, no control characters.
/// Project names may contain spaces.
fn valid_azure_name(name: &str) -> bool {
    !name.trim().is_empty() && !name.contains('/') && !name.chars().any(char::is_control)
}

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
        let mqtt = &config.mqtt;
        anyhow::ensure!(
            mqtt.client_cert_file.is_some() == mqtt.client_key_file.is_some(),
            "mqtt.client_cert_file and mqtt.client_key_file go together"
        );
        anyhow::ensure!(
            mqtt.tls || (mqtt.ca_file.is_none() && mqtt.client_cert_file.is_none()),
            "mqtt.ca_file and mqtt.client_cert_file need mqtt.tls = true"
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
                gitea.poll_s > 0 && gitea.job_poll_s > 0 && gitea.runner_poll_s > 0,
                "source.gitea {name}: poll_s, job_poll_s and runner_poll_s must be above 0"
            );
            gitea.runner_scopes()?;
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
                github.poll_s > 0 && github.job_poll_s > 0 && github.runner_poll_s > 0,
                "source.github {name}: poll_s, job_poll_s and runner_poll_s must be above 0"
            );
            github.runner_scopes()?;
            anyhow::ensure!(
                !github.repos.is_empty(),
                "source.github {name}: repos needs at least one owner/name"
            );
            anyhow::ensure!(
                github.repos.iter().all(|r| valid_repo(r)),
                "source.github {name}: repos entries must look like owner/name"
            );
        }
        let mut names = HashSet::new();
        for azure in &config.source.azure_devops {
            let name = &azure.name;
            anyhow::ensure!(
                valid_name(name),
                "source.azure_devops name {name:?} may only use letters, digits, - and _"
            );
            anyhow::ensure!(
                names.insert(name),
                "two source.azure_devops blocks are named {name:?}"
            );
            anyhow::ensure!(
                azure.base_url.starts_with("https://") || azure.base_url.starts_with("http://"),
                "source.azure_devops {name}: base_url must start with https://"
            );
            anyhow::ensure!(
                valid_azure_name(&azure.organization),
                "source.azure_devops {name}: organization must be the name from dev.azure.com/<organization>"
            );
            anyhow::ensure!(
                !azure.projects.is_empty() && azure.projects.iter().all(|p| valid_azure_name(p)),
                "source.azure_devops {name}: projects needs at least one project name"
            );
            anyhow::ensure!(
                azure.poll_s > 0 && azure.job_poll_s > 0,
                "source.azure_devops {name}: poll_s and job_poll_s must be above 0"
            );
        }
        let mut names = HashSet::new();
        for prom in &config.source.prometheus {
            let name = &prom.name;
            anyhow::ensure!(
                valid_name(name),
                "source.prometheus name {name:?} may only use letters, digits, - and _"
            );
            anyhow::ensure!(
                names.insert(name),
                "two source.prometheus blocks are named {name:?}"
            );
            anyhow::ensure!(
                prom.url.starts_with("https://") || prom.url.starts_with("http://"),
                "source.prometheus {name}: url must start with http:// or https://"
            );
            anyhow::ensure!(
                prom.interval_s > 0,
                "source.prometheus {name}: interval_s must be above 0"
            );
            anyhow::ensure!(
                !prom.hosts.is_empty(),
                "source.prometheus {name}: hosts needs at least one entry"
            );
            // The mount point and device go into PromQL label matchers.
            for (key, value) in [
                ("disk_mountpoint", Some(&prom.disk_mountpoint)),
                ("net_device", prom.net_device.as_ref()),
            ] {
                anyhow::ensure!(
                    value.is_none_or(|v| !v.contains(['"', '\\', '\n'])),
                    "source.prometheus {name}: {key} must not contain quotes or backslashes"
                );
            }
            let mut pages = HashSet::new();
            for host in &prom.hosts {
                anyhow::ensure!(
                    valid_name(&host.name),
                    "source.prometheus {name}: host name {:?} may only use letters, digits, - and _ \
                     (set `name` for instance {:?})",
                    host.name,
                    host.instance
                );
                anyhow::ensure!(
                    pages.insert(&host.name),
                    "source.prometheus {name}: two hosts are named {:?}",
                    host.name
                );
                anyhow::ensure!(
                    host.expect_containers.is_empty() || host.cadvisor.is_some(),
                    "source.prometheus {name}: host {} lists expect_containers but has no cadvisor instance",
                    host.name
                );
            }
            for key in prom.queries.keys() {
                anyhow::ensure!(
                    crate::prometheus::QUERY_KEYS.contains(&key.as_str()),
                    "source.prometheus {name}: unknown query {key:?} (known: {})",
                    crate::prometheus::QUERY_KEYS.join(", ")
                );
            }
        }
        config.kiosk.validate()?;
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
        Self::path_given(arg).unwrap_or_else(default_config_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_uses_defaults() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config.mqtt.host, "localhost");
        assert!(config.mqtt.enabled);
        assert!(
            !Config::from_toml("[mqtt]\nenabled = false\n")
                .unwrap()
                .mqtt
                .enabled
        );
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
    fn mqtt_tls_settings_are_checked() {
        let ok = "[mqtt]\ntls = true\nport = 8883\nca_file = \"/etc/ca.pem\"\n";
        assert!(Config::from_toml(ok).is_ok());
        assert!(Config::from_toml("[mqtt]\nca_file = \"/etc/ca.pem\"\n").is_err());
        let half = "[mqtt]\ntls = true\nclient_cert_file = \"/etc/c.pem\"\n";
        assert!(Config::from_toml(half).is_err());
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
    fn dashboard_example_config_parses() {
        let config =
            Config::from_toml(include_str!("../../deploy/dashboard.example.toml")).unwrap();
        assert!(!config.mqtt.enabled);
        assert!(config.kiosk.enabled);
        assert_eq!(config.kiosk.token_file.as_deref(), Some("kiosk-token"));
        assert!(config.source.gitea.is_empty());
    }

    #[test]
    fn example_github_blocks_parse_when_uncommented() {
        let text = include_str!("../config.example.toml");
        let start = text.find("# [[source.github]]").unwrap();
        let end = text.find("# Azure DevOps Services").unwrap();
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

        // Runners are off unless scopes are listed.
        assert!(gitea.runners.is_empty());
        assert_eq!(gitea.runner_poll_s, 30);
        let runners = Config::from_toml(&block(
            "runners = [\"user\", \"org:team\", \"repo:me/a\"]\nrunner_poll_s = 15",
        ))
        .unwrap();
        assert_eq!(runners.source.gitea[0].runner_scopes().unwrap().len(), 3);
        let bad_scope = format!(
            "{:#}",
            Config::from_toml(&block("runners = [\"team\"]")).unwrap_err()
        );
        assert!(
            bad_scope.contains("source.gitea home: runners"),
            "{bad_scope}"
        );
        assert!(Config::from_toml(&block("runner_poll_s = 0")).is_err());

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

        // Runners are off unless scopes are listed; GitHub has no user or admin list.
        assert!(github.runners.is_empty());
        assert_eq!(github.runner_poll_s, 300);
        let runners = Config::from_toml(&block(
            "runners = [\"repo:me/a\", \"org:team\"]\nrunner_poll_s = 120",
        ))
        .unwrap();
        assert_eq!(runners.source.github[0].runner_scopes().unwrap().len(), 2);
        for bad in ["user", "admin", "team", "repo:me"] {
            let toml = block(&format!("runners = [\"{bad}\"]"));
            assert!(
                Config::from_toml(&toml).is_err(),
                "{bad:?} should be refused"
            );
        }
        assert!(Config::from_toml(&block("runner_poll_s = 0")).is_err());

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
    fn example_azure_devops_block_parses_when_uncommented() {
        let text = include_str!("../config.example.toml");
        assert!(
            Config::from_toml(text)
                .unwrap()
                .source
                .azure_devops
                .is_empty(),
            "Azure DevOps block is commented out"
        );
        let start = text.find("# [[source.azure_devops]]").unwrap();
        let end = text.find("# Idle pages").unwrap();
        let block: String = text[start..end]
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l.trim_start_matches('#')))
            .collect::<Vec<_>>()
            .join("\n");
        let config = Config::from_toml(&block).unwrap();
        let azure = &config.source.azure_devops[0];
        assert_eq!(azure.organization, "your-org");
        assert_eq!(azure.projects, ["project-a"]);
        assert_eq!(azure.alias["project-a/repo-a"], "A");
    }

    #[test]
    fn azure_devops_defaults_and_checks() {
        let block = |extra: &str| {
            format!(
                "[[source.azure_devops]]\nname = \"work\"\norganization = \"your-org\"\n\
                 projects = [\"project-a\"]\ntoken_file = \"azdo-token\"\n{extra}"
            )
        };
        let config = Config::from_toml(&block("")).unwrap();
        let azure = &config.source.azure_devops[0];
        assert_eq!(azure.base_url, "https://dev.azure.com");
        assert_eq!((azure.poll_s, azure.job_poll_s), (60, 5));
        assert_eq!(azure.interrupt, Interrupt::All(false), "quiet by default");
        assert!(azure.pull_requests && azure.notify_new_prs);
        assert_eq!(azure.deploy_words, ["deploy", "terraform"]);

        let some = Config::from_toml(&block("interrupt = [\"project-a\"]")).unwrap();
        assert!(some.source.azure_devops[0].interrupt.allows("project-a"));
        // Project names may contain spaces.
        let spaced = block("").replace("project-a", "My Project");
        assert!(Config::from_toml(&spaced).is_ok());

        let twice = format!("{}{}", block(""), block(""));
        assert!(Config::from_toml(&twice).is_err(), "duplicate names");
        let no_projects = block("").replace("projects = [\"project-a\"]", "projects = []");
        assert!(Config::from_toml(&no_projects).is_err());
        let bad_project = block("").replace("project-a", "a/b");
        assert!(Config::from_toml(&bad_project).is_err());
        let bad_org = block("").replace("your-org", "");
        assert!(Config::from_toml(&bad_org).is_err());
        assert!(Config::from_toml(&block("base_url = \"dev.azure.com\"")).is_err());
        let no_token = block("").replace("token_file = \"azdo-token\"\n", "");
        assert!(
            Config::from_toml(&no_token).is_err(),
            "token_file is required"
        );
        assert!(
            Config::from_toml(&block("token = \"abc\"")).is_err(),
            "no inline tokens"
        );
    }

    #[test]
    fn example_prometheus_block_parses_when_uncommented() {
        let text = include_str!("../config.example.toml");
        let start = text.find("# [[source.prometheus]]").unwrap();
        let end = text.find("# GitHub, GitHub Enterprise").unwrap();
        let block: String = text[start..end]
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l.trim_start_matches('#')))
            .collect::<Vec<_>>()
            .join("\n");
        let config = Config::from_toml(&block).unwrap();
        let prom = &config.source.prometheus[0];
        assert_eq!(prom.interval_s, 5);
        assert_eq!(prom.hosts.len(), 2);
        assert_eq!(prom.hosts[0].name, "home");
        assert_eq!(prom.hosts[0].expect_containers, ["gitea", "mosquitto"]);
        assert_eq!(prom.hosts[1].instance, "nas.example.lan:9100");
        assert_eq!(prom.hosts[1].name, "nas-example-lan");
        assert!(prom.queries.is_empty(), "the override line stays commented");
    }

    #[test]
    fn prometheus_defaults_and_checks() {
        let block = |extra: &str| {
            format!(
                "[[source.prometheus]]\nname = \"home\"\nurl = \"http://prom.local:9090\"\n\
                 hosts = [\"node-exporter:9100\"]\n{extra}"
            )
        };
        let config = Config::from_toml(&block("")).unwrap();
        let prom = &config.source.prometheus[0];
        assert_eq!((prom.interval_s, prom.disk_mountpoint.as_str()), (5, "/"));
        assert!(prom.token_file.is_none() && prom.net_device.is_none());
        assert_eq!(prom.hosts[0].name, "node-exporter");
        assert_eq!(prom.hosts[0].cadvisor, None);

        let tabled = Config::from_toml(&block("")
            .replace("[\"node-exporter:9100\"]", "[{ instance = \"a:9100\", name = \"srv\", cadvisor = \"c:8080\", expect_containers = [\"db\"] }]"))
        .unwrap();
        assert_eq!(tabled.source.prometheus[0].hosts[0].name, "srv");

        assert!(Config::from_toml(&block("interval_s = 0")).is_err());
        assert!(Config::from_toml(&block("disk_mountpoint = \"/\\\"x\"")).is_err());
        assert!(Config::from_toml(&block("queries = { nonsense = \"up\" }")).is_err());
        assert!(
            Config::from_toml(&block("").replace("http://prom.local:9090", "prom.local")).is_err()
        );
        let no_hosts = block("").replace("[\"node-exporter:9100\"]", "[]");
        assert!(Config::from_toml(&no_hosts).is_err());
        // Containers need a cAdvisor instance to be looked up on.
        let orphan = block("").replace(
            "[\"node-exporter:9100\"]",
            "[{ instance = \"a:9100\", expect_containers = [\"db\"] }]",
        );
        assert!(Config::from_toml(&orphan).is_err());
        // Two hosts cannot share a page name.
        let twice = block("").replace("[\"node-exporter:9100\"]", "[\"a:9100\", \"a:9101\"]");
        assert!(Config::from_toml(&twice).is_err());
        let twice = format!("{}{}", block(""), block(""));
        assert!(Config::from_toml(&twice).is_err(), "duplicate source names");
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

    #[test]
    fn kiosk_is_off_by_default_with_a_default_layout() {
        let config = Config::from_toml("").unwrap();
        assert!(!config.kiosk.enabled);
        assert_eq!((config.kiosk.columns, config.kiosk.rows), (4, 3));
        let panels = config.kiosk.panels();
        assert_eq!(panels, default_panels());
        // The default layout fills the default grid exactly.
        let cells: u32 = panels
            .iter()
            .map(|p| u32::from(p.span[0] * p.span[1]))
            .sum();
        assert_eq!(cells, 12);
    }

    #[test]
    fn kiosk_panels_parse_in_order() {
        let config = Config::from_toml(
            r#"
[kiosk]
enabled = true
columns = 3
rows = 2
token_file = "kiosk-token"

[[kiosk.panel]]
widget = "stats"
host = "homeserver"

[[kiosk.panel]]
widget = "prs"
rows = 12
span = [1, 2]
title = "Reviews"
"#,
        )
        .unwrap();
        assert!(config.kiosk.enabled);
        assert_eq!(config.kiosk.token_file.as_deref(), Some("kiosk-token"));
        let panels = config.kiosk.panels();
        assert_eq!(panels.len(), 2);
        assert_eq!(panels[0].widget, Widget::Stats);
        assert_eq!(panels[0].host.as_deref(), Some("homeserver"));
        assert_eq!(panels[0].span, [1, 1]);
        assert_eq!(panels[1].widget, Widget::Prs);
        assert_eq!((panels[1].rows, panels[1].span), (Some(12), [1, 2]));
        assert_eq!(panels[1].title.as_deref(), Some("Reviews"));
    }

    #[test]
    fn runners_widget_is_not_in_the_default_layout() {
        assert!(default_panels().iter().all(|p| p.widget != Widget::Runners));
        let config =
            Config::from_toml("[[kiosk.panel]]\nwidget = \"runners\"\nspan = [2, 1]").unwrap();
        assert_eq!(config.kiosk.panels()[0].widget, Widget::Runners);
    }

    #[test]
    fn kiosk_theme_pixel_shift_and_filters_default_to_the_current_look() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config.kiosk.theme, Theme::Day);
        assert!(!config.kiosk.pixel_shift);
        assert!(
            config
                .kiosk
                .panels()
                .iter()
                .all(|p| p.source.is_none() && p.repo.is_none())
        );
    }

    #[test]
    fn kiosk_theme_pixel_shift_and_filters_parse() {
        let config = Config::from_toml(
            r#"
[kiosk]
theme = "auto"
night_from = "21:30"
night_to = "06:15"
pixel_shift = true
pixel_shift_minutes = 3

[[kiosk.panel]]
widget = "prs"
source = "github"
repo = "acme/*"
"#,
        )
        .unwrap();
        assert_eq!(config.kiosk.theme, Theme::Auto);
        assert_eq!(config.kiosk.night_from, "21:30");
        assert!(config.kiosk.pixel_shift);
        assert_eq!(config.kiosk.pixel_shift_minutes, 3);
        let panel = &config.kiosk.panels()[0];
        assert_eq!(panel.source.as_deref(), Some("github"));
        assert_eq!(panel.repo.as_deref(), Some("acme/*"));
    }

    #[test]
    fn kiosk_rejects_bad_theme_shift_and_filters() {
        let bad = |toml: &str| format!("{:#}", Config::from_toml(toml).unwrap_err());
        assert!(bad("[kiosk]\ntheme = \"dusk\"").contains("unknown variant"));
        assert!(bad("[kiosk]\nnight_from = \"25:00\"").contains("HH:MM"));
        assert!(bad("[kiosk]\nnight_to = \"7:00\"").contains("HH:MM"));
        assert!(bad("[kiosk]\npixel_shift_minutes = 0").contains("between 1 and 120"));
        assert!(bad("[[kiosk.panel]]\nwidget = \"stats\"\nrepo = \"x\"").contains("only apply to"));
        assert!(
            bad("[[kiosk.panel]]\nwidget = \"jobs\"\nsource = \"svn\"").contains("source must be")
        );
        assert!(
            bad("[[kiosk.panel]]\nwidget = \"prs\"\nrepo = \" \"").contains("must not be empty")
        );
    }

    #[test]
    fn kiosk_rejects_bad_layouts() {
        let bad = |toml: &str| format!("{:#}", Config::from_toml(toml).unwrap_err());
        assert!(bad("[kiosk]\ncolumns = 0").contains("between 1 and 12"));
        assert!(bad("[kiosk]\nrows = 13").contains("between 1 and 12"));
        assert!(bad("[kiosk]\nmax_ws_clients = 0").contains("at least 1"));
        assert!(bad("[[kiosk.panel]]\nwidget = \"jobs\"\nspan = [5, 1]").contains("span must fit"));
        assert!(bad("[[kiosk.panel]]\nwidget = \"jobs\"\nspan = [1, 0]").contains("span must fit"));
        assert!(bad("[[kiosk.panel]]\nwidget = \"prs\"\nhost = \"x\"").contains("only applies"));
        assert!(bad("[[kiosk.panel]]\nwidget = \"clock\"").contains("unknown variant"));
        assert!(bad("[kiosk]\nflavour = 1").contains("unknown field"));
        assert!(bad("[[kiosk.panel]]\nwidget = \"prs\"\nrows = 0").contains("rows must be"));
    }
}
