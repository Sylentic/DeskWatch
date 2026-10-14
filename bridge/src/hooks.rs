//! The one HTTP listener that push sources and the kiosk page share.
//!
//! Each webhook source adds its own route (`/webhook/gitea/<name>`, later
//! `/webhook/grafana/<name>` and so on) while it is being built. The listener
//! only opens when at least one route exists, so a bridge with no webhook
//! sources, no kiosk page and no MQTT (which adds `/healthz`) has no open port.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tokio::net::TcpListener;
use tower::ServiceExt;
use tracing::{info, warn};

/// Path prefix of the routes that sources can add and remove while the
/// bridge runs.
const WEBHOOK_PREFIX: &str = "/webhook/";

/// The webhook routes that can change while the listener runs. A source that
/// is added, replaced or removed from the setup page registers or drops its
/// route here; the listener looks the path up on every request.
#[derive(Clone, Default)]
pub struct Webhooks(Arc<RwLock<HashMap<String, Router>>>);

impl Webhooks {
    /// Add a route, replacing the one at the same path.
    pub fn set(&self, path: &str, router: Router) {
        if let Ok(mut routes) = self.0.write() {
            routes.insert(path.to_string(), router);
        }
    }

    /// Drop a route; later requests to it get a 404.
    pub fn remove(&self, path: &str) {
        if let Ok(mut routes) = self.0.write() {
            routes.remove(path);
        }
    }

    fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .0
            .read()
            .map(|routes| routes.keys().cloned().collect())
            .unwrap_or_default();
        paths.sort();
        paths
    }

    fn get(&self, path: &str) -> Option<Router> {
        self.0.read().ok()?.get(path).cloned()
    }
}

pub(crate) async fn dispatch(State(webhooks): State<Webhooks>, request: Request) -> Response {
    match webhooks.get(request.uri().path()) {
        Some(router) => router
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {}),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Routes collected from the push sources.
#[derive(Default)]
pub struct Hooks {
    router: Router,
    paths: Vec<String>,
    webhooks: Webhooks,
}

impl Hooks {
    /// A collection that registers its webhook routes in `webhooks`, so
    /// routes added after the listener started are served too.
    pub fn sharing(webhooks: Webhooks) -> Self {
        Self {
            webhooks,
            ..Self::default()
        }
    }

    /// The routes that can change while the listener runs.
    pub fn webhooks(&self) -> Webhooks {
        self.webhooks.clone()
    }

    /// Add one source's route. `router` must only handle `path`. A webhook
    /// path goes to the shared, changeable set; any other route is fixed.
    pub fn add(&mut self, path: &str, router: Router) {
        if path.starts_with(WEBHOOK_PREFIX) {
            self.webhooks.set(path, router);
        } else {
            self.router = std::mem::take(&mut self.router).merge(router);
        }
        self.paths.push(path.to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.webhooks.paths().is_empty()
    }

    /// Open the socket and serve in the background. The socket opens here,
    /// before anything else runs, so a port that is already taken stops the
    /// bridge at startup instead of failing quietly later. Does nothing when
    /// no source added a route.
    pub async fn start(self, listen: SocketAddr) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("cannot listen on {listen} (webhooks or kiosk page)"))?;
        let fixed = self.paths.iter().filter(|p| !p.starts_with(WEBHOOK_PREFIX));
        for path in fixed.cloned().chain(self.webhooks.paths()) {
            info!("serving http://{listen}{path}");
        }
        let router = self.router.route(
            "/webhook/{*rest}",
            axum::routing::any(dispatch).with_state(self.webhooks.clone()),
        );
        tokio::spawn(async move {
            if let Err(err) = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                warn!("webhook server stopped: {err:#}");
            }
        });
        Ok(())
    }
}
