use super::*;
use axum::Router;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use tower::ServiceExt;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn local_addresses() {
    for local in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.9",
        "172.31.255.1",
        "192.168.1.20",
        "169.254.1.1",
        "100.64.0.1",
        "100.127.255.254",
        "::1",
        "fd12:3456::1",
        "fe80::1",
        "::ffff:192.168.1.5",
    ] {
        assert!(is_local(ip(local)), "{local} should be local");
    }
    for public in [
        "8.8.8.8",
        "172.32.0.1",
        "100.128.0.1",
        "100.63.255.255",
        "203.0.113.9",
        "2001:db8::1",
        "::ffff:8.8.8.8",
    ] {
        assert!(!is_local(ip(public)), "{public} should not be local");
    }
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in pairs {
        map.insert(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
    }
    map
}

#[test]
fn proxy_headers_decide_the_client() {
    let proxy = ip("192.168.1.2");
    assert_eq!(client_ip(proxy, &headers(&[])), Some(proxy));
    // The last entry is the one the proxy added; earlier ones are the client's claim.
    let spoof = headers(&[("x-forwarded-for", "192.168.1.5, 203.0.113.9")]);
    assert_eq!(client_ip(proxy, &spoof), Some(ip("203.0.113.9")));
    let lan = headers(&[("x-forwarded-for", "192.168.1.77")]);
    assert_eq!(client_ip(proxy, &lan), Some(ip("192.168.1.77")));
    // Unreadable or unknown forwarding headers are not trusted.
    assert_eq!(
        client_ip(proxy, &headers(&[("x-forwarded-for", "nope")])),
        None
    );
    assert_eq!(
        client_ip(proxy, &headers(&[("forwarded", "for=1.2.3.4")])),
        None
    );
    assert_eq!(
        client_ip(proxy, &headers(&[("x-real-ip", "1.2.3.4")])),
        None
    );
}

#[test]
fn a_public_peer_cannot_claim_a_forwarded_address() {
    // No proxy in front: the peer is the client, its headers are its own claim.
    let public = ip("203.0.113.9");
    for hdrs in [
        [("x-forwarded-for", "192.168.1.5")],
        [("x-forwarded-for", "nope")],
        [("forwarded", "for=10.0.0.1")],
        [("x-real-ip", "127.0.0.1")],
    ] {
        assert_eq!(client_ip(public, &headers(&hdrs)), Some(public), "{hdrs:?}");
    }
}

async fn call(allow_public: bool, peer: Option<&str>, hdrs: &[(&str, &str)]) -> StatusCode {
    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(from_fn_with_state(Access::new(allow_public), require_local));
    let mut req = HttpRequest::builder().uri("/x");
    for (k, v) in hdrs {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::empty()).unwrap();
    if let Some(peer) = peer {
        let addr = SocketAddr::new(ip(peer), 40000);
        req.extensions_mut().insert(ConnectInfo(addr));
    }
    app.oneshot(req).await.unwrap().status()
}

#[test]
fn rebound_host_names_are_refused() {
    for ok in [
        "192.168.1.5:8787",
        "[::1]:8787",
        "localhost",
        "deskwatch",
        "desk.lan:8787",
        "Desk.Local",
        "",
    ] {
        assert!(host_is_ours(ok, &[]), "{ok}");
    }
    for bad in ["evil.example", "evil.example:8787", "a.lan.evil.com"] {
        assert!(!host_is_ours(bad, &[]), "{bad}");
    }
    assert!(host_is_ours(
        "Dash.example.com:443",
        &["dash.example.com".into()]
    ));
}

#[tokio::test]
async fn middleware_allows_local_and_refuses_the_rest() {
    assert_eq!(call(false, Some("192.168.1.5"), &[]).await, StatusCode::OK);
    assert_eq!(call(false, Some("127.0.0.1"), &[]).await, StatusCode::OK);
    assert_eq!(
        call(false, Some("8.8.8.8"), &[]).await,
        StatusCode::FORBIDDEN
    );
    // A proxy on the LAN that forwards a public client.
    let public = [("x-forwarded-for", "203.0.113.9")];
    assert_eq!(
        call(false, Some("192.168.1.2"), &public).await,
        StatusCode::FORBIDDEN
    );
    // A public client with no proxy in front, claiming a LAN address.
    let claim = [("x-forwarded-for", "192.168.1.5")];
    assert_eq!(
        call(false, Some("203.0.113.9"), &claim).await,
        StatusCode::FORBIDDEN
    );
    // No peer address at all: refused (fail closed).
    assert_eq!(call(false, None, &[]).await, StatusCode::FORBIDDEN);
    // The setting opens it.
    assert_eq!(call(true, Some("8.8.8.8"), &[]).await, StatusCode::OK);
}

#[test]
fn allow_public_is_off_unless_set() {
    use crate::config::Config;
    assert!(!Config::from_toml("").unwrap().http.allow_public);
    let on = Config::from_toml("[http]\nallow_public = true\n").unwrap();
    assert!(on.http.allow_public);
}

#[tokio::test]
async fn a_local_client_with_a_public_host_name_is_refused() {
    let local = Some("192.168.1.5");
    assert_eq!(
        call(false, local, &[("host", "evil.example")]).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(false, local, &[("host", "192.168.1.9:8787")]).await,
        StatusCode::OK
    );
    // With `allow_public`, or once a login exists, the check steps aside.
    assert_eq!(
        call(true, local, &[("host", "evil.example")]).await,
        StatusCode::OK
    );
}
