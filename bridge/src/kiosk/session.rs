//! Login sessions for the dashboard.
//!
//! The browser trades the access token once (`POST /api/login`) for a cookie,
//! so the token never has to travel in an address. The cookie holds no server
//! state: it is `<expiry>.<hex HMAC-SHA256 of the expiry>`, signed with a key
//! derived from the token. Two things follow from that:
//!
//! - restarting the bridge keeps everybody logged in;
//! - changing the token file changes the key, which logs everybody out.
//!
//! This module only knows about cookies, origins and the failure limit; the
//! routes live in `kiosk.rs`.

use std::collections::VecDeque;
use std::sync::Mutex;

use axum::http::{HeaderMap, header};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

/// Name of the dashboard's session cookie, and the setup page's.
pub const COOKIE: &str = "deskwatch_session";
pub const SETUP_COOKIE: &str = "deskwatch_setup";

/// Wrong tokens allowed per `WINDOW_S` before logins are refused for a while.
pub const MAX_FAILURES: usize = 5;
pub const WINDOW_S: u64 = 60;

/// Signs and checks session cookies.
pub struct Sessions {
    key: [u8; 32],
    lifetime_s: u64,
}

impl Sessions {
    /// `purpose` keeps the keys of different pages apart: a cookie signed for
    /// the dashboard is worthless on the setup page even if both used the
    /// same token.
    pub fn new(purpose: &str, token: &str, lifetime_s: u64) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"deskwatch-session-v1\0");
        hasher.update(purpose.as_bytes());
        hasher.update(b"\0");
        hasher.update(token.as_bytes());
        Self {
            key: hasher.finalize().into(),
            lifetime_s,
        }
    }

    pub fn lifetime_s(&self) -> u64 {
        self.lifetime_s
    }

    fn mac(&self) -> Hmac<Sha256> {
        Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC takes a key of any length")
    }

    /// A new cookie value, valid until `now` plus the lifetime.
    pub fn issue(&self, now: u64) -> String {
        let expiry = (now + self.lifetime_s).to_string();
        let mut mac = self.mac();
        mac.update(expiry.as_bytes());
        format!("{expiry}.{}", hex::encode(mac.finalize().into_bytes()))
    }

    /// Is this a cookie value this bridge signed with the current token, not expired?
    pub fn valid(&self, value: &str, now: u64) -> bool {
        let Some((expiry, signature)) = value.split_once('.') else {
            return false;
        };
        let (Ok(expires), Ok(signature)) = (expiry.parse::<u64>(), hex::decode(signature)) else {
            return false;
        };
        let mut mac = self.mac();
        mac.update(expiry.as_bytes());
        mac.verify_slice(&signature).is_ok() && expires > now
    }
}

/// The value of cookie `name` in a request, if it has one.
pub fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|line| line.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
}

/// The `Set-Cookie` header for a new session. `Secure` only when the request
/// came in over HTTPS (a proxy says so in `X-Forwarded-Proto`), because a
/// Secure cookie would never come back over plain HTTP.
pub fn set_cookie(name: &str, value: &str, max_age_s: u64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{name}={value}; Max-Age={max_age_s}; Path=/; HttpOnly; SameSite=Strict{secure}")
}

/// Did the request come in over HTTPS, according to the proxy in front?
pub fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .next()
                .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
        })
}

/// Is the request from the page itself? A browser sends `Origin` on
/// WebSocket upgrades and on POSTs; if it names another site than `Host`,
/// the request was started by some other page (cross-site WebSocket
/// hijacking, cross-site login). No `Origin` means a script or `curl`,
/// which are not the cookie's concern and are allowed.
pub fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    // Behind a proxy the browser's address is in X-Forwarded-Host when the
    // proxy does not pass Host on. A page on another site cannot set either.
    let hosts = [header::HOST.as_str(), "x-forwarded-host"]
        .into_iter()
        .filter_map(|name| headers.get(name)?.to_str().ok());
    let Some(authority) = origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://"))
        .map(|(_, rest)| rest)
    else {
        return false; // "null" and other odd values
    };
    hosts
        .into_iter()
        .any(|host| authority.eq_ignore_ascii_case(host.trim()))
}

/// Remembers recent wrong tokens. Global on purpose: the peer address cannot
/// be trusted behind a proxy or Docker, so the limit cannot be per client.
/// The price is that someone who keeps guessing also stops the owner from
/// logging in for a minute; sessions already open are not affected.
#[derive(Default)]
pub struct Limiter {
    failures: Mutex<VecDeque<u64>>,
}

impl Limiter {
    /// `Some(seconds to wait)` when too many wrong tokens came in lately.
    pub fn blocked(&self, now: u64) -> Option<u64> {
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        while failures.front().is_some_and(|t| t + WINDOW_S <= now) {
            failures.pop_front();
        }
        (failures.len() >= MAX_FAILURES).then(|| {
            failures
                .front()
                .map_or(WINDOW_S, |t| (t + WINDOW_S).saturating_sub(now).max(1))
        })
    }

    pub fn record_failure(&self, now: u64) {
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        failures.push_back(now);
    }
}
