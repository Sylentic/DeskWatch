//! `deskwatch-bridge --healthcheck`: the probe behind the Docker `HEALTHCHECK`.
//!
//! The image has no curl or shell, so the bridge checks itself. It asks its own
//! HTTP listener over loopback, which exposes nothing sensitive: the kiosk page
//! file carries no data and needs no token, and `/healthz` only says `ok` or
//! `mqtt down`. With the kiosk page on, `/` must answer 200; with MQTT on,
//! `/healthz` must answer 200, which it does only while the broker connection is
//! up; with only a webhook source, any HTTP answer counts. A bridge with no
//! listener at all (no kiosk, no Gitea source, no MQTT) has nothing to probe and
//! is reported healthy; startup problems already end the process.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::Config;

/// Longest the probe waits for a connection plus the answer.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Whether the MQTT connection is up right now. The MQTT event loop sets it,
/// `/healthz` reports it. Cheap to clone; all clones share the flag.
#[derive(Clone, Default)]
pub struct MqttLink(Arc<AtomicBool>);

impl MqttLink {
    pub fn set(&self, up: bool) {
        self.0.store(up, Ordering::Relaxed);
    }

    pub fn is_up(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `GET /healthz`: 200 `ok` while connected, else 503 `mqtt down`. Says
    /// nothing else, so it needs no token.
    pub fn route(&self) -> Router {
        let link = self.clone();
        Router::new().route(
            "/healthz",
            get(move || async move {
                if link.is_up() {
                    (StatusCode::OK, "ok")
                } else {
                    (StatusCode::SERVICE_UNAVAILABLE, "mqtt down")
                }
            }),
        )
    }
}

/// What the probe found out about the listener.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The listener answered as expected.
    Healthy,
    /// No kiosk page and no webhook source, so no listener is expected.
    NoListener,
}

/// Probe the listener that `config` says should be open. `Err` means unhealthy.
pub async fn check(config: &Config) -> Result<Outcome> {
    let kiosk = config.kiosk.enabled;
    let mqtt = config.mqtt.enabled;
    if !kiosk && !mqtt && config.source.gitea.is_empty() {
        return Ok(Outcome::NoListener);
    }
    let listen = config.http.listen;
    let timeout = "no answer from the HTTP listener within 3 seconds";
    let status = tokio::time::timeout(TIMEOUT, http_get(listen, "/"))
        .await
        .context(timeout)??;
    if kiosk && status != 200 {
        bail!("the dashboard page answered HTTP {status}, expected 200");
    }
    if mqtt {
        let status = tokio::time::timeout(TIMEOUT, http_get(listen, "/healthz"))
            .await
            .context(timeout)??;
        if status != 200 {
            bail!("the MQTT broker connection is down (/healthz answered HTTP {status})");
        }
    }
    Ok(Outcome::Healthy)
}

/// `GET <path>` on the listener, via loopback when it listens on all
/// addresses. The `Host` header is the address the probe connects to, so a
/// setup that checks it sees a real authority. Returns the HTTP status code.
async fn http_get(listen: SocketAddr, path: &str) -> Result<u16> {
    let ip = if listen.ip().is_unspecified() {
        match listen.ip() {
            IpAddr::V4(_) => IpAddr::from([127, 0, 0, 1]),
            IpAddr::V6(_) => IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]),
        }
    } else {
        listen.ip()
    };
    let addr = SocketAddr::new(ip, listen.port());
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("cannot connect to {addr}"))?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
    // The status line is all that matters: "HTTP/1.0 200 OK".
    let mut head = [0u8; 32];
    let mut len = 0;
    while len < head.len() {
        let n = stream.read(&mut head[len..]).await?;
        if n == 0 {
            break;
        }
        len += n;
        if head[..len].contains(&b'\r') {
            break;
        }
    }
    let line = String::from_utf8_lossy(&head[..len]);
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next().map(str::parse::<u16>)) {
        (Some(proto), Some(Ok(status))) if proto.starts_with("HTTP/") => Ok(status),
        _ => bail!("{addr} did not answer with HTTP"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use tokio::net::TcpListener;

    async fn serve(router: Router) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        addr
    }

    fn config(listen: SocketAddr, extra: &str) -> Config {
        // MQTT is off unless a test turns it on, so the probe has one thing to check.
        Config::from_toml(&format!(
            "[http]\nlisten = \"{listen}\"\n[mqtt]\nenabled = false\n{extra}"
        ))
        .unwrap()
    }

    fn mqtt_on(listen: SocketAddr) -> Config {
        Config::from_toml(&format!("[http]\nlisten = \"{listen}\"\n")).unwrap()
    }

    #[tokio::test]
    async fn no_listener_expected_is_healthy() {
        let config = config("127.0.0.1:1".parse().unwrap(), "");
        assert_eq!(check(&config).await.unwrap(), Outcome::NoListener);
    }

    #[tokio::test]
    async fn kiosk_page_answering_200_is_healthy() {
        let addr = serve(Router::new().route("/", get(|| async { "page" }))).await;
        let config = config(addr, "[kiosk]\nenabled = true\n");
        assert_eq!(check(&config).await.unwrap(), Outcome::Healthy);
    }

    #[tokio::test]
    async fn kiosk_page_answering_404_is_unhealthy() {
        let addr = serve(Router::new()).await;
        let config = config(addr, "[kiosk]\nenabled = true\n");
        let err = format!("{:#}", check(&config).await.unwrap_err());
        assert!(err.contains("404"), "{err}");
    }

    #[tokio::test]
    async fn closed_port_is_unhealthy() {
        // Bind and drop to find a port nobody listens on.
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = free.local_addr().unwrap();
        drop(free);
        let config = config(addr, "[kiosk]\nenabled = true\n");
        assert!(check(&config).await.is_err());
    }

    #[tokio::test]
    async fn webhook_only_accepts_any_http_answer() {
        let addr = serve(Router::new()).await;
        let config = config(
            addr,
            "[[source.gitea]]\nname = \"a\"\nbase_url = \"https://g.example\"\nwebhook_secret_file = \"s\"\nrepos = []\n",
        );
        assert_eq!(check(&config).await.unwrap(), Outcome::Healthy);
    }

    #[tokio::test]
    async fn mqtt_on_is_healthy_only_while_connected() {
        let link = MqttLink::default();
        let addr = serve(link.route()).await;
        let config = mqtt_on(addr);

        let err = format!("{:#}", check(&config).await.unwrap_err());
        assert!(err.contains("503"), "{err}");

        link.set(true);
        assert_eq!(check(&config).await.unwrap(), Outcome::Healthy);

        link.set(false);
        assert!(check(&config).await.is_err());
    }

    #[tokio::test]
    async fn kiosk_and_mqtt_both_have_to_be_fine() {
        let link = MqttLink::default();
        link.set(true);
        let router = link.route().route("/", get(|| async { "page" }));
        let addr = serve(router).await;
        let config = Config::from_toml(&format!(
            "[http]\nlisten = \"{addr}\"\n[kiosk]\nenabled = true\n"
        ))
        .unwrap();
        assert_eq!(check(&config).await.unwrap(), Outcome::Healthy);
        link.set(false);
        assert!(check(&config).await.is_err());
    }

    #[tokio::test]
    async fn probe_sends_the_address_it_connects_to_as_host() {
        // A page that only answers 200 when Host is the address it listens on.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let strict = Router::new().route(
            "/",
            get(move |headers: axum::http::HeaderMap| async move {
                let host = headers.get("host").and_then(|h| h.to_str().ok());
                if host == Some(addr.to_string().as_str()) {
                    StatusCode::OK
                } else {
                    StatusCode::BAD_REQUEST
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, strict).await });
        assert_eq!(http_get(addr, "/").await.unwrap(), 200);
    }
}
