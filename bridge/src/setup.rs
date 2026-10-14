//! The setup page: a place to see how the bridge is configured, so it can
//! later be changed without editing files (docs/setup-page.md).
//!
//! This step is read-only. It serves:
//!
//! - `GET /setup`, `/setup.js`, `/setup.css`: the page;
//! - `POST /api/setup/login`: trades the setup token for a session cookie;
//! - `GET /api/setup/status`: sources, credentials (only whether each one
//!   exists, never its value), MQTT, the data folder and live source health;
//! - `POST /api/setup/test` with `{"id": "github:main"}` or `{"id": "mqtt"}`:
//!   tries the connection with the real credentials and reports each check
//!   (`probe.rs`). One test at a time;
//! - `POST /api/setup/source` and `POST /api/setup/source/delete`: add, change
//!   or remove a source kept in the data folder's `sources.toml` (`edit.rs`),
//!   and store its tokens there. Needs a data folder. A change is tested first
//!   and only saved when the test passes (or the person insists). It takes
//!   effect after a restart; `bridge.toml` is never touched.
//!
//! The page is off unless `[setup] enabled = true`. It has no login: whoever
//! can reach the port can use it, so it belongs on a trusted network only (the
//! bridge warns at startup). Secrets are never sent to the browser and a page
//! from another site cannot make changes (the `Origin` check). The deprecated
//! `[setup] token_file` still turns a login on, with its own token, cookie
//! (`SESSION_S`) and failure limit, apart from the dashboard's.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{Notify, Semaphore};

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
use crate::store::Store;
use crate::supervisor::Supervisor;

pub mod edit;

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

/// The optional login (the deprecated `[setup] token_file`).
struct Auth {
    token: [u8; 32],
    sessions: Sessions,
    limiter: Limiter,
}

struct Shared {
    auth: Option<Auth>,
    /// The config the bridge runs with, for connection tests.
    config: Config,
    /// What `bridge.toml` alone says: the data folder's sources are added to it.
    base: Config,
    /// The data folder, when there is one. Without it the page cannot save.
    store: Option<Store>,
    /// Starts, stops and replaces sources when something is saved. Without it
    /// (tests) the sources that started with the bridge are the only ones that run.
    supervisor: Option<Supervisor>,
    started: BTreeSet<String>,
    /// A saved change could not be applied while running: it needs a restart.
    changed: AtomicBool,
    /// `[setup] allow_restart`: the page may ask the bridge to restart itself.
    allow_restart: bool,
    /// Signalled by the restart button; the main loop shuts down cleanly.
    restart: Arc<Notify>,
    /// One save at a time.
    saving: tokio::sync::Mutex<()>,
    /// One connection test at a time: each one makes real requests.
    testing: Semaphore,
    mqtt_enabled: bool,
    mqtt: MqttLink,
    /// Source id (`github:main`) to health name, kept current by the main loop.
    health: Mutex<BTreeMap<String, &'static str>>,
}

impl Setup {
    /// A setup page behind its own token (the deprecated `[setup] token_file`).
    pub fn new(
        config: &Config,
        token: &Secret,
        mqtt: MqttLink,
        store: Option<Store>,
        supervisor: Option<Supervisor>,
    ) -> Self {
        Self::build(config, Some(token), mqtt, store, supervisor)
    }

    /// A setup page without a login: for a trusted network only.
    pub fn open(
        config: &Config,
        mqtt: MqttLink,
        store: Option<Store>,
        supervisor: Option<Supervisor>,
    ) -> Self {
        Self::build(config, None, mqtt, store, supervisor)
    }

    fn build(
        config: &Config,
        token: Option<&Secret>,
        mqtt: MqttLink,
        store: Option<Store>,
        supervisor: Option<Supervisor>,
    ) -> Self {
        let started = source_ids(config);
        Self {
            shared: Arc::new(Shared {
                auth: token.map(|token| Auth {
                    token: digest(token.expose()),
                    sessions: Sessions::new("setup", token.expose(), SESSION_S),
                    limiter: Limiter::default(),
                }),
                config: config.clone(),
                base: config.without_overlay(),
                store,
                supervisor,
                started,
                changed: AtomicBool::new(false),
                allow_restart: config.setup.allow_restart,
                restart: Arc::new(Notify::new()),
                saving: tokio::sync::Mutex::new(()),
                testing: Semaphore::new(1),
                mqtt_enabled: config.mqtt.enabled,
                mqtt,
                health: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Fires when the page asked for a restart (and `allow_restart` is on).
    pub fn restart_signal(&self) -> Arc<Notify> {
        self.shared.restart.clone()
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
            .route("/api/setup/test", post(test))
            .route("/api/setup/source", post(save_source))
            .route("/api/setup/source/delete", post(delete_source))
            .route("/api/setup/mqtt", post(save_mqtt))
            .route("/api/setup/restart", post(restart))
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
    // Without a token there is nothing to log in to.
    let Some(auth) = &shared.auth else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let now = unix_now();
    if let Some(wait) = auth.limiter.blocked(now) {
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
    if !same(&auth.token, &digest(&given)) {
        auth.limiter.record_failure(now);
        tracing::warn!("setup login with a wrong token");
        tokio::time::sleep(Duration::from_millis(400)).await;
        return (StatusCode::UNAUTHORIZED, "token wrong").into_response();
    }
    tracing::info!("setup login");
    let cookie = session::set_cookie(
        session::SETUP_COOKIE,
        &auth.sessions.issue(now),
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
    if !logged_in(&shared, &headers) {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    }
    let health = shared
        .health
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    // What is saved on disk, which can be ahead of what runs.
    let effective = effective_config(&shared);
    let mut out = describe(&effective);
    let can_edit = shared.store.is_some();
    let running = match &shared.supervisor {
        Some(supervisor) => supervisor.running().await,
        None => shared.started.clone(),
    };
    if let Some(sources) = out["sources"].as_array_mut() {
        for source in sources {
            let id = source["id"].as_str().unwrap_or_default().to_string();
            source["health"] = json!(health.get(&id).copied().unwrap_or("unknown"));
            source["running"] = json!(running.contains(&id));
            source["editable"] = json!(can_edit && source["origin"] == "data_folder");
        }
    }
    out["can_edit"] = json!(can_edit);
    // The dashboard is served too, so the page can link back to it.
    out["kiosk"] = json!(shared.config.kiosk.enabled);
    out["restart_needed"] = json!(shared.changed.load(Ordering::Relaxed));
    out["can_restart"] = json!(shared.allow_restart);
    out["mqtt"]["editable"] = json!(can_edit);
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

/// `bridge.toml` plus the data folder's `sources.toml` as it is on disk now;
/// the running config if there is no data folder or the file does not parse.
fn effective_config(shared: &Shared) -> Config {
    shared
        .store
        .as_ref()
        .and_then(|store| store.read_sources().ok())
        .and_then(|text| shared.base.clone().with_overlay(&text).ok())
        .unwrap_or_else(|| shared.config.clone())
}

/// `kind:name` of every source in a config.
fn source_ids(config: &Config) -> BTreeSet<String> {
    let s = &config.source;
    let mut ids = BTreeSet::new();
    ids.extend(s.gitea.iter().map(|c| format!("gitea:{}", c.name)));
    ids.extend(s.github.iter().map(|c| format!("github:{}", c.name)));
    ids.extend(
        s.azure_devops
            .iter()
            .map(|c| format!("azure_devops:{}", c.name)),
    );
    ids.extend(
        s.prometheus
            .iter()
            .map(|c| format!("prometheus:{}", c.name)),
    );
    ids
}

/// Make the running sources match the saved config. `Err` says why source
/// `id` could not be started (or that there is no supervisor); then the change
/// is saved but needs a restart.
async fn apply(shared: &Shared, config: &Config, id: &str) -> Result<(), String> {
    let Some(supervisor) = &shared.supervisor else {
        shared.changed.store(true, Ordering::Relaxed);
        return Err("this bridge cannot apply changes while running".into());
    };
    let outcome = supervisor.reconcile(config, &[id.to_string()]).await;
    match outcome.failed.into_iter().find(|(failed, _)| failed == id) {
        Some((_, reason)) => {
            shared.changed.store(true, Ordering::Relaxed);
            Err(reason)
        }
        None => Ok(()),
    }
}

/// The longest a connection test may take as a whole.
const TEST_TIMEOUT: Duration = Duration::from_secs(45);

async fn test(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !session::same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
    }
    if !logged_in(&shared, &headers) {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    }
    let id = (body.len() <= 1024)
        .then(|| serde_json::from_slice::<Value>(&body).ok())
        .flatten()
        .and_then(|v| v.get("id")?.as_str().map(str::to_string));
    let Some(id) = id else {
        return (StatusCode::BAD_REQUEST, "expected {\"id\": \"...\"}").into_response();
    };
    let Ok(_permit) = shared.testing.try_acquire() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "another test is still running",
        )
            .into_response();
    };
    tracing::info!(?id, "setup: connection test");
    let run = async {
        if id == "mqtt" {
            Some(crate::probe::mqtt(&effective_config(&shared).mqtt).await)
        } else {
            crate::probe::source(&effective_config(&shared), &id).await
        }
    };
    let report = match tokio::time::timeout(TEST_TIMEOUT, run).await {
        Ok(Some(report)) => report,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such source").into_response(),
        Err(_) => return (StatusCode::GATEWAY_TIMEOUT, "the test took too long").into_response(),
    };
    tracing::info!(?id, ok = report.ok, "setup: connection test done");
    let mut response = json!({ "id": id, "ok": report.ok, "checks": report.checks })
        .to_string()
        .into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn logged_in(shared: &Shared, headers: &HeaderMap) -> bool {
    match &shared.auth {
        None => true,
        Some(auth) => session::cookie_value(headers, session::SETUP_COOKIE)
            .is_some_and(|v| auth.sessions.valid(v, unix_now())),
    }
}

/// A JSON answer with `no-store`.
fn json_response(status: StatusCode, value: Value) -> Response {
    let mut response = (status, value.to_string()).into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn refuse(status: StatusCode, message: impl std::fmt::Display) -> Response {
    json_response(status, json!({ "error": message.to_string() }))
}

/// Checks shared by the routes that change something: same origin, logged in,
/// and a data folder to write to. Gives back the parsed JSON body.
#[allow(clippy::result_large_err)]
fn change_request(shared: &Shared, headers: &HeaderMap, body: &[u8]) -> Result<Value, Response> {
    if !session::same_origin(headers) {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "cross-origin request refused",
        ));
    }
    if !logged_in(shared, headers) {
        return Err(refuse(StatusCode::UNAUTHORIZED, "login required"));
    }
    if shared.store.is_none() {
        return Err(refuse(
            StatusCode::CONFLICT,
            "no data folder is configured, so nothing can be saved (docs/data-dir.md)",
        ));
    }
    if body.len() > 16 * 1024 {
        return Err(refuse(StatusCode::PAYLOAD_TOO_LARGE, "request too large"));
    }
    serde_json::from_slice(body).map_err(|_| refuse(StatusCode::BAD_REQUEST, "expected JSON"))
}

async fn save_source(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let store = shared.store.as_ref().expect("checked above");
    let text = |key: &str| {
        request
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let (kind, name) = (text("kind"), text("name"));
    let create = request
        .get("create")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let force = request
        .get("force")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let fields = request
        .get("fields")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let secrets: BTreeMap<String, String> = request
        .get("secrets")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // One save at a time, so two browsers cannot overwrite each other's file.
    let _saving = shared.saving.lock().await;
    let current = match store.read_sources() {
        Ok(current) => current,
        Err(err) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    };
    let plan = match edit::upsert(&current, &kind, &name, create, &fields, &secrets) {
        Ok(plan) => plan,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    // The whole config, as it would be, must be valid before anything is tried.
    let candidate = match shared.base.clone().with_overlay(&plan.text) {
        Ok(candidate) => candidate,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    let overrides: crate::probe::Overrides = plan.secrets.iter().cloned().collect();
    let id = format!("{kind}:{name}");
    let report = {
        let Ok(_permit) = shared.testing.try_acquire() else {
            return refuse(
                StatusCode::TOO_MANY_REQUESTS,
                "a connection test is still running",
            );
        };
        match tokio::time::timeout(
            TEST_TIMEOUT,
            crate::probe::source_with(&candidate, &id, &overrides),
        )
        .await
        {
            Ok(Some(report)) => report,
            Ok(None) => return refuse(StatusCode::NOT_FOUND, "no such source"),
            Err(_) => return refuse(StatusCode::GATEWAY_TIMEOUT, "the test took too long"),
        }
    };
    if !report.ok && !force {
        tracing::info!(?id, "setup: not saved, the connection test failed");
        return json_response(
            StatusCode::OK,
            json!({ "saved": false, "id": id, "report": report }),
        );
    }
    for (credential, secret) in &plan.secrets {
        if let Err(err) = store.write_secret(credential, secret.expose()) {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
        }
    }
    if let Err(err) = store.write_sources(&plan.text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    let applied = apply(&shared, &candidate, &id).await;
    let replaced: Vec<&str> = plan.secrets.iter().map(|(n, _)| n.as_str()).collect();
    tracing::info!(
        ?id,
        created = create,
        forced = force && !report.ok,
        secrets_written = ?replaced,
        applied = applied.is_ok(),
        "setup: source saved"
    );
    json_response(
        StatusCode::OK,
        json!({
            "saved": true, "id": id, "report": report,
            "applied": applied.is_ok(), "apply_error": applied.err(),
            "restart_needed": shared.changed.load(Ordering::Relaxed),
        }),
    )
}

async fn delete_source(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let store = shared.store.as_ref().expect("checked above");
    let text = |key: &str| {
        request
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let (kind, name) = (text("kind"), text("name"));
    let _saving = shared.saving.lock().await;
    let current = match store.read_sources() {
        Ok(current) => current,
        Err(err) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    };
    let (new_text, credentials) = match edit::remove(&current, &kind, &name) {
        Ok(done) => done,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    let candidate = match shared.base.clone().with_overlay(&new_text) {
        Ok(candidate) => candidate,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    if let Err(err) = store.write_sources(&new_text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    // Delete only the data folder's own files; a mounted credential is not ours.
    let mut removed = Vec::new();
    for credential in credentials {
        if store.has_secret(&credential) && store.remove_secret(&credential).is_ok() {
            removed.push(credential);
        }
    }
    let id = format!("{kind}:{name}");
    let applied = apply(&shared, &candidate, &id).await;
    tracing::info!(?id, secrets_removed = ?removed, applied = applied.is_ok(), "setup: source removed");
    json_response(
        StatusCode::OK,
        json!({
            "saved": true, "id": id,
            "applied": applied.is_ok(), "apply_error": applied.err(),
            "restart_needed": shared.changed.load(Ordering::Relaxed),
        }),
    )
}

async fn save_mqtt(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let store = shared.store.as_ref().expect("checked above");
    let force = request
        .get("force")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let fields = request
        .get("fields")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let secrets: BTreeMap<String, String> = request
        .get("secrets")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let _saving = shared.saving.lock().await;
    let current = match store.read_sources() {
        Ok(current) => current,
        Err(err) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    };
    let plan = match edit::set_mqtt(&current, &fields, &secrets) {
        Ok(plan) => plan,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    let candidate = match shared.base.clone().with_overlay(&plan.text) {
        Ok(candidate) => candidate,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    let overrides: crate::probe::Overrides = plan.secrets.iter().cloned().collect();
    let report = {
        let Ok(_permit) = shared.testing.try_acquire() else {
            return refuse(
                StatusCode::TOO_MANY_REQUESTS,
                "a connection test is still running",
            );
        };
        match tokio::time::timeout(
            TEST_TIMEOUT,
            crate::probe::mqtt_with(&candidate.mqtt, &overrides),
        )
        .await
        {
            Ok(report) => report,
            Err(_) => return refuse(StatusCode::GATEWAY_TIMEOUT, "the test took too long"),
        }
    };
    if !report.ok && !force {
        tracing::info!("setup: MQTT settings not saved, the connection test failed");
        return json_response(
            StatusCode::OK,
            json!({ "saved": false, "id": "mqtt", "report": report }),
        );
    }
    for (credential, secret) in &plan.secrets {
        if let Err(err) = store.write_secret(credential, secret.expose()) {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
        }
    }
    if let Err(err) = store.write_sources(&plan.text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    // The broker connection is made once at startup.
    shared.changed.store(true, Ordering::Relaxed);
    tracing::info!(
        forced = force && !report.ok,
        password_written = !plan.secrets.is_empty(),
        "setup: MQTT settings saved (apply after a restart)"
    );
    json_response(
        StatusCode::OK,
        json!({ "saved": true, "id": "mqtt", "report": report, "restart_needed": true }),
    )
}

/// Ask the bridge to shut down cleanly and exit, so its supervisor starts it
/// again with the saved settings. Off unless `[setup] allow_restart` is set.
async fn restart(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    _body: axum::body::Bytes,
) -> Response {
    if !session::same_origin(&headers) {
        return refuse(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    if !logged_in(&shared, &headers) {
        return refuse(StatusCode::UNAUTHORIZED, "login required");
    }
    if !shared.allow_restart {
        return refuse(
            StatusCode::CONFLICT,
            "restarting from the page is off ([setup] allow_restart, docs/setup-page.md)",
        );
    }
    tracing::info!("setup: restart requested from the page");
    // `notify_one` keeps the signal if the main loop is busy for a moment.
    shared.restart.notify_one();
    json_response(StatusCode::ACCEPTED, json!({ "restarting": true }))
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
            "fields": { "base_url": s.base_url, "repos": s.repos },
        }));
    }
    for s in &config.source.github {
        sources.push(json!({
            "id": format!("github:{}", s.name), "kind": "github", "name": s.name,
            "origin": origin("github", &s.name),
            "address": s.base_url, "repos": s.repos,
            "credentials": [cred("token", &s.token_file, true)],
            "fields": { "base_url": s.base_url, "repos": s.repos },
        }));
    }
    for s in &config.source.azure_devops {
        sources.push(json!({
            "id": format!("azure_devops:{}", s.name), "kind": "azure_devops", "name": s.name,
            "origin": origin("azure_devops", &s.name),
            "address": format!(
                "{}/{}",
                crate::azure_devops::api::clean_base_url(&s.base_url, &s.organization),
                s.organization
            ),
            "repos": s.projects,
            "credentials": [cred("token", &s.token_file, true)],
            "fields": {
                "organization": s.organization, "base_url": s.base_url,
                "projects": s.projects, "pull_requests": s.pull_requests,
            },
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
            "fields": {
                "url": s.url,
                "hosts": s.hosts.iter().map(|h| h.instance.clone()).collect::<Vec<_>>(),
                // A host with its own settings cannot be edited as a plain list.
                "hosts_advanced": s.hosts.iter().any(|h| {
                    h.cadvisor.is_some() || !h.expect_containers.is_empty()
                        || h.name != h.instance.rsplit_once(':').map_or(h.instance.as_str(), |(host, _)| host)
                }),
            },
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
            "origin": if config.mqtt_before_overlay.is_some() { "data_folder" } else { "bridge_toml" },
        },
        "sources": sources,
    })
}

#[cfg(test)]
mod tests;
