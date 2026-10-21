//! MQTT side of the bridge: connection, Last Will, and retained publishing.
//!
//! The bridge announces itself on `<prefix>/bridge/status` with a retained
//! `online`, and registers a retained `offline` Last Will so Home Assistant and
//! other listeners learn when the bridge dies.
//!
//! Incoming: alerts from Home Assistant or scripts on `<prefix>/alert`.

use std::time::Duration;

use anyhow::{Context, Result};
use rumqttc::{
    AsyncClient, Event, EventLoop, LastWill, MqttOptions, Packet, QoS, TlsConfiguration, Transport,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::alerts::AlertMessage;
use crate::config::MqttConfig;
use crate::health::MqttLink;
use crate::source::{Secret, load_secret};

/// Payloads for the status topic.
const ONLINE: &str = "online";
const OFFLINE: &str = "offline";

/// Wait before retrying after a broker connection error.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Topic names under the configured prefix (schema section 4).
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

    pub fn bridge_status(&self) -> String {
        format!("{}/bridge/status", self.prefix)
    }

    pub fn alert(&self) -> String {
        format!("{}/alert", self.prefix)
    }
}

/// A message for the main loop, from an alert publisher.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Alert(AlertMessage),
}

/// Publishes the bridge status. Cheap to clone.
#[derive(Clone)]
pub struct Publisher {
    client: AsyncClient,
    topics: Topics,
}

/// Build the client and its event loop. Nothing connects until the event loop
/// is polled, which `run_event_loop` does. `password` is used with
/// `config.username`. Fails only on unreadable TLS files.
pub fn connect(config: &MqttConfig, password: Option<Secret>) -> Result<(Publisher, EventLoop)> {
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

    if config.tls {
        options.set_transport(Transport::tls_with_config(tls_config(config)?));
    }

    // Capacity 16 is plenty: the bridge sends a few messages every few seconds.
    let (client, eventloop) = AsyncClient::new(options, 16);
    let publisher = Publisher { client, topics };
    Ok((publisher, eventloop))
}

/// TLS settings: the platform's trusted roots, or the CA in `ca_file`, plus an
/// optional client certificate for brokers that require one.
fn tls_config(config: &MqttConfig) -> Result<TlsConfiguration> {
    let Some(ca_file) = &config.ca_file else {
        anyhow::ensure!(
            config.client_cert_file.is_none(),
            "mqtt.client_cert_file needs mqtt.ca_file"
        );
        return Ok(TlsConfiguration::default());
    };
    let ca = std::fs::read(ca_file)
        .with_context(|| format!("cannot read mqtt.ca_file {}", ca_file.display()))?;
    let client_auth = match (&config.client_cert_file, &config.client_key_file) {
        (Some(cert), Some(key)) => {
            let cert = std::fs::read(cert)
                .with_context(|| format!("cannot read mqtt.client_cert_file {}", cert.display()))?;
            let key = load_secret(key).context("mqtt.client_key_file")?;
            Some((cert, key.expose().as_bytes().to_vec()))
        }
        _ => None,
    };
    Ok(TlsConfiguration::Simple {
        ca,
        alpn: None,
        client_auth,
    })
}

/// The broker password: `mqtt.password_file` if set, else the environment.
pub fn password(config: &MqttConfig) -> Result<Option<Secret>> {
    if let Some(file) = &config.password_file {
        return load_secret(file).context("mqtt.password_file").map(Some);
    }
    Ok(std::env::var(crate::config::MQTT_PASSWORD_ENV)
        .ok()
        .filter(|p| !p.is_empty())
        .map(Secret::new))
}

/// Try to connect and log in with `config`, then disconnect: the setup
/// page's connection test. It uses its own client id (the same id would make
/// the broker drop the bridge's live connection) and sets no Last Will (an
/// abrupt end would otherwise mark the bridge offline). Says nothing about
/// what the login may publish or subscribe to.
pub async fn probe(config: &MqttConfig, password: Option<Secret>) -> Result<()> {
    let id = format!("{}-setup-test", config.client_id);
    let mut options = MqttOptions::new(id, &config.host, config.port);
    options.set_keep_alive(Duration::from_secs(config.keep_alive_s));
    if let Some(username) = &config.username {
        let password = password.as_ref().map(Secret::expose).unwrap_or_default();
        options.set_credentials(username, password);
    }
    if config.tls {
        options.set_transport(Transport::tls_with_config(tls_config(config)?));
    }
    let (client, mut eventloop) = AsyncClient::new(options, 4);
    let connected = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => return Ok(()),
                Ok(_) => {}
                Err(err) => return Err(anyhow::Error::new(err)),
            }
        }
    })
    .await;
    let _ = client.try_disconnect();
    // Let the DISCONNECT go out, so the broker sees a clean end.
    let _ = tokio::time::timeout(Duration::from_millis(500), eventloop.poll()).await;
    connected.map_err(|_| anyhow::anyhow!("no answer from the broker within 8 seconds"))?
}

impl Publisher {
    /// Mark the bridge offline and disconnect cleanly (used on shutdown; a
    /// crash is covered by the Last Will instead).
    pub async fn shutdown(&self) -> Result<()> {
        self.client
            .publish(self.topics.bridge_status(), QoS::AtLeastOnce, true, OFFLINE)
            .await?;
        self.client.disconnect().await?;
        Ok(())
    }
}

/// Drive the MQTT connection forever: reconnects on errors, announces
/// `online` after every (re)connect, and forwards alerts (when `alerts` is
/// true) to `inbound` for the composer.
/// `link` tells `/healthz` whether the broker connection is up.
pub async fn run_event_loop(
    mut eventloop: EventLoop,
    publisher: Publisher,
    inbound: mpsc::Sender<Inbound>,
    alerts: bool,
    link: MqttLink,
) {
    let topics = publisher.topics.clone();
    let mut subscriptions = Vec::new();
    if alerts {
        subscriptions.push(topics.alert());
    }
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("connected to MQTT broker");
                link.set(true);
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
            Ok(Event::Incoming(Packet::Disconnect)) => link.set(false),
            Ok(event) => debug!(?event, "mqtt"),
            Err(err) => {
                link.set(false);
                warn!("MQTT connection error: {err}, retrying in {RECONNECT_DELAY:?}");
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

/// Handle an incoming message: return the alert on the alert topic.
fn handle_incoming(topics: &Topics, topic: &str, payload: &[u8]) -> Option<Inbound> {
    if topic == topics.alert() {
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
        assert_eq!(topics.bridge_status(), "deskpanel/bridge/status");
        assert_eq!(topics.alert(), "deskpanel/alert");
    }

    #[test]
    fn tls_with_missing_ca_file_is_an_error() {
        let config = MqttConfig {
            tls: true,
            ca_file: Some("/nonexistent/ca.pem".into()),
            ..MqttConfig::default()
        };
        let err = connect_err(&config);
        assert!(err.contains("mqtt.ca_file"), "{err}");
    }

    fn connect_err(config: &MqttConfig) -> String {
        format!("{:#}", tls_config(config).unwrap_err())
    }

    #[test]
    fn incoming_messages_are_routed_by_topic() {
        let topics = Topics::new("deskpanel");
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
            handle_incoming(&topics, "deskpanel/bridge/status", b"online"),
            None
        );
    }
}
