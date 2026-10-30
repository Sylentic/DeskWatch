use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

use super::*;

const SECRET: &str = "ghp_SUPER_SECRET_VALUE";

/// A config with one GitHub source whose token file really exists, and one
/// Gitea source whose credentials do not.
fn config(dir: &std::path::Path) -> Config {
    let token = dir.join("github-token");
    std::fs::write(&token, SECRET).unwrap();
    let text = format!(
        "[[source.github]]\nname = \"main\"\ntoken_file = {:?}\nrepos = [\"o/a\"]\n\
         [[source.gitea]]\nname = \"home\"\nbase_url = \"https://git.example\"\n\
         webhook_secret_file = \"nowhere-hook\"\ntoken_file = \"nowhere-token\"\n\
         [mqtt]\nusername = \"deskwatch-bridge\"\n",
        token.to_str().unwrap()
    );
    Config::from_toml(&text).unwrap()
}

struct Temp(std::path::PathBuf);
impl Temp {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("deskwatch-setup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(temp: &Temp) -> Setup {
    Setup::new(&config(&temp.0), MqttLink::default(), None, None)
}

async fn send(
    router: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}
#[tokio::test]
async fn status_needs_no_login_and_never_carries_a_secret() {
    let temp = Temp::new("status");
    let router = setup(&temp).router();
    let (status, headers, body) = send(&router, "GET", "/api/setup/status", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(!body.contains(SECRET), "a token value reached the browser");
    assert!(!body.contains("setup-token"));

    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["mqtt"]["username"], "deskwatch-bridge");
    assert_eq!(v["mqtt"]["connected"], false);
    let sources = v["sources"].as_array().unwrap();
    let gh = sources.iter().find(|s| s["id"] == "github:main").unwrap();
    assert_eq!(gh["credentials"][0]["status"], "mounted");
    assert_eq!(gh["origin"], "bridge_toml");
    let gitea = sources.iter().find(|s| s["id"] == "gitea:home").unwrap();
    assert_eq!(gitea["credentials"][0]["status"], "missing");
    assert_eq!(gitea["credentials"][0]["required"], true);
    assert_eq!(gitea["credentials"][1]["required"], false);
}

#[tokio::test]
async fn health_and_mqtt_state_are_live() {
    let temp = Temp::new("live");
    let link = MqttLink::default();
    let setup = Setup::new(&config(&temp.0), link.clone(), None, None);
    let router = setup.router();
    let get = || async {
        let (_, _, body) = send(&router, "GET", "/api/setup/status", &[], "").await;
        serde_json::from_str::<Value>(&body).unwrap()
    };
    let before = get().await;
    let health_of = |v: &Value, id: &str| {
        v["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap()["health"]
            .clone()
    };
    assert_eq!(health_of(&before, "github:main"), "unknown");
    setup.update_health([("github:main".to_string(), "auth_failed")].into_iter());
    link.set(true);
    let after = get().await;
    assert_eq!(health_of(&after, "github:main"), "auth_failed");
    let reason_of = |v: &Value| {
        v["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "github:main")
            .unwrap()["health_reason"]
            .clone()
    };
    assert!(
        reason_of(&after).is_null(),
        "no reason until one is reported"
    );
    setup.update_reasons([("github:main".to_string(), "me/a: timed out".to_string())].into_iter());
    assert_eq!(reason_of(&get().await), "me/a: timed out");
    assert_eq!(after["mqtt"]["connected"], true);
}
#[test]
fn sources_from_the_data_folder_are_marked() {
    let base = Config::from_toml("").unwrap();
    let overlay = "[[source.prometheus]]\nname = \"extra\"\nurl = \"http://prom.example:9090\"\nhosts = [\"nas:9100\"]\n";
    let cfg = base.with_overlay(overlay).unwrap();
    let v = describe(&cfg);
    assert_eq!(v["sources"][0]["id"], "prometheus:extra");
    assert_eq!(v["sources"][0]["origin"], "data_folder");
}

#[tokio::test]
async fn the_page_is_served_and_builds_text_safely() {
    let temp = Temp::new("page");
    let router = setup(&temp).router();
    let (status, headers, body) = send(&router, "GET", "/setup", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("setup.js"));
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
    // Config text (names, addresses) must never be parsed as markup.
    assert!(!SETUP_JS.contains("innerHTML"));
    assert_eq!(
        send(&router, "GET", "/setup.js", &[], "").await.0,
        StatusCode::OK
    );
    assert_eq!(
        send(&router, "GET", "/setup.css", &[], "").await.0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn connection_tests_need_same_origin_and_a_known_source() {
    let temp = Temp::new("probe");
    let router = setup(&temp).router();
    let body = json!({ "id": "github:main" }).to_string();
    let evil = [("host", "b.lan"), ("origin", "http://evil.example")];
    assert_eq!(
        send(&router, "POST", "/api/setup/test", &evil, &body)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&router, "POST", "/api/setup/test", &[], "{}").await.0,
        StatusCode::BAD_REQUEST
    );
    let unknown = json!({ "id": "github:nope" }).to_string();
    assert_eq!(
        send(&router, "POST", "/api/setup/test", &[], &unknown)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_connection_test_runs_the_real_checks_and_one_runs_at_a_time() {
    use crate::probe::tests::{Temp as ProbeTemp, gitea_mock, serve};
    let temp = ProbeTemp::new("endpoint");
    let base = serve(gitea_mock()).await;
    let text = format!(
        "[[source.gitea]]\nname = \"home\"\nbase_url = {base:?}\nwebhook_secret_file = {}\n\
         token_file = {}\nrepos = [\"o/r\"]\n",
        temp.token("hook", "h"),
        temp.token("tok", "good-token-value")
    );
    let config = Config::from_toml(&text).unwrap();
    let setup = Setup::new(&config, MqttLink::default(), None, None);
    let router = setup.router();
    let body = json!({ "id": "gitea:home" }).to_string();

    let (status, _, text) = send(&router, "POST", "/api/setup/test", &[], &body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["ok"], true);
    assert!(
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "repository o/r")
    );
    assert!(!text.contains("good-token-value"));

    // While a test holds the permit, a second one is refused, not queued.
    let _held = setup.shared.testing.try_acquire().unwrap();
    let (status, ..) = send(&router, "POST", "/api/setup/test", &[], &body).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn the_page_can_test_a_connection() {
    assert!(SETUP_JS.contains("api/setup/test"));
    assert!(SETUP_JS.contains("Test connection"));
}

/// A setup router with a data folder, plus the folder's path.
async fn with_data(tag: &str) -> (Router, Temp) {
    let temp = Temp::new(tag);
    let data = temp.0.join("data");
    let store = Store::open(&data).unwrap();
    let config = Config::from_toml("").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let supervisor = Supervisor::new(tx, crate::hooks::Webhooks::default());
    let router = Setup::new(&config, MqttLink::default(), Some(store), Some(supervisor)).router();
    (router, temp)
}

#[tokio::test]
async fn changes_need_same_origin_and_a_data_folder() {
    let temp = Temp::new("change-guards");
    let body = json!({ "kind": "github", "name": "x", "create": true }).to_string();
    // No data folder: refused.
    let router = setup(&temp).router();
    let (status, ..) = send(&router, "POST", "/api/setup/source", &[], &body).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (router, _temp) = with_data("change-guards2").await;
    let evil = [("host", "b.lan"), ("origin", "http://evil.example")];
    for uri in ["/api/setup/source", "/api/setup/source/delete"] {
        let (status, ..) = send(&router, "POST", uri, &evil, &body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let (status, ..) = send(&router, "POST", "/api/setup/source", &[], "nope").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_good_source_is_saved_a_failing_one_is_not_unless_forced() {
    use crate::probe::tests::{gitea_mock, serve};
    let (router, temp) = with_data("save").await;
    let data = temp.0.join("data");
    let base = serve(gitea_mock()).await;
    let request = |token: &str, force: bool| {
        json!({
            "kind": "gitea", "name": "home", "create": true, "force": force,
            "fields": { "base_url": base, "repos": ["o/r"] },
            "secrets": { "token": token, "webhook_secret": "hook-secret-value" },
        })
        .to_string()
    };

    // A wrong token fails the test, so nothing is written.
    let (status, _, text) = send(
        &router,
        "POST",
        "/api/setup/source",
        &[],
        &request("wrong-token", false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["saved"], false);
    assert!(!text.contains("wrong-token") && !text.contains("hook-secret-value"));
    assert!(!data.join("credentials/gitea-home-token").exists());
    assert!(
        !std::fs::read_to_string(data.join("sources.toml"))
            .unwrap_or_default()
            .contains("home")
    );

    // The right token saves the entry and the secret files.
    let (status, _, text) = send(
        &router,
        "POST",
        "/api/setup/source",
        &[],
        &request("good-token-value", false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["saved"], true, "{text}");
    assert_eq!(v["applied"], true, "{text}");
    assert_eq!(v["restart_needed"], false);
    assert!(!text.contains("good-token-value") && !text.contains("hook-secret-value"));
    let saved = std::fs::read_to_string(data.join("sources.toml")).unwrap();
    assert!(saved.contains("gitea") && saved.contains("home"));
    assert!(!saved.contains("good-token-value"));
    assert_eq!(
        std::fs::read_to_string(data.join("credentials/gitea-home-token")).unwrap(),
        "good-token-value"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(data.join("credentials/gitea-home-token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // The status now lists it as editable and running, with no restart needed.
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["restart_needed"], false);
    assert_eq!(v["can_edit"], true);
    let src = &v["sources"][0];
    assert_eq!(src["id"], "gitea:home");
    assert_eq!(src["editable"], true);
    assert_eq!(src["running"], true);
    assert!(!text.contains("good-token-value"));

    // A duplicate create is a bad request.
    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/source",
        &[],
        &request("good-token-value", false),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Forcing saves a failing configuration anyway.
    let forced = json!({
        "kind": "gitea", "name": "home", "force": true,
        "fields": { "base_url": base, "repos": ["o/r"] },
        "secrets": { "token": "still-wrong" },
    })
    .to_string();
    let (_, _, text) = send(&router, "POST", "/api/setup/source", &[], &forced).await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["saved"], true, "{text}");
    assert_eq!(
        std::fs::read_to_string(data.join("credentials/gitea-home-token")).unwrap(),
        "still-wrong"
    );
}

#[tokio::test]
async fn removing_a_source_deletes_its_entry_and_its_own_secrets() {
    use crate::probe::tests::{gitea_mock, serve};
    let (router, temp) = with_data("remove").await;
    let data = temp.0.join("data");
    let base = serve(gitea_mock()).await;
    let add = json!({
        "kind": "gitea", "name": "home", "create": true,
        "fields": { "base_url": base, "repos": ["o/r"] },
        "secrets": { "token": "good-token-value", "webhook_secret": "h" },
    })
    .to_string();
    let (_, _, text) = send(&router, "POST", "/api/setup/source", &[], &add).await;
    assert!(text.contains("\"saved\":true"), "{text}");
    assert!(data.join("credentials/gitea-home-token").exists());

    let gone = json!({ "kind": "gitea", "name": "home" }).to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/source/delete", &[], &gone).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!data.join("credentials/gitea-home-token").exists());
    assert!(!data.join("credentials/gitea-home-webhook_secret").exists());
    let (status, ..) = send(&router, "POST", "/api/setup/source/delete", &[], &gone).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A broker that accepts any login: answers a CONNECT with a CONNACK and then
/// reads until the client hangs up.
async fn fake_broker() -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                if socket.read(&mut buf).await.unwrap_or(0) > 0 {
                    let _ = socket.write_all(&[0x20, 0x02, 0x00, 0x00]).await;
                }
                while socket.read(&mut buf).await.unwrap_or(0) > 0 {}
            });
        }
    });
    port
}

#[tokio::test]
async fn mqtt_settings_are_tested_then_saved_and_need_a_restart() {
    let (router, temp) = with_data("mqtt").await;
    let data = temp.0.join("data");
    let port = fake_broker().await;
    let request = |port: u16, force: bool| {
        json!({
            "fields": { "host": "127.0.0.1", "port": port, "username": "deskwatch-bridge", "tls": false },
            "secrets": { "password": "broker-password-value" },
            "force": force,
        })
        .to_string()
    };

    // Nothing listens on port 1: the test fails and nothing is written.
    let (status, _, text) = send(&router, "POST", "/api/setup/mqtt", &[], &request(1, false)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["saved"], false, "{text}");
    assert!(!text.contains("broker-password-value"));
    assert!(!data.join("credentials/mqtt-password").exists());

    // A broker that answers: saved, restart needed, password stored privately.
    let (_, _, text) = send(
        &router,
        "POST",
        "/api/setup/mqtt",
        &[],
        &request(port, false),
    )
    .await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["saved"], true, "{text}");
    assert_eq!(v["restart_needed"], true);
    assert!(!text.contains("broker-password-value"));
    assert_eq!(
        std::fs::read_to_string(data.join("credentials/mqtt-password")).unwrap(),
        "broker-password-value"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(data.join("credentials/mqtt-password"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["restart_needed"], true);
    assert_eq!(v["mqtt"]["port"], port);
    assert_eq!(v["mqtt"]["origin"], "data_folder");
    assert_eq!(v["mqtt"]["editable"], true);
    assert!(!text.contains("broker-password-value"));

    // "Save anyway" writes settings whose test failed.
    let (_, _, text) = send(&router, "POST", "/api/setup/mqtt", &[], &request(1, true)).await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["saved"], true, "{text}");
    assert_eq!(v["report"]["ok"], false);

    // Bad input and the usual guards.
    let bad = json!({ "fields": { "port": 0 } }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/mqtt", &[], &bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_host_view_is_saved_and_needs_a_restart() {
    let (router, temp) = with_data("hostview").await;
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["host_view"], "detailed");
    assert_eq!(v["host_view_editable"], true);

    let compact = json!({ "host_view": "compact" }).to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/kiosk", &[], &compact).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    // The page shows "Saved, but ... could not be started" unless `applied` is
    // true, which made a good save look like an error.
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["applied"], true, "{text}");
    assert_eq!(v["restart_needed"], true, "{text}");
    let saved = std::fs::read_to_string(temp.0.join("data/sources.toml")).unwrap();
    assert!(saved.contains("host_view = \"compact\""), "{saved}");
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["host_view"], "compact");
    assert_eq!(v["host_view_origin"], "data_folder");
    assert_eq!(v["restart_needed"], true);

    let alerting = json!({ "host_view": "alerting" }).to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/kiosk", &[], &alerting).await;
    assert_eq!(status, StatusCode::OK, "{text}");

    let bad = json!({ "host_view": "huge" }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/kiosk", &[], &bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn alert_settings_are_saved_and_need_a_restart() {
    let (router, temp) = with_data("alert-settings").await;
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["alerts"]["ticker"], true);
    assert_eq!(v["alerts"]["editable"], true);
    assert_eq!(v["alerts"]["max_active"], 10);
    assert_eq!(v["alerts"]["http_input"], false);
    assert_eq!(v["alerts"]["failed_pill"], true);
    assert_eq!(v["alerts"]["failed_as_alerts"], false);

    let body = json!({
        "ticker": false, "ticker_speed": "fast", "max_active": 5, "http_input": true,
        "failed_pill": false, "failed_as_alerts": true
    })
    .to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/alerts", &[], &body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let saved = std::fs::read_to_string(temp.0.join("data/sources.toml")).unwrap();
    assert!(saved.contains("ticker = false"), "{saved}");
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["alerts"]["ticker"], false);
    assert_eq!(v["alerts"]["ticker_speed"], "fast");
    assert_eq!(v["alerts"]["max_active"], 5);
    assert_eq!(v["alerts"]["http_input"], true);
    assert_eq!(v["alerts"]["failed_pill"], false);
    assert_eq!(v["alerts"]["failed_as_alerts"], true);
    assert_eq!(v["restart_needed"], true);

    for bad in [json!({ "max_active": 99 }), json!({ "enabled": false })] {
        let (status, ..) = send(&router, "POST", "/api/setup/alerts", &[], &bad.to_string()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let evil = [("origin", "https://evil.example"), ("host", "bridge.lan")];
    let (status, ..) = send(&router, "POST", "/api/setup/alerts", &evil, "{}").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_test_button_can_only_raise_or_clear_its_own_alert() {
    use crate::mqtt::Inbound;
    let temp = Temp::new("test-alert");
    let config = Config::from_toml("").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let router = Setup::new(&config, MqttLink::default(), None, None)
        .with_alert_sender(tx)
        .router();
    drop(temp);

    // No data folder needed. The page sends no alert text, only an action.
    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/alerts/test",
        &[],
        r#"{"action":"send"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let Some(Inbound::Alert(sent)) = rx.recv().await else {
        panic!("expected an alert")
    };
    assert_eq!(sent.id.as_deref(), Some("setup-test"));
    assert_eq!(sent.title.as_deref(), Some("Test alert"));
    assert_eq!(sent.ttl_s, Some(60));
    assert!(!sent.clear);

    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/alerts/test",
        &[],
        r#"{"action":"clear"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let Some(Inbound::Alert(cleared)) = rx.recv().await else {
        panic!("expected a clear")
    };
    assert!(cleared.clear && cleared.id.as_deref() == Some("setup-test"));

    // Text of its own is ignored; anything but the two actions is refused.
    let own = r#"{"action":"create","id":"x","title":"mine","severity":"critical"}"#;
    let (status, ..) = send(&router, "POST", "/api/setup/alerts/test", &[], own).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(rx.try_recv().is_err());
    let evil = [("origin", "https://evil.example"), ("host", "bridge.lan")];
    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/alerts/test",
        &evil,
        r#"{"action":"send"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn a_source_added_from_the_page_can_be_tested_at_once() {
    use crate::probe::tests::{gitea_mock, serve};
    let (router, _temp) = with_data("test-new").await;
    let base = serve(gitea_mock()).await;
    let add = json!({
        "kind": "gitea", "name": "home", "create": true,
        "fields": { "base_url": base, "repos": ["o/r"] },
        "secrets": { "token": "good-token-value", "webhook_secret": "h" },
    })
    .to_string();
    let (_, _, text) = send(&router, "POST", "/api/setup/source", &[], &add).await;
    assert!(text.contains("\"saved\":true"), "{text}");
    let body = json!({ "id": "gitea:home" }).to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/test", &[], &body).await;
    // Found (not a 404 "no such source") and checked. Whether the checks pass
    // depends on the process-wide credentials folder, which the test runs share.
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("\"checks\":["), "{text}");
}

#[tokio::test]
async fn restarting_needs_the_option_and_the_same_origin() {
    let temp = Temp::new("restart");
    let mut config = config(&temp.0);
    let off = Setup::new(&config, MqttLink::default(), None, None);
    let router = off.router();
    let (status, ..) = send(&router, "POST", "/api/setup/restart", &[], "").await;
    assert_eq!(status, StatusCode::CONFLICT, "off by default");

    config.setup.allow_restart = true;
    let on = Setup::new(&config, MqttLink::default(), None, None);
    let signal = on.restart_signal();
    let router = on.router();
    let evil = [("host", "b.lan"), ("origin", "http://evil.example")];
    assert_eq!(
        send(&router, "POST", "/api/setup/restart", &evil, "")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // The refused call raised no signal; a real one does.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), signal.notified())
            .await
            .is_err()
    );
    let (status, ..) = send(&router, "POST", "/api/setup/restart", &[], "").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    tokio::time::timeout(Duration::from_secs(1), signal.notified())
        .await
        .expect("the restart signal fired");

    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    assert!(text.contains("\"can_restart\":true"), "{text}");
}

#[tokio::test]
async fn the_old_login_route_is_gone() {
    let temp = Temp::new("open");
    let router = setup(&temp).router();
    let (status, ..) = send(&router, "POST", "/api/setup/login", &[], "{}").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A stray cookie or token changes nothing.
    let extras = [
        ("authorization", "Bearer whatever"),
        ("cookie", "deskwatch_setup=1.2"),
    ];
    let (status, ..) = send(&router, "GET", "/api/setup/status", &extras, "").await;
    assert_eq!(status, StatusCode::OK);
}

#[test]
fn the_page_has_no_login_form() {
    for word in ["loginForm", "api/setup/login", "setup token"] {
        assert!(!SETUP_JS.contains(word), "the page still mentions {word}");
    }
}

#[tokio::test]
async fn another_site_cannot_change_anything() {
    let temp = Temp::new("open-origin");
    let router = setup(&temp).router();
    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/test",
        &[
            ("host", "bridge.lan:8787"),
            ("origin", "https://evil.example"),
        ],
        r#"{"id":"mqtt"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn status_says_whether_the_dashboard_is_there_to_link_back_to() {
    let temp = Temp::new("kiosk-flag");
    let router = setup(&temp).router();
    let (_, _, body) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let status: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status["kiosk"], false);
}

/// A setup router whose `bridge.toml` has one GitHub source (pointing nowhere)
/// and a data folder, plus the folder's path.
async fn with_toml_source(tag: &str) -> (Router, Temp) {
    let temp = Temp::new(tag);
    let store = Store::open(&temp.0.join("data")).unwrap();
    let token = temp.0.join("github-token");
    std::fs::write(&token, SECRET).unwrap();
    let text = format!(
        "[[source.github]]\nname = \"main\"\nbase_url = \"http://127.0.0.1:1\"\n\
         token_file = {:?}\nrepos = [\"o/a\"]\n[mqtt]\nusername = \"from-toml\"\n",
        token.to_str().unwrap()
    );
    let config = Config::from_toml(&text).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let supervisor = Supervisor::new(tx, crate::hooks::Webhooks::default());
    let router = Setup::new(&config, MqttLink::default(), Some(store), Some(supervisor)).router();
    (router, temp)
}

async fn status_json(router: &Router) -> Value {
    let (_, _, text) = send(router, "GET", "/api/setup/status", &[], "").await;
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn switching_a_bridge_toml_source_off_and_on_leaves_bridge_toml_alone() {
    let (router, temp) = with_toml_source("hide").await;
    let v = status_json(&router).await;
    assert_eq!(v["sources"][0]["id"], "github:main");
    assert_eq!(v["sources"][0]["editable"], false);
    assert_eq!(v["sources"][0]["hideable"], true);
    assert_eq!(v["sources"][0]["hidden"], false);

    let off = json!({ "kind": "github", "name": "main", "hidden": true }).to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/source/hide", &[], &off).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let saved = std::fs::read_to_string(temp.0.join("data/sources.toml")).unwrap();
    assert!(saved.contains("github:main"), "{saved}");
    // Still listed (so it can come back), but not running and not editable.
    let v = status_json(&router).await;
    assert_eq!(v["sources"].as_array().unwrap().len(), 1);
    assert_eq!(v["sources"][0]["hidden"], true);
    assert_eq!(v["sources"][0]["running"], false);
    assert_eq!(v["sources"][0]["editable"], false);
    // The token stays: it belongs to bridge.toml's source, not to the page.
    assert!(temp.0.join("github-token").exists());
    assert!(!status_json(&router).await.to_string().contains(SECRET));

    let on = json!({ "kind": "github", "name": "main", "hidden": false }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/source/hide", &[], &on).await;
    assert_eq!(status, StatusCode::OK);
    let v = status_json(&router).await;
    assert_eq!(v["sources"][0]["hidden"], false);
    let saved = std::fs::read_to_string(temp.0.join("data/sources.toml")).unwrap();
    assert!(!saved.contains("github:main"), "{saved}");

    // Only bridge.toml sources can be switched off, and only from the same origin.
    let nope = json!({ "kind": "github", "name": "unknown" }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/source/hide", &[], &nope).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let evil = [("host", "b.lan"), ("origin", "http://evil.example")];
    let (status, ..) = send(&router, "POST", "/api/setup/source/hide", &evil, &off).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn saved_broker_settings_can_be_removed_with_their_password() {
    let (router, temp) = with_toml_source("mqtt-remove").await;
    let data = temp.0.join("data");
    assert_eq!(status_json(&router).await["mqtt"]["removable"], false);
    std::fs::write(
        data.join("sources.toml"),
        "[mqtt]\nhost = \"b.lan\"\nusername = \"me\"\npassword_file = \"mqtt-password\"\n",
    )
    .unwrap();
    Store::open(&data)
        .unwrap()
        .write_secret("mqtt-password", "pw-value")
        .unwrap();
    let v = status_json(&router).await;
    assert_eq!(v["mqtt"]["removable"], true);
    assert_eq!(v["mqtt"]["has_login"], true);

    // The login goes, the host stays, the password file is deleted.
    let body = json!({ "what": "login" }).to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/mqtt/delete", &[], &body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!data.join("credentials/mqtt-password").exists());
    let v = status_json(&router).await;
    assert_eq!(v["mqtt"]["host"], "b.lan");
    assert_eq!(v["mqtt"]["username"], Value::Null);
    assert_eq!(v["restart_needed"], true);

    // Everything saved goes: back to bridge.toml (the name it has).
    let body = json!({ "what": "settings" }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/mqtt/delete", &[], &body).await;
    assert_eq!(status, StatusCode::OK);
    let v = status_json(&router).await;
    assert_eq!(v["mqtt"]["username"], "from-toml");
    assert_eq!(v["mqtt"]["removable"], false);

    let (status, ..) = send(&router, "POST", "/api/setup/mqtt/delete", &[], "{}").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let evil = [("host", "b.lan"), ("origin", "http://evil.example")];
    let (status, ..) = send(&router, "POST", "/api/setup/mqtt/delete", &evil, &body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn only_stored_secrets_nothing_uses_can_be_deleted() {
    use crate::probe::tests::{gitea_mock, serve};
    let (router, temp) = with_data("secrets").await;
    let data = temp.0.join("data");
    let base = serve(gitea_mock()).await;
    let add = json!({
        "kind": "gitea", "name": "home", "create": true,
        "fields": { "base_url": base, "repos": ["o/r"] },
        "secrets": { "token": "good-token-value", "webhook_secret": "h" },
    })
    .to_string();
    send(&router, "POST", "/api/setup/source", &[], &add).await;
    // Left behind by hand or by an older version.
    Store::open(&data)
        .unwrap()
        .write_secret("old-token", "x")
        .unwrap();

    let v = status_json(&router).await;
    let secrets = v["secrets"].as_array().unwrap();
    let used = |name: &str| {
        secrets
            .iter()
            .find(|s| s["name"] == name)
            .map(|s| s["used_by"].clone())
    };
    assert_eq!(used("gitea-home-token"), Some(json!("gitea:home")));
    assert_eq!(used("old-token"), Some(Value::Null));
    assert!(!v.to_string().contains("good-token-value"));

    let in_use = json!({ "name": "gitea-home-token" }).to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/secret/delete", &[], &in_use).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(data.join("credentials/gitea-home-token").exists());

    let orphan = json!({ "name": "old-token" }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/secret/delete", &[], &orphan).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!data.join("credentials/old-token").exists());
    let (status, ..) = send(&router, "POST", "/api/setup/secret/delete", &[], &orphan).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A name that tries to leave the credentials folder is not a secret.
    let sneaky = json!({ "name": "../sources" }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/secret/delete", &[], &sneaky).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(data.join("sources.toml").exists());
}

#[tokio::test]
async fn status_carries_a_boot_id_so_the_page_can_tell_a_restart_happened() {
    let temp = Temp::new("boot");
    let a = status_json(&setup(&temp).router()).await;
    let b = status_json(&setup(&temp).router()).await;
    assert!(a["boot"].as_str().is_some_and(|s| !s.is_empty()));
    assert_ne!(a["boot"], b["boot"]);
}

#[test]
fn the_page_has_the_sections_the_confirm_boxes_and_the_restart_wait() {
    for needle in [
        "api/setup/source/hide",
        "api/setup/mqtt/delete",
        "api/setup/secret/delete",
        "api/setup/restart",
        "confirmBox",
        "restarting.boot",
        "allow_restart = true",
    ] {
        assert!(SETUP_JS.contains(needle), "the page lacks {needle}");
    }
    // Deleting never goes through a browser dialog that could be blocked or skipped.
    assert!(!SETUP_JS.contains("window.confirm") && !SETUP_JS.contains("window.alert"));
    // Add source sits next to the section title and opens the pop-up.
    assert!(SETUP_JS.contains("addButton('Add source')"));
    assert!(SETUP_JS.contains("addSource);"));
}

/// Adding and changing things happens in one <dialog> outside the redrawn
/// part of the page, so the refresh every few seconds cannot wipe a form.
#[test]
fn forms_open_in_a_pop_up_outside_the_redrawn_page() {
    assert!(INDEX_HTML.contains(r#"<dialog id="modal""#));
    let main = INDEX_HTML.find(r#"<main id="app""#).unwrap();
    let dialog = INDEX_HTML.find(r#"<dialog id="modal""#).unwrap();
    assert!(dialog > INDEX_HTML.find("</main>").unwrap() && dialog > main);
    for needle in [
        "showModal()",
        "function newUserModal",
        "function newKeyModal",
        "function passwordModal",
        "function githubModal",
        "function microsoftModal",
        "function drawForm",
    ] {
        assert!(SETUP_JS.contains(needle), "the page lacks {needle}");
    }
}

/// The top bar holds icon buttons only; each one names itself for a tooltip
/// and for a screen reader, and Sign out is the last one.
#[test]
fn the_top_bar_has_named_icon_buttons_with_sign_out_last() {
    let start = INDEX_HTML.find("<header").unwrap();
    let header = &INDEX_HTML[start..INDEX_HTML.find("</header>").unwrap()];
    for id in ["dashboard-link", "account-link", "theme", "signout"] {
        let at = header
            .find(&format!(r#"id="{id}""#))
            .unwrap_or_else(|| panic!("no {id}"));
        let tag = &header[at..at + header[at..].find('>').unwrap()];
        assert!(
            tag.contains("data-tip=") && tag.contains("aria-label="),
            "{id} is not named"
        );
    }
    let signout = header.find(r#"id="signout""#).unwrap();
    assert!(!header[signout..].contains("<button id=") && !header[signout..].contains("<a id="));
}

/// The setup page is dark unless the person picks light with its button: the
/// stylesheet must not follow the system's colour scheme.
#[test]
fn the_setup_page_is_dark_by_default_and_ignores_the_system_scheme() {
    assert!(!SETUP_CSS.contains("prefers-color-scheme"));
    assert!(SETUP_CSS.contains("color-scheme: dark"));
    assert!(SETUP_CSS.contains(r#":root[data-theme="light"]"#));
    assert!(INDEX_HTML.contains(r#"content="dark""#));
    assert!(!INDEX_HTML.contains("light dark"));
}

#[tokio::test]
async fn the_update_check_switch_is_saved_and_needs_a_restart() {
    let (router, temp) = with_data("update-check").await;
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["updates"]["check"], true);
    assert_eq!(v["updates"]["editable"], true);
    assert_eq!(v["update"], Value::Null);

    let off = json!({ "check": false }).to_string();
    let (status, _, text) = send(&router, "POST", "/api/setup/updates", &[], &off).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let saved = std::fs::read_to_string(temp.0.join("data/sources.toml")).unwrap();
    assert!(saved.contains("check = false"), "{saved}");
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["updates"]["check"], false);
    assert_eq!(v["restart_needed"], true);

    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/updates",
        &[],
        r#"{"check":"no"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let evil = [("origin", "https://evil.example"), ("host", "bridge.lan")];
    let (status, ..) = send(&router, "POST", "/api/setup/updates", &evil, &off).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_found_update_shows_in_the_status() {
    let temp = Temp::new("update-found");
    let notice = crate::update::UpdateNotice::default();
    notice.set(Some(crate::update::Update {
        version: "0.9.20".into(),
        url: "https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.20".into(),
    }));
    let router = setup(&temp).with_update(notice).router();
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["update"]["version"], "0.9.20");
}

/// Like `with_data`, without touching the process-wide credentials folder.
fn isolated(tag: &str) -> (Router, Temp, Store) {
    let temp = Temp::new(tag);
    let store = Store::open_isolated(&temp.0.join("data")).unwrap();
    let config = Config::from_toml("").unwrap();
    let router = Setup::new(&config, MqttLink::default(), Some(store.clone()), None).router();
    (router, temp, store)
}

#[tokio::test]
async fn backup_export_and_import_over_http() {
    let (router, _temp, store) = isolated("backup-from");
    store
        .write_sources("[[source.gitea]]\nname = \"p\"\nbase_url = \"https://prom.example\"\ntoken_file = \"tok\"\nwebhook_secret_file = \"hook\"\n")
        .unwrap();
    store.write_secret("tok", "VERY-SECRET-VALUE").unwrap();

    // No secret in the plain export.
    let (status, headers, plain) = send(&router, "GET", "/api/setup/export", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains(".deskwatch")
    );
    assert!(!plain.contains("VERY-SECRET-VALUE"));

    // Locked export needs a long password and a same-origin request.
    let evil = [("host", "b.lan"), ("origin", "http://evil.example")];
    let ask = |pw: &str| json!({ "password": pw }).to_string();
    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/export",
        &evil,
        &ask("long enough password"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, ..) = send(&router, "POST", "/api/setup/export", &[], &ask("short")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, locked) = send(
        &router,
        "POST",
        "/api/setup/export",
        &[],
        &ask("long enough password"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!locked.contains("VERY-SECRET-VALUE"));

    // Restore the locked file into another install.
    let (other, _other_temp, restored) = isolated("backup-to");
    let with = |file: &str, pw: Option<&str>| json!({ "file": file, "password": pw }).to_string();
    let (status, _, body) = send(
        &other,
        "POST",
        "/api/setup/import/check",
        &[],
        &with(&locked, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["needs_password"],
        true
    );
    let (status, ..) = send(
        &other,
        "POST",
        "/api/setup/import",
        &[],
        &with(&locked, Some("wrong password!!")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = send(
        &other,
        "POST",
        "/api/setup/import/check",
        &[],
        &with(&locked, Some("long enough password")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["summary"]["secrets"],
        1
    );
    let (status, ..) = send(
        &other,
        "POST",
        "/api/setup/import",
        &[],
        &with(&locked, Some("long enough password")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(restored.read_sources().unwrap().contains("prom.example"));
    assert_eq!(restored.read_secret("tok").unwrap(), "VERY-SECRET-VALUE");
}

/// A Prometheus that knows one Linux and one Windows machine, and cAdvisor.
fn prometheus_mock() -> Router {
    use axum::extract::Query;
    use axum::routing::get;
    async fn query(Query(q): Query<std::collections::HashMap<String, String>>) -> String {
        let promql = q.get("query").map(String::as_str).unwrap_or_default();
        let series = |items: &[(&str, f64)]| {
            let result: Vec<Value> = items
                .iter()
                .map(|(i, v)| json!({ "metric": { "instance": i }, "value": [1.0, v.to_string()] }))
                .collect();
            json!({ "status": "success", "data": { "resultType": "vector", "result": result } })
                .to_string()
        };
        match promql {
            "up" => series(&[
                ("nas:9100", 1.0),
                ("office-pc:9182", 1.0),
                ("cadvisor:8080", 1.0),
            ]),
            "max by (instance) (node_memory_MemTotal_bytes)" => series(&[("nas:9100", 1.0)]),
            "max by (instance) (windows_cs_physical_memory_bytes)" => {
                series(&[("office-pc:9182", 1.0)])
            }
            p if p.contains("node_cpu_seconds_total") => {
                series(&[("nas:9100", 12.0), ("office-pc:9182", 30.0)])
            }
            p if p.contains("node_apt_upgrades_pending") => series(&[("office-pc:9182", 3.0)]),
            _ => series(&[]),
        }
    }
    Router::new().route("/api/v1/query", get(query))
}

#[tokio::test]
async fn hosts_are_discovered_chosen_renamed_and_saved() {
    use crate::probe::tests::serve;
    let (router, temp) = with_data("hosts").await;
    let base = serve(prometheus_mock()).await;
    let add = json!({
        "kind": "prometheus", "name": "home", "create": true,
        "fields": { "url": base, "hosts": [] },
    })
    .to_string();
    let (_, _, text) = send(&router, "POST", "/api/setup/source", &[], &add).await;
    assert!(text.contains("\"saved\":true"), "{text}");

    // Discovery lists machines, not cAdvisor, and what each one reports.
    let (status, _, text) = send(&router, "GET", "/api/setup/hosts", &[], "").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let source = &v["sources"][0];
    assert_eq!(source["name"], "home");
    assert_eq!(source["editable"], true);
    let found = source["found"].as_array().unwrap();
    assert_eq!(found.len(), 2, "{text}");
    assert_eq!(found[0]["instance"], "nas:9100");
    assert_eq!(found[0]["kind"], "linux");
    assert_eq!(found[1]["kind"], "windows");
    assert_eq!(found[1]["reports"], json!(["cpu", "updates"]));
    assert_eq!(source["configured"], json!([]));

    // Choose one, rename it and show two parts.
    let save = json!({
        "source": "home",
        "hosts": [{ "instance": "office-pc:9182", "name": "Office PC", "show": ["cpu", "updates"] }],
    })
    .to_string();
    let evil = [("host", "b.lan"), ("origin", "http://evil.example")];
    let (status, ..) = send(&router, "POST", "/api/setup/hosts", &evil, &save).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, text) = send(&router, "POST", "/api/setup/hosts", &[], &save).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let saved = std::fs::read_to_string(temp.0.join("data/sources.toml")).unwrap();
    assert!(
        saved.contains("Office PC") && saved.contains("updates"),
        "{saved}"
    );

    let (_, _, text) = send(&router, "GET", "/api/setup/hosts", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["sources"][0]["configured"][0],
        json!({ "instance": "office-pc:9182", "name": "Office PC", "show": ["cpu", "updates"], "gpu_names": [], "gpu_detail": true })
    );

    // A bad name or an unknown part is refused and nothing changes.
    for bad in [
        json!({ "source": "home", "hosts": [{ "instance": "a", "name": "" }] }),
        json!({ "source": "home", "hosts": [{ "instance": "a", "name": "a", "show": ["fans"] }] }),
        json!({ "source": "nope", "hosts": [] }),
    ] {
        let (status, ..) = send(&router, "POST", "/api/setup/hosts", &[], &bad.to_string()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(
        std::fs::read_to_string(temp.0.join("data/sources.toml")).unwrap(),
        saved
    );
}
