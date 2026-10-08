//! MQTT side of the simulator, behaving like the firmware will: Last Will on
//! `<prefix>/panel/status`, subscribe to the three panel topics, publish
//! button presses on `<prefix>/panel/event`.
//!
//! The connection runs on its own thread and hands messages to the window
//! loop over a channel, so a slow broker never freezes the window.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use deskwatch_ui::Topic;
use rumqttc::{Client, Event, LastWill, MqttOptions, Packet, QoS};

use crate::args::Args;

/// Env var holding the broker password, the same one the bridge reads.
const PASSWORD_ENV: &str = "DESKWATCH_MQTT_PASSWORD";

/// Wait before the next attempt after a broker connection error.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// Short or long button press, as in schema section 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Short,
    Long,
}

impl Press {
    /// The `deskpanel/panel/event` payload.
    pub fn payload(self) -> &'static str {
        match self {
            Press::Short => r#"{"v":2,"event":"button","action":"short"}"#,
            Press::Long => r#"{"v":2,"event":"button","action":"long"}"#,
        }
    }
}

/// What the window loop gets from the connection thread.
pub enum Incoming {
    Message(Topic, Vec<u8>),
    /// Connected (true) or lost the broker (false), for the log.
    Connected(bool),
}

/// A running connection: messages in, button presses out.
pub struct Link {
    pub rx: Receiver<Incoming>,
    client: Client,
    event_topic: String,
}

impl Link {
    pub fn press(&self, press: Press) {
        println!("button: {press:?}");
        if let Err(e) =
            self.client
                .try_publish(&self.event_topic, QoS::AtLeastOnce, false, press.payload())
        {
            eprintln!("could not publish button press: {e}");
        }
    }
}

pub fn connect(args: &Args) -> Result<Link> {
    let prefix = args.prefix.clone();
    let status_topic = format!("{prefix}/panel/status");

    // A distinct client id per process, so two simulators do not kick each other off.
    let client_id = format!("deskwatch-sim-{}", std::process::id());
    let mut options = MqttOptions::new(client_id, &args.host, args.port);
    options.set_keep_alive(Duration::from_secs(15));
    options.set_last_will(LastWill::new(
        &status_topic,
        "offline",
        QoS::AtLeastOnce,
        true,
    ));
    if let Some(user) = &args.user {
        options.set_credentials(user, std::env::var(PASSWORD_ENV).unwrap_or_default());
    }

    let (client, mut connection) = Client::new(options, 16);
    let (tx, rx) = mpsc::channel();
    let thread_client = client.clone();
    let broker = format!("{}:{}", args.host, args.port);

    thread::spawn(move || {
        for event in connection.iter() {
            match event {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    println!("connected to {broker}");
                    // Subscribe again on every connect: the session is not persistent.
                    for (_, suffix) in Topic::ALL {
                        let _ = thread_client
                            .try_subscribe(format!("{prefix}/{suffix}"), QoS::AtLeastOnce);
                    }
                    let _ =
                        thread_client.try_publish(&status_topic, QoS::AtLeastOnce, true, "online");
                    if tx.send(Incoming::Connected(true)).is_err() {
                        return;
                    }
                }
                Ok(Event::Incoming(Packet::Publish(p))) => {
                    let Some(topic) = Topic::from_topic(&prefix, &p.topic) else {
                        continue;
                    };
                    if tx
                        .send(Incoming::Message(topic, p.payload.to_vec()))
                        .is_err()
                    {
                        return; // window closed
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("mqtt {broker}: {e}, retrying");
                    if tx.send(Incoming::Connected(false)).is_err() {
                        return;
                    }
                    thread::sleep(RECONNECT_DELAY);
                }
            }
        }
    });

    Ok(Link {
        rx,
        client,
        event_topic: format!("{}/panel/event", args.prefix),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_payloads_match_schema() {
        assert_eq!(
            Press::Short.payload(),
            r#"{"v":2,"event":"button","action":"short"}"#
        );
        assert_eq!(
            Press::Long.payload(),
            r#"{"v":2,"event":"button","action":"long"}"#
        );
    }
}
