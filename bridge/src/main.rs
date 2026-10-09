//! DeskWatch bridge: collects data on the home server and tells the desk panel
//! what to draw over MQTT. See `mqtt-schema.md` (schema v2) for the contract.
//!
//! The main loop owns all state, so there are no locks. Sources and the MQTT
//! connection run as tasks and send what they learn over channels:
//!
//! - every second, and after every event, the composer picks the screen and
//!   badges; they are published only when they change;
//! - every `server.interval_s` the local stats are sampled;
//! - each `[[source.*]]` block runs as its own task and sends fact updates
//!   and its health as `SourceMsg`s;
//! - button presses and alerts arrive from the MQTT task as `Inbound`s.
//!
//! With `--demo` no sources run: a scripted loop of fake data feeds the
//! composer instead, for watching the panel in the desktop simulator.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use deskwatch_bridge::alerts::Alerts;
use deskwatch_bridge::azure_devops::AzureDevops;
use deskwatch_bridge::composer::Composer;
use deskwatch_bridge::config::{Config, MQTT_PASSWORD_ENV, MqttConfig};
use deskwatch_bridge::demo::{self, Demo};
use deskwatch_bridge::gitea::Gitea;
use deskwatch_bridge::github::Github;
use deskwatch_bridge::health;
use deskwatch_bridge::hooks::Hooks;
use deskwatch_bridge::kiosk::Kiosk;
use deskwatch_bridge::model::{Badges, Screen};
use deskwatch_bridge::mqtt::{self, Inbound, Publisher};
use deskwatch_bridge::prometheus::Prometheus;
use deskwatch_bridge::source::{self, Secret, unix_now};
use deskwatch_bridge::stats::StatsCollector;

/// How often the composer re-checks timers (alert expiry, rotation dwell).
const COMPOSE_INTERVAL: Duration = Duration::from_secs(1);

/// Republish unchanged badges this often (seconds), so a broker that lost its
/// retained messages in a restart gets them back. The screen needs no such
/// refresh: stats change every few seconds.
const BADGES_REFRESH_S: u64 = 60;

const USAGE: &str = "\
usage: deskwatch-bridge [--demo] [--kiosk] [--no-mqtt] [--healthcheck] [CONFIG]

CONFIG defaults to $DESKWATCH_CONFIG, then /etc/deskwatch/bridge.toml
(on Windows %ProgramData%\\DeskWatch\\bridge.toml).
--demo  play a loop of fake data instead of running the sources; only the
        [mqtt] and [alerts] settings are used, and CONFIG is optional
--kiosk serve the browser dashboard even if [kiosk] enabled is not set
        (see docs/kiosk.md)
--no-mqtt  do not connect to a broker (same as [mqtt] enabled = false), for a
        bridge that only serves the dashboard page
--healthcheck  check that the bridge's HTTP listener answers on this machine
        (and, with MQTT on, that the broker connection is up), then exit 0
        (healthy) or 1; used by the Docker HEALTHCHECK

--demo, --kiosk and --no-mqtt can also be set with DESKWATCH_DEMO=1,
DESKWATCH_KIOSK=1 and DESKWATCH_NO_MQTT=1. A container's health check sees the
environment but not the command line, so set them there.";

/// Command line: `[--demo] [--kiosk] [--no-mqtt] [--healthcheck] [CONFIG]`.
struct Args {
    demo: bool,
    kiosk: bool,
    no_mqtt: bool,
    healthcheck: bool,
    config: Option<PathBuf>,
}

/// Is an on/off environment variable switched on (`1`, `true` or `yes`)?
fn env_flag(name: &str) -> bool {
    flag_value(std::env::var(name).ok().as_deref())
}

fn flag_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        demo: env_flag("DESKWATCH_DEMO"),
        kiosk: env_flag("DESKWATCH_KIOSK"),
        no_mqtt: env_flag("DESKWATCH_NO_MQTT"),
        healthcheck: false,
        config: None,
    };
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--demo") => args.demo = true,
            Some("--kiosk") => args.kiosk = true,
            Some("--no-mqtt") => args.no_mqtt = true,
            Some("--healthcheck") => args.healthcheck = true,
            Some("-h" | "--help") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            Some(flag) if flag.starts_with('-') => anyhow::bail!("unknown option {flag}\n{USAGE}"),
            _ if args.config.is_none() => args.config = Some(arg.into()),
            _ => anyhow::bail!("only one config path is allowed\n{USAGE}"),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    let mut config = match Config::path_given(args.config.clone()) {
        // The demo runs without a config file, against a broker on localhost.
        None if args.demo => Config::from_toml("")?,
        // The health probe also runs against a container started without a
        // config file (the demo); it then has nothing to probe unless told.
        given if args.healthcheck && !Config::path(given.clone()).exists() => {
            Config::from_toml("")?
        }
        given => {
            let path = Config::path(given);
            let config = Config::load(&path)?;
            info!("loaded config from {}", path.display());
            config
        }
    };

    config.kiosk.enabled |= args.kiosk;
    config.mqtt.enabled &= !args.no_mqtt;

    if args.healthcheck {
        // The Docker HEALTHCHECK: no logging, just the exit code.
        match health::check(&config).await {
            Ok(_) => return Ok(()),
            Err(err) => {
                eprintln!("unhealthy: {err:#}");
                std::process::exit(1);
            }
        }
    }

    // Build every source first: missing credentials or a taken port stop the
    // bridge here, before it connects anywhere. The demo runs none of them.
    let mut gitea = Vec::new();
    let mut github = Vec::new();
    let mut azure_devops = Vec::new();
    let mut prometheus = Vec::new();
    let mut hooks = Hooks::default();
    if !args.demo {
        for gitea_config in &config.source.gitea {
            gitea.push(Gitea::new(gitea_config, &mut hooks)?);
        }
        for github_config in &config.source.github {
            github.push(Github::new(github_config)?);
        }
        for azure_config in &config.source.azure_devops {
            azure_devops.push(AzureDevops::new(azure_config)?);
        }
        for prometheus_config in &config.source.prometheus {
            prometheus.push(Prometheus::new(prometheus_config)?);
        }
    }
    let mut kiosk = None;
    if config.kiosk.enabled {
        let token = match &config.kiosk.token_file {
            Some(file) => Some(source::load_secret(file).context("kiosk.token_file")?),
            None => None,
        };
        if token.is_none() && !config.http.listen.ip().is_loopback() {
            warn!(
                "the kiosk page is open to the network on {} without kiosk.token_file; \
                 anyone who can reach it can read your PR titles, pipelines and host stats",
                config.http.listen
            );
        }
        let built = Kiosk::new(&config.kiosk, token);
        hooks.add("/", built.router());
        kiosk = Some(built);
    }
    // `/healthz` (the Docker health probe) reports the broker connection, so
    // the listener also opens for a bridge that only talks to an ESP panel.
    let mqtt_link = health::MqttLink::default();
    if config.mqtt.enabled {
        hooks.add("/healthz", mqtt_link.route());
    }
    hooks.start(config.http.listen).await?;

    // The channel's sender stays alive here either way, so with MQTT off the
    // receiver just waits forever.
    let (inbound_tx, mut inbound_rx) = mpsc::channel(16);
    let publisher = if config.mqtt.enabled {
        let (publisher, eventloop) = mqtt::connect(&config.mqtt, mqtt_password(&config.mqtt)?)?;
        tokio::spawn(mqtt::run_event_loop(
            eventloop,
            publisher.clone(),
            inbound_tx.clone(),
            config.alerts.enabled,
            mqtt_link,
        ));
        Some(publisher)
    } else {
        info!("MQTT is off: serving the dashboard page only, no panel, button or MQTT alerts");
        None
    };

    // Sources report here. The sender stays alive in this function, so `recv`
    // simply waits forever when no source is configured.
    let (source_tx, mut source_rx) = mpsc::channel(64);
    for source in gitea {
        source::spawn(source, source_tx.clone());
    }
    for source in github {
        source::spawn(source, source_tx.clone());
    }
    for source in azure_devops {
        source::spawn(source, source_tx.clone());
    }
    for source in prometheus {
        source::spawn(source, source_tx.clone());
    }

    let mut collector = StatsCollector::new(&config.server);
    let mut composer;
    let mut demo = None;
    if args.demo {
        info!("demo mode: playing fake data, sources are not started");
        composer = Composer::new(demo::rotation(), unix_now());
        demo = Some(Demo::new(unix_now()));
    } else {
        composer = Composer::new(config.rotation.clone(), unix_now());
        composer.alerts = Alerts::new(config.alerts.clone());
    }
    let mut shown = Shown::default();

    let mut stats_ticker = tokio::time::interval(Duration::from_secs(config.server.interval_s));
    let mut compose_ticker = tokio::time::interval(COMPOSE_INTERVAL);
    let mut shutdown = Shutdown::new()?;
    loop {
        tokio::select! {
            _ = stats_ticker.tick(), if demo.is_none() => composer.set_stats(collector.collect()),
            _ = compose_ticker.tick() => {}
            Some(msg) = source_rx.recv() => composer.apply(msg, unix_now()),
            Some(msg) = inbound_rx.recv() => match msg {
                Inbound::Button(event) => composer.on_button(event.action, unix_now()),
                Inbound::Alert(alert) => composer.alert(alert, unix_now()),
            },
            _ = shutdown.wait() => break,
        }
        if let Some(demo) = &mut demo {
            demo.tick(&mut composer, unix_now());
        }
        if let Some(kiosk) = &mut kiosk {
            kiosk.update(&mut composer, unix_now());
        }
        if let Some(publisher) = &publisher {
            shown.publish(publisher, &mut composer, unix_now()).await;
        }
    }

    info!("shutting down");
    if let Some(publisher) = &publisher {
        // Not forever: with the broker down the goodbye cannot be queued.
        match tokio::time::timeout(Duration::from_secs(2), publisher.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!("clean shutdown failed: {err:#}"),
            Err(_) => warn!("clean shutdown timed out"),
        }
        // Give the event loop a moment to flush the offline message and disconnect.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

/// Resolves when the process is asked to stop, so the bridge can say goodbye
/// to the broker. Ctrl+C everywhere; SIGTERM (systemd) on Unix; closing the
/// console window, logoff and system shutdown on Windows.
#[cfg(unix)]
struct Shutdown(tokio::signal::unix::Signal);

#[cfg(unix)]
impl Shutdown {
    fn new() -> Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self(signal(SignalKind::terminate())?))
    }

    async fn wait(&mut self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = self.0.recv() => {}
        }
    }
}

#[cfg(windows)]
struct Shutdown {
    close: tokio::signal::windows::CtrlClose,
    logoff: tokio::signal::windows::CtrlLogoff,
    shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl Shutdown {
    fn new() -> Result<Self> {
        use tokio::signal::windows;
        Ok(Self {
            close: windows::ctrl_close()?,
            logoff: windows::ctrl_logoff()?,
            shutdown: windows::ctrl_shutdown()?,
        })
    }

    async fn wait(&mut self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = self.close.recv() => {}
            _ = self.logoff.recv() => {}
            _ = self.shutdown.recv() => {}
        }
    }
}

/// The broker password: `mqtt.password_file` if set, else the environment.
fn mqtt_password(config: &MqttConfig) -> Result<Option<Secret>> {
    if let Some(file) = &config.password_file {
        return source::load_secret(file)
            .context("mqtt.password_file")
            .map(Some);
    }
    Ok(std::env::var(MQTT_PASSWORD_ENV)
        .ok()
        .filter(|p| !p.is_empty())
        .map(Secret::new))
}

/// What was published last, so only changes go out.
#[derive(Default)]
struct Shown {
    /// Did the last publish fail? Only the first failure of a streak is
    /// logged, so a broker that is down does not fill the log.
    failing: bool,
    screen: Option<Screen>,
    badges: Option<Badges>,
    /// Unix seconds of the last badges publish.
    badges_at: u64,
}

impl Shown {
    fn failed(&mut self, what: &str, err: &anyhow::Error) {
        if !self.failing {
            warn!(
                "cannot publish {what}: {err:#} (retrying every turn, not logged again until it works)"
            );
        }
        self.failing = true;
    }

    async fn publish(&mut self, publisher: &Publisher, composer: &mut Composer, now: u64) {
        // `seq` is set by the publisher, so compare with it zeroed.
        let screen = composer.screen(now);
        if self.screen.as_ref() != Some(&screen) {
            match publisher.publish_screen(screen.clone()).await {
                Ok(()) => {
                    self.screen = Some(screen);
                    self.failing = false;
                }
                Err(err) => self.failed("screen", &err),
            }
        }
        let badges = composer.badges();
        let due = now.saturating_sub(self.badges_at) >= BADGES_REFRESH_S;
        if self.badges.as_ref() != Some(&badges) || due {
            match publisher.publish_badges(&badges).await {
                Ok(()) => {
                    self.badges = Some(badges);
                    self.badges_at = now;
                    self.failing = false;
                }
                Err(err) => self.failed("badges", &err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::flag_value;

    #[test]
    fn environment_flags_accept_the_usual_spellings() {
        for on in ["1", "true", "TRUE", "yes"] {
            assert!(flag_value(Some(on)), "{on}");
        }
        for off in ["", "0", "false", "no", "off"] {
            assert!(!flag_value(Some(off)), "{off}");
        }
        assert!(!flag_value(None));
    }
}
