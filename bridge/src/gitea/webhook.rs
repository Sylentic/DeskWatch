//! HTTP endpoint that receives Gitea webhooks.
//!
//! Every request must carry a valid `X-Gitea-Signature`: the hex HMAC-SHA256
//! of the raw body with the shared webhook secret. Anything else is rejected
//! before the body is parsed, so only Gitea can change what the panel shows.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use hmac::{KeyInit, Mac};
use sha2::Sha256;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::GiteaEvent;

/// URL path Gitea posts to, as in `http://<bridge-host>:<port>/webhook/gitea`.
pub const PATH: &str = "/webhook/gitea";

/// Shared state of the HTTP handler.
struct Hook {
    secret: Vec<u8>,
    events: mpsc::Sender<GiteaEvent>,
}

/// Build the router. Split from `serve` so tests can call it without a socket.
pub fn router(secret: Vec<u8>, events: mpsc::Sender<GiteaEvent>) -> Router {
    let state = Arc::new(Hook { secret, events });
    Router::new().route(PATH, post(handle)).with_state(state)
}

/// Open the listening socket. Done before `serve` so a port that is already
/// taken stops the bridge at startup instead of failing quietly later.
pub async fn bind(listen: SocketAddr) -> Result<TcpListener> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("cannot listen on {listen} for Gitea webhooks"))?;
    info!("Gitea webhooks on http://{listen}{PATH}");
    Ok(listener)
}

/// Forward verified events until the process stops.
pub async fn serve(
    listener: TcpListener,
    secret: Vec<u8>,
    events: mpsc::Sender<GiteaEvent>,
) -> Result<()> {
    axum::serve(listener, router(secret, events))
        .await
        .context("webhook server stopped")
}

async fn handle(State(hook): State<Arc<Hook>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    let Some(signature) = header("x-gitea-signature") else {
        warn!("webhook without signature rejected");
        return StatusCode::UNAUTHORIZED;
    };
    if !signature_is_valid(&hook.secret, &body, signature) {
        warn!("webhook with bad signature rejected");
        return StatusCode::UNAUTHORIZED;
    }

    let kind = header("x-gitea-event").unwrap_or_default();
    match parse_event(kind, &body) {
        Ok(Some(event)) => {
            debug!(kind, "webhook accepted");
            if hook.events.send(event).await.is_err() {
                return StatusCode::SERVICE_UNAVAILABLE;
            }
            StatusCode::NO_CONTENT
        }
        // Events the bridge does not use (push, issues, ...) are fine to send.
        Ok(None) => {
            debug!(kind, "webhook ignored");
            StatusCode::NO_CONTENT
        }
        Err(err) => {
            warn!(kind, "webhook with bad JSON rejected: {err:#}");
            StatusCode::BAD_REQUEST
        }
    }
}

/// Check `X-Gitea-Signature` (hex HMAC-SHA256 of the body) in constant time.
pub fn signature_is_valid(secret: &[u8], body: &[u8], signature_hex: &str) -> bool {
    let Ok(signature) = hex::decode(signature_hex.trim()) else {
        return false;
    };
    let Ok(mut mac) = hmac::Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

/// Turn a webhook body into an event, based on the `X-Gitea-Event` header.
/// Returns `None` for event types the bridge does not use.
pub fn parse_event(kind: &str, body: &[u8]) -> Result<Option<GiteaEvent>> {
    let event = match kind {
        "workflow_run" => GiteaEvent::WorkflowRun(serde_json::from_slice(body)?),
        "workflow_job" => GiteaEvent::WorkflowJob(serde_json::from_slice(body)?),
        "pull_request" => GiteaEvent::PullRequest(serde_json::from_slice(body)?),
        _ => return Ok(None),
    };
    Ok(Some(event))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    const SECRET: &[u8] = b"test-secret";

    fn sign(body: &[u8]) -> String {
        let mut mac = hmac::Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    async fn send(
        kind: &str,
        body: &str,
        signature: Option<String>,
    ) -> (StatusCode, Option<GiteaEvent>) {
        let (tx, mut rx) = mpsc::channel(4);
        let mut request = Request::post(PATH).header("x-gitea-event", kind);
        if let Some(signature) = signature {
            request = request.header("x-gitea-signature", signature);
        }
        let request = request.body(Body::from(body.to_string())).unwrap();
        let response = router(SECRET.to_vec(), tx).oneshot(request).await.unwrap();
        (response.status(), rx.try_recv().ok())
    }

    #[test]
    fn signature_check() {
        let body = br#"{"action":"opened"}"#;
        assert!(signature_is_valid(SECRET, body, &sign(body)));
        assert!(!signature_is_valid(SECRET, b"tampered", &sign(body)));
        assert!(!signature_is_valid(b"other", body, &sign(body)));
        assert!(!signature_is_valid(SECRET, body, "not hex"));
        assert!(!signature_is_valid(SECRET, body, ""));
    }

    #[tokio::test]
    async fn signed_pull_request_is_forwarded() {
        let body = r#"{"action":"opened","pull_request":{"number":1,"title":"x"},"repository":{"full_name":"me/demo","name":"demo"}}"#;
        let (status, event) = send("pull_request", body, Some(sign(body.as_bytes()))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(matches!(event, Some(GiteaEvent::PullRequest(_))));
    }

    #[tokio::test]
    async fn unsigned_or_badly_signed_requests_are_rejected() {
        let body = r#"{"action":"opened"}"#;
        let (status, event) = send("pull_request", body, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(event.is_none());

        let (status, event) = send("pull_request", body, Some(sign(b"other body"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(event.is_none());
    }

    #[tokio::test]
    async fn unused_event_types_are_accepted_and_dropped() {
        let body = r#"{"ref":"refs/heads/main"}"#;
        let (status, event) = send("push", body, Some(sign(body.as_bytes()))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(event.is_none());
    }

    #[tokio::test]
    async fn bad_json_is_rejected() {
        let body = "not json";
        let (status, _) = send("workflow_job", body, Some(sign(body.as_bytes()))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
