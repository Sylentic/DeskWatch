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
use deskwatch_bridge::hooks::Hooks;
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
usage: deskwatch-bridge [--demo] [CONFIG]

CONFIG defaults to $DESKWATCH_CONFIG, then /etc/deskwatch/bridge.toml
(on Windows %ProgramData%\\DeskWatch\\bridge.toml).
--demo  play a loop of fake data instead of running the sources; only the
        [mqtt] and [alerts] settings are used, and CONFIG is optional";

/// Command line: `[--demo] [CONFIG]`.
struct Args {
    demo: bool,
    config: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        demo: false,
        config: None,
    };
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--demo") => args.demo = true,
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
    let config = match Config::path_given(args.config.clone()) {
        // The demo runs without a config file, against a broker on localhost.
        None if args.demo => Config::from_toml("")?,
        given => {
            let path = Config::path(given);
            let config = Config::load(&path)?;
            info!("loaded config from {}", path.display());
            config
        }
    };

    // Build every source first: missing credentials or a taken port stop the
    // bridge here, before it connects anywhere. The demo runs none of them.
    let mut gitea = Vec::new();
    let mut github = Vec::new();
    let mut azure_devops = Vec::new();
    let mut prometheus = Vec::new();
    if !args.demo {
        let mut hooks = Hooks::default();
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
        hooks.start(config.http.listen).await?;
    }

    let (inbound_tx, mut inbound_rx) = mpsc::channel(16);
    let (publisher, eventloop) = mqtt::connect(&config.mqtt, mqtt_password(&config.mqtt)?)?;
    tokio::spawn(mqtt::run_event_loop(
        eventloop,
        publisher.clone(),
        inbound_tx,
        config.alerts.enabled,
    ));

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
        shown.publish(&publisher, &mut composer, unix_now()).await;
    }

    info!("shutting down");
    if let Err(err) = publisher.shutdown().await {
        warn!("clean shutdown failed: {err:#}");
    }
    // Give the event loop a moment to flush the offline message and disconnect.
    tokio::time::sleep(Duration::from_millis(500)).await;
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
    screen: Option<Screen>,
    badges: Option<Badges>,
    /// Unix seconds of the last badges publish.
    badges_at: u64,
}

impl Shown {
    async fn publish(&mut self, publisher: &Publisher, composer: &mut Composer, now: u64) {
        // `seq` is set by the publisher, so compare with it zeroed.
        let screen = composer.screen(now);
        if self.screen.as_ref() != Some(&screen) {
            match publisher.publish_screen(screen.clone()).await {
                Ok(()) => self.screen = Some(screen),
                Err(err) => warn!("cannot publish screen: {err:#}"),
            }
        }
        let badges = composer.badges();
        let due = now.saturating_sub(self.badges_at) >= BADGES_REFRESH_S;
        if self.badges.as_ref() != Some(&badges) || due {
            match publisher.publish_badges(&badges).await {
                Ok(()) => {
                    self.badges = Some(badges);
                    self.badges_at = now;
                }
                Err(err) => warn!("cannot publish badges: {err:#}"),
            }
        }
    }
}
