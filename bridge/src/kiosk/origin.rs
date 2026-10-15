//! The `Origin` check shared by the dashboard and the setup page.
//!
//! Neither page has a login (they are for a trusted network). What a browser
//! still must not do is let some *other* site's page talk to them: a
//! WebSocket is not covered by the same-origin rules, and a POST from another
//! site could change the setup. So a request whose `Origin` names another
//! host than the one it was sent to is refused.

use axum::http::{HeaderMap, header};

/// Is the request from the page itself? A browser sends `Origin` on
/// WebSocket upgrades and on POSTs; if it names another site than `Host`,
/// the request was started by some other page (cross-site WebSocket
/// hijacking, a cross-site change of the setup). No `Origin` means a script or `curl`,
/// which are not a browser's cross-site problem and are allowed.
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
