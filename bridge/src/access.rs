//! Who may reach the setup page and `POST /api/alert`.
//!
//! Both change things (sources, tokens, the alerts on the dashboard) and have no
//! login yet, so by default they answer only clients on the local network:
//! loopback, private (RFC 1918), link-local, unique-local (IPv6) and the
//! shared range of Tailscale and carrier-grade NAT. A client with any other
//! address gets a 403 that names the setting (`[http] allow_public = true`).
//!
//! The client is the peer of the TCP connection. Behind a reverse proxy the
//! peer is the proxy, which is usually on a private address, so a request that
//! carries forwarding headers is judged by the address the proxy saw: the
//! last entry of `X-Forwarded-For`, which is the one the nearest proxy added
//! (entries before it can be made up by the client). A forwarding header that
//! cannot be read is refused. Forwarding headers are only read from a peer
//! that is itself local: a public peer is judged by its own address.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tracing::warn;

/// Shared state of the check: the setting and a flag to log the first refusal.
#[derive(Clone)]
pub struct Access {
    allow_public: bool,
    /// Set once a login password exists: the login then protects the page,
    /// so the address check steps aside.
    login_on: Arc<AtomicBool>,
    logged: Arc<AtomicBool>,
    /// `[http] allowed_hosts`: names besides the local ones that `Host` may carry.
    allowed_hosts: Arc<Vec<String>>,
}

impl Access {
    pub fn new(allow_public: bool) -> Self {
        Self {
            allow_public,
            allowed_hosts: Arc::default(),
            login_on: Arc::new(AtomicBool::new(false)),
            logged: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Follow this flag (`Auth::enabled_flag`): while it is true every client may reach the guarded routes.
    pub fn with_login_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.login_on = flag;
        self
    }
}

impl Access {
    pub fn with_allowed_hosts(mut self, hosts: Vec<String>) -> Self {
        self.allowed_hosts = Arc::new(hosts);
        self
    }
}

/// Is `Host` a name of this install? Without it a website whose DNS name is rebound to a LAN address
/// would pass the `Origin` check, because it sends its own name in both headers. A local name is an IP
/// address, `localhost`, a name without a dot or one ending in `.lan`, `.local`, `.home.arpa` or `.internal`.
fn host_is_ours(host: &str, extra: &[String]) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
            _ => &host,
        },
    };
    name.parse::<IpAddr>().is_ok()
        || !name.contains('.')
        || [".lan", ".local", ".home.arpa", ".internal"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        || extra.iter().any(|h| h.eq_ignore_ascii_case(name))
}

/// Is this a local address (see the module comment)?
pub fn is_local(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || (a == 100 && (64..=127).contains(&b))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link local fe80::/10
        }
    }
}

/// The address the request came from, looking through one proxy. `None` when
/// a forwarding header is present but cannot be read.
///
/// Only a peer on the local network counts as a proxy. A public peer is the
/// client itself, whatever it puts in `X-Forwarded-For`: otherwise a bridge
/// reachable without a proxy would take a made-up LAN address at its word.
pub(crate) fn client_ip(peer: IpAddr, headers: &HeaderMap) -> Option<IpAddr> {
    if !is_local(peer) {
        return Some(peer);
    }
    if let Some(value) = headers.get("x-forwarded-for") {
        let last = value.to_str().ok()?.rsplit(',').next()?.trim();
        return last.parse::<IpAddr>().ok();
    }
    // Other forwarding headers: we do not read them, so do not trust the peer.
    if headers.contains_key("forwarded") || headers.contains_key("x-real-ip") {
        return None;
    }
    Some(peer)
}

/// Middleware: refuse clients that are not on the local network unless
/// `allow_public` is set. Needs `into_make_service_with_connect_info`; a
/// request without a peer address is refused.
pub async fn require_local(State(access): State<Access>, request: Request, next: Next) -> Response {
    if access.allow_public || access.login_on.load(Ordering::Relaxed) {
        return next.run(request).await;
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let client = peer.and_then(|ip| client_ip(ip, request.headers()));
    let host = request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if client.is_some_and(is_local) && host_is_ours(host, &access.allowed_hosts) {
        return next.run(request).await;
    }
    if !access.logged.swap(true, Ordering::Relaxed) {
        warn!(
            "refused a request to {} from a client or host name that is not local. \
             Set `[http] allow_public = true` (or `allowed_hosts` for a host name) only behind TLS and a proxy with a login",
            request.uri().path()
        );
    }
    (
        StatusCode::FORBIDDEN,
        "this address only answers the local network and local host names (see [http] allow_public and allowed_hosts)",
    )
        .into_response()
}

#[cfg(test)]
mod tests;
