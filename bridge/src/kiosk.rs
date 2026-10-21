//! The kiosk dashboard: a web page for a big screen (a Raspberry Pi in a
//! browser kiosk, a wall tablet) and the data it draws.
//!
//! The page itself is plain HTML, CSS and JavaScript in `bridge/kiosk/`,
//! compiled into the binary, so there is nothing to install next to it and
//! it works on a LAN with no internet. It is served by the shared HTTP
//! listener (`hooks.rs`) together with three read-only routes:
//!
//! - `GET /api/kiosk`: the latest snapshot as JSON;
//! - `GET /api/kiosk/ws`: a WebSocket that sends a snapshot whenever
//!   something changes, and at least every `HEARTBEAT_S` seconds, so the page
//!   can tell "nothing new" from "the bridge is gone". At most
//!   `kiosk.max_ws_clients` at a time (more get 503, the page then polls), and
//!   a client that does not take a frame within `WRITE_TIMEOUT_S` is dropped;
//! - `GET /`, `/kiosk.css`, `/kiosk.js`: the page.
//!
//! The snapshot is built from the same facts the composer uses for the ESP
//! panel, so both screens always agree. Nothing here changes any state.
//! There is no login: the page and its data are for a trusted network (see
//! docs/kiosk.md). The WebSocket still refuses an `Origin` that is not this
//! host, so a page on another site cannot read it through the browser.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Map, Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};

use crate::alerts::AlertMessage;
use crate::composer::Composer;
use crate::config::{AlertsConfig, KioskConfig, PanelConfig};
use crate::model::{Badges, Level, Screen};
use crate::mqtt::Inbound;

pub mod origin;

/// Version of the snapshot layout, the `"v"` field. Separate from the MQTT
/// schema version: the two outputs evolve on their own.
pub const SNAPSHOT_VERSION: u8 = 1;

/// A snapshot goes out at least this often even when nothing changed.
/// The page treats 15 s without one as a lost connection.
pub const HEARTBEAT_S: u64 = 5;

/// A WebSocket client that has not accepted a frame after this long is
/// stalled or gone. It is dropped so it cannot hold its slot for ever; the
/// page reconnects by itself.
pub const WRITE_TIMEOUT_S: u64 = 10;

const INDEX_HTML: &str = include_str!("../kiosk/index.html");
const KIOSK_CSS: &str = include_str!("../kiosk/kiosk.css");
const KIOSK_JS: &str = include_str!("../kiosk/kiosk.js");

/// The page has no inline scripts and loads nothing from other origins.
/// `style-src` allows inline styles because the widgets set bar widths with
/// `style` attributes.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   connect-src 'self'; img-src 'self' data:; font-src 'self'; base-uri 'none'; form-action 'none'";

/// The latest snapshot, as JSON text.
type Frame = Arc<str>;

/// What people clicked away on a screen. It is kept on the bridge, so a
/// dismissal holds on every screen, and in memory only: a restart shows the
/// failures again.
#[derive(Default)]
struct Clicked {
    /// Failed runs (`key@started`) taken off the list and the badge.
    failed: HashSet<String>,
    /// Sources (id) whose "source down" pill is muted until they work again.
    muted: HashSet<String>,
}

/// More than this many ids in one set means something is wrong with the sender.
const MAX_CLICKED: usize = 500;

type SharedClicked = Arc<Mutex<Clicked>>;

/// Builds snapshots and hands them to the HTTP routes.
pub struct Kiosk {
    layout: Value,
    tx: watch::Sender<Frame>,
    /// One permit per allowed WebSocket client.
    slots: Arc<Semaphore>,
    /// The last snapshot without its timestamp, to see what changed.
    last: Option<Value>,
    /// Unix seconds when the last snapshot went out.
    sent_at: u64,
    /// Where a dismissed alert goes, when `[alerts] dismiss` is on.
    alert_tx: Option<mpsc::Sender<Inbound>>,
    /// A newer release found by the daily check, shown as a chip in the header.
    update: Option<crate::update::UpdateNotice>,
    /// Dismissed failed runs and muted sources, shared with the routes.
    clicked: SharedClicked,
}

impl Kiosk {
    pub fn new(config: &KioskConfig) -> Self {
        let layout = layout_json(config);
        let first =
            json!({ "v": SNAPSHOT_VERSION, "build": env!("CARGO_PKG_VERSION"), "layout": layout });
        let (tx, _) = watch::channel(Frame::from(first.to_string()));
        Self {
            layout,
            tx,
            slots: Arc::new(Semaphore::new(config.max_ws_clients.into())),
            last: None,
            sent_at: 0,
            alert_tx: None,
            update: None,
            clicked: SharedClicked::default(),
        }
    }

    /// Show a chip when the daily check finds a newer release.
    pub fn with_update(mut self, update: crate::update::UpdateNotice) -> Self {
        self.update = Some(update);
        self
    }

    /// Put the ticker settings in the layout. With `dismiss` on, the page may
    /// dismiss an alert; that goes to the main loop as a clear message.
    pub fn with_alerts(mut self, config: &AlertsConfig, tx: mpsc::Sender<Inbound>) -> Self {
        self.layout["alerts"] = json!({
            "ticker": config.ticker, "speed": config.ticker_speed.factor(),
            "show_source": config.show_source, "dismiss": config.dismiss,
        });
        if config.dismiss {
            self.alert_tx = Some(tx);
        }
        self.send_layout();
        self
    }

    /// Send the layout again as the first snapshot (before any real one).
    fn send_layout(&self) {
        let first = json!({
            "v": SNAPSHOT_VERSION, "build": env!("CARGO_PKG_VERSION"), "layout": self.layout
        });
        self.tx.send_replace(Frame::from(first.to_string()));
    }

    /// Show a "Setup" link in the page header (when the setup page is served).
    /// It travels in the layout part of every snapshot as `"setup": true`.
    pub fn with_setup_link(mut self, on: bool) -> Self {
        if on {
            self.layout["setup"] = json!(true);
            self.send_layout();
        }
        self
    }

    /// Routes for the shared listener. Cheap to call once at startup.
    pub fn router(&self) -> Router {
        let state = AppState {
            rx: self.tx.subscribe(),
            slots: self.slots.clone(),
            alert_tx: self.alert_tx.clone(),
            clicked: self.clicked.clone(),
        };
        let api = Router::new()
            .route("/api/kiosk", get(snapshot_handler))
            .route("/api/kiosk/ws", get(ws_handler))
            .route("/api/kiosk/alert/dismiss", post(dismiss_handler))
            .route("/api/kiosk/failed/dismiss", post(dismiss_failed_handler))
            .route("/api/kiosk/source/mute", post(mute_source_handler))
            .route_layer(middleware::from_fn(require_same_origin));
        Router::new()
            .route("/", get(|| asset("text/html; charset=utf-8", INDEX_HTML)))
            .route(
                "/index.html",
                get(|| asset("text/html; charset=utf-8", INDEX_HTML)),
            )
            .route(
                "/kiosk.css",
                get(|| asset("text/css; charset=utf-8", KIOSK_CSS)),
            )
            .route(
                "/kiosk.js",
                get(|| asset("text/javascript; charset=utf-8", KIOSK_JS)),
            )
            .merge(api)
            .with_state(state)
    }

    /// Take a new snapshot of the composer's facts and send it on if it
    /// changed or the heartbeat is due. Call after every main loop turn.
    pub fn update(&mut self, composer: &mut Composer, now: u64) {
        let screen = composer.screen(now);
        let badges = composer.badges();
        let mut value = snapshot(composer, &screen, &badges, &self.layout);
        self.apply_clicks(&mut value);
        if let Some(update) = self.update.as_ref().and_then(|u| u.get()) {
            value["update"] = json!(update);
        }
        let changed = self.last.as_ref() != Some(&value);
        if changed || now.saturating_sub(self.sent_at) >= HEARTBEAT_S {
            value["now"] = json!(now);
            self.sent_at = now;
            // `send_replace` also stores the frame when nobody is connected yet.
            self.tx.send_replace(Frame::from(value.to_string()));
            if let Some(object) = value.as_object_mut() {
                object.remove("now");
            }
            self.last = Some(value);
        }
    }
}

impl Kiosk {
    /// Leave out the failed runs somebody dismissed and flag the muted
    /// sources. A dismissal or mute that no longer matches anything (the run
    /// went green, the source works again) is forgotten, so a new failure or
    /// a new outage shows up again. The failed badge counts what is left.
    fn apply_clicks(&self, value: &mut Value) {
        let mut clicked = self.clicked.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(failed) = value["failed"].as_array_mut() {
            let id = |f: &Value| format!("{}@{}", f["key"].as_str().unwrap_or(""), f["started"]);
            let live: HashSet<String> = failed.iter().map(id).collect();
            clicked.failed.retain(|x| live.contains(x));
            failed.retain(|f| !clicked.failed.contains(&id(f)));
            let left = failed.len();
            if let Some(badge) = value["badges"]
                .as_array_mut()
                .and_then(|b| b.iter_mut().find(|b| b["id"] == "failed"))
            {
                badge["count"] = json!(left);
            }
        }
        let down: HashSet<String> = value["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["health"] != "ok")
            .filter_map(|s| s["id"].as_str().map(str::to_string))
            .collect();
        clicked.muted.retain(|x| down.contains(x));
        if let Some(sources) = value["sources"].as_array_mut() {
            for source in sources {
                let muted = source["id"]
                    .as_str()
                    .is_some_and(|id| clicked.muted.contains(id));
                if muted {
                    source["muted"] = json!(true);
                }
            }
        }
    }
}

/// The layout part of a snapshot: grid size, the widgets in order, the
/// theme and the pixel shift interval in seconds (0 means off). The page
/// decides day or night itself, by its own clock.
fn layout_json(config: &KioskConfig) -> Value {
    let panels: Vec<PanelConfig> = config.panels();
    let shift_s = if config.pixel_shift {
        u32::from(config.pixel_shift_minutes) * 60
    } else {
        0
    };
    json!({
        "columns": config.columns, "rows": config.rows, "panels": panels,
        "theme": { "mode": config.theme, "night_from": config.night_from, "night_to": config.night_to },
        "pixel_shift_s": shift_s,
    })
}

/// Everything the page draws, from the composer's facts. `now` is added by
/// the caller, so equal facts compare equal.
pub fn snapshot(composer: &Composer, screen: &Screen, badges: &Badges, layout: &Value) -> Value {
    // Hosts: the machine the bridge runs on first, then what sources report.
    let mut hosts = Vec::new();
    if let Some(stats) = composer.stats() {
        hosts.push(json!({ "name": stats.host, "up": true, "stale": false, "stats": stats }));
    }
    for (host, stale) in composer.fleet.hosts() {
        if hosts.iter().all(|h| h["name"] != host.name.as_str()) {
            hosts.push(
                json!({ "name": host.name, "up": host.up, "stale": stale, "stats": host.stats }),
            );
        }
    }

    let jobs: Vec<Value> = composer
        .ci
        .jobs()
        .into_iter()
        .map(|(key, job)| {
            let mut data = serde_json::to_value(&job.data).unwrap_or_default();
            if let Some(map) = data.as_object_mut() {
                map.remove("others");
                map.insert("key".into(), json!(key));
                map.insert("updated".into(), json!(job.updated));
                map.insert("url".into(), json!(job.url));
                map.insert("group".into(), json!(job.group));
            }
            data
        })
        .collect();

    // The parallel jobs of each running run, by the `group` its jobs carry.
    let mut run_jobs = serde_json::Map::new();
    for (_, job) in composer.ci.jobs() {
        let Some(group) = job.group.as_deref() else {
            continue;
        };
        let Some(list) = composer.ci.run_jobs(group) else {
            continue;
        };
        let list: Vec<Value> = list
            .iter()
            .map(|j| {
                json!({
                    "name": j.name, "status": j.status.name(), "step": j.step,
                    "progress": j.progress, "started": j.started,
                    "finished": j.finished, "url": j.url,
                })
            })
            .collect();
        run_jobs.insert(group.to_string(), Value::Array(list));
    }

    // Newest run first, whatever the source or status, so the top of the
    // Pipelines panel is the latest activity. The sort is stable: runs from
    // the same moment keep the order of `ci.runs()` (failed and running
    // first). A running run counts from its start, a finished one from its end.
    let mut ordered = composer.ci.runs();
    ordered.sort_by_key(|(_, run, _)| std::cmp::Reverse(run.finished.unwrap_or(run.started)));
    let runs: Vec<Value> = ordered
        .into_iter()
        .map(|(key, run, updated)| {
            json!({
                "key": key, "source": run.source, "project": run.project,
                "pipeline": run.pipeline, "ref": run.git_ref,
                "status": run.status.name(), "step": run.step,
                "started": run.started, "finished": run.finished, "updated": updated,
                "url": run.url,
            })
        })
        .collect();

    let mut repos: Vec<_> = composer.ci.pulls().collect();
    repos.sort_by(|a, b| (&a.project, &a.source).cmp(&(&b.project, &b.source)));
    let pulls: Vec<Value> = repos
        .into_iter()
        .map(|repo| {
            let mut items: Vec<_> = repo.pulls.iter().collect();
            items.sort_by_key(|p| std::cmp::Reverse(p.created));
            let items: Vec<Value> = items
                .into_iter()
                .map(|p| json!({ "number": p.number, "title": p.title, "created": p.created, "url": p.url }))
                .collect();
            json!({ "source": repo.source, "project": repo.project, "total": repo.total, "items": items })
        })
        .collect();

    // The failures behind the header badge. The user can dismiss them (the
    // bridge remembers which, see `apply_clicks`), so each one carries what makes a new
    // failure different from an old one: the key of its pipeline and the
    // start time of the failed run.
    let failed: Vec<Value> = composer
        .ci
        .failed_runs()
        .into_iter()
        .map(|(key, alert, run)| {
            json!({
                "key": key, "source": alert.source,
                "project": alert.project, "pipeline": alert.pipeline,
                "ref": run.map(|r| r.git_ref.as_str()), "step": alert.step,
                "started": alert.started, "finished": alert.finished,
                "url": run.and_then(|r| r.url.as_deref()),
            })
        })
        .collect();

    let runners: Vec<Value> = composer
        .ci
        .runners()
        .into_iter()
        .map(|(runner, updated)| {
            json!({
                "source": runner.source, "name": runner.name,
                "status": runner.status.name(), "disabled": runner.disabled,
                "labels": runner.labels, "updated": updated, "url": runner.url,
            })
        })
        .collect();

    let queued: Vec<Value> = composer
        .ci
        .queued_jobs()
        .into_iter()
        .map(|(q, updated)| {
            json!({ "source": q.source, "pool": q.pool, "queued": q.queued, "updated": updated })
        })
        .collect();

    let down: Vec<Value> = composer
        .fleet
        .down()
        .into_iter()
        .map(|d| json!({ "text": d.text, "sub": d.sub }))
        .collect();

    let sources: Vec<Value> = composer
        .health
        .iter()
        .map(|(id, health)| json!({ "id": id.to_string(), "health": health.name() }))
        .collect();

    // The composer's pick is only interesting while it is an interrupt; the
    // rotation page would just repeat a widget.
    let screen = if screen.level == Level::Rotation {
        Value::Null
    } else {
        serde_json::to_value(screen).unwrap_or_default()
    };

    let mut out = Map::new();
    out.insert("v".into(), json!(SNAPSHOT_VERSION));
    out.insert("build".into(), json!(env!("CARGO_PKG_VERSION")));
    out.insert("layout".into(), layout.clone());
    out.insert("screen".into(), screen);
    out.insert("badges".into(), json!(badges.items));
    out.insert("hosts".into(), Value::Array(hosts));
    out.insert("down".into(), Value::Array(down));
    out.insert("jobs".into(), Value::Array(jobs));
    out.insert("run_jobs".into(), Value::Object(run_jobs));
    out.insert("runs".into(), Value::Array(runs));
    out.insert("pulls".into(), Value::Array(pulls));
    out.insert("failed".into(), Value::Array(failed));
    out.insert("runners".into(), Value::Array(runners));
    out.insert("queued_jobs".into(), Value::Array(queued));
    out.insert("alerts".into(), json!(composer.alerts.views()));
    out.insert("sources".into(), Value::Array(sources));
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    rx: watch::Receiver<Frame>,
    slots: Arc<Semaphore>,
    alert_tx: Option<mpsc::Sender<Inbound>>,
    clicked: SharedClicked,
}

/// `POST /api/kiosk/failed/dismiss {"ids": ["key@started", ...]}`: take failed
/// runs off the list and the badge, for every screen. A new failure of the
/// same pipeline has another start time, so it is not hidden.
async fn dismiss_failed_handler(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let ids = (body.len() <= 64 * 1024)
        .then(|| serde_json::from_slice::<Value>(&body).ok())
        .flatten()
        .and_then(|v| {
            v.get("ids")?
                .as_array()?
                .iter()
                .map(|id| id.as_str().filter(|s| !s.is_empty() && s.len() <= 512))
                .map(|id| id.map(str::to_string))
                .collect::<Option<Vec<_>>>()
        });
    let Some(ids) = ids else {
        return (StatusCode::BAD_REQUEST, "expected {\"ids\": [\"...\"]}").into_response();
    };
    let mut clicked = state.clicked.lock().unwrap_or_else(|e| e.into_inner());
    if clicked.failed.len() + ids.len() > MAX_CLICKED {
        return (StatusCode::BAD_REQUEST, "too many ids").into_response();
    }
    clicked.failed.extend(ids);
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/kiosk/source/mute {"id": "..."}`: hide the "source down" pill of
/// one source on every screen. The header chip still counts the problem, and
/// the mute ends by itself when the source works again.
async fn mute_source_handler(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let id = (body.len() <= 1024)
        .then(|| serde_json::from_slice::<Value>(&body).ok())
        .flatten()
        .and_then(|v| v.get("id")?.as_str().map(str::to_string))
        .filter(|id| !id.trim().is_empty());
    let Some(id) = id else {
        return (StatusCode::BAD_REQUEST, "expected {\"id\": \"...\"}").into_response();
    };
    let mut clicked = state.clicked.lock().unwrap_or_else(|e| e.into_inner());
    if clicked.muted.len() >= MAX_CLICKED {
        return (StatusCode::BAD_REQUEST, "too many ids").into_response();
    }
    clicked.muted.insert(id);
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/kiosk/alert/dismiss {"id": "..."}`: remove one active alert, for
/// every screen. It can only clear; it cannot raise or change an alert.
async fn dismiss_handler(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Some(tx) = &state.alert_tx else {
        return (
            StatusCode::CONFLICT,
            "dismissing alerts is off ([alerts] dismiss)",
        )
            .into_response();
    };
    let id = (body.len() <= 1024)
        .then(|| serde_json::from_slice::<Value>(&body).ok())
        .flatten()
        .and_then(|v| v.get("id")?.as_str().map(str::to_string))
        .filter(|id| !id.trim().is_empty());
    let Some(id) = id else {
        return (StatusCode::BAD_REQUEST, "expected {\"id\": \"...\"}").into_response();
    };
    match tx.try_send(Inbound::Alert(AlertMessage::clear(&id))) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the bridge is busy, try again",
        )
            .into_response(),
    }
}

pub(crate) async fn asset(content_type: &'static str, body: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // A page left open across a bridge upgrade must not keep old files.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// A cookie-less page cannot be hijacked, but a WebSocket is not covered by
/// the browser's same-origin rules, so a foreign `Origin` is refused here.
async fn require_same_origin(request: Request<axum::body::Body>, next: Next) -> Response {
    if !origin::same_origin(request.headers()) {
        tracing::warn!(
            path = request.uri().path(),
            "kiosk request refused: Origin does not match Host"
        );
        return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
    }
    next.run(request).await
}

async fn snapshot_handler(State(state): State<AppState>) -> Response {
    let frame = state.rx.borrow().clone();
    let mut response = frame.to_string().into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn ws_handler(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    // The permit lives as long as the socket task, so a closed or dropped
    // client frees its slot. A full house is answered before the upgrade.
    let Ok(permit) = state.slots.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "too many live connections").into_response();
    };
    upgrade.on_upgrade(move |socket| serve_socket(socket, state.rx, permit))
}

/// Send the current snapshot, then every new one, until the page goes away or
/// stops reading.
async fn serve_socket(
    mut socket: WebSocket,
    mut rx: watch::Receiver<Frame>,
    _permit: OwnedSemaphorePermit,
) {
    let write_timeout = Duration::from_secs(WRITE_TIMEOUT_S);
    loop {
        let frame = rx.borrow_and_update().clone();
        // A client that never reads fills its socket buffer and would block
        // this send for ever; give up on it instead.
        match tokio::time::timeout(write_timeout, socket.send(Message::text(frame.to_string())))
            .await
        {
            Ok(Ok(())) => {}
            _ => return,
        }
        tokio::select! {
            changed = rx.changed() => if changed.is_err() { return },
            // The page never sends anything; reading notices it closing.
            incoming = socket.recv() => match incoming {
                None | Some(Err(_) | Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests;
