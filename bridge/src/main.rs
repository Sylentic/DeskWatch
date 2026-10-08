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
//! - button presses arrive from the MQTT task as `PanelEvent`s.

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use deskwatch_bridge::composer::Composer;
use deskwatch_bridge::config::{Config, MQTT_PASSWORD_ENV, MqttConfig};
use deskwatch_bridge::gitea::Gitea;
use deskwatch_bridge::github::Github;
use deskwatch_bridge::hooks::Hooks;
use deskwatch_bridge::model::{Badges, Screen};
use deskwatch_bridge::mqtt::{self, Publisher};
use deskwatch_bridge::source::{self, Secret, unix_now};
use deskwatch_bridge::stats::StatsCollector;

/// How often the composer re-checks timers (alert expiry, rotation dwell).
const COMPOSE_INTERVAL: Duration = Duration::from_secs(1);

/// Republish unchanged badges this often (seconds), so a broker that lost its
/// retained messages in a restart gets them back. The screen needs no such
/// refresh: stats change every few seconds.
const BADGES_REFRESH_S: u64 = 60;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let path = Config::path_from_env();
    let config = Config::load(&path)?;
    info!("loaded config from {}", path.display());

    // Build every source first: missing credentials or a taken port stop the
    // bridge here, before it connects anywhere.
    let mut hooks = Hooks::default();
    let mut gitea = Vec::new();
    for gitea_config in &config.source.gitea {
        gitea.push(Gitea::new(gitea_config, &mut hooks)?);
    }
    let mut github = Vec::new();
    for github_config in &config.source.github {
        github.push(Github::new(github_config)?);
    }
    hooks.start(config.http.listen).await?;

    let (panel_tx, mut panel_rx) = mpsc::channel(16);
    let (publisher, eventloop) = mqtt::connect(&config.mqtt, mqtt_password(&config.mqtt)?);
    tokio::spawn(mqtt::run_event_loop(eventloop, publisher.clone(), panel_tx));

    // Sources report here. The sender stays alive in this function, so `recv`
    // simply waits forever when no source is configured.
    let (source_tx, mut source_rx) = mpsc::channel(64);
    for source in gitea {
        source::spawn(source, source_tx.clone());
    }
    for source in github {
        source::spawn(source, source_tx.clone());
    }

    let mut collector = StatsCollector::new(&config.server);
    let mut composer = Composer::new(config.rotation.clone(), unix_now());
    let mut shown = Shown::default();

    let mut stats_ticker = tokio::time::interval(Duration::from_secs(config.server.interval_s));
    let mut compose_ticker = tokio::time::interval(COMPOSE_INTERVAL);
    let mut sigterm = signal(SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = stats_ticker.tick() => composer.set_stats(collector.collect()),
            _ = compose_ticker.tick() => {}
            Some(msg) = source_rx.recv() => composer.apply(msg, unix_now()),
            Some(event) = panel_rx.recv() => composer.on_button(event.action, unix_now()),
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
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
