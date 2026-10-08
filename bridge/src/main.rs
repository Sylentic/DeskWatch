//! DeskWatch bridge: collects data on the home server and tells the desk panel
//! what to draw over MQTT. See `mqtt-schema.md` (schema v2) for the contract.
//!
//! This first version publishes the server stats page on a fixed interval.
//! CI sources (Gitea, GitHub, Azure DevOps) and the full composer loop that
//! uses `model::pick` and `model::Rotation` come in later changes.

use std::time::Duration;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use deskwatch_bridge::config::Config;
use deskwatch_bridge::model::{Badges, Level, Rotation, Screen, ScreenData};
use deskwatch_bridge::mqtt;
use deskwatch_bridge::stats::StatsCollector;

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

    let (publisher, eventloop) = mqtt::connect(&config.mqtt);
    tokio::spawn(mqtt::run_event_loop(eventloop, publisher.clone()));

    // No badge sources yet: publish an empty strip so a stale retained one is cleared.
    publisher.publish_badges(&Badges::new([])).await?;

    let mut collector = StatsCollector::new(&config.server);
    let rotation = Rotation::new(config.rotation.clone());
    let mut ticker = tokio::time::interval(Duration::from_secs(config.server.interval_s));

    let mut sigterm = signal(SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let data = collector.collect();
                let mut screen = Screen::new(Level::Rotation, "stats", ScreenData::Stats(data));
                // Only the stats page has data so far, so every skippable page counts as empty.
                screen.position = rotation.position(|page| page != "stats");
                screen.pinned = rotation.is_pinned();
                if let Err(err) = publisher.publish_screen(screen).await {
                    warn!("cannot publish stats: {err:#}");
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        }
    }

    info!("shutting down");
    if let Err(err) = publisher.shutdown().await {
        warn!("clean shutdown failed: {err:#}");
    }
    // Give the event loop a moment to flush the offline message and disconnect.
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}
