//! MQTT side of the bridge: connection, Last Will, and retained publishing.
//!
//! The bridge announces itself on `<prefix>/bridge/status` with a retained
//! `online`, and registers a retained `offline` Last Will so the panel learns
//! when the bridge dies. Screens and badges are published retained, so a panel
//! that (re)connects immediately gets the current page.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use rumqttc::{AsyncClient, Event, EventLoop, LastWill, MqttOptions, Packet, QoS};
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::{MQTT_PASSWORD_ENV, MqttConfig};
use crate::model::{Badges, PanelEvent, Screen};

/// Payloads for the two status topics.
const ONLINE: &str = "online";
const OFFLINE: &str = "offline";

/// Wait before retrying after a broker connection error.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Topic names under the configured prefix (schema section 5).
#[derive(Debug, Clone)]
pub struct Topics {
    prefix: String,
}

impl Topics {
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.trim_end_matches('/').to_string(),
        }
    }

    pub fn screen(&self) -> String {
        format!("{}/screen", self.prefix)
    }

    pub fn badges(&self) -> String {
        format!("{}/badges", self.prefix)
    }

    pub fn bridge_status(&self) -> String {
        format!("{}/bridge/status", self.prefix)
    }

    pub fn panel_status(&self) -> String {
        format!("{}/panel/status", self.prefix)
    }

    pub fn panel_event(&self) -> String {
        format!("{}/panel/event", self.prefix)
    }
}

/// Publishes bridge payloads. Cheap to clone; all clones share `seq`.
#[derive(Clone)]
pub struct Publisher {
    client: AsyncClient,
    topics: Topics,
    seq: std::sync::Arc<AtomicU64>,
}

/// Build the client and its event loop. Nothing connects until the event loop
/// is polled, which `run_event_loop` does.
pub fn connect(config: &MqttConfig) -> (Publisher, EventLoop) {
    let topics = Topics::new(&config.topic_prefix);

    let mut options = MqttOptions::new(&config.client_id, &config.host, config.port);
    options.set_keep_alive(Duration::from_secs(config.keep_alive_s));
    options.set_last_will(LastWill::new(
        topics.bridge_status(),
        OFFLINE,
        QoS::AtLeastOnce,
        true,
    ));
    if let Some(username) = &config.username {
        let password = std::env::var(MQTT_PASSWORD_ENV).unwrap_or_default();
        if password.is_empty() {
            warn!("mqtt.username is set but {MQTT_PASSWORD_ENV} is empty");
        }
        options.set_credentials(username, password);
    }

    // Capacity 16 is plenty: the bridge sends a few messages every few seconds.
    let (client, eventloop) = AsyncClient::new(options, 16);
    let publisher = Publisher {
        client,
        topics,
        seq: Default::default(),
    };
    (publisher, eventloop)
}

impl Publisher {
    /// Publish a screen on `<prefix>/screen`, retained, with the next `seq`.
    pub async fn publish_screen(&self, mut screen: Screen) -> Result<()> {
        screen.seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.publish_json(&self.topics.screen(), &screen).await
    }

    /// Publish the header badges on `<prefix>/badges`, retained.
    pub async fn publish_badges(&self, badges: &Badges) -> Result<()> {
        self.publish_json(&self.topics.badges(), badges).await
    }

    /// Mark the bridge offline and disconnect cleanly (used on shutdown; a
    /// crash is covered by the Last Will instead).
    pub async fn shutdown(&self) -> Result<()> {
        self.client
            .publish(self.topics.bridge_status(), QoS::AtLeastOnce, true, OFFLINE)
            .await?;
        self.client.disconnect().await?;
        Ok(())
    }

    async fn publish_json(&self, topic: &str, payload: &impl Serialize) -> Result<()> {
        let bytes = serde_json::to_vec(payload).context("cannot encode payload")?;
        // The schema keeps payloads under 1 KB so the panel's buffers stay small.
        if bytes.len() > 1024 {
            warn!(
                topic,
                size = bytes.len(),
                "payload is over the 1 KB schema limit"
            );
        }
        self.client
            .publish(topic, QoS::AtLeastOnce, true, bytes)
            .await
            .with_context(|| format!("cannot queue publish to {topic}"))
    }
}

/// Drive the MQTT connection forever: reconnects on errors, announces
/// `online` after every (re)connect, logs panel status, and forwards button
/// events to `panel_events` for the composer.
pub async fn run_event_loop(
    mut eventloop: EventLoop,
    publisher: Publisher,
    panel_events: mpsc::Sender<PanelEvent>,
) {
    let topics = publisher.topics.clone();
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("connected to MQTT broker");
                // try_* so the event loop never waits on its own request queue.
                let client = &publisher.client;
                if let Err(err) =
                    client.try_publish(topics.bridge_status(), QoS::AtLeastOnce, true, ONLINE)
                {
                    warn!("cannot announce online: {err}");
                }
                for topic in [topics.panel_status(), topics.panel_event()] {
                    if let Err(err) = client.try_subscribe(&topic, QoS::AtLeastOnce) {
                        warn!("cannot subscribe to {topic}: {err}");
                    }
                }
            }
            Ok(Event::Incoming(Packet::Publish(message))) => {
                if let Some(event) = handle_incoming(&topics, &message.topic, &message.payload) {
                    // try_send: never block the MQTT connection on the main loop.
                    if let Err(err) = panel_events.try_send(event) {
                        warn!("dropping panel event: {err}");
                    }
                }
            }
            Ok(event) => debug!(?event, "mqtt"),
            Err(err) => {
                warn!("MQTT connection error: {err}, retrying in {RECONNECT_DELAY:?}");
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

/// Handle a message from the panel: log status changes and return button
/// events, which the composer acts on.
fn handle_incoming(topics: &Topics, topic: &str, payload: &[u8]) -> Option<PanelEvent> {
    if topic == topics.panel_status() {
        info!(
            "panel is {}",
            String::from_utf8_lossy(payload).trim().to_lowercase()
        );
    } else if topic == topics.panel_event() {
        match serde_json::from_slice::<PanelEvent>(payload) {
            Ok(event) => {
                info!(?event, "panel event");
                return Some(event);
            }
            Err(err) => warn!("ignoring malformed panel event: {err}"),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_follow_schema() {
        let topics = Topics::new("deskpanel/");
        assert_eq!(topics.screen(), "deskpanel/screen");
        assert_eq!(topics.badges(), "deskpanel/badges");
        assert_eq!(topics.bridge_status(), "deskpanel/bridge/status");
        assert_eq!(topics.panel_status(), "deskpanel/panel/status");
        assert_eq!(topics.panel_event(), "deskpanel/panel/event");
    }
}
