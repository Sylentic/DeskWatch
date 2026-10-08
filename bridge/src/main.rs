//! DeskWatch bridge: collects data on the home server and tells the desk panel
//! what to draw over MQTT. See `mqtt-schema.md` (schema v2) for the contract.
//!
//! The main loop owns all state, so there are no locks. Sources and the MQTT
//! connection run as tasks and send what they learn over channels:
//!
//! - every second, and after every event, the composer picks the screen and
//!   badges; they are published only when they change;
//! - every `server.interval_s` the stats are sampled;
//! - Gitea webhooks and polls arrive as `GiteaEvent`s;
//! - button presses arrive from the MQTT task as `PanelEvent`s.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use deskwatch_bridge::composer::Composer;
use deskwatch_bridge::config::{Config, GITEA_SECRET_ENV, GITEA_TOKEN_ENV, GiteaConfig};
use deskwatch_bridge::gitea::{self, GiteaEvent, GiteaSource, JobRef};
use deskwatch_bridge::model::{Badges, Screen};
use deskwatch_bridge::mqtt::{self, Publisher};
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

    let (panel_tx, mut panel_rx) = mpsc::channel(16);
    let (publisher, eventloop) = mqtt::connect(&config.mqtt);
    tokio::spawn(mqtt::run_event_loop(eventloop, publisher.clone(), panel_tx));

    // Gitea events from the webhook server and the poller. The sender stays
    // alive here, so `recv` simply waits forever when Gitea is switched off.
    let (gitea_tx, mut gitea_rx) = mpsc::channel(64);
    let (jobs_tx, jobs_rx) = watch::channel(Vec::new());
    let mut gitea = GiteaSource::new(config.gitea.as_ref().is_some_and(|g| g.interrupt));
    if let Some(gitea_config) = &config.gitea {
        start_gitea(gitea_config, gitea_tx.clone(), jobs_rx).await?;
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
            Some(event) = gitea_rx.recv() => {
                gitea.apply(event, &mut composer.ci, unix_now());
                // Tell the poller which jobs to follow, only when that changed.
                let running = gitea.running_jobs();
                jobs_tx.send_if_modified(|jobs: &mut Vec<JobRef>| {
                    let changed = *jobs != running;
                    *jobs = running;
                    changed
                });
            }
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

/// Start the webhook server and the poller for the Gitea source.
async fn start_gitea(
    config: &GiteaConfig,
    events: mpsc::Sender<GiteaEvent>,
    jobs: watch::Receiver<Vec<JobRef>>,
) -> Result<()> {
    let secret = std::env::var(GITEA_SECRET_ENV).unwrap_or_default();
    anyhow::ensure!(
        !secret.is_empty(),
        "[gitea] is configured but {GITEA_SECRET_ENV} is empty; set it to the webhook secret"
    );
    let token = std::env::var(GITEA_TOKEN_ENV)
        .ok()
        .filter(|t| !t.is_empty());
    if token.is_none() {
        info!("{GITEA_TOKEN_ENV} not set, polling Gitea anonymously (public repos only)");
    }

    let listener = gitea::webhook::bind(config.listen).await?;
    let hook_events = events.clone();
    tokio::spawn(async move {
        if let Err(err) = gitea::webhook::serve(listener, secret.into_bytes(), hook_events).await {
            warn!("{err:#}");
        }
    });

    let api = gitea::api::HttpApi::new(&config.base_url, token).context("Gitea API")?;
    tokio::spawn(gitea::api::poll_loop(
        api,
        config.repos.clone(),
        Duration::from_secs(config.job_poll_s),
        Duration::from_secs(config.poll_s),
        jobs,
        events,
    ));
    Ok(())
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
