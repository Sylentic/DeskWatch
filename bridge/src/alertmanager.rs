//! Prometheus Alertmanager as an alert source: `POST /api/alertmanager`.
//!
//! Alertmanager's webhook receiver (payload version 4) sends a group of
//! alerts, each `firing` or `resolved`. Every one becomes an ordinary alert
//! of the alerts module, so severity, `max_active`, Dismiss and the kiosk
//! list work as for any other source. `resolved` clears the alert again.
//!
//! The `severity` label decides how loud an alert is: `critical`, `page` and
//! `error` are critical, `info` is info, everything else (also no label) is a
//! warning, because an alert that shows too quietly is worse than one that
//! shows too loudly.

use std::collections::BTreeMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::alerts::{AlertMessage, Severity, activity_id};
use crate::model::SCHEMA_VERSION;
use crate::mqtt::Inbound;

/// Path of the web address Alertmanager posts to (`[alerts] http_input`).
pub const HTTP_PATH: &str = "/api/alertmanager";

/// Largest webhook body accepted. A big group is far over the 1 KB of a
/// plain alert.
pub const MAX_BODY: usize = 256 * 1024;

/// Most alerts taken from one webhook call. The rest is ignored.
pub const MAX_ALERTS: usize = 100;

/// Label shown as the source of every alert from here.
const SOURCE: &str = "alertmanager";

/// The part of the webhook payload that is used. Unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(default)]
    alerts: Vec<Item>,
}

#[derive(Debug, Deserialize)]
struct Item {
    /// `firing` or `resolved`.
    status: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
    #[serde(default)]
    fingerprint: Option<String>,
}

/// Severity for the value of the `severity` label.
fn severity(label: Option<&str>) -> Severity {
    match label.map(str::to_ascii_lowercase).as_deref() {
        Some("critical" | "page" | "error") => Severity::Critical,
        Some("info" | "informational" | "none") => Severity::Info,
        _ => Severity::Warning,
    }
}

/// Stable id: `am:<alertname>:<fingerprint>`. Older Alertmanagers send no
/// fingerprint, then the label set stands in for it.
fn id_of(item: &Item) -> String {
    let name = item.labels.get("alertname").map_or("alert", String::as_str);
    let print = match &item.fingerprint {
        Some(print) if !print.is_empty() => print.clone(),
        _ => item
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(","),
    };
    activity_id(&format!("am:{name}:{print}"))
}

fn message_of(item: &Item) -> AlertMessage {
    let id = id_of(item);
    if item.status == "resolved" {
        return AlertMessage::clear(&id);
    }
    let title = item
        .annotations
        .get("summary")
        .or_else(|| item.labels.get("alertname"))
        .cloned()
        .unwrap_or_else(|| "Alert".to_string());
    let mut message = item
        .annotations
        .get("description")
        .or_else(|| item.annotations.get("message"))
        .cloned()
        .unwrap_or_default();
    if message.is_empty() {
        message = item.labels.get("instance").cloned().unwrap_or_default();
    }
    AlertMessage {
        v: SCHEMA_VERSION,
        id: Some(id),
        severity: Some(severity(item.labels.get("severity").map(String::as_str))),
        title: Some(title),
        message: Some(message),
        ttl_s: None,
        source: Some(SOURCE.to_string()),
        clear: false,
        url: None,
    }
}

/// The alert messages in one webhook body, at most `MAX_ALERTS`.
pub fn parse(body: &[u8]) -> Result<Vec<AlertMessage>, String> {
    if body.len() > MAX_BODY {
        return Err(format!("body is over the {MAX_BODY} byte limit"));
    }
    let payload: Payload = serde_json::from_slice(body)
        .map_err(|err| format!("invalid Alertmanager payload: {err}"))?;
    Ok(payload
        .alerts
        .iter()
        .take(MAX_ALERTS)
        .map(message_of)
        .collect())
}

/// `POST /api/alertmanager`, guarded like `/api/alert` (see `alerts.rs`).
pub fn http_router(tx: mpsc::Sender<Inbound>) -> Router {
    Router::new().route(HTTP_PATH, post(http_alertmanager).with_state(tx))
}

async fn http_alertmanager(
    State(tx): State<mpsc::Sender<Inbound>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !crate::kiosk::origin::same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
    }
    if body.len() > MAX_BODY {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("body is over the {MAX_BODY} byte limit"),
        )
            .into_response();
    }
    let messages = match parse(&body) {
        Ok(messages) => messages,
        Err(err) => return (StatusCode::BAD_REQUEST, err).into_response(),
    };
    for msg in messages {
        if tx.try_send(Inbound::Alert(msg)).is_err() {
            // Alertmanager retries on a 5xx, so nothing is lost.
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "the bridge is busy, try again",
            )
                .into_response();
        }
    }
    StatusCode::ACCEPTED.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const BODY: &str = r#"{
      "version": "4", "groupKey": "{}:{alertname=\"DiskFull\"}", "status": "firing",
      "receiver": "deskwatch", "externalURL": "http://am.example:9093",
      "alerts": [
        {"status": "firing", "fingerprint": "abc123",
         "labels": {"alertname": "DiskFull", "severity": "critical", "instance": "nas:9100"},
         "annotations": {"summary": "Disk almost full", "description": "95% used on /data"},
         "generatorURL": "http://prom.example/graph"},
        {"status": "resolved", "fingerprint": "def456",
         "labels": {"alertname": "HighLoad", "severity": "warning"}, "annotations": {}}
      ]}"#;

    #[test]
    fn firing_becomes_an_alert_and_resolved_a_clear() {
        let msgs = parse(BODY.as_bytes()).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id.as_deref(), Some("am:DiskFull:abc123"));
        assert_eq!(msgs[0].severity, Some(Severity::Critical));
        assert_eq!(msgs[0].title.as_deref(), Some("Disk almost full"));
        assert_eq!(msgs[0].message.as_deref(), Some("95% used on /data"));
        assert_eq!(msgs[0].source.as_deref(), Some("alertmanager"));
        assert!(msgs[0].url.is_none(), "generatorURL is not taken over");
        assert!(msgs[1].clear);
        assert_eq!(msgs[1].id.as_deref(), Some("am:HighLoad:def456"));
    }

    #[test]
    fn severity_label_is_mapped() {
        assert_eq!(severity(Some("critical")), Severity::Critical);
        assert_eq!(severity(Some("PAGE")), Severity::Critical);
        assert_eq!(severity(Some("warning")), Severity::Warning);
        assert_eq!(severity(Some("info")), Severity::Info);
        assert_eq!(severity(Some("whatever")), Severity::Warning);
        assert_eq!(severity(None), Severity::Warning);
    }

    #[test]
    fn title_and_message_fall_back_to_labels() {
        let body = r#"{"alerts":[{"status":"firing",
            "labels":{"alertname":"Down","instance":"web:80"},"annotations":{}}]}"#;
        let msg = &parse(body.as_bytes()).unwrap()[0];
        assert_eq!(msg.title.as_deref(), Some("Down"));
        assert_eq!(msg.message.as_deref(), Some("web:80"));
    }

    #[test]
    fn without_a_fingerprint_the_labels_make_the_id() {
        let body = r#"{"alerts":[{"status":"firing","labels":{"alertname":"A","job":"x"}}]}"#;
        let a = parse(body.as_bytes()).unwrap();
        let resolved = body.replace("firing", "resolved");
        let b = parse(resolved.as_bytes()).unwrap();
        assert_eq!(a[0].id, b[0].id, "resolved finds the firing alert");
    }

    #[test]
    fn long_ids_stay_unique() {
        let long = |n: u8| {
            format!(
                r#"{{"alerts":[{{"status":"firing","fingerprint":"{}{n}","labels":{{"alertname":"{}"}}}}]}}"#,
                "f".repeat(30),
                "N".repeat(60)
            )
        };
        let a = parse(long(1).as_bytes()).unwrap();
        let b = parse(long(2).as_bytes()).unwrap();
        assert_ne!(a[0].id, b[0].id);
    }

    #[test]
    fn bad_and_big_bodies_are_refused_and_groups_are_capped() {
        assert!(parse(b"not json").is_err());
        assert!(parse(&vec![b' '; MAX_BODY + 1]).is_err());
        let one = r#"{"status":"firing","labels":{"alertname":"A"}}"#;
        let many = vec![one; MAX_ALERTS + 20].join(",");
        let body = format!(r#"{{"alerts":[{many}]}}"#);
        assert_eq!(parse(body.as_bytes()).unwrap().len(), MAX_ALERTS);
    }

    #[tokio::test]
    async fn route_queues_alerts_and_answers_status_codes() {
        let (tx, mut rx) = mpsc::channel(8);
        let app = http_router(tx);
        let post = |body: &'static str| Request::post(HTTP_PATH).body(Body::from(body)).unwrap();
        let res = app.clone().oneshot(post(BODY)).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        assert!(matches!(rx.try_recv(), Ok(Inbound::Alert(m)) if !m.clear));
        assert!(matches!(rx.try_recv(), Ok(Inbound::Alert(m)) if m.clear));
        let res = app.clone().oneshot(post("nope")).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let req = Request::post(HTTP_PATH)
            .header("origin", "http://evil.example")
            .header("host", "bridge.lan:8787")
            .body(Body::from(BODY))
            .unwrap();
        assert_eq!(
            app.oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn a_full_queue_answers_503() {
        let (tx, _rx) = mpsc::channel(1);
        let res = http_router(tx)
            .oneshot(Request::post(HTTP_PATH).body(Body::from(BODY)).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
