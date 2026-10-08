//! MQTT side of the bridge: connection, Last Will, and retained publishing.
//!
//! The bridge announces itself on `<prefix>/bridge/status` with a retained
//! `online`, and registers a retained `offline` Last Will so the panel learns
//! when the bridge dies. Screens and badges are published retained, so a panel
//! that (re)connects immediately gets the current page.
//!
//! Incoming: button presses from the panel on `<prefix>/panel/event`, and
//! alerts from Home Assistant or scripts on `<prefix>/alert`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use rumqttc::{AsyncClient, Event, EventLoop, LastWill, MqttOptions, Packet, QoS};
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::alerts::AlertMessage;
use crate::config::MqttConfig;
use crate::model::{Badges, PanelEvent, Screen};
use crate::source::Secret;

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

    pub fn alert(&self) -> String {
        format!("{}/alert", self.prefix)
    }
}

/// A message for the main loop, from the panel or from an alert publisher.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Button(PanelEvent),
    Alert(AlertMessage),
}

/// Publishes bridge payloads. Cheap to clone; all clones share `seq`.
#[derive(Clone)]
pub struct Publisher {
    client: AsyncClient,
    topics: Topics,
    seq: std::sync::Arc<AtomicU64>,
}

/// Build the client and its event loop. Nothing connects until the event loop
/// is polled, which `run_event_loop` does. `password` is used with
/// `config.username`.
pub fn connect(config: &MqttConfig, password: Option<Secret>) -> (Publisher, EventLoop) {
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
        if password.is_none() {
            warn!("mqtt.username is set but there is no password");
        }
        let password = password.as_ref().map(Secret::expose).unwrap_or_default();
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
/// events (and alerts, when `alerts` is true) to `inbound` for the composer.
pub async fn run_event_loop(
    mut eventloop: EventLoop,
    publisher: Publisher,
    inbound: mpsc::Sender<Inbound>,
    alerts: bool,
) {
    let topics = publisher.topics.clone();
    let mut subscriptions = vec![topics.panel_status(), topics.panel_event()];
    if alerts {
        subscriptions.push(topics.alert());
    }
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
                for topic in &subscriptions {
                    if let Err(err) = client.try_subscribe(topic, QoS::AtLeastOnce) {
                        warn!("cannot subscribe to {topic}: {err}");
                    }
                }
            }
            Ok(Event::Incoming(Packet::Publish(message))) => {
                if let Some(msg) = handle_incoming(&topics, &message.topic, &message.payload) {
                    // try_send: never block the MQTT connection on the main loop.
                    if let Err(err) = inbound.try_send(msg) {
                        warn!("dropping incoming message: {err}");
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

/// Handle an incoming message: log panel status changes and return button
/// events and alerts, which the composer acts on.
fn handle_incoming(topics: &Topics, topic: &str, payload: &[u8]) -> Option<Inbound> {
    if topic == topics.panel_status() {
        info!(
            "panel is {}",
            String::from_utf8_lossy(payload).trim().to_lowercase()
        );
    } else if topic == topics.panel_event() {
        match serde_json::from_slice::<PanelEvent>(payload) {
            Ok(event) => {
                info!(?event, "panel event");
                return Some(Inbound::Button(event));
            }
            Err(err) => warn!("ignoring malformed panel event: {err}"),
        }
    } else if topic == topics.alert() {
        match AlertMessage::parse(payload) {
            Ok(alert) => return Some(Inbound::Alert(alert)),
            Err(err) => warn!("ignoring alert: {err:#}"),
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
        assert_eq!(topics.alert(), "deskpanel/alert");
    }

    #[test]
    fn incoming_messages_are_routed_by_topic() {
        let topics = Topics::new("deskpanel");
        let button = br#"{"v":2,"event":"button","action":"short"}"#;
        assert!(matches!(
            handle_incoming(&topics, "deskpanel/panel/event", button),
            Some(Inbound::Button(_))
        ));
        let alert = br#"{"v":2,"id":"washer","severity":"info","title":"Washer"}"#;
        assert!(matches!(
            handle_incoming(&topics, "deskpanel/alert", alert),
            Some(Inbound::Alert(_))
        ));
        assert_eq!(
            handle_incoming(&topics, "deskpanel/alert", b"not json"),
            None
        );
        assert_eq!(
            handle_incoming(&topics, "deskpanel/panel/status", b"online"),
            None
        );
    }
}
