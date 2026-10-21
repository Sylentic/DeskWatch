//! Alerts raised over MQTT on `deskpanel/alert` (docs/mqtt-schema.md) or with
//! `POST /api/alert`, by Home Assistant or any script on the LAN.
//!
//! The severity decides how loud an alert is:
//!
//! | Severity   | Level | Screen                         | Leaves the screen          |
//! |------------|-------|--------------------------------|----------------------------|
//! | `critical` | 0     | red `alert`, above running jobs | Dismiss or clear           |
//! | `warning`  | 2     | amber `alert`                  | Dismiss or 10 minutes      |
//! | `info`     | 4     | `notice` flash                 | 5 seconds                  |
//!
//! Leaving the screen does not end an alert: it stays in the kiosk's alerts
//! list, and warnings and criticals stay in the `home` header badge, until it
//! is cleared with `{"id": ..., "clear": true}` or its `ttl_s` runs out.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::AlertsConfig;
use crate::model::{
    AlertData, AlertStatus, Badge, BadgeIcon, Candidate, Level, MAX_TITLE_CHARS, NoticeData,
    RowStatus, SCHEMA_VERSION, ScreenData, truncate,
};
use crate::mqtt::Inbound;

/// Largest inbound payload accepted. Bigger ones are dropped unread.
pub const MAX_PAYLOAD: usize = 1024;

/// Default lifetime of an `info` alert when it has no `ttl_s`. Warnings and
/// criticals stay until cleared.
pub const INFO_TTL_S: u64 = 30 * 60;

/// Longest alert id kept. Longer ids are cut, not rejected.
const MAX_ID_CHARS: usize = 64;

/// How urgent an alert is. Declared most urgent first, so sorting puts
/// critical alerts at the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// An active alert as the kiosk page shows it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AlertView {
    pub id: String,
    pub severity: &'static str,
    pub title: String,
    pub message: String,
    pub source: String,
    pub since: u64,
    /// Page of the issue or comment, for alerts raised by a git source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Severity {
    /// Name used by the kiosk page (`warn`, not the payload's `warning`).
    pub fn name(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Warning => "warn",
            Severity::Info => "info",
        }
    }

    /// Screen level while the alert is on screen.
    fn level(self) -> Level {
        match self {
            Severity::Critical => Level::Critical,
            Severity::Warning => Level::AlertFailed,
            Severity::Info => Level::Notice,
        }
    }
}

/// A `deskpanel/alert` payload. Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AlertMessage {
    pub v: u8,
    /// Same id replaces the earlier alert; needed to clear. Missing means a
    /// new alert every time.
    #[serde(default)]
    pub id: Option<String>,
    /// Defaults to `info`, the quietest.
    #[serde(default)]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Lifetime in seconds.
    #[serde(default)]
    pub ttl_s: Option<u64>,
    /// Free text label, default `mqtt`.
    #[serde(default)]
    pub source: Option<String>,
    /// True to remove the alert with this `id`.
    #[serde(default)]
    pub clear: bool,
    /// Web page behind the alert. Only set by the bridge's own sources, never
    /// read from a payload, so an outside script cannot plant a link.
    #[serde(skip)]
    pub url: Option<String>,
}

/// Id of the alert the setup page's test button raises.
pub const TEST_ALERT_ID: &str = "setup-test";

/// How long the test alert stays, in seconds.
pub const TEST_ALERT_TTL_S: u64 = 60;

/// How long a "new issue" or "new comment" alert stays when nobody dismisses
/// it: a week.
pub const ACTIVITY_TTL_S: u64 = 7 * 24 * 3600;

impl AlertMessage {
    /// A message that removes the alert with this id.
    pub fn clear(id: &str) -> Self {
        Self {
            v: SCHEMA_VERSION,
            id: Some(id.to_string()),
            severity: None,
            title: None,
            message: None,
            ttl_s: None,
            source: None,
            clear: true,
            url: None,
        }
    }

    /// A quiet alert from a git source about a new issue or comment. It stays
    /// in the alerts list until dismissed (or for `ACTIVITY_TTL_S`), but does
    /// not count in the header badge.
    pub fn activity(
        id: String,
        title: String,
        message: String,
        source: &str,
        url: Option<String>,
    ) -> Self {
        Self {
            v: SCHEMA_VERSION,
            id: Some(id),
            severity: Some(Severity::Info),
            title: Some(title),
            message: Some(message),
            ttl_s: Some(ACTIVITY_TTL_S),
            source: Some(source.to_string()),
            clear: false,
            url,
        }
    }

    /// The fixed alert of the setup page's test button: a warning that
    /// removes itself after a minute. Nothing the page sends can change it.
    pub fn setup_test() -> Self {
        Self {
            v: SCHEMA_VERSION,
            id: Some(TEST_ALERT_ID.to_string()),
            severity: Some(Severity::Warning),
            title: Some("Test alert".to_string()),
            message: Some("sent from the setup page".to_string()),
            ttl_s: Some(TEST_ALERT_TTL_S),
            source: Some("setup".to_string()),
            clear: false,
            url: None,
        }
    }

    /// Parse and check a payload: at most `MAX_PAYLOAD` bytes, schema v2.
    pub fn parse(payload: &[u8]) -> Result<Self> {
        if payload.len() > MAX_PAYLOAD {
            bail!(
                "alert is {} bytes, over the {MAX_PAYLOAD} limit",
                payload.len()
            );
        }
        let msg: AlertMessage = serde_json::from_slice(payload).context("invalid alert")?;
        if msg.v != SCHEMA_VERSION {
            bail!("alert has schema v{}, expected v{SCHEMA_VERSION}", msg.v);
        }
        if msg.clear && msg.id.is_none() {
            bail!("clear needs an id");
        }
        Ok(msg)
    }
}

/// Path of the web address that takes alerts (`[alerts] http_input`).
pub const HTTP_PATH: &str = "/api/alert";

/// `POST /api/alert`: the same JSON as on MQTT (schema section 4), for scripts,
/// `curl`, Home Assistant's `rest_command` and anything else that can send an
/// HTTP request. It has no login (like the dashboard), so a page on another
/// site is refused by its `Origin`; `curl` and scripts send none. An alert
/// without a `source` is labelled `http`.
pub fn http_router(tx: mpsc::Sender<Inbound>) -> Router {
    Router::new().route(HTTP_PATH, post(http_alert).with_state(tx))
}

async fn http_alert(
    State(tx): State<mpsc::Sender<Inbound>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !crate::kiosk::origin::same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
    }
    if body.len() > MAX_PAYLOAD {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("alert is over the {MAX_PAYLOAD} byte limit"),
        )
            .into_response();
    }
    let mut msg = match AlertMessage::parse(&body) {
        Ok(msg) => msg,
        Err(err) => return (StatusCode::BAD_REQUEST, format!("{err:#}")).into_response(),
    };
    if msg.source.is_none() {
        msg.source = Some("http".to_string());
    }
    match tx.try_send(Inbound::Alert(msg)) {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the bridge is busy, try again",
        )
            .into_response(),
    }
}

/// One active alert.
#[derive(Debug, Clone, PartialEq)]
struct Active {
    severity: Severity,
    title: String,
    message: String,
    source: String,
    url: Option<String>,
    /// Unix seconds of the last raise. Screen timers count from here.
    raised_at: u64,
    /// Unix seconds when it ends on its own, or `None` to wait for `clear`.
    expires: Option<u64>,
}

/// Every active alert, by id.
#[derive(Debug, Default)]
pub struct Alerts {
    config: AlertsConfig,
    active: BTreeMap<String, Active>,
    /// Counter for alerts sent without an id.
    anonymous: u64,
}

impl Alerts {
    pub fn new(config: AlertsConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    /// Raise, replace or clear an alert.
    pub fn apply(&mut self, msg: AlertMessage, now: u64) {
        if msg.clear {
            let id = msg.id.as_deref().map(alert_id).unwrap_or_default();
            if self.active.remove(&id).is_some() {
                info!(id, "alert cleared");
            }
            return;
        }

        let id = match &msg.id {
            Some(id) => alert_id(id),
            None => {
                self.anonymous += 1;
                format!("~{}", self.anonymous)
            }
        };
        // One update per id per second, so a looping automation cannot
        // flicker the page.
        if self.active.get(&id).is_some_and(|a| a.raised_at == now) {
            debug!(id, "alert repeated within a second, ignored");
            return;
        }

        let mut severity = msg.severity.unwrap_or(Severity::Info);
        if severity == Severity::Critical && !self.config.allows_critical(&id) {
            warn!(
                id,
                "alert id is not in alerts.critical_ids, shown as a warning"
            );
            severity = Severity::Warning;
        }
        let ttl = msg.ttl_s.or(match severity {
            Severity::Info => Some(INFO_TTL_S),
            Severity::Warning | Severity::Critical => None,
        });
        let title = msg.title.as_deref().unwrap_or("Alert");
        let alert = Active {
            severity,
            title: truncate(title, MAX_TITLE_CHARS),
            message: truncate(msg.message.as_deref().unwrap_or(""), MAX_TITLE_CHARS),
            source: truncate(msg.source.as_deref().unwrap_or("mqtt"), MAX_TITLE_CHARS),
            url: msg.url.clone(),
            raised_at: now,
            expires: ttl.map(|ttl| now.saturating_add(ttl)),
        };
        info!(id, ?severity, title = alert.title, "alert raised");
        self.active.insert(id, alert);

        // Keep at most `max_active`: drop the least urgent, oldest one.
        while self.active.len() > self.config.max_active.max(1) {
            let drop = self
                .active
                .iter()
                .max_by_key(|(_, a)| (a.severity, std::cmp::Reverse(a.raised_at)))
                .map(|(id, _)| id.clone());
            let Some(drop) = drop else { break };
            warn!(id = drop, "too many active alerts, dropping one");
            self.active.remove(&drop);
        }
    }

    /// Drop alerts whose `ttl_s` has run out.
    pub fn expire(&mut self, now: u64) {
        self.active
            .retain(|_, a| a.expires.is_none_or(|expires| now < expires));
    }

    /// Forget every alert (used by the demo when it starts over).
    pub fn clear_all(&mut self) {
        self.active.clear();
    }

    /// Alerts that want the screen. `model::pick` chooses among these and the
    /// CI candidates.
    pub fn candidates(&self, now: u64) -> Vec<Candidate> {
        let showing = |a: &Active| !candidate(a).is_expired(now);
        self.active
            .values()
            .filter(|a| showing(a))
            .map(|a| {
                let mut c = candidate(a);
                if let ScreenData::Alert(data) = &mut c.data {
                    let same = |b: &&Active| b.severity == a.severity && showing(b);
                    data.others =
                        self.active.values().filter(same).count().saturating_sub(1) as u32;
                }
                c
            })
            .collect()
    }

    /// Every active alert for the kiosk page, most urgent first, then newest.
    pub fn views(&self) -> Vec<AlertView> {
        let mut alerts: Vec<(&String, &Active)> = self.active.iter().collect();
        alerts.sort_by_key(|(_, a)| (a.severity, std::cmp::Reverse(a.raised_at)));
        alerts
            .into_iter()
            .map(|(id, a)| AlertView {
                id: id.clone(),
                severity: a.severity.name(),
                title: a.title.clone(),
                message: a.message.clone(),
                source: a.source.clone(),
                since: a.raised_at,
                url: a.url.clone(),
            })
            .collect()
    }

    /// The `home` header badge: active warning and critical alerts, red when
    /// any of them is critical.
    pub fn badge(&self) -> Badge {
        let loud = || {
            self.active
                .values()
                .filter(|a| a.severity != Severity::Info)
        };
        let critical = loud().any(|a| a.severity == Severity::Critical);
        Badge {
            id: "alerts".into(),
            icon: BadgeIcon::Home,
            count: loud().count() as u32,
            status: if critical {
                RowStatus::Failed
            } else {
                RowStatus::Review
            },
        }
    }
}

/// Screen candidate for one alert: a red or amber `alert` page, or a
/// `notice` for info alerts.
fn candidate(a: &Active) -> Candidate {
    let data = match a.severity {
        Severity::Info => ScreenData::Notice(NoticeData {
            text: a.title.clone(),
            sub: a.message.clone(),
            source: a.source.clone(),
        }),
        Severity::Warning | Severity::Critical => ScreenData::Alert(AlertData {
            status: if a.severity == Severity::Critical {
                AlertStatus::Failed
            } else {
                AlertStatus::Warn
            },
            source: a.source.clone(),
            project: None,
            pipeline: None,
            step: None,
            title: Some(a.title.clone()),
            message: Some(a.message.clone()).filter(|m| !m.is_empty()),
            started: a.raised_at,
            finished: None,
            others: 0,
        }),
    };
    let kind = if a.severity == Severity::Info {
        "notice"
    } else {
        "alert"
    };
    Candidate {
        level: a.severity.level(),
        page: format!("{}-{kind}", a.source),
        data,
        raised_at: a.raised_at,
    }
}

/// Ids are trimmed and cut to `MAX_ID_CHARS`.
fn alert_id(id: &str) -> String {
    truncate(id.trim(), MAX_ID_CHARS)
}

/// An open issue as a polling git source sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct NewIssue {
    pub number: u64,
    pub title: String,
    pub url: Option<String>,
    /// Who opened it, if known.
    pub by: Option<String>,
}

/// "New issue" alerts for sources that poll (GitHub, GitLab, Bitbucket):
/// remembers the highest issue number per repository and raises an alert for
/// each higher one. The first poll of a repository only remembers, so
/// existing issues do not all alert at startup.
#[derive(Debug, Default)]
pub struct IssueWatch {
    last: HashMap<String, u64>,
}

impl IssueWatch {
    /// `source` is the source id (`github:home`), `kind` its adapter type for
    /// the mark, `label` the repository's short name on the dashboard.
    pub fn check(
        &mut self,
        source: &str,
        kind: &str,
        repo: &str,
        label: &str,
        issues: Vec<NewIssue>,
    ) -> Vec<AlertMessage> {
        let newest = issues.iter().map(|i| i.number).max().unwrap_or(0);
        let Some(last) = self.last.get(repo).copied() else {
            self.last.insert(repo.to_string(), newest);
            return Vec::new();
        };
        self.last.insert(repo.to_string(), last.max(newest));
        let mut issues: Vec<NewIssue> = issues.into_iter().filter(|i| i.number > last).collect();
        // Oldest first, so the newest ends up on top of the list.
        issues.sort_by_key(|i| i.number);
        issues
            .into_iter()
            .map(|i| {
                let by = i.by.map(|by| format!(" by {by}")).unwrap_or_default();
                AlertMessage::activity(
                    activity_id(&format!("{source}:issue:{repo}#{}", i.number)),
                    i.title,
                    format!("New issue #{} in {label}{by}", i.number),
                    kind,
                    i.url,
                )
            })
            .collect()
    }
}

/// An alert id for a source's own alert that stays unique when it is longer
/// than `MAX_ID_CHARS`: the tail is replaced by a hash of the whole id
/// (FNV-1a, stable across restarts and builds).
pub fn activity_id(id: &str) -> String {
    if id.chars().count() <= MAX_ID_CHARS {
        return id.to_string();
    }
    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let head: String = id.chars().take(MAX_ID_CHARS - 17).collect();
    format!("{head}~{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pick;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn raise(id: &str, severity: &str) -> AlertMessage {
        let json = format!(
            r#"{{"v":2,"id":"{id}","severity":"{severity}","title":"{id} title","message":"msg","source":"homeassistant"}}"#
        );
        AlertMessage::parse(json.as_bytes()).unwrap()
    }

    fn clear(id: &str) -> AlertMessage {
        AlertMessage::parse(format!(r#"{{"v":2,"id":"{id}","clear":true}}"#).as_bytes()).unwrap()
    }

    fn top(alerts: &Alerts, now: u64) -> Option<(Level, ScreenData)> {
        let candidates = alerts.candidates(now);
        pick(&candidates, now).map(|c| (c.level, c.data.clone()))
    }

    #[test]
    fn long_activity_ids_stay_unique_and_fit() {
        let a = activity_id(&format!("github:work:issue:{}#1", "x".repeat(80)));
        let b = activity_id(&format!("github:work:issue:{}#2", "x".repeat(80)));
        assert_ne!(a, b);
        assert_eq!(a.chars().count(), MAX_ID_CHARS);
        assert_eq!(alert_id(&a), a, "not cut again");
        assert_eq!(
            activity_id("gitea:home:issue:me/app#3"),
            "gitea:home:issue:me/app#3"
        );
    }

    #[test]
    fn parse_checks_size_version_and_clear_id() {
        assert!(AlertMessage::parse(br#"{"v":2,"title":"x"}"#).is_ok());
        assert!(AlertMessage::parse(br#"{"v":1,"title":"x"}"#).is_err());
        assert!(AlertMessage::parse(br#"{"v":2,"clear":true}"#).is_err());
        assert!(AlertMessage::parse(br#"{"v":2,"severity":"panic"}"#).is_err());
        let big = format!(r#"{{"v":2,"title":"{}"}}"#, "x".repeat(MAX_PAYLOAD));
        assert!(AlertMessage::parse(big.as_bytes()).is_err());
    }

    #[test]
    fn severities_map_to_levels() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("washer", "info"), 0);
        let (level, data) = top(&alerts, 0).unwrap();
        assert_eq!(level, Level::Notice);
        assert!(matches!(data, ScreenData::Notice(n) if n.text == "washer title"));

        alerts.apply(raise("freezer", "warning"), 1);
        let (level, data) = top(&alerts, 1).unwrap();
        assert_eq!(level, Level::AlertFailed);
        let ScreenData::Alert(alert) = data else {
            panic!("expected an alert")
        };
        assert_eq!(alert.status, AlertStatus::Warn);
        assert_eq!(alert.title.as_deref(), Some("freezer title"));
        assert_eq!(alert.project, None);

        alerts.apply(raise("leak", "critical"), 2);
        let (level, data) = top(&alerts, 2).unwrap();
        assert_eq!(level, Level::Critical);
        assert!(matches!(data, ScreenData::Alert(a) if a.status == AlertStatus::Failed));
    }

    #[test]
    fn info_flashes_then_stays_in_the_list_until_its_ttl() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("washer", "info"), 100);
        assert!(top(&alerts, 104).is_some());
        assert!(top(&alerts, 105).is_none(), "notice flash is 5 s");
        assert_eq!(alerts.views().len(), 1);
        assert_eq!(alerts.badge().count, 0, "info is not in the home badge");
        alerts.expire(100 + INFO_TTL_S);
        assert_eq!(alerts.views().len(), 0);
    }

    #[test]
    fn warning_drops_to_badge_after_ten_minutes() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("freezer", "warning"), 0);
        assert!(top(&alerts, 599).is_some());
        assert!(top(&alerts, 600).is_none());
        alerts.expire(10_000);
        assert_eq!(alerts.badge().count, 1, "stays until cleared");
        assert_eq!(alerts.badge().status, RowStatus::Review);
    }

    #[test]
    fn clear_and_ttl_remove_alerts() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("leak", "critical"), 0);
        alerts.apply(clear("leak"), 1);
        assert!(top(&alerts, 1).is_none());
        assert_eq!(alerts.badge().count, 0);

        let mut short = raise("door", "critical");
        short.ttl_s = Some(30);
        alerts.apply(short, 0);
        alerts.expire(29);
        assert_eq!(alerts.badge().count, 1);
        alerts.expire(30);
        assert_eq!(alerts.badge().count, 0);
    }

    #[test]
    fn same_id_replaces_and_others_counts_the_rest() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("a", "warning"), 0);
        alerts.apply(raise("a", "warning"), 1);
        alerts.apply(raise("b", "warning"), 2);
        assert_eq!(alerts.views().len(), 2);
        let Some((_, ScreenData::Alert(alert))) = top(&alerts, 2) else {
            panic!("expected an alert")
        };
        assert_eq!(alert.title.as_deref(), Some("b title"), "newest first");
        assert_eq!(alert.others, 1);
    }

    #[test]
    fn rate_limit_cap_and_critical_ids() {
        let mut alerts = Alerts::new(AlertsConfig {
            max_active: 2,
            critical_ids: Some(vec!["leak".into()]),
            ..Default::default()
        });
        alerts.apply(raise("door", "critical"), 0);
        assert_eq!(top(&alerts, 0).unwrap().0, Level::AlertFailed, "downgraded");
        alerts.apply(raise("leak", "critical"), 0);
        assert_eq!(top(&alerts, 0).unwrap().0, Level::Critical);

        // A second update of the same id in the same second is ignored.
        let mut again = raise("leak", "critical");
        again.title = Some("changed".into());
        alerts.apply(again, 0);
        assert_eq!(alerts.views()[0].title, "leak title");

        // Over the cap, the least urgent, oldest alert goes first.
        alerts.apply(raise("washer", "info"), 1);
        let ids: Vec<String> = alerts.views().into_iter().map(|a| a.title).collect();
        assert_eq!(ids, ["leak title", "door title"]);
    }

    #[test]
    fn long_text_is_truncated_and_defaults_apply() {
        let mut alerts = Alerts::default();
        let long = "x".repeat(100);
        let json = format!(r#"{{"v":2,"title":"{long}"}}"#);
        alerts.apply(AlertMessage::parse(json.as_bytes()).unwrap(), 0);
        let view = &alerts.views()[0];
        assert_eq!(view.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(view.source, "mqtt");
        assert_eq!(view.severity, "info", "default severity is info");
    }

    async fn post_alert(router: &Router, headers: &[(&str, &str)], body: &str) -> StatusCode {
        let mut request = Request::builder().method("POST").uri(HTTP_PATH);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        router
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_web_address_takes_the_mqtt_json_and_labels_the_source() {
        let (tx, mut rx) = mpsc::channel(4);
        let router = http_router(tx);
        let ok = r#"{"v":2,"id":"door","severity":"warning","title":"Freezer door"}"#;
        assert_eq!(post_alert(&router, &[], ok).await, StatusCode::ACCEPTED);
        let Some(Inbound::Alert(msg)) = rx.recv().await else {
            panic!("expected an alert")
        };
        assert_eq!(msg.id.as_deref(), Some("door"));
        assert_eq!(msg.source.as_deref(), Some("http"));

        // A source the sender chose is kept, and a clear works like on MQTT.
        let own = r#"{"v":2,"id":"door","title":"x","source":"uptime-kuma"}"#;
        assert_eq!(post_alert(&router, &[], own).await, StatusCode::ACCEPTED);
        let Some(Inbound::Alert(msg)) = rx.recv().await else {
            panic!("expected an alert")
        };
        assert_eq!(msg.source.as_deref(), Some("uptime-kuma"));
        let clear = r#"{"v":2,"id":"door","clear":true}"#;
        assert_eq!(post_alert(&router, &[], clear).await, StatusCode::ACCEPTED);
        let Some(Inbound::Alert(msg)) = rx.recv().await else {
            panic!("expected a clear")
        };
        assert!(msg.clear);
    }

    #[tokio::test]
    async fn the_web_address_refuses_bad_big_and_cross_origin_requests() {
        let (tx, mut rx) = mpsc::channel(4);
        let router = http_router(tx);
        assert_eq!(
            post_alert(&router, &[], "nope").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post_alert(&router, &[], r#"{"v":1,"title":"old"}"#).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post_alert(&router, &[], r#"{"v":2,"clear":true}"#).await,
            StatusCode::BAD_REQUEST,
            "clear needs an id"
        );
        let big = format!(r#"{{"v":2,"title":"{}"}}"#, "x".repeat(MAX_PAYLOAD));
        assert_eq!(
            post_alert(&router, &[], &big).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let evil = [("origin", "https://evil.example"), ("host", "bridge.lan")];
        assert_eq!(
            post_alert(&router, &evil, r#"{"v":2,"title":"x"}"#).await,
            StatusCode::FORBIDDEN
        );
        assert!(rx.try_recv().is_err());
        // GET is not a thing here.
        let got = router
            .clone()
            .oneshot(Request::get(HTTP_PATH).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(got.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn the_web_address_says_when_the_bridge_is_busy() {
        let (tx, _rx) = mpsc::channel(1);
        let router = http_router(tx);
        let body = r#"{"v":2,"title":"x"}"#;
        assert_eq!(post_alert(&router, &[], body).await, StatusCode::ACCEPTED);
        assert_eq!(
            post_alert(&router, &[], body).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
