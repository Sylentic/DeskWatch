//! The setup page: a place to see how the bridge is configured, so it can
//! later be changed without editing files (docs/setup-page.md).
//!
//! This step is read-only. It serves:
//!
//! - `GET /setup`, `/setup.js`, `/setup.css`: the page;
//! - `GET /api/setup/status`: sources, credentials (only whether each one
//!   exists, never its value), MQTT, the data folder and live source health;
//! - `POST /api/setup/test` with `{"id": "github:main"}` or `{"id": "mqtt"}`:
//!   tries the connection with the real credentials and reports each check
//!   (`probe.rs`). One test at a time;
//! - `POST /api/setup/source`, `.../source/delete`, `.../source/hide`, `/api/setup/mqtt/delete`
//!   and `/api/setup/secret/delete`: add, change
//!   or remove a source kept in the data folder's `sources.toml` (`edit.rs`),
//!   and store its tokens there. Needs a data folder. A change is tested first
//!   and only saved when the test passes (or the person insists). It takes
//!   effect after a restart; `bridge.toml` is never touched.
//!
//! The page is off unless `[setup] enabled = true`. It has no login: whoever
//! can reach the port can use it, so it belongs on a trusted network only (the
//! bridge warns at startup). Secrets are never sent to the browser and a page
//! from another site cannot make changes (the `Origin` check).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, Semaphore, mpsc};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::alerts::{AlertMessage, TEST_ALERT_ID};
use crate::config::Config;
use crate::health::MqttLink;
use crate::kiosk::asset;
use crate::kiosk::origin;
use crate::mqtt::Inbound;
use crate::source::credential_status;
use crate::store::Store;
use crate::supervisor::Supervisor;

pub mod backup;
pub mod edit;

const INDEX_HTML: &str = include_str!("../setup/index.html");
const SETUP_CSS: &str = include_str!("../setup/setup.css");
const SETUP_JS: &str = include_str!("../setup/setup.js");

/// Everything the status endpoint needs, shared with the main loop.
#[derive(Clone)]
pub struct Setup {
    shared: Arc<Shared>,
}

struct Shared {
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
    /// Differs on every start of the process, so a page that asked for a
    /// restart can tell the bridge it is talking to is the new one.
    boot: u128,
    /// One save at a time.
    saving: tokio::sync::Mutex<()>,
    /// One connection test at a time: each one makes real requests.
    testing: Semaphore,
    mqtt_enabled: bool,
    mqtt: MqttLink,
    /// Where the test alert goes: the main loop, like an alert from MQTT.
    alert_tx: Option<mpsc::Sender<Inbound>>,
    /// A newer release found by the daily check, for the Bridge section.
    update: crate::update::UpdateNotice,
    /// Source id (`github:main`) to health name, kept current by the main loop.
    health: Mutex<BTreeMap<String, &'static str>>,
}

impl Setup {
    /// A setup page without a login: for a trusted network only.
    pub fn new(
        config: &Config,
        mqtt: MqttLink,
        store: Option<Store>,
        supervisor: Option<Supervisor>,
    ) -> Self {
        let started = source_ids(config);
        Self {
            shared: Arc::new(Shared {
                config: config.clone(),
                base: config.without_overlay(),
                store,
                supervisor,
                started,
                changed: AtomicBool::new(false),
                allow_restart: config.setup.allow_restart,
                restart: Arc::new(Notify::new()),
                boot: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
                saving: tokio::sync::Mutex::new(()),
                testing: Semaphore::new(1),
                mqtt_enabled: config.mqtt.enabled,
                mqtt,
                alert_tx: None,
                update: crate::update::UpdateNotice::default(),
                health: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Let the page's test button raise its fixed test alert. Call before
    /// `router`.
    pub fn with_alert_sender(mut self, tx: mpsc::Sender<Inbound>) -> Self {
        if let Some(shared) = Arc::get_mut(&mut self.shared) {
            shared.alert_tx = Some(tx);
        }
        self
    }

    /// Show the newer release the daily check found. Call before `router`.
    pub fn with_update(mut self, update: crate::update::UpdateNotice) -> Self {
        if let Some(shared) = Arc::get_mut(&mut self.shared) {
            shared.update = update;
        }
        self
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
            .route("/api/setup/status", get(status))
            .route("/api/setup/test", post(test))
            .route("/api/setup/source", post(save_source))
            .route("/api/setup/source/delete", post(delete_source))
            .route("/api/setup/source/hide", post(hide_source))
            .route("/api/setup/mqtt", post(save_mqtt))
            .route("/api/setup/mqtt/delete", post(delete_mqtt))
            .route("/api/setup/secret/delete", post(delete_secret))
            .route("/api/setup/kiosk", post(save_kiosk))
            .route("/api/setup/alerts", post(save_alerts))
            .route("/api/setup/alerts/test", post(test_alert))
            .route("/api/setup/updates", post(save_updates))
            .route("/api/setup/restart", post(restart))
            .route("/api/setup/export", get(export_plain).post(export_locked))
            .route("/api/setup/import/check", post(import_check))
            .route("/api/setup/import", post(import_apply))
            .with_state(self.shared.clone())
    }
}

async fn status(State(shared): State<Arc<Shared>>) -> Response {
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
    // Sources of `bridge.toml` that were switched off from the page: still
    // listed, so they can be switched on again.
    if let Some(sources) = out["sources"].as_array_mut() {
        for source in sources.iter_mut() {
            let from_toml = source["origin"] == "bridge_toml";
            source["hideable"] = json!(can_edit && from_toml);
            source["hidden"] = json!(false);
        }
        for mut source in describe(&shared.base)["sources"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let id = source["id"].as_str().unwrap_or_default().to_string();
            if effective.hidden_sources.contains(&id) {
                source["hidden"] = json!(true);
                source["hideable"] = json!(can_edit);
                source["editable"] = json!(false);
                source["running"] = json!(false);
                source["health"] = json!("hidden");
                sources.push(source);
            }
        }
    }
    out["secrets"] = json!(stored_secrets(&shared, &effective));
    out["boot"] = json!(shared.boot.to_string());
    out["can_edit"] = json!(can_edit);
    // The dashboard is served too, so the page can link back to it.
    out["kiosk"] = json!(shared.config.kiosk.enabled);
    out["restart_needed"] = json!(shared.changed.load(Ordering::Relaxed));
    out["can_restart"] = json!(shared.allow_restart);
    out["mqtt"]["editable"] = json!(can_edit);
    // Is there anything saved by the page that can be taken away again?
    let saved_mqtt = shared
        .store
        .as_ref()
        .and_then(|store| store.read_sources().ok())
        .is_some_and(|text| {
            text.parse::<toml::Table>()
                .is_ok_and(|t| t.contains_key("mqtt"))
        });
    out["mqtt"]["removable"] = json!(can_edit && saved_mqtt);
    out["mqtt"]["has_login"] = json!(effective.mqtt.username.is_some());
    out["host_view_editable"] = json!(can_edit);
    out["alerts"] = json!({
        "editable": can_edit,
        "test_available": shared.alert_tx.is_some(),
        "mqtt_enabled": shared.mqtt_enabled,
        "mqtt_connected": shared.mqtt_enabled && shared.mqtt.is_up(),
        "ticker": effective.alerts.ticker,
        "ticker_speed": effective.alerts.ticker_speed,
        "show_source": effective.alerts.show_source,
        "dismiss": effective.alerts.dismiss,
        "http_input": effective.alerts.http_input,
        "max_active": effective.alerts.max_active,
        "enabled": effective.alerts.enabled,
    });
    out["update"] = json!(shared.update.get());
    out["updates"] = json!({ "editable": can_edit, "check": effective.updates.check });
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
pub(crate) fn source_ids(config: &Config) -> BTreeSet<String> {
    let s = &config.source;
    let mut ids = BTreeSet::new();
    ids.extend(s.gitea.iter().map(|c| format!("gitea:{}", c.name)));
    ids.extend(s.github.iter().map(|c| format!("github:{}", c.name)));
    ids.extend(s.gitlab.iter().map(|c| format!("gitlab:{}", c.name)));
    ids.extend(s.bitbucket.iter().map(|c| format!("bitbucket:{}", c.name)));
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
    if !origin::same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
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

/// Checks shared by the routes that change something: same origin
/// and a data folder to write to. Gives back the parsed JSON body.
#[allow(clippy::result_large_err)]
fn change_request(shared: &Shared, headers: &HeaderMap, body: &[u8]) -> Result<Value, Response> {
    change_request_up_to(shared, headers, body, 16 * 1024)
}

/// `change_request` for a body that may be bigger (a backup file).
#[allow(clippy::result_large_err)]
fn change_request_up_to(
    shared: &Shared,
    headers: &HeaderMap,
    body: &[u8],
    limit: usize,
) -> Result<Value, Response> {
    if !origin::same_origin(headers) {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "cross-origin request refused",
        ));
    }
    if shared.store.is_none() {
        return Err(refuse(
            StatusCode::CONFLICT,
            "no data folder is configured, so nothing can be saved (docs/data-dir.md)",
        ));
    }
    if body.len() > limit {
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

/// Save how the dashboard's built-in layout shows the hosts. There is nothing
/// to test, and the layout is built at startup, so it applies after a restart.
async fn save_kiosk(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let store = shared.store.as_ref().expect("checked above");
    let view = request
        .get("host_view")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let _saving = shared.saving.lock().await;
    let current = match store.read_sources() {
        Ok(current) => current,
        Err(err) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    };
    let text = match edit::set_kiosk(&current, view) {
        Ok(text) => text,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    if let Err(err) = shared.base.clone().with_overlay(&text) {
        return refuse(StatusCode::BAD_REQUEST, format!("{err:#}"));
    }
    if let Err(err) = store.write_sources(&text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    shared.changed.store(true, Ordering::Relaxed);
    tracing::info!(
        host_view = view,
        "setup: host view saved (apply after a restart)"
    );
    json_response(
        StatusCode::OK,
        json!({ "saved": true, "id": "kiosk", "restart_needed": true }),
    )
}

/// Save the ticker settings of `[alerts]`. The ticker is part of the layout
/// and the limit lives in the composer, so it applies after a restart.
async fn save_alerts(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(fields) = request.as_object() else {
        return refuse(StatusCode::BAD_REQUEST, "expected a JSON object");
    };
    let store = shared.store.as_ref().expect("checked above");
    let _saving = shared.saving.lock().await;
    let current = match store.read_sources() {
        Ok(current) => current,
        Err(err) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    };
    let text = match edit::set_alerts(&current, fields) {
        Ok(text) => text,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    if let Err(err) = shared.base.clone().with_overlay(&text) {
        return refuse(StatusCode::BAD_REQUEST, format!("{err:#}"));
    }
    if let Err(err) = store.write_sources(&text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    shared.changed.store(true, Ordering::Relaxed);
    tracing::info!("setup: alert settings saved (apply after a restart)");
    json_response(
        StatusCode::OK,
        json!({ "saved": true, "id": "alerts", "restart_needed": true }),
    )
}

/// Save the update check switch (`{"check": true|false}`). The check starts
/// with the bridge, so it applies after a restart.
async fn save_updates(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(check) = request.get("check").and_then(Value::as_bool) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "expected {\"check\": true or false}",
        );
    };
    let store = shared.store.as_ref().expect("checked above");
    let _saving = shared.saving.lock().await;
    let current = match store.read_sources() {
        Ok(current) => current,
        Err(err) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    };
    let text = match edit::set_updates(&current, check) {
        Ok(text) => text,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    if let Err(err) = shared.base.clone().with_overlay(&text) {
        return refuse(StatusCode::BAD_REQUEST, format!("{err:#}"));
    }
    if let Err(err) = store.write_sources(&text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    shared.changed.store(true, Ordering::Relaxed);
    tracing::info!(check, "setup: update check saved (apply after a restart)");
    json_response(
        StatusCode::OK,
        json!({ "saved": true, "id": "updates", "restart_needed": true }),
    )
}

/// The page's test button: raise the fixed test alert (`{"action":"send"}`) or
/// take it away again (`{"action":"clear"}`). The page sends no alert text, so
/// this cannot create any other alert. It needs no data folder.
async fn test_alert(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !origin::same_origin(&headers) {
        return refuse(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    let Some(tx) = &shared.alert_tx else {
        return refuse(StatusCode::CONFLICT, "this bridge cannot raise alerts");
    };
    let action = (body.len() <= 256)
        .then(|| serde_json::from_slice::<Value>(&body).ok())
        .flatten()
        .and_then(|v| v.get("action")?.as_str().map(str::to_string));
    let message = match action.as_deref() {
        Some("send") => AlertMessage::setup_test(),
        Some("clear") => AlertMessage::clear(TEST_ALERT_ID),
        _ => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "expected {\"action\": \"send\"} or {\"action\": \"clear\"}",
            );
        }
    };
    if tx.try_send(Inbound::Alert(message)).is_err() {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "the bridge is busy, try again",
        );
    }
    tracing::info!(?action, "setup: test alert");
    json_response(StatusCode::OK, json!({ "ok": true }))
}

/// Ask the bridge to shut down cleanly and exit, so its supervisor starts it
/// again with the saved settings. Off unless `[setup] allow_restart` is set.
async fn restart(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    _body: axum::body::Bytes,
) -> Response {
    if !origin::same_origin(&headers) {
        return refuse(StatusCode::FORBIDDEN, "cross-origin request refused");
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

/// Who uses which credential: a source (also one that is switched off), the
/// broker login.
fn credential_owners(shared: &Shared, effective: &Config) -> BTreeMap<String, String> {
    let mut owners = BTreeMap::new();
    let mut add = |config: &Config, hidden_only: bool| {
        let described = describe(config);
        for source in described["sources"].as_array().into_iter().flatten() {
            let id = source["id"].as_str().unwrap_or_default();
            if hidden_only && !effective.hidden_sources.contains(id) {
                continue;
            }
            for c in source["credentials"].as_array().into_iter().flatten() {
                if let Some(name) = c["name"].as_str() {
                    let who = if hidden_only {
                        format!("{id} (switched off)")
                    } else {
                        id.to_string()
                    };
                    owners.insert(name.to_string(), who);
                }
            }
        }
    };
    add(effective, false);
    add(&shared.base, true);
    if let Some(file) = &effective.mqtt.password_file {
        owners.insert(file.clone(), "the MQTT login".to_string());
    }
    owners
}

/// The secrets in the data folder with what uses each one. Names only.
fn stored_secrets(shared: &Shared, effective: &Config) -> Vec<Value> {
    let Some(store) = &shared.store else {
        return Vec::new();
    };
    let owners = credential_owners(shared, effective);
    store
        .list_secrets()
        .into_iter()
        .map(|name| {
            let used_by = owners.get(&name).cloned();
            json!({ "name": name, "used_by": used_by })
        })
        .collect()
}

/// Read `kind` and `name` from a request.
fn kind_and_name(request: &Value) -> (String, String) {
    let text = |key: &str| {
        request
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    (text("kind"), text("name"))
}

/// Switch a `bridge.toml` source off or on again. `bridge.toml` is never
/// touched: the data folder only remembers that the source is left out.
async fn hide_source(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let store = shared.store.as_ref().expect("checked above");
    let (kind, name) = kind_and_name(&request);
    let hide = request
        .get("hidden")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let id = format!("{kind}:{name}");
    if !source_ids(&shared.base).contains(&id) {
        return refuse(
            StatusCode::BAD_REQUEST,
            format!(
                "{id} is not a source from bridge.toml (sources from the data folder are removed instead)"
            ),
        );
    }
    let _saving = shared.saving.lock().await;
    let current = match store.read_sources() {
        Ok(current) => current,
        Err(err) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    };
    let text = match edit::set_hidden(&current, &kind, &name, hide) {
        Ok(text) => text,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    let candidate = match shared.base.clone().with_overlay(&text) {
        Ok(candidate) => candidate,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    if let Err(err) = store.write_sources(&text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    let applied = apply(&shared, &candidate, &id).await;
    tracing::info!(
        ?id,
        hidden = hide,
        applied = applied.is_ok(),
        "setup: source switched {}",
        if hide { "off" } else { "on" }
    );
    json_response(
        StatusCode::OK,
        json!({
            "saved": true, "id": id,
            "applied": applied.is_ok(), "apply_error": applied.err(),
            "restart_needed": shared.changed.load(Ordering::Relaxed),
        }),
    )
}

/// Take saved broker settings away: `{"what": "login"}` or `{"what": "settings"}`.
async fn delete_mqtt(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let store = shared.store.as_ref().expect("checked above");
    let what = match request.get("what").and_then(Value::as_str) {
        Some("login") => edit::MqttRemoval::Login,
        Some("settings") => edit::MqttRemoval::Settings,
        _ => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "expected {\"what\": \"login\"} or {\"what\": \"settings\"}",
            );
        }
    };
    let _saving = shared.saving.lock().await;
    let current = match store.read_sources() {
        Ok(current) => current,
        Err(err) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    };
    let (text, credentials) = match edit::remove_mqtt(&current, what) {
        Ok(done) => done,
        Err(err) => return refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    if let Err(err) = shared.base.clone().with_overlay(&text) {
        return refuse(StatusCode::BAD_REQUEST, format!("{err:#}"));
    }
    if let Err(err) = store.write_sources(&text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    let mut removed = Vec::new();
    for credential in credentials {
        if store.has_secret(&credential) && store.remove_secret(&credential).is_ok() {
            removed.push(credential);
        }
    }
    // The broker connection is made once at startup.
    shared.changed.store(true, Ordering::Relaxed);
    tracing::info!(?what, secrets_removed = ?removed, "setup: saved MQTT settings removed (apply after a restart)");
    json_response(
        StatusCode::OK,
        json!({ "saved": true, "id": "mqtt", "restart_needed": true }),
    )
}

/// Delete a stored secret that nothing uses any more.
async fn delete_secret(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let store = shared.store.as_ref().expect("checked above");
    let name = request
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let _saving = shared.saving.lock().await;
    if !store.has_secret(&name) {
        return refuse(StatusCode::NOT_FOUND, "no such stored secret");
    }
    let effective = effective_config(&shared);
    if let Some(owner) = credential_owners(&shared, &effective).get(&name) {
        return refuse(
            StatusCode::CONFLICT,
            format!("{name} is still used by {owner}; remove or change that first"),
        );
    }
    if let Err(err) = store.remove_secret(&name) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    tracing::info!(secret = %name, "setup: unused stored secret removed");
    json_response(StatusCode::OK, json!({ "removed": true, "name": name }))
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
            "fields": { "base_url": s.base_url, "repos": s.repos, "container": s.container },
        }));
    }
    for s in &config.source.github {
        sources.push(json!({
            "id": format!("github:{}", s.name), "kind": "github", "name": s.name,
            "origin": origin("github", &s.name),
            "address": s.base_url, "repos": s.repos,
            "credentials": [cred("token", &s.token_file, true)],
            "fields": { "base_url": s.base_url, "repos": s.repos, "container": s.container },
        }));
    }
    for s in &config.source.gitlab {
        sources.push(json!({
            "id": format!("gitlab:{}", s.name), "kind": "gitlab", "name": s.name,
            "origin": origin("gitlab", &s.name),
            "address": s.base_url, "repos": s.projects,
            "credentials": [cred("token", &s.token_file, true)],
            "fields": { "base_url": s.base_url, "projects": s.projects, "container": s.container },
        }));
    }
    for s in &config.source.bitbucket {
        sources.push(json!({
            "id": format!("bitbucket:{}", s.name), "kind": "bitbucket", "name": s.name,
            "origin": origin("bitbucket", &s.name),
            "address": s.base_url, "repos": s.repos,
            "credentials": [cred("token", &s.token_file, true)],
            "fields": { "repos": s.repos, "container": s.container },
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
            "credentials": [match s.auth {
                crate::config::AzureDevopsAuth::Pat => cred("token", &s.token_file, true),
                crate::config::AzureDevopsAuth::Entra => {
                    cred("client secret", &s.client_secret_file, true)
                }
            }],
            "fields": {
                "organization": s.organization, "base_url": s.base_url,
                "projects": s.projects, "pull_requests": s.pull_requests,
                "container": s.container,
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
        "host_view": config.kiosk.host_view,
        "host_view_origin": if config.host_view_before_overlay.is_some() { "data_folder" } else { "bridge_toml" },
        "sources": sources,
    })
}

#[cfg(test)]
mod tests;

// ---- Backup (backup.rs) ----

/// The most a backup file may be: sources and layouts are small.
const BACKUP_LIMIT: usize = 1024 * 1024;

fn download(bytes: Vec<u8>) -> Response {
    let day = time::OffsetDateTime::now_utc().date();
    let mut response = Response::new(axum::body::Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"deskwatch-{day}.deskwatch\""
    )) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// The backup without secrets: safe to keep and share.
async fn export_plain(State(shared): State<Arc<Shared>>) -> Response {
    let Some(store) = shared.store.as_ref() else {
        return refuse(
            StatusCode::CONFLICT,
            "no data folder is configured, so there is nothing to export (docs/data-dir.md)",
        );
    };
    match backup::export(store, None) {
        Ok(bytes) => {
            tracing::info!("setup: backup downloaded (no secrets)");
            download(bytes)
        }
        Err(err) => refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
    }
}

/// The backup with the stored secrets, encrypted with the given password.
async fn export_locked(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request(&shared, &headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let password = request
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let shared2 = shared.clone();
    let made = tokio::task::spawn_blocking(move || {
        backup::export(
            shared2.store.as_ref().expect("checked above"),
            Some(&password),
        )
    })
    .await;
    match made {
        Ok(Ok(bytes)) => {
            tracing::info!("setup: backup downloaded (secrets locked with a password)");
            download(bytes)
        }
        Ok(Err(err)) => refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
        Err(_) => refuse(StatusCode::INTERNAL_SERVER_ERROR, "the backup task failed"),
    }
}

/// Open the file in a request: `{ "file": "<text>", "password": "<optional>" }`.
async fn open_backup(request: &Value) -> Result<backup::Backup, Response> {
    let file = request
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .as_bytes()
        .to_vec();
    let password = request
        .get("password")
        .and_then(Value::as_str)
        .map(str::to_string);
    let opened = tokio::task::spawn_blocking(move || backup::open(&file, password.as_deref()))
        .await
        .map_err(|_| refuse(StatusCode::INTERNAL_SERVER_ERROR, "the backup task failed"))?;
    opened.map_err(|err| match err {
        backup::OpenError::NeedsPassword => {
            json_response(StatusCode::OK, json!({ "needs_password": true }))
        }
        backup::OpenError::WrongPassword => refuse(
            StatusCode::BAD_REQUEST,
            "wrong password, or the file was changed",
        ),
        backup::OpenError::Invalid(err) => refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    })
}

/// What is in a backup file, without changing anything.
async fn import_check(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request_up_to(&shared, &headers, &body, BACKUP_LIMIT) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let opened = match open_backup(&request).await {
        Ok(opened) => opened,
        Err(response) => return response,
    };
    match backup::check(&opened, &shared.base) {
        Ok(summary) => json_response(StatusCode::OK, json!({ "summary": summary })),
        Err(err) => refuse(StatusCode::BAD_REQUEST, format!("{err:#}")),
    }
}

/// Restore a backup file into the data folder. It takes effect after a restart.
async fn import_apply(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request = match change_request_up_to(&shared, &headers, &body, BACKUP_LIMIT) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let opened = match open_backup(&request).await {
        Ok(opened) => opened,
        Err(response) => return response,
    };
    if let Err(err) = backup::check(&opened, &shared.base) {
        return refuse(StatusCode::BAD_REQUEST, format!("{err:#}"));
    }
    let store = shared.store.as_ref().expect("checked above");
    let _saving = shared.saving.lock().await;
    if let Err(err) = backup::restore(store, &opened) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
    }
    shared.changed.store(true, Ordering::Relaxed);
    tracing::info!(
        secrets = opened.secrets.len(),
        "setup: backup restored, restart needed"
    );
    json_response(
        StatusCode::OK,
        json!({ "restored": true, "restart_needed": true }),
    )
}
