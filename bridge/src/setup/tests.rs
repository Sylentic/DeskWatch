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
    Setup::new(
        &config(&temp.0),
        &Secret::new("setup-token"),
        MqttLink::default(),
        None,
    )
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

fn login_body(token: &str) -> String {
    json!({ "token": token }).to_string()
}

async fn logged_in(router: &Router) -> String {
    let (status, headers, _) = send(
        router,
        "POST",
        "/api/setup/login",
        &[],
        &login_body("setup-token"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let set = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(set.starts_with("deskwatch_setup="), "{set}");
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"));
    set.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn status_needs_a_login_and_never_carries_a_secret() {
    let temp = Temp::new("status");
    let router = setup(&temp).router();
    let (status, ..) = send(&router, "GET", "/api/setup/status", &[], "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let cookie = logged_in(&router).await;
    let (status, headers, body) = send(
        &router,
        "GET",
        "/api/setup/status",
        &[("cookie", &cookie)],
        "",
    )
    .await;
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
    let setup = Setup::new(
        &config(&temp.0),
        &Secret::new("setup-token"),
        link.clone(),
        None,
    );
    let router = setup.router();
    let cookie = logged_in(&router).await;
    let get = || async {
        let (_, _, body) = send(
            &router,
            "GET",
            "/api/setup/status",
            &[("cookie", &cookie)],
            "",
        )
        .await;
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

#[tokio::test]
async fn wrong_tokens_are_refused_and_limited() {
    let temp = Temp::new("limit");
    let router = setup(&temp).router();
    for _ in 0..session::MAX_FAILURES {
        let (status, headers, _) = send(
            &router,
            "POST",
            "/api/setup/login",
            &[],
            &login_body("nope"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!headers.contains_key(header::SET_COOKIE));
    }
    let (status, headers, _) = send(
        &router,
        "POST",
        "/api/setup/login",
        &[],
        &login_body("setup-token"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
}

#[tokio::test]
async fn login_from_another_site_is_refused() {
    let temp = Temp::new("origin");
    let router = setup(&temp).router();
    let evil = [
        ("host", "bridge.lan:8787"),
        ("origin", "http://evil.example"),
    ];
    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/login",
        &evil,
        &login_body("setup-token"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_dashboard_session_is_worth_nothing_on_the_setup_page() {
    // Even with the same token, the two pages sign their cookies differently.
    let temp = Temp::new("purpose");
    let router = setup(&temp).router();
    let kiosk = Sessions::new("kiosk", "setup-token", 86_400);
    let forged = format!("deskwatch_setup={}", kiosk.issue(unix_now()));
    let (status, ..) = send(
        &router,
        "GET",
        "/api/setup/status",
        &[("cookie", &forged)],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let named_like_kiosk = format!("deskwatch_session={}", kiosk.issue(unix_now()));
    let (status, ..) = send(
        &router,
        "GET",
        "/api/setup/status",
        &[("cookie", &named_like_kiosk)],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn trusted_networks_do_not_open_the_setup_page() {
    // No peer address is consulted at all: a request from loopback with a
    // trusted_networks range that covers it still needs the login.
    let temp = Temp::new("trusted");
    let mut cfg = config(&temp.0);
    cfg.kiosk.trusted_networks = vec!["0.0.0.0/0".into()];
    let router = Setup::new(&cfg, &Secret::new("setup-token"), MqttLink::default(), None).router();
    let (status, ..) = send(&router, "GET", "/api/setup/status", &[], "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
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

#[test]
fn setup_is_off_without_a_token_file() {
    assert!(Config::from_toml("").unwrap().setup.token_file.is_none());
    let cfg = Config::from_toml("[setup]\ntoken_file = \"setup-token\"\n").unwrap();
    assert_eq!(cfg.setup.token_file.as_deref(), Some("setup-token"));
}

#[tokio::test]
async fn connection_tests_need_login_origin_and_a_known_source() {
    let temp = Temp::new("probe");
    let router = setup(&temp).router();
    let body = json!({ "id": "github:main" }).to_string();
    let (status, ..) = send(&router, "POST", "/api/setup/test", &[], &body).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let cookie = logged_in(&router).await;
    let evil = [
        ("cookie", cookie.as_str()),
        ("host", "b.lan"),
        ("origin", "http://evil.example"),
    ];
    assert_eq!(
        send(&router, "POST", "/api/setup/test", &evil, &body)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let ok = [("cookie", cookie.as_str())];
    assert_eq!(
        send(&router, "POST", "/api/setup/test", &ok, "{}").await.0,
        StatusCode::BAD_REQUEST
    );
    let unknown = json!({ "id": "github:nope" }).to_string();
    assert_eq!(
        send(&router, "POST", "/api/setup/test", &ok, &unknown)
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
    let setup = Setup::new(
        &config,
        &Secret::new("setup-token"),
        MqttLink::default(),
        None,
    );
    let router = setup.router();
    let cookie = logged_in(&router).await;
    let headers = [("cookie", cookie.as_str())];
    let body = json!({ "id": "gitea:home" }).to_string();

    let (status, _, text) = send(&router, "POST", "/api/setup/test", &headers, &body).await;
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
    let (status, ..) = send(&router, "POST", "/api/setup/test", &headers, &body).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn the_page_can_test_a_connection() {
    assert!(SETUP_JS.contains("api/setup/test"));
    assert!(SETUP_JS.contains("Test connection"));
}

/// A setup router with a data folder, plus the folder's path and a login cookie.
async fn with_data(tag: &str) -> (Router, Temp, String) {
    let temp = Temp::new(tag);
    let data = temp.0.join("data");
    let store = Store::open(&data).unwrap();
    let config = Config::from_toml("").unwrap();
    let router = Setup::new(
        &config,
        &Secret::new("setup-token"),
        MqttLink::default(),
        Some(store),
    )
    .router();
    let cookie = logged_in(&router).await;
    (router, temp, cookie)
}

#[tokio::test]
async fn changes_need_login_same_origin_and_a_data_folder() {
    let temp = Temp::new("change-guards");
    let body = json!({ "kind": "github", "name": "x", "create": true }).to_string();
    // No data folder: refused even when logged in.
    let router = setup(&temp).router();
    let cookie = logged_in(&router).await;
    let ok = [("cookie", cookie.as_str())];
    let (status, ..) = send(&router, "POST", "/api/setup/source", &ok, &body).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (router, _temp, cookie) = with_data("change-guards2").await;
    let (status, ..) = send(&router, "POST", "/api/setup/source", &[], &body).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let evil = [
        ("cookie", cookie.as_str()),
        ("host", "b.lan"),
        ("origin", "http://evil.example"),
    ];
    for uri in ["/api/setup/source", "/api/setup/source/delete"] {
        let (status, ..) = send(&router, "POST", uri, &evil, &body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let ok = [("cookie", cookie.as_str())];
    let (status, ..) = send(&router, "POST", "/api/setup/source", &ok, "nope").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_good_source_is_saved_a_failing_one_is_not_unless_forced() {
    use crate::probe::tests::{gitea_mock, serve};
    let (router, temp, cookie) = with_data("save").await;
    let data = temp.0.join("data");
    let base = serve(gitea_mock()).await;
    let headers = [("cookie", cookie.as_str())];
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
        &headers,
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
        &headers,
        &request("good-token-value", false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["saved"], true, "{text}");
    assert_eq!(v["restart_needed"], true);
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

    // The status now lists it as editable, not running yet, restart needed.
    let (_, _, text) = send(&router, "GET", "/api/setup/status", &headers, "").await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["restart_needed"], true);
    assert_eq!(v["can_edit"], true);
    let src = &v["sources"][0];
    assert_eq!(src["id"], "gitea:home");
    assert_eq!(src["editable"], true);
    assert_eq!(src["running"], false);
    assert!(!text.contains("good-token-value"));

    // A duplicate create is a bad request.
    let (status, ..) = send(
        &router,
        "POST",
        "/api/setup/source",
        &headers,
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
    let (_, _, text) = send(&router, "POST", "/api/setup/source", &headers, &forced).await;
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
    let (router, temp, cookie) = with_data("remove").await;
    let data = temp.0.join("data");
    let base = serve(gitea_mock()).await;
    let headers = [("cookie", cookie.as_str())];
    let add = json!({
        "kind": "gitea", "name": "home", "create": true,
        "fields": { "base_url": base, "repos": ["o/r"] },
        "secrets": { "token": "good-token-value", "webhook_secret": "h" },
    })
    .to_string();
    let (_, _, text) = send(&router, "POST", "/api/setup/source", &headers, &add).await;
    assert!(text.contains("\"saved\":true"), "{text}");
    assert!(data.join("credentials/gitea-home-token").exists());

    let gone = json!({ "kind": "gitea", "name": "home" }).to_string();
    let (status, _, text) =
        send(&router, "POST", "/api/setup/source/delete", &headers, &gone).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!data.join("credentials/gitea-home-token").exists());
    assert!(!data.join("credentials/gitea-home-webhook_secret").exists());
    let (status, ..) = send(&router, "POST", "/api/setup/source/delete", &headers, &gone).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
