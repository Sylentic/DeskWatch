//! The MQTT task: connect, subscribe to the bridge's retained topics, hand
//! what arrives to the display task, and start over whenever anything breaks.
//!
//! rust-mqtt has no reconnect logic and no keep-alive timer of its own, so
//! this loop owns both. NOT TESTED ON HARDWARE or against a live broker.

use core::fmt::Write as _;
use core::net::Ipv4Addr;

use embassy_futures::select::{Either, select};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_time::{Duration, Timer};
use heapless::{String, Vec};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::AllocBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::event::Event;
use rust_mqtt::client::options::{
    ConnectOptions, PublicationOptions, RetainHandling, SubscriptionOptions, TopicReference,
    WillOptions,
};
use rust_mqtt::config::KeepAlive;
use rust_mqtt::types::{MqttBinary, MqttString, TopicFilter, TopicName};

use deskwatch_ui::Topic;

use crate::config::Config;

/// One MQTT message for the display task. Payloads stay under 1 KB (schema
/// section 4), anything longer is dropped with a log line.
pub struct Message {
    pub topic: Topic,
    pub payload: Vec<u8, 1024>,
}

/// Wait before trying again after a failed connection.
const RETRY: Duration = Duration::from_secs(5);

type MqttClient<'a> = Client<'a, 'a, TcpSocket<'a>, AllocBuffer, 3, 4, 2, 1, 4, 0, 0>;

#[embassy_executor::task]
pub async fn run(
    stack: Stack<'static>,
    cfg: Config,
    out: Sender<'static, CriticalSectionRawMutex, Message, 4>,
) {
    loop {
        stack.wait_config_up().await;
        match session(stack, &cfg, &out).await {
            Ok(()) => log::warn!("mqtt: session ended"),
            Err(e) => log::warn!("mqtt: {e}"),
        }
        Timer::after(RETRY).await;
    }
}

/// One connection: returns when it breaks, with a short reason for the log.
async fn session(
    stack: Stack<'static>,
    cfg: &Config,
    out: &Sender<'static, CriticalSectionRawMutex, Message, 4>,
) -> Result<(), &'static str> {
    let addr = resolve(stack, cfg.mqtt_host.as_str()).await?;
    log::info!("mqtt: connecting to {}:{}", addr, cfg.mqtt_port);

    let mut rx = [0u8; 1024];
    let mut tx = [0u8; 1024];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(
        u64::from(cfg.keep_alive_s) * 3 / 2 + 5,
    )));
    socket
        .connect(IpEndpoint::new(addr.into(), cfg.mqtt_port))
        .await
        .map_err(|_| "tcp connect failed")?;

    // Topics, built once per session from the configured prefix.
    let topic = |suffix: &str| -> Result<String<64>, &'static str> {
        let mut s = String::new();
        write!(s, "{}/{}", cfg.topic_prefix, suffix).map_err(|_| "topic too long")?;
        Ok(s)
    };
    let status_topic = topic("panel/status")?;

    let mut buffer = AllocBuffer;
    let mut client: MqttClient<'_> = Client::new(&mut buffer);

    // Last Will: the broker publishes "offline" if the panel disappears.
    let will_topic = TopicName::new(
        MqttString::from_str(status_topic.as_str()).map_err(|_| "bad status topic")?,
    )
    .ok_or("bad status topic")?;
    let will_payload = MqttBinary::try_from(b"offline".as_slice()).map_err(|_| "will payload")?;

    let mut options = ConnectOptions::new()
        .clean_start()
        .keep_alive(KeepAlive::Seconds(
            core::num::NonZero::new(cfg.keep_alive_s.max(5)).unwrap(),
        ))
        .will(
            WillOptions::new(will_topic, will_payload)
                .at_least_once()
                .retain(),
        );
    if !cfg.mqtt_username.is_empty() {
        options = options
            .user_name(
                MqttString::from_str(cfg.mqtt_username.as_str()).map_err(|_| "bad username")?,
            )
            .password(
                MqttBinary::try_from(cfg.mqtt_password.as_bytes()).map_err(|_| "bad password")?,
            );
    }

    client
        .connect(
            socket,
            &options,
            Some(MqttString::from_str(cfg.mqtt_client_id.as_str()).map_err(|_| "bad client id")?),
        )
        .await
        .map_err(|_| "broker refused the connection (check host, login and ACL)")?;
    log::info!("mqtt: connected");

    // Announce ourselves (retained, replaced by the will on a crash).
    publish_status(&mut client, &status_topic, b"online").await?;

    // Subscribe one topic at a time and wait for each SUBACK, because rust-mqtt
    // sends one topic per SUBSCRIBE packet. Retained messages follow at once.
    for (_, suffix) in Topic::ALL {
        let filter = topic(suffix)?;
        let filter = MqttString::from_str(filter.as_str()).map_err(|_| "bad topic")?;
        let filter = TopicFilter::new(filter).ok_or("bad topic filter")?;
        let options = SubscriptionOptions::new()
            .at_least_once()
            .retain_handling(RetainHandling::AlwaysSend);
        client
            .subscribe(filter, &options)
            .await
            .map_err(|_| "subscribe failed")?;
        loop {
            match client
                .poll()
                .await
                .map_err(|_| "connection lost while subscribing")?
            {
                Event::Suback(s) => {
                    log::info!(
                        "mqtt: subscribed to {}/{} ({:?})",
                        cfg.topic_prefix,
                        suffix,
                        s.reason_code
                    );
                    break;
                }
                // A retained message can overtake the SUBACK on some brokers.
                Event::Publish(p) => forward(cfg, out, &p.topic, p.message.as_ref()).await,
                _ => {}
            }
        }
    }

    // Main loop: wait for a packet header or the keep-alive timer, whichever
    // comes first. `poll_header` is cancel-safe, `poll` is not.
    let ping_every = Duration::from_secs(u64::from(cfg.keep_alive_s.max(5)) / 2);
    loop {
        match select(client.poll_header(), Timer::after(ping_every)).await {
            Either::First(header) => {
                let header = header.map_err(|_| "connection lost")?;
                let event = client
                    .poll_body(header)
                    .await
                    .map_err(|_| "bad packet from broker")?;
                if let Event::Publish(p) = event {
                    forward(cfg, out, &p.topic, p.message.as_ref()).await;
                }
            }
            Either::Second(()) => client.ping().await.map_err(|_| "ping failed")?,
        }
    }
}

/// Match a received message to a panel topic and queue it for the display.
async fn forward(
    cfg: &Config,
    out: &Sender<'static, CriticalSectionRawMutex, Message, 4>,
    topic: &TopicReference<'_>,
    payload: &[u8],
) {
    let Some(name) = topic.name() else { return };
    let Some(topic) = Topic::from_topic(cfg.topic_prefix.as_str(), name.as_ref().as_str()) else {
        return;
    };
    log::info!("mqtt: {} bytes on {}", payload.len(), topic.suffix());
    if let Ok(payload) = Vec::from_slice(payload) {
        out.send(Message { topic, payload }).await;
    } else {
        log::warn!("mqtt: dropped a payload over 1 KB on {}", topic.suffix());
    }
}

async fn publish_status(
    client: &mut MqttClient<'_>,
    topic: &String<64>,
    payload: &[u8],
) -> Result<(), &'static str> {
    let name = TopicName::new(MqttString::from_str(topic.as_str()).map_err(|_| "bad topic")?)
        .ok_or("bad topic")?;
    let options = PublicationOptions::new(TopicReference::Name(name))
        .at_least_once()
        .retain();
    client
        .publish(&options, Bytes::from(payload))
        .await
        .map_err(|_| "status publish failed")?;
    Ok(())
}

/// The broker is an IPv4 address or a DNS name.
async fn resolve(stack: Stack<'static>, host: &str) -> Result<Ipv4Addr, &'static str> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(ip);
    }
    let found = stack
        .dns_query(host, DnsQueryType::A)
        .await
        .map_err(|_| "could not resolve the broker name")?;
    found
        .iter()
        .find_map(|ip| match ip {
            IpAddress::Ipv4(v4) => Some(*v4),
            #[allow(unreachable_patterns)]
            _ => None,
        })
        .ok_or("broker name has no IPv4 address")
}
