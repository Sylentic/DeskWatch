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
    let saved = std::fs::read_to_string(temp.0.join("data/sources.toml")).unwrap();
    assert!(saved.contains("host_view = \"compact\""), "{saved}");
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &[], "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["host_view"], "compact");
    assert_eq!(v["host_view_origin"], "data_folder");
    assert_eq!(v["restart_needed"], true);

    let bad = json!({ "host_view": "huge" }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/kiosk", &[], &bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
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
