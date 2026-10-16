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
use axum_server::tls_rustls::RustlsConfig;
use tokio::net::TcpListener;
use tower::ServiceExt;
use tracing::{info, warn};

use crate::config::TlsConfig;

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
    pub async fn start(self, listen: SocketAddr, tls: Option<&TlsConfig>) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("cannot listen on {listen} (webhooks or kiosk page)"))?;
        // Load the certificate now too, so a wrong path stops the bridge at startup.
        let rustls = match tls {
            Some(tls) => Some(
                RustlsConfig::from_pem_file(&tls.cert, &tls.key)
                    .await
                    .with_context(|| {
                        format!(
                            "cannot load the certificate {} and key {} from [http.tls]",
                            tls.cert.display(),
                            tls.key.display()
                        )
                    })?,
            ),
            None => None,
        };
        let scheme = if rustls.is_some() { "https" } else { "http" };
        let fixed = self.paths.iter().filter(|p| !p.starts_with(WEBHOOK_PREFIX));
        for path in fixed.cloned().chain(self.webhooks.paths()) {
            info!("serving {scheme}://{listen}{path}");
        }
        let router = self.router.route(
            "/webhook/{*rest}",
            axum::routing::any(dispatch).with_state(self.webhooks.clone()),
        );
        let app = router.into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move {
            let result = match rustls {
                Some(rustls) => {
                    let std_listener = match listener.into_std() {
                        Ok(l) => l,
                        Err(err) => return warn!("webhook server stopped: {err:#}"),
                    };
                    axum_server::from_tcp_rustls(std_listener, rustls)
                        .map(|server| server.serve(app))
                }
                None => {
                    if let Err(err) = axum::serve(listener, app).await {
                        warn!("webhook server stopped: {err:#}");
                    }
                    return;
                }
            };
            match result {
                Ok(server) => {
                    if let Err(err) = server.await {
                        warn!("webhook server stopped: {err:#}");
                    }
                }
                Err(err) => warn!("webhook server stopped: {err:#}"),
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tls_tests {
    use super::*;
    use crate::config::TlsConfig;

    /// With `[http.tls]` the listener answers HTTPS, and a wrong path stops
    /// the start instead of failing later.
    #[tokio::test]
    async fn serves_https_with_a_certificate() {
        let dir = std::env::temp_dir().join(format!("deskwatch-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let tls = TlsConfig {
            cert: dir.join("cert.pem"),
            key: dir.join("key.pem"),
        };
        std::fs::write(&tls.cert, certified.cert.pem()).unwrap();
        std::fs::write(&tls.key, certified.signing_key.serialize_pem()).unwrap();

        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let listen = SocketAddr::from(([127, 0, 0, 1], port));
        let mut hooks = Hooks::default();
        hooks.add(
            "/",
            Router::new().route("/", axum::routing::get(|| async { "ok" })),
        );
        hooks.start(listen, Some(&tls)).await.unwrap();

        let status = crate::health::http_get(listen, true, "/").await.unwrap();
        assert_eq!(status, 200);

        let mut missing = Hooks::default();
        missing.add("/", Router::new());
        let bad = TlsConfig {
            cert: dir.join("nope.pem"),
            key: dir.join("nope.key"),
        };
        let other = SocketAddr::from(([127, 0, 0, 1], 0));
        assert!(missing.start(other, Some(&bad)).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
