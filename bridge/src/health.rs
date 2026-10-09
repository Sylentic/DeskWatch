//! `deskwatch-bridge --healthcheck`: the probe behind the Docker `HEALTHCHECK`.
//!
//! The image has no curl or shell, so the bridge checks itself. It asks its own
//! HTTP listener for `/` over loopback, which opens nothing new: the kiosk page
//! file carries no data and needs no token. With the kiosk page on, the page
//! must answer 200; with only a webhook source, any HTTP answer counts. A bridge
//! with no listener at all (no kiosk, no Gitea source) has nothing to probe and
//! is reported healthy; startup problems already end the process.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::Config;

/// Longest the probe waits for a connection plus the answer.
const TIMEOUT: Duration = Duration::from_secs(3);

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
    if !kiosk && config.source.gitea.is_empty() {
        return Ok(Outcome::NoListener);
    }
    let status = tokio::time::timeout(TIMEOUT, get_root(config.http.listen))
        .await
        .context("no answer from the HTTP listener within 3 seconds")??;
    if kiosk && status != 200 {
        bail!("the dashboard page answered HTTP {status}, expected 200");
    }
    Ok(Outcome::Healthy)
}

/// `GET /` on the listener, via loopback when it listens on all addresses.
/// Returns the HTTP status code.
async fn get_root(listen: SocketAddr) -> Result<u16> {
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
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
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
        Config::from_toml(&format!("[http]\nlisten = \"{listen}\"\n{extra}")).unwrap()
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
}
