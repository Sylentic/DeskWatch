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

/// What `sources.toml` in the data folder may hold.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overlay {
    #[serde(default)]
    source: Sources,
    /// Broker settings the setup page changed. Only the keys present replace
    /// the ones from `bridge.toml`.
    #[serde(default)]
    mqtt: Option<MqttPatch>,
    /// Dashboard settings the setup page changed.
    #[serde(default)]
    kiosk: Option<KioskPatch>,
    /// Alert and ticker settings the setup page changed.
    #[serde(default)]
    alerts: Option<AlertsPatch>,
    /// Update check setting the setup page changed.
    #[serde(default)]
    updates: Option<UpdatesPatch>,
    /// `kind:name` of `bridge.toml` sources the setup page switched off.
    /// `bridge.toml` itself is never edited; the source is just left out.
    #[serde(default)]
    hidden: Vec<String>,
}

/// The `[kiosk]` key the setup page can change: how the default layout shows
/// the hosts. The rest of `[kiosk]` stays in `bridge.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KioskPatch {
    host_view: Option<HostView>,
}

/// The `[updates]` key the setup page can change.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatesPatch {
    check: Option<bool>,
}

/// The `[alerts]` keys the setup page can change: how the header ticker looks
/// and behaves. `critical_ids` and `enabled` stay in `bridge.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertsPatch {
    ticker: Option<bool>,
    ticker_speed: Option<TickerSpeed>,
    show_source: Option<bool>,
    dismiss: Option<bool>,
    max_active: Option<usize>,
    http_input: Option<bool>,
}

/// The `[mqtt]` keys the setup page can change (docs/setup-page.md). Anything
/// else (client id, certificates, topic prefix) stays in `bridge.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct MqttPatch {
    enabled: Option<bool>,
    host: Option<String>,
    port: Option<u16>,
    tls: Option<bool>,
    /// An empty text means no login.
    username: Option<String>,
    password_file: Option<String>,
}

/// Why the bridge refuses a login setting that no longer exists. It names the
/// section and key, so the line to delete is easy to find.
fn removed_login_message(section: &str, key: &str) -> String {
    format!(
        "[{section}] {key} is no longer supported: the dashboard and the setup page no longer have a login, \
         they are for a trusted network only (docs/kiosk.md). Delete the `{key}` line from the \
         [{section}] section of the config. To keep a login, put the bridge behind a reverse proxy that has \
         its own"
    )
}

/// The setup page (`/setup`, docs/setup-page.md). Off unless enabled.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SetupConfig {
    /// Serve the setup page. It has no login: use it on a trusted network only.
    pub enabled: bool,
    /// Removed. Only read so that the bridge can refuse it with a clear
    /// message (see `Config::validate`) instead of an "unknown field" error.
    #[serde(rename = "token_file")]
    pub(crate) removed_token_file: Option<toml::Value>,
    /// Let the setup page restart the bridge (the process exits with code 75
    /// after a clean shutdown). Only for a bridge that is started again by
    /// its supervisor: Docker `restart: unless-stopped`, systemd
    /// `Restart=on-failure`. Off by default, because otherwise the bridge
    /// would simply stop.
    pub allow_restart: bool,
}

impl SetupConfig {
    /// Is the setup page served?
    pub fn is_on(&self) -> bool {
        self.enabled
    }
}

/// Where the bridge may keep files it writes itself.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DataConfig {
    /// A writable folder for `sources.toml` and `credentials/` (docs/data-dir.md).
    /// Without it (and without `DESKWATCH_DATA_DIR`) the bridge reads only
    /// `bridge.toml` and writes nothing, as before.
    pub dir: Option<PathBuf>,
}

impl Config {
    /// The Firefox container each source type opens its links in, by type
    /// (`azure_devops`). Only sources that name one are listed. The links on
    /// the page only know the type, so the first source of a type that names
    /// a container decides for the whole type.
    pub fn containers(&self) -> std::collections::BTreeMap<String, String> {
        let s = &self.source;
        let named = [
            (
                "gitea",
                s.gitea
                    .iter()
                    .map(|c| c.container.as_str())
                    .collect::<Vec<_>>(),
            ),
            (
                "github",
                s.github.iter().map(|c| c.container.as_str()).collect(),
            ),
            (
                "gitlab",
                s.gitlab.iter().map(|c| c.container.as_str()).collect(),
            ),
            (
                "bitbucket",
                s.bitbucket.iter().map(|c| c.container.as_str()).collect(),
            ),
            (
                "azure_devops",
                s.azure_devops
                    .iter()
                    .map(|c| c.container.as_str())
                    .collect(),
            ),
        ];
        let mut out = std::collections::BTreeMap::new();
        for (kind, list) in named {
            if let Some(name) = list.into_iter().find(|n| !n.is_empty()) {
                out.insert(kind.to_string(), name.to_string());
            }
        }
        out
    }

    /// The data folder: `[data] dir`, else the `DESKWATCH_DATA_DIR` environment variable.
    pub fn data_dir(&self) -> Option<PathBuf> {
        self.data.dir.clone().or_else(|| {
            std::env::var_os("DESKWATCH_DATA_DIR")
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
        })
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
    /// Folder the bridge may write to. Off unless set.
    #[serde(default)]
    pub data: DataConfig,
    /// The setup page. Off unless `enabled = true`.
    #[serde(default)]
    pub setup: SetupConfig,
    /// `kind:name` of the sources that came from `sources.toml` in the data
    /// folder rather than `bridge.toml`. Filled by `with_overlay`.
    #[serde(skip)]
    pub overlay_sources: std::collections::BTreeSet<String>,
    /// The `[mqtt]` section as `bridge.toml` had it, when the data folder
    /// changed some of its keys. Filled by `with_overlay`.
    #[serde(skip)]
    pub mqtt_before_overlay: Option<MqttConfig>,
    /// `kiosk.host_view` as `bridge.toml` had it, when the data folder changed it.
    #[serde(skip)]
    pub host_view_before_overlay: Option<HostView>,
    /// The `[alerts]` section as `bridge.toml` had it, when the data folder
    /// changed some of its keys. Filled by `with_overlay`.
    #[serde(skip)]
    pub alerts_before_overlay: Option<AlertsConfig>,
    /// `updates.check` as `bridge.toml` had it, when the data folder changed it.
    #[serde(skip)]
    pub update_check_before_overlay: Option<bool>,
    /// `kind:name` of the `bridge.toml` sources the data folder switched off.
    /// Filled by `with_overlay`.
    #[serde(skip)]
    pub hidden_sources: std::collections::BTreeSet<String>,
    /// The sources as `bridge.toml` had them, when some were switched off.
    #[serde(skip)]
    pub sources_before_overlay: Option<Sources>,
    /// Data sources, one array per adapter type.
    #[serde(default)]
    pub source: Sources,
    /// Alerts raised on `<topic_prefix>/alert` by Home Assistant or scripts.
    #[serde(default)]
    pub alerts: AlertsConfig,
    /// The daily look for a newer release.
    #[serde(default)]
    pub updates: UpdatesConfig,
    /// The browser dashboard for a big screen, served on `[http]`. Off by default.
    #[serde(default)]
    pub kiosk: KioskConfig,
    /// `[[rotation]]` belonged to the removed ESP panel. Still accepted so an
    /// old config keeps starting; the value is ignored (see `from_toml`).
    #[serde(default)]
    pub rotation: Option<toml::Value>,
}

/// Connection to the Mosquitto broker.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttConfig {
    /// Connect to the broker at all. Set to false for a bridge that only
    /// serves the kiosk page (docs/docker.md): no connection attempts, no
    /// warnings, and no MQTT alerts.
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
    /// Address the HTTP routes listen on: webhooks, the kiosk page, the setup page and
    /// `/healthz`. Only opened when at least one of them exists.
    pub listen: SocketAddr,
    /// Serve HTTPS instead of HTTP. Off unless `[http.tls]` is given.
    pub tls: Option<TlsConfig>,
    /// Let clients outside the local network reach the setup page and
    /// `POST /api/alert`. Off by default: both have no login yet, so they only
    /// answer loopback, private and similar addresses (see `access.rs`).
    pub allow_public: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 8787)),
            tls: None,
            allow_public: false,
        }
    }
}

/// The `[http.tls]` table: a certificate chain and private key, both PEM files.
/// With it the one `[http]` listener speaks HTTPS only (kiosk page, setup page,
/// `/api/alert`, webhooks and the WebSocket, which becomes `wss`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Certificate (and intermediates), PEM.
    pub cert: PathBuf,
    /// Private key, PEM.
    pub key: PathBuf,
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
    /// Removed (the dashboard has no login). Only read so that the bridge can
    /// refuse them with a clear message instead of an "unknown field" error.
    #[serde(rename = "token_file")]
    pub(crate) removed_token_file: Option<toml::Value>,
    #[serde(rename = "trusted_networks")]
    pub(crate) removed_trusted_networks: Option<toml::Value>,
    #[serde(rename = "session_days")]
    pub(crate) removed_session_days: Option<toml::Value>,
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
    /// How the built-in layout shows the hosts (ignored when `panel` is set).
    pub host_view: HostView,
}

/// What the hosts panel of the built-in layout shows. `detailed`: the chosen
/// host in detail (bars, temperature, load, network, a CPU graph) with every
/// host as a list below it; tapping a host in the list chooses it. `compact`:
/// only that list, with the network rates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostView {
    #[default]
    Detailed,
    Compact,
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
            removed_token_file: None,
            removed_trusted_networks: None,
            removed_session_days: None,
            max_ws_clients: 16,
            theme: Theme::Day,
            night_from: "22:00".into(),
            night_to: "07:00".into(),
            pixel_shift: false,
            pixel_shift_minutes: 5,
            panel: Vec::new(),
            host_view: HostView::Detailed,
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
        for (key, present) in [
            ("token_file", self.removed_token_file.is_some()),
            ("trusted_networks", self.removed_trusted_networks.is_some()),
            ("session_days", self.removed_session_days.is_some()),
        ] {
            anyhow::ensure!(!present, "{}", removed_login_message("kiosk", key));
        }
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
            anyhow::ensure!(
                panel.view.is_none() || panel.widget == Widget::Hosts,
                "kiosk.panel {n}: view only applies to the hosts widget"
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
            default_panels(self.host_view)
        } else {
            self.panel.clone()
        }
    }
}

/// The CI systems a `source` filter can name, as the kiosk page tags them.
const SOURCE_KINDS: [&str; 5] = ["gitea", "github", "gitlab", "bitbucket", "azure_devops"];

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
    /// Only what comes from this CI system: `gitea`, `github`, `gitlab`,
    /// `bitbucket` or `azure_devops`. For the `jobs`, `prs` and `pipelines` widgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Only this repository or project, matched without regard to case. A
    /// trailing `*` matches a prefix (`acme/*`). Same widgets as `source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// How a `hosts` widget shows its hosts: `detailed` (the chosen host in
    /// detail on top, every host as a list below) or `compact` (the list
    /// only). Without it the widget is the plain table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<HostView>,
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
        view: None,
        span,
    }
}

/// The layout used when `[[kiosk.panel]]` is not set, for the default 4x3 grid:
/// Running now (2 wide, 2 tall) and Hosts (2 wide, 2 tall) on top, Pipelines
/// (2 wide), Open PRs and Alerts (1 cell each) below them. The sources are in
/// the bar under the header, so they have no panel here.
pub fn default_panels(view: HostView) -> Vec<PanelConfig> {
    vec![
        panel(Widget::Jobs, [2, 2]),
        PanelConfig {
            view: Some(view),
            ..panel(Widget::Hosts, [2, 2])
        },
        panel(Widget::Pipelines, [2, 1]),
        panel(Widget::Prs, [1, 1]),
        panel(Widget::Alerts, [1, 1]),
    ]
}

/// The `[updates]` table: the "newer version available" notice.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    /// Once a day, ask GitHub for the number of the latest release and show a
    /// quiet notice when it is newer. Only the version number is compared;
    /// nothing about this install is sent. Turn off for a bridge without internet.
    pub check: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self { check: true }
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
    /// Show the alert ticker in the kiosk header.
    pub ticker: bool,
    /// How fast the ticker moves.
    pub ticker_speed: TickerSpeed,
    /// Show "via <source>" behind each alert in the ticker.
    pub show_source: bool,
    /// Let the kiosk page dismiss an alert (Dismiss buttons in the Alerts
    /// menu). It clears the alert for every screen, like a clear message would.
    pub dismiss: bool,
    /// Accept alerts as `POST /api/alert` on the shared HTTP listener, with
    /// the same JSON as on MQTT. Off by default: like the dashboard it has no
    /// login, so it is for a trusted network only.
    pub http_input: bool,
}

/// How fast the header ticker moves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TickerSpeed {
    Slow,
    #[default]
    Normal,
    Fast,
}

impl TickerSpeed {
    /// Pixels per rem-second factor used by the page: the ticker moves at
    /// `5 * rem * factor` pixels per second.
    pub fn factor(self) -> f64 {
        match self {
            TickerSpeed::Slow => 0.6,
            TickerSpeed::Normal => 1.0,
            TickerSpeed::Fast => 1.7,
        }
    }
}

/// Most alerts `max_active` may keep.
pub const MAX_ACTIVE_ALERTS: usize = 50;

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_active: 10,
            critical_ids: None,
            ticker: true,
            ticker_speed: TickerSpeed::Normal,
            show_source: true,
            dismiss: true,
            http_input: false,
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

/// Take source `kind:name` out of `sources`. `Ok(false)` when there is none
/// (the entry is then harmless); an id that is not `kind:name` is an error.
fn hide_source(sources: &mut Sources, id: &str) -> Result<bool> {
    let (kind, name) = id
        .split_once(':')
        .with_context(|| format!("hidden source {id:?} is not kind:name"))?;
    macro_rules! drop_from {
        ($list:expr) => {{
            let before = $list.len();
            $list.retain(|s| s.name != name);
            $list.len() != before
        }};
    }
    Ok(match kind {
        "gitea" => drop_from!(sources.gitea),
        "github" => drop_from!(sources.github),
        "gitlab" => drop_from!(sources.gitlab),
        "bitbucket" => drop_from!(sources.bitbucket),
        "azure_devops" => drop_from!(sources.azure_devops),
        "prometheus" => drop_from!(sources.prometheus),
        _ => anyhow::bail!("hidden source {id:?} has an unknown type"),
    })
}

/// All configured sources, by adapter type.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    pub gitea: Vec<GiteaConfig>,
    pub github: Vec<GithubConfig>,
    pub gitlab: Vec<GitlabConfig>,
    pub bitbucket: Vec<BitbucketConfig>,
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
    /// Firefox container the links of this source open in (docs/firefox-containers.md).
    /// Empty: links open as usual.
    #[serde(default)]
    pub container: String,
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
    /// Short labels for repositories, such as `{ "team/service-a" = "work A" }`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
    /// Put a new issue in the alerts list until it is dismissed. Needs the
    /// "Issues" event ticked in the Gitea webhook.
    #[serde(default = "yes")]
    pub alert_new_issues: bool,
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
    /// Firefox container the links of this source open in (docs/firefox-containers.md).
    /// Empty: links open as usual.
    #[serde(default)]
    pub container: String,
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
    /// Put a new issue in the alerts list until it is dismissed.
    #[serde(default = "yes")]
    pub alert_new_issues: bool,
    /// Self-hosted runner lists for the kiosk `runners` widget:
    /// `"repo:<owner>/<name>"` or `"org:<name>"`. Needs extra token rights
    /// (docs/runners.md). Empty means runners are not polled. Do not list
    /// overlapping scopes, a runner would show twice.
    #[serde(default)]
    pub runners: Vec<String>,
    /// Seconds between runner polls.
    #[serde(default = "default_github_runner_poll_s")]
    pub runner_poll_s: u64,
    /// Short labels for repositories, such as `{ "team/service-a" = "work A" }`.
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

/// One `[[source.gitlab]]` block: open merge requests and pipelines from
/// gitlab.com or a self-managed GitLab, by polling.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitlabConfig {
    /// Instance name, unique among GitLab sources. Used in logs and fact keys.
    pub name: String,
    /// Firefox container the links of this source open in (docs/firefox-containers.md).
    /// Empty: links open as usual.
    #[serde(default)]
    pub container: String,
    /// The GitLab site: `https://gitlab.com` (default) or
    /// `https://gitlab.example.com`. `/api/v4` is added by the bridge.
    #[serde(default = "default_gitlab_base_url")]
    pub base_url: String,
    /// Credential with a read-only token (`read_api` scope, Reporter role).
    pub token_file: String,
    /// Projects to watch, by path: `group/project` or `group/subgroup/project`.
    pub projects: Vec<String>,
    /// Seconds between polls of open merge requests and recent pipelines.
    #[serde(default = "default_github_poll_s")]
    pub poll_s: u64,
    /// Seconds between stage progress polls while a pipeline is running.
    #[serde(default = "default_github_job_poll_s")]
    pub job_poll_s: u64,
    /// May running pipelines and finished runs take over the screen? `true`,
    /// `false`, or a list of projects that may.
    #[serde(default = "interrupt_all")]
    pub interrupt: Interrupt,
    /// Flash "New PR" when a poll finds a new, non-draft merge request.
    #[serde(default = "yes")]
    pub notify_new_prs: bool,
    /// Put a new issue in the alerts list until it is dismissed.
    #[serde(default = "yes")]
    pub alert_new_issues: bool,
    /// Runner lists for the kiosk `runners` widget: `"project:<group>/<name>"`,
    /// `"group:<path>"` or `"instance"` (all runners, needs an administrator
    /// token). Needs extra rights (docs/runners.md). Empty means runners are
    /// not polled. Do not list overlapping scopes, a runner would show twice.
    #[serde(default)]
    pub runners: Vec<String>,
    /// Seconds between runner polls.
    #[serde(default = "default_github_runner_poll_s")]
    pub runner_poll_s: u64,
    /// Short labels for projects, such as `{ "team/service-a" = "work A" }`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
}

impl GitlabConfig {
    /// The parsed `runners` entries.
    pub fn runner_scopes(&self) -> Result<Vec<crate::gitlab::runners::RunnerScope>> {
        self.runners
            .iter()
            .map(|text| {
                crate::gitlab::runners::RunnerScope::parse(text)
                    .with_context(|| format!("source.gitlab {}: runners", self.name))
            })
            .collect()
    }
}

/// One `[[source.bitbucket]]` block: open pull requests and Pipelines runs from
/// Bitbucket Cloud (bitbucket.org), by polling.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BitbucketConfig {
    /// Instance name, unique among Bitbucket sources. Used in logs and fact keys.
    pub name: String,
    /// Firefox container the links of this source open in (docs/firefox-containers.md).
    /// Empty: links open as usual.
    #[serde(default)]
    pub container: String,
    /// API root. Only changes for tests.
    #[serde(default = "default_bitbucket_base_url")]
    pub base_url: String,
    /// Credential with a read-only repository, project or workspace access
    /// token (Pull requests: Read, Pipelines: Read).
    pub token_file: String,
    /// Repositories to watch, as `workspace/repo-slug`.
    pub repos: Vec<String>,
    /// Seconds between polls of open pull requests and recent pipelines.
    /// Bitbucket Cloud limits requests per hour, hence the slower default.
    #[serde(default = "default_bitbucket_poll_s")]
    pub poll_s: u64,
    /// Seconds between step progress polls while a pipeline is in progress.
    #[serde(default = "default_bitbucket_job_poll_s")]
    pub job_poll_s: u64,
    /// May running pipelines and finished runs take over the screen? `true`,
    /// `false`, or a list of repositories that may.
    #[serde(default = "interrupt_all")]
    pub interrupt: Interrupt,
    /// Flash "New PR" when a poll finds a new, non-draft pull request.
    #[serde(default = "yes")]
    pub notify_new_prs: bool,
    /// Put a new issue in the alerts list until it is dismissed.
    #[serde(default = "yes")]
    pub alert_new_issues: bool,
    /// Self-hosted runner lists for the kiosk `runners` widget:
    /// `"repo:<workspace>/<repo>"` or `"workspace:<slug>"`. Needs the Runners:
    /// Read right (docs/runners.md). Empty means runners are not polled. Do not
    /// list overlapping scopes, a runner would show twice.
    #[serde(default)]
    pub runners: Vec<String>,
    /// Seconds between runner polls.
    #[serde(default = "default_github_runner_poll_s")]
    pub runner_poll_s: u64,
    /// Short labels for repositories, such as `{ "team/service-a" = "work A" }`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
}

impl BitbucketConfig {
    /// The parsed `runners` entries.
    pub fn runner_scopes(&self) -> Result<Vec<crate::bitbucket::runners::RunnerScope>> {
        self.runners
            .iter()
            .map(|text| {
                crate::bitbucket::runners::RunnerScope::parse(text)
                    .with_context(|| format!("source.bitbucket {}: runners", self.name))
            })
            .collect()
    }
}

/// How an Azure DevOps source proves who it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AzureDevopsAuth {
    /// A personal access token (`token_file`): acts as one person.
    #[default]
    Pat,
    /// A Microsoft Entra service principal (`tenant_id`, `client_id`,
    /// `client_secret_file`): acts as an app, not as a person.
    Entra,
}

/// One `[[source.azure_devops]]` block: pipeline runs and open PRs from Azure
/// DevOps Services, by polling.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AzureDevopsConfig {
    /// Instance name, unique among Azure DevOps sources. Used in logs and fact keys.
    pub name: String,
    /// Firefox container the links of this source open in (docs/firefox-containers.md).
    /// Empty: links open as usual.
    #[serde(default)]
    pub container: String,
    /// The organisation: the first part of `https://dev.azure.com/<organization>`.
    pub organization: String,
    /// Service address. Only changes for tests.
    #[serde(default = "default_azure_devops_base_url")]
    pub base_url: String,
    /// Projects of the organisation to watch.
    pub projects: Vec<String>,
    /// How the bridge signs in: `pat` (default) or `entra`.
    #[serde(default)]
    pub auth: AzureDevopsAuth,
    /// Credential with a read-only personal access token (Build: Read,
    /// Code: Read). For `auth = "pat"`.
    #[serde(default)]
    pub token_file: String,
    /// Microsoft Entra tenant (directory) id or domain. For `auth = "entra"`.
    #[serde(default)]
    pub tenant_id: String,
    /// Application (client) id of the service principal. For `auth = "entra"`.
    #[serde(default)]
    pub client_id: String,
    /// Credential holding the service principal's client secret. For `auth = "entra"`.
    #[serde(default)]
    pub client_secret_file: String,
    /// Entra sign-in address. Only changes for tests or a national cloud.
    #[serde(default = "default_entra_login_url")]
    pub login_url: String,
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
    /// Put a new comment on an open PR in the alerts list until it is
    /// dismissed. Your own comments are skipped. Costs one request per open
    /// PR per poll.
    #[serde(default = "yes")]
    pub alert_pr_comments: bool,
    /// Pipelines whose name contains one of these words (any case) are
    /// deploys; the rest are builds.
    #[serde(default = "default_deploy_words")]
    pub deploy_words: Vec<String>,
    /// Short labels: for a project by its name, for the PRs of a
    /// repository by `project/repository`.
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
    /// Self-hosted agent pools to show on the kiosk `runners` widget, by pool
    /// name (docs/runners.md). Empty means agents are not polled. Needs the
    /// Agent Pools (Read) scope on the token.
    #[serde(default)]
    pub agent_pools: Vec<String>,
    /// Seconds between agent pool polls.
    #[serde(default = "default_azure_devops_agent_poll_s")]
    pub agent_poll_s: u64,
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
    /// The host name shown on the kiosk page.
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

fn default_bitbucket_base_url() -> String {
    crate::bitbucket::api::BITBUCKET_API.into()
}

fn default_bitbucket_poll_s() -> u64 {
    120
}

fn default_bitbucket_job_poll_s() -> u64 {
    10
}

fn default_gitlab_base_url() -> String {
    crate::gitlab::api::GITLAB_COM.into()
}

fn default_azure_devops_agent_poll_s() -> u64 {
    60
}

fn default_entra_login_url() -> String {
    crate::azure_devops::api::ENTRA_LOGIN.into()
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

/// `owner/name` with no empty part. The name goes into API URLs.
/// Organisation and project names: not empty, no slash, no control characters.
/// Project names may contain spaces.
fn valid_azure_name(name: &str) -> bool {
    !name.trim().is_empty() && !name.contains('/') && !name.chars().any(char::is_control)
}

/// A GitLab group or project path: `min_segments` or more names joined by `/`,
/// each of letters, digits, `-`, `_` and `.`.
pub(crate) fn valid_gitlab_path(path: &str, min_segments: usize) -> bool {
    let mut count = 0;
    let ok = path.split('/').all(|part| {
        count += 1;
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    });
    ok && count >= min_segments
}

/// One name of a repository path: letters, digits, `-`, `_` and `.`.
pub(crate) fn valid_repo_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub(crate) fn valid_repo(repo: &str) -> bool {
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
        if config.rotation.is_some() {
            tracing::warn!(
                "[[rotation]] is ignored: the ESP panel was removed, so there is no idle rotation any more; \
                 delete the [[rotation]] blocks from the config"
            );
        }
        config.validate()?;
        Ok(config)
    }

    /// Add the sources of an overlay file (`sources.toml` in the data
    /// folder, see `store.rs`) and check the result as a whole. The overlay
    /// holds only `[[source.*]]` blocks; a source name already used in the
    /// main config for the same type is refused instead of silently replaced.
    pub fn with_overlay(mut self, overlay: &str) -> Result<Self> {
        let Overlay {
            source,
            mqtt,
            kiosk,
            alerts,
            updates,
            hidden,
        } = toml::from_str(overlay).context("invalid sources overlay")?;
        if !hidden.is_empty() {
            self.sources_before_overlay = Some(self.source.clone());
            for id in &hidden {
                let found = hide_source(&mut self.source, id)?;
                if found {
                    self.hidden_sources.insert(id.clone());
                }
            }
        }
        if let Some(patch) = mqtt {
            self.mqtt_before_overlay = Some(self.mqtt.clone());
            let m = &mut self.mqtt;
            m.enabled = patch.enabled.unwrap_or(m.enabled);
            m.host = patch.host.unwrap_or_else(|| m.host.clone());
            m.port = patch.port.unwrap_or(m.port);
            m.tls = patch.tls.unwrap_or(m.tls);
            if let Some(username) = patch.username {
                m.username = Some(username).filter(|u| !u.is_empty());
            }
            if let Some(file) = patch.password_file {
                m.password_file = Some(file);
            }
        }
        if let Some(view) = kiosk.and_then(|k| k.host_view) {
            self.host_view_before_overlay = Some(self.kiosk.host_view);
            self.kiosk.host_view = view;
        }
        if let Some(check) = updates.and_then(|u| u.check) {
            self.update_check_before_overlay = Some(self.updates.check);
            self.updates.check = check;
        }
        if let Some(patch) = alerts {
            self.alerts_before_overlay = Some(self.alerts.clone());
            let a = &mut self.alerts;
            a.ticker = patch.ticker.unwrap_or(a.ticker);
            a.ticker_speed = patch.ticker_speed.unwrap_or(a.ticker_speed);
            a.show_source = patch.show_source.unwrap_or(a.show_source);
            a.dismiss = patch.dismiss.unwrap_or(a.dismiss);
            a.max_active = patch.max_active.unwrap_or(a.max_active);
            a.http_input = patch.http_input.unwrap_or(a.http_input);
        }
        macro_rules! merge {
            ($($kind:ident),+) => {$(
                for extra in source.$kind {
                    anyhow::ensure!(
                        !self.source.$kind.iter().any(|s| s.name == extra.name),
                        "source.{} {:?} is in both bridge.toml and the data folder's sources.toml; \
                         rename or remove one",
                        stringify!($kind),
                        extra.name
                    );
                    self.overlay_sources
                        .insert(format!("{}:{}", stringify!($kind), extra.name));
                    self.source.$kind.push(extra);
                }
            )+};
        }
        merge!(gitea, github, gitlab, bitbucket, azure_devops, prometheus);
        self.validate()?;
        Ok(self)
    }

    /// This config without the sources that came from the data folder: what
    /// `bridge.toml` alone says. The setup page adds a changed overlay to it.
    pub fn without_overlay(&self) -> Self {
        let mut base = self.clone();
        let keep =
            |kind: &str, name: &str| !self.overlay_sources.contains(&format!("{kind}:{name}"));
        base.source.gitea.retain(|s| keep("gitea", &s.name));
        base.source.github.retain(|s| keep("github", &s.name));
        base.source.gitlab.retain(|s| keep("gitlab", &s.name));
        base.source.bitbucket.retain(|s| keep("bitbucket", &s.name));
        base.source
            .azure_devops
            .retain(|s| keep("azure_devops", &s.name));
        base.source
            .prometheus
            .retain(|s| keep("prometheus", &s.name));
        base.overlay_sources.clear();
        base.hidden_sources.clear();
        if let Some(original) = base.sources_before_overlay.take() {
            base.source = original;
        }
        if let Some(original) = base.mqtt_before_overlay.take() {
            base.mqtt = original;
        }
        if let Some(original) = base.host_view_before_overlay.take() {
            base.kiosk.host_view = original;
        }
        if let Some(original) = base.alerts_before_overlay.take() {
            base.alerts = original;
        }
        if let Some(original) = base.update_check_before_overlay.take() {
            base.updates.check = original;
        }
        base
    }

    /// The checks that need a whole config: ranges, unique names, consistent
    /// pairs. Run again after an overlay is added.
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.setup.removed_token_file.is_none(),
            "{}",
            removed_login_message("setup", "token_file")
        );
        anyhow::ensure!(
            (1..=MAX_ACTIVE_ALERTS).contains(&self.alerts.max_active),
            "alerts.max_active must be between 1 and {MAX_ACTIVE_ALERTS}"
        );
        anyhow::ensure!(
            self.server.interval_s > 0,
            "server.interval_s must be above 0"
        );
        let mqtt = &self.mqtt;
        anyhow::ensure!(
            mqtt.client_cert_file.is_some() == mqtt.client_key_file.is_some(),
            "mqtt.client_cert_file and mqtt.client_key_file go together"
        );
        anyhow::ensure!(
            mqtt.tls || (mqtt.ca_file.is_none() && mqtt.client_cert_file.is_none()),
            "mqtt.ca_file and mqtt.client_cert_file need mqtt.tls = true"
        );
        let mut names = HashSet::new();
        for gitea in &self.source.gitea {
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
        for (kind, container) in &self.containers() {
            anyhow::ensure!(
                container.chars().count() <= 64 && !container.chars().any(char::is_control),
                "source.{kind}: container must be a short name without control characters"
            );
        }
        let mut names = HashSet::new();
        for github in &self.source.github {
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
        for gitlab in &self.source.gitlab {
            let name = &gitlab.name;
            anyhow::ensure!(
                valid_name(name),
                "source.gitlab name {name:?} may only use letters, digits, - and _"
            );
            anyhow::ensure!(
                names.insert(name),
                "two source.gitlab blocks are named {name:?}"
            );
            anyhow::ensure!(
                gitlab.base_url.starts_with("https://") || gitlab.base_url.starts_with("http://"),
                "source.gitlab {name}: base_url must start with https://"
            );
            anyhow::ensure!(
                gitlab.poll_s > 0 && gitlab.job_poll_s > 0 && gitlab.runner_poll_s > 0,
                "source.gitlab {name}: poll_s, job_poll_s and runner_poll_s must be above 0"
            );
            gitlab.runner_scopes()?;
            anyhow::ensure!(
                !gitlab.projects.is_empty(),
                "source.gitlab {name}: projects needs at least one group/project"
            );
            anyhow::ensure!(
                gitlab.projects.iter().all(|p| valid_gitlab_path(p, 2)),
                "source.gitlab {name}: projects entries must look like group/project"
            );
        }
        let mut names = HashSet::new();
        for bitbucket in &self.source.bitbucket {
            let name = &bitbucket.name;
            anyhow::ensure!(
                valid_name(name),
                "source.bitbucket name {name:?} may only use letters, digits, - and _"
            );
            anyhow::ensure!(
                names.insert(name),
                "two source.bitbucket blocks are named {name:?}"
            );
            anyhow::ensure!(
                bitbucket.base_url.starts_with("https://")
                    || bitbucket.base_url.starts_with("http://"),
                "source.bitbucket {name}: base_url must start with https://"
            );
            anyhow::ensure!(
                bitbucket.poll_s > 0 && bitbucket.job_poll_s > 0 && bitbucket.runner_poll_s > 0,
                "source.bitbucket {name}: poll_s, job_poll_s and runner_poll_s must be above 0"
            );
            bitbucket.runner_scopes()?;
            anyhow::ensure!(
                !bitbucket.repos.is_empty(),
                "source.bitbucket {name}: repos needs at least one workspace/repo"
            );
            anyhow::ensure!(
                bitbucket.repos.iter().all(|r| valid_repo(r)),
                "source.bitbucket {name}: repos entries must look like workspace/repo"
            );
        }
        let mut names = HashSet::new();
        for azure in &self.source.azure_devops {
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
            match azure.auth {
                AzureDevopsAuth::Pat => anyhow::ensure!(
                    !azure.token_file.trim().is_empty(),
                    "source.azure_devops {name}: token_file is needed (or set auth = \"entra\")"
                ),
                AzureDevopsAuth::Entra => {
                    anyhow::ensure!(
                        !azure.tenant_id.trim().is_empty()
                            && !azure.client_id.trim().is_empty()
                            && !azure.client_secret_file.trim().is_empty(),
                        "source.azure_devops {name}: auth = \"entra\" needs tenant_id, client_id and client_secret_file"
                    );
                    anyhow::ensure!(
                        azure.login_url.starts_with("https://")
                            || azure.login_url.starts_with("http://"),
                        "source.azure_devops {name}: login_url must start with https://"
                    );
                }
            }
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
            anyhow::ensure!(
                azure.agent_poll_s > 0,
                "source.azure_devops {name}: agent_poll_s must be above 0"
            );
            anyhow::ensure!(
                azure.agent_pools.iter().all(|p| !p.trim().is_empty()),
                "source.azure_devops {name}: agent_pools entries must be pool names"
            );
        }
        let mut names = HashSet::new();
        for prom in &self.source.prometheus {
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
        self.kiosk.validate()?;
        Ok(())
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
        assert!(config.rotation.is_none());
        assert!(config.source.gitea.is_empty());
        assert_eq!(config.http.listen.port(), 8787);
        assert!(config.http.tls.is_none());
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
    fn ticker_settings_have_defaults_and_the_setup_page_can_change_them() {
        let config = Config::from_toml("").unwrap();
        let a = &config.alerts;
        assert!(a.ticker && a.show_source && a.dismiss);
        assert!(
            !a.http_input,
            "the web address is off until it is switched on"
        );
        assert_eq!(a.ticker_speed, TickerSpeed::Normal);

        let overlay = "[alerts]\nticker = false\nticker_speed = \"fast\"\nmax_active = 3\nhttp_input = true\n";
        let with = config.clone().with_overlay(overlay).unwrap();
        assert!(!with.alerts.ticker && with.alerts.show_source);
        assert_eq!(with.alerts.ticker_speed, TickerSpeed::Fast);
        assert_eq!(with.alerts.max_active, 3);
        assert!(with.alerts.http_input);
        // Taking the overlay away gives bridge.toml's own values back.
        let base = with.without_overlay();
        assert!(base.alerts.ticker);
        assert_eq!(base.alerts.max_active, 10);
        assert!(!base.alerts.http_input);
    }

    #[test]
    fn the_update_check_is_on_by_default_and_the_data_folder_can_switch_it() {
        let config = Config::from_toml("").unwrap();
        assert!(config.updates.check);
        let off = Config::from_toml("[updates]\ncheck = false\n").unwrap();
        assert!(!off.updates.check);
        let with = config.with_overlay("[updates]\ncheck = false\n").unwrap();
        assert!(!with.updates.check);
        assert!(with.without_overlay().updates.check);
        assert!(Config::from_toml("[updates]\nother = 1\n").is_err());
    }

    #[test]
    fn max_active_alerts_stays_between_1_and_50() {
        for bad in ["0", "51"] {
            let err = Config::from_toml(&format!("[alerts]\nmax_active = {bad}\n")).unwrap_err();
            assert!(format!("{err:#}").contains("max_active"), "{err:#}");
        }
        assert!(Config::from_toml("[alerts]\nmax_active = 50\n").is_ok());
    }

    #[test]
    fn critical_ids_limit_critical_alerts() {
        let config = Config::from_toml("[alerts]\ncritical_ids = [\"leak\"]\n").unwrap();
        assert!(config.alerts.allows_critical("leak"));
        assert!(!config.alerts.allows_critical("door"));
    }

    #[test]
    fn a_leftover_rotation_block_is_accepted_and_ignored() {
        let text = "[[rotation]]\npage = \"stats\"\ndwell_s = 20\n\n\
[[rotation]]\npage = \"prs\"\ndwell_s = 8\nskip_when_empty = true\n";
        let config = Config::from_toml(text).unwrap();
        assert!(config.rotation.is_some());
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.toml");
        let config = Config::from_toml(text).unwrap();
        assert!(config.rotation.is_none(), "the example has no [[rotation]]");
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
        assert!(config.source.gitea.is_empty());
    }

    #[test]
    fn example_github_blocks_parse_when_uncommented() {
        let text = include_str!("../config.example.toml");
        let start = text.find("# [[source.github]]").unwrap();
        let end = text.find("# GitLab (gitlab.com").unwrap();
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
    fn example_gitlab_block_parses_when_uncommented() {
        let text = include_str!("../config.example.toml");
        assert!(
            Config::from_toml(text).unwrap().source.gitlab.is_empty(),
            "GitLab block is commented out"
        );
        let start = text.find("# [[source.gitlab]]").unwrap();
        let end = text.find("# Bitbucket Cloud").unwrap();
        let block: String = text[start..end]
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l.trim_start_matches('#')))
            .collect::<Vec<_>>()
            .join("\n");
        let config = Config::from_toml(&block).unwrap();
        let gitlab = &config.source.gitlab[0];
        assert_eq!(gitlab.base_url, "https://gitlab.com");
        assert_eq!(gitlab.projects, ["your-group/project-a"]);
        assert_eq!(gitlab.alias["your-group/project-a"], "A");
    }

    #[test]
    fn gitlab_defaults_and_checks() {
        let block = |extra: &str| {
            format!(
                "[[source.gitlab]]\nname = \"work\"\nprojects = [\"team/sub/app\"]\n\
                 token_file = \"gitlab-token\"\n{extra}"
            )
        };
        let config = Config::from_toml(&block("")).unwrap();
        let gitlab = &config.source.gitlab[0];
        assert_eq!(gitlab.base_url, "https://gitlab.com");
        assert_eq!((gitlab.poll_s, gitlab.job_poll_s), (60, 5));
        assert_eq!(gitlab.interrupt, Interrupt::All(true));
        assert!(gitlab.runners.is_empty() && gitlab.notify_new_prs);

        let scopes = Config::from_toml(&block(
            "runners = [\"project:team/app\", \"group:team/sub\", \"instance\"]",
        ))
        .unwrap();
        assert_eq!(scopes.source.gitlab[0].runner_scopes().unwrap().len(), 3);

        for bad in [
            "runners = [\"repo:team/app\"]",
            "runners = [\"project:app\"]",
            "base_url = \"gitlab.example.com\"",
            "poll_s = 0",
        ] {
            assert!(Config::from_toml(&block(bad)).is_err(), "{bad}");
        }
        // A project needs group and name; the paths allow only plain characters.
        for bad in ["[\"app\"]", "[\"team//app\"]", "[\"team/ap p\"]", "[]"] {
            let text =
                format!("[[source.gitlab]]\nname = \"w\"\ntoken_file = \"t\"\nprojects = {bad}\n");
            assert!(Config::from_toml(&text).is_err(), "{bad}");
        }
        let twice = format!("{}\n{}", block(""), block(""));
        assert!(Config::from_toml(&twice).is_err(), "names are unique");
    }

    #[test]
    fn example_bitbucket_block_parses_when_uncommented() {
        let text = include_str!("../config.example.toml");
        assert!(
            Config::from_toml(text).unwrap().source.bitbucket.is_empty(),
            "Bitbucket block is commented out"
        );
        let start = text.find("# [[source.bitbucket]]").unwrap();
        let end = text.find("# Azure DevOps Services").unwrap();
        let block: String = text[start..end]
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l.trim_start_matches('#')))
            .collect::<Vec<_>>()
            .join("\n");
        let config = Config::from_toml(&block).unwrap();
        let bitbucket = &config.source.bitbucket[0];
        assert_eq!(bitbucket.repos, ["your-workspace/repo-a"]);
        assert_eq!(bitbucket.alias["your-workspace/repo-a"], "A");
    }

    #[test]
    fn bitbucket_defaults_and_checks() {
        let block = |extra: &str| {
            format!(
                "[[source.bitbucket]]\nname = \"work\"\nrepos = [\"team/app\"]\n\
                 token_file = \"bitbucket-token\"\n{extra}"
            )
        };
        let config = Config::from_toml(&block("")).unwrap();
        let bitbucket = &config.source.bitbucket[0];
        assert_eq!(bitbucket.base_url, "https://api.bitbucket.org/2.0");
        assert_eq!((bitbucket.poll_s, bitbucket.job_poll_s), (120, 10));
        assert_eq!(bitbucket.interrupt, Interrupt::All(true));
        assert!(bitbucket.runners.is_empty() && bitbucket.notify_new_prs);

        let scopes =
            Config::from_toml(&block("runners = [\"repo:team/app\", \"workspace:team\"]")).unwrap();
        assert_eq!(scopes.source.bitbucket[0].runner_scopes().unwrap().len(), 2);

        for bad in [
            "runners = [\"project:team/app\"]",
            "runners = [\"repo:app\"]",
            "base_url = \"api.bitbucket.org\"",
            "poll_s = 0",
        ] {
            assert!(Config::from_toml(&block(bad)).is_err(), "{bad}");
        }
        for bad in ["[\"app\"]", "[\"team/sub/app\"]", "[\"team/ap p\"]", "[]"] {
            let text =
                format!("[[source.bitbucket]]\nname = \"w\"\ntoken_file = \"t\"\nrepos = {bad}\n");
            assert!(Config::from_toml(&text).is_err(), "{bad}");
        }
        let twice = format!("{}\n{}", block(""), block(""));
        assert!(Config::from_toml(&twice).is_err(), "names are unique");
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
        let block: String = text[start..]
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
        // Agent pools are off unless named.
        assert!(azure.agent_pools.is_empty());
        assert_eq!(azure.agent_poll_s, 60);

        let pools = Config::from_toml(&block(
            "agent_pools = [\"Homelab\", \"Build Farm\"]\nagent_poll_s = 30",
        ))
        .unwrap();
        assert_eq!(pools.source.azure_devops[0].agent_pools.len(), 2);
        assert!(Config::from_toml(&block("agent_pools = [\" \"]")).is_err());
        assert!(Config::from_toml(&block("agent_poll_s = 0")).is_err());

        // Sign in as a service principal instead of with a token.
        let entra = |extra: &str| {
            format!(
                "[[source.azure_devops]]\nname = \"work\"\norganization = \"your-org\"\n\
                 projects = [\"project-a\"]\nauth = \"entra\"\n{extra}"
            )
        };
        let full = "tenant_id = \"t\"\nclient_id = \"c\"\nclient_secret_file = \"azdo-secret\"\n";
        let sp = Config::from_toml(&entra(full)).unwrap();
        let sp = &sp.source.azure_devops[0];
        assert_eq!(sp.auth, AzureDevopsAuth::Entra);
        assert_eq!(sp.login_url, "https://login.microsoftonline.com");
        assert_eq!(config.source.azure_devops[0].auth, AzureDevopsAuth::Pat);
        // Each way needs its own credentials, and says so.
        let err = Config::from_toml(&entra("tenant_id = \"t\"\n")).unwrap_err();
        assert!(format!("{err:#}").contains("client_secret_file"), "{err:#}");
        let no_token = block("").replace("token_file = \"azdo-token\"\n", "");
        let err = Config::from_toml(&no_token).unwrap_err();
        assert!(format!("{err:#}").contains("token_file"), "{err:#}");

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
        assert_eq!(panels, default_panels(HostView::Detailed));
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
    fn removed_login_settings_stop_the_bridge_and_name_the_line_to_delete() {
        for (toml, section, key) in [
            (
                "[kiosk]\ntoken_file = \"kiosk-token\"\n",
                "kiosk",
                "token_file",
            ),
            (
                "[kiosk]\ntrusted_networks = [\"10.0.0.0/8\"]\n",
                "kiosk",
                "trusted_networks",
            ),
            ("[kiosk]\nsession_days = 7\n", "kiosk", "session_days"),
            (
                "[setup]\ntoken_file = \"setup-token\"\n",
                "setup",
                "token_file",
            ),
            // Also when the page is switched off or the value is empty.
            (
                "[setup]\nenabled = false\ntoken_file = \"\"\n",
                "setup",
                "token_file",
            ),
        ] {
            let err = Config::from_toml(toml).unwrap_err().to_string();
            assert!(
                err.contains(&format!("[{section}] {key}")) && err.contains("Delete the"),
                "{toml:?} gave: {err}"
            );
        }
        // Without them the same sections load.
        let ok = Config::from_toml("[kiosk]\nenabled = true\n[setup]\nenabled = true\n").unwrap();
        assert!(ok.setup.is_on() && ok.kiosk.enabled);
        assert!(!Config::from_toml("").unwrap().setup.is_on());
    }

    #[test]
    fn hidden_sources_leave_the_config_and_come_back_without_the_overlay() {
        let text = "[[source.github]]\nname = \"main\"\ntoken_file = \"t\"\nrepos = [\"o/a\"]\n\
                [[source.github]]\nname = \"other\"\ntoken_file = \"t\"\nrepos = [\"o/b\"]\n";
        let base = Config::from_toml(text).unwrap();
        let hidden = base
            .clone()
            .with_overlay("hidden = [\"github:main\", \"gitea:gone\"]\n")
            .unwrap();
        assert_eq!(hidden.source.github.len(), 1);
        assert_eq!(hidden.source.github[0].name, "other");
        // Only what was really there counts as hidden.
        assert_eq!(
            hidden.hidden_sources.iter().collect::<Vec<_>>(),
            ["github:main"]
        );
        assert_eq!(hidden.without_overlay().source.github.len(), 2);
        assert!(
            base.clone()
                .with_overlay("hidden = [\"nonsense\"]\n")
                .is_err()
        );
    }

    #[test]
    fn the_default_layout_has_one_hosts_panel_and_no_sources_panel() {
        for view in [HostView::Detailed, HostView::Compact] {
            let panels = default_panels(view);
            let widgets: Vec<Widget> = panels.iter().map(|p| p.widget).collect();
            // The sources are in the bar under the header.
            assert_eq!(
                widgets,
                [
                    Widget::Jobs,
                    Widget::Hosts,
                    Widget::Pipelines,
                    Widget::Prs,
                    Widget::Alerts
                ]
            );
            let hosts = &panels[1];
            assert_eq!((hosts.span, hosts.view), ([2, 2], Some(view)));
            // Running now and Hosts big on top; Pipelines, Open PRs and Alerts below.
            assert_eq!(panels[0].span, [2, 2]);
            assert_eq!((panels[2].span, panels[2].rows), ([2, 1], None));
            assert_eq!((panels[3].span, panels[4].span), ([1, 1], [1, 1]));
        }
        // `host_view` picks the view of that panel, from bridge.toml or the setup page.
        let config = Config::from_toml("[kiosk]\nhost_view = \"compact\"\n").unwrap();
        assert_eq!(config.kiosk.panels(), default_panels(HostView::Compact));
        // The setup page's overlay wins over bridge.toml and can be taken off again.
        let with = Config::from_toml("")
            .unwrap()
            .with_overlay("[kiosk]\nhost_view = \"compact\"\n")
            .unwrap();
        assert_eq!(with.kiosk.host_view, HostView::Compact);
        assert_eq!(with.without_overlay().kiosk.host_view, HostView::Detailed);
        // Panels set by hand are never touched, and a `stats` panel with a
        // fixed host or a plain `hosts` table keep working.
        let custom = Config::from_toml(
            "[kiosk]\nhost_view = \"compact\"\n[[kiosk.panel]]\nwidget = \"stats\"\nhost = \"nas\"\n\
             [[kiosk.panel]]\nwidget = \"hosts\"\n[[kiosk.panel]]\nwidget = \"alerts\"\n\
             [[kiosk.panel]]\nwidget = \"health\"\n",
        )
        .unwrap();
        let panels = custom.kiosk.panels();
        assert_eq!(panels.len(), 4);
        assert_eq!(panels[1].view, None);
        // `view` only means something on a hosts panel.
        assert!(
            Config::from_toml("[[kiosk.panel]]\nwidget = \"prs\"\nview = \"compact\"\n").is_err()
        );
    }

    #[test]
    fn runners_widget_is_not_in_the_default_layout() {
        assert!(
            default_panels(HostView::Detailed)
                .iter()
                .all(|p| p.widget != Widget::Runners)
        );
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
