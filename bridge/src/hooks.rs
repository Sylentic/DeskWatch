//! The one HTTP listener that push sources and the kiosk page share.
//!
//! Each webhook source adds its own route (`/webhook/gitea/<name>`, later
//! `/webhook/grafana/<name>` and so on) while it is being built. The listener
//! only opens when at least one route exists, so a bridge with no webhook
//! sources, no kiosk page and no MQTT (which adds `/healthz`) has no open port.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::Router;
use tokio::net::TcpListener;
use tracing::{info, warn};

/// Routes collected from the push sources.
#[derive(Default)]
pub struct Hooks {
    router: Router,
    paths: Vec<String>,
}

impl Hooks {
    /// Add one source's route. `router` must only handle `path`.
    pub fn add(&mut self, path: &str, router: Router) {
        self.router = std::mem::take(&mut self.router).merge(router);
        self.paths.push(path.to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
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
        for path in &self.paths {
            info!("serving http://{listen}{path}");
        }
        tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, self.router).await {
                warn!("webhook server stopped: {err:#}");
            }
        });
        Ok(())
    }
}
