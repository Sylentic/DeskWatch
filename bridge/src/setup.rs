//! The setup page: a place to see how the bridge is configured, so it can
//! later be changed without editing files (docs/setup-page.md).
//!
//! This step is read-only. It serves:
//!
//! - `GET /setup`, `/setup.js`, `/setup.css`: the page;
//! - `POST /api/setup/login`: trades the setup token for a session cookie;
//! - `GET /api/setup/status`: sources, credentials (only whether each one
//!   exists, never its value), MQTT, the data folder and live source health.
//!
//! The page is off unless `[setup] token_file` is set, and it has its own
//! token, cookie and failure limit, apart from the dashboard's. Unlike the
//! dashboard it ignores `kiosk.trusted_networks` completely: the peer address
//! is not good enough to decide who may see (and later change) credentials.
//! The cookie lasts `SESSION_S`, shorter than the dashboard's.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::config::Config;
use crate::health::MqttLink;
use crate::kiosk::session::{self, Limiter, Sessions};
use crate::kiosk::{asset, digest, same};
use crate::source::{Secret, credential_status};

/// How long a setup login lasts.
pub const SESSION_S: u64 = 12 * 3600;

const INDEX_HTML: &str = include_str!("../setup/index.html");
const SETUP_CSS: &str = include_str!("../setup/setup.css");
const SETUP_JS: &str = include_str!("../setup/setup.js");

/// Everything the status endpoint needs, shared with the main loop.
#[derive(Clone)]
pub struct Setup {
    shared: Arc<Shared>,
}

struct Shared {
    token: [u8; 32],
    sessions: Sessions,
    limiter: Limiter,
    /// Facts that only change with the config: built once.
    fixed: Value,
    mqtt_enabled: bool,
    mqtt: MqttLink,
    /// Source id (`github:main`) to health name, kept current by the main loop.
    health: Mutex<BTreeMap<String, &'static str>>,
}

impl Setup {
    pub fn new(config: &Config, token: &Secret, mqtt: MqttLink) -> Self {
        Self {
            shared: Arc::new(Shared {
                token: digest(token.expose()),
                sessions: Sessions::new("setup", token.expose(), SESSION_S),
                limiter: Limiter::default(),
                fixed: describe(config),
                mqtt_enabled: config.mqtt.enabled,
                mqtt,
                health: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Record the current health of every source. Called every main loop turn.
    pub fn update_health<'a>(&self, health: impl Iterator<Item = (String, &'static str)> + 'a) {
        let mut map = self.shared.health.lock().unwrap_or_else(|e| e.into_inner());
        map.clear();
        map.extend(health);
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route(
                "/setup",
                get(|| asset("text/html; charset=utf-8", INDEX_HTML)),
            )
            .route(
                "/setup.css",
                get(|| asset("text/css; charset=utf-8", SETUP_CSS)),
            )
            .route(
                "/setup.js",
                get(|| asset("text/javascript; charset=utf-8", SETUP_JS)),
            )
            .route("/api/setup/login", post(login))
            .route("/api/setup/status", get(status))
            .with_state(self.shared.clone())
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn login(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !session::same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
    }
    let now = unix_now();
    if let Some(wait) = shared.limiter.blocked(now) {
        tracing::warn!("setup login refused: too many wrong tokens lately");
        let mut response = (StatusCode::TOO_MANY_REQUESTS, "too many wrong tokens").into_response();
        if let Ok(value) = HeaderValue::from_str(&wait.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return response;
    }
    let given = (body.len() <= 4096)
        .then(|| serde_json::from_slice::<Value>(&body).ok())
        .flatten()
        .and_then(|v| v.get("token")?.as_str().map(str::to_string));
    let Some(given) = given else {
        return (StatusCode::BAD_REQUEST, "expected {\"token\": \"...\"}").into_response();
    };
    if !same(&shared.token, &digest(&given)) {
        shared.limiter.record_failure(now);
        tracing::warn!("setup login with a wrong token");
        tokio::time::sleep(Duration::from_millis(400)).await;
        return (StatusCode::UNAUTHORIZED, "token wrong").into_response();
    }
    tracing::info!("setup login");
    let cookie = session::set_cookie(
        session::SETUP_COOKIE,
        &shared.sessions.issue(now),
        SESSION_S,
        session::is_https(&headers),
    );
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn status(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let allowed = session::cookie_value(&headers, session::SETUP_COOKIE)
        .is_some_and(|v| shared.sessions.valid(v, unix_now()));
    if !allowed {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    }
    let health = shared
        .health
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut out = shared.fixed.clone();
    if let Some(sources) = out["sources"].as_array_mut() {
        for source in sources {
            let id = source["id"].as_str().unwrap_or_default().to_string();
            source["health"] = json!(health.get(&id).copied().unwrap_or("unknown"));
        }
    }
    out["mqtt"]["connected"] = json!(shared.mqtt_enabled && shared.mqtt.is_up());
    let mut response = out.to_string().into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The part of the status that depends only on the config. Holds names and
/// places, never a secret value: a credential is reported as `mounted`,
/// `data_folder` or `missing`.
pub fn describe(config: &Config) -> Value {
    let cred = |label: &str, name: &str, required: bool| {
        json!({
            "label": label,
            "name": name,
            "required": required,
            "status": credential_status(name).name(),
        })
    };
    let origin = |kind: &str, name: &str| {
        if config.overlay_sources.contains(&format!("{kind}:{name}")) {
            "data_folder"
        } else {
            "bridge_toml"
        }
    };
    let mut sources = Vec::new();
    for s in &config.source.gitea {
        let mut creds = vec![cred("webhook secret", &s.webhook_secret_file, true)];
        if let Some(t) = &s.token_file {
            creds.push(cred("token", t, false));
        }
        sources.push(json!({
            "id": format!("gitea:{}", s.name), "kind": "gitea", "name": s.name,
            "origin": origin("gitea", &s.name),
            "address": s.base_url, "repos": s.repos, "credentials": creds,
        }));
    }
    for s in &config.source.github {
        sources.push(json!({
            "id": format!("github:{}", s.name), "kind": "github", "name": s.name,
            "origin": origin("github", &s.name),
            "address": s.base_url, "repos": s.repos,
            "credentials": [cred("token", &s.token_file, true)],
        }));
    }
    for s in &config.source.azure_devops {
        sources.push(json!({
            "id": format!("azure_devops:{}", s.name), "kind": "azure_devops", "name": s.name,
            "origin": origin("azure_devops", &s.name),
            "address": format!("{}/{}", s.base_url.trim_end_matches('/'), s.organization),
            "repos": s.projects,
            "credentials": [cred("token", &s.token_file, true)],
        }));
    }
    for s in &config.source.prometheus {
        let creds: Vec<Value> = s
            .token_file
            .iter()
            .map(|t| cred("token", t, false))
            .collect();
        sources.push(json!({
            "id": format!("prometheus:{}", s.name), "kind": "prometheus", "name": s.name,
            "origin": origin("prometheus", &s.name),
            "address": s.url,
            "repos": s.hosts.iter().map(|h| h.name.clone()).collect::<Vec<_>>(),
            "credentials": creds,
        }));
    }
    let mqtt = &config.mqtt;
    let mqtt_password = match &mqtt.password_file {
        Some(file) => credential_status(file).name(),
        None if std::env::var_os(crate::config::MQTT_PASSWORD_ENV).is_some() => "environment",
        None => "none",
    };
    json!({
        "v": 1,
        "build": env!("CARGO_PKG_VERSION"),
        "data_folder": config.data_dir().map(|d| d.display().to_string()),
        "http_listen": config.http.listen.to_string(),
        "mqtt": {
            "enabled": mqtt.enabled, "host": mqtt.host, "port": mqtt.port, "tls": mqtt.tls,
            "username": mqtt.username, "password": mqtt_password,
        },
        "sources": sources,
    })
}

#[cfg(test)]
mod tests;
