//! Starts, stops and replaces source tasks while the bridge runs.
//!
//! The bridge builds its sources through the supervisor at startup. When the
//! setup page saves or removes a source it asks the supervisor to reconcile
//! again: sources whose configuration did not change keep running, new ones
//! start, removed ones stop, and a changed one is replaced. A source that
//! cannot be built (a credential went missing) leaves the old one running and
//! is reported back. Everything a stopped source reported is dropped from the
//! main loop, so a removed source disappears from the pages and the `warn`
//! badge.
//!
//! What cannot be changed here: the MQTT connection, the HTTP listener and
//! the rotation. Those still need a restart.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::azure_devops::AzureDevops;
use crate::config::Config;
use crate::gitea::{self, Gitea};
use crate::github::Github;
use crate::hooks::{Hooks, Webhooks};
use crate::prometheus::Prometheus;
use crate::source::{self, FactSink, SourceId, SourceMsg};

/// A source that was built from its config and is ready to run.
enum Built {
    Gitea(Gitea),
    Github(Github),
    AzureDevops(AzureDevops),
    Prometheus(Prometheus),
}

impl Built {
    fn start(self, tx: mpsc::Sender<SourceMsg>) -> JoinHandle<()> {
        match self {
            Built::Gitea(s) => source::spawn(s, tx),
            Built::Github(s) => source::spawn(s, tx),
            Built::AzureDevops(s) => source::spawn(s, tx),
            Built::Prometheus(s) => source::spawn(s, tx),
        }
    }
}

type Build = Box<dyn FnOnce(&mut Hooks) -> Result<Built> + Send>;

/// One source the config asks for.
struct Wanted {
    id: SourceId,
    /// The settings as text. Equal text means the running source is current.
    fingerprint: String,
    build: Build,
}

fn wanted(config: &Config) -> Vec<Wanted> {
    let mut out = Vec::new();
    for c in &config.source.gitea {
        let c = c.clone();
        out.push(Wanted {
            id: SourceId::new(gitea::KIND, &c.name),
            fingerprint: format!("{c:?}"),
            build: Box::new(move |hooks| Ok(Built::Gitea(Gitea::new(&c, hooks)?))),
        });
    }
    for c in &config.source.github {
        let c = c.clone();
        out.push(Wanted {
            id: SourceId::new("github", &c.name),
            fingerprint: format!("{c:?}"),
            build: Box::new(move |_| Ok(Built::Github(Github::new(&c)?))),
        });
    }
    for c in &config.source.azure_devops {
        let c = c.clone();
        out.push(Wanted {
            id: SourceId::new("azure_devops", &c.name),
            fingerprint: format!("{c:?}"),
            build: Box::new(move |_| Ok(Built::AzureDevops(AzureDevops::new(&c)?))),
        });
    }
    for c in &config.source.prometheus {
        let c = c.clone();
        out.push(Wanted {
            id: SourceId::new("prometheus", &c.name),
            fingerprint: format!("{c:?}"),
            build: Box::new(move |_| Ok(Built::Prometheus(Prometheus::new(&c)?))),
        });
    }
    out
}

struct Running {
    id: SourceId,
    fingerprint: String,
    handle: JoinHandle<()>,
}

/// What a reconcile did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub started: Vec<String>,
    pub stopped: Vec<String>,
    /// Sources that could not be built, with the reason. An older version of
    /// the same source, if there was one, keeps running.
    pub failed: Vec<(String, String)>,
}

/// Cheap to clone; all clones manage the same set of sources.
#[derive(Clone)]
pub struct Supervisor {
    running: Arc<Mutex<BTreeMap<String, Running>>>,
    tx: mpsc::Sender<SourceMsg>,
    webhooks: Webhooks,
}

impl Supervisor {
    pub fn new(tx: mpsc::Sender<SourceMsg>, webhooks: Webhooks) -> Self {
        Self {
            running: Arc::default(),
            tx,
            webhooks,
        }
    }

    /// Ids (`gitea:home`) of the sources whose task is running now. A source
    /// that stopped with an error is not in the list.
    pub async fn running(&self) -> BTreeSet<String> {
        let running = self.running.lock().await;
        running
            .iter()
            .filter(|(_, r)| !r.handle.is_finished())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Make the running sources match `config`. `touched` names sources to
    /// restart even though their settings are the same, because a credential
    /// they read was replaced.
    pub async fn reconcile(&self, config: &Config, touched: &[String]) -> Outcome {
        let mut running = self.running.lock().await;
        let wanted = wanted(config);
        let wanted_ids: BTreeSet<String> = wanted.iter().map(|w| w.id.to_string()).collect();
        let mut outcome = Outcome::default();

        let gone: Vec<String> = running
            .keys()
            .filter(|id| !wanted_ids.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            if let Some(old) = running.remove(&id) {
                self.stop(old, true).await;
                outcome.stopped.push(id);
            }
        }

        for want in wanted {
            let id = want.id.to_string();
            let current = running.get(&id).is_some_and(|r| {
                !r.handle.is_finished()
                    && r.fingerprint == want.fingerprint
                    && !touched.contains(&id)
            });
            if current {
                continue;
            }
            // Webhook routes of a Gitea source are registered while it is
            // built; the new route replaces the old one at the same path.
            let mut scratch = Hooks::sharing(self.webhooks.clone());
            let built = match (want.build)(&mut scratch) {
                Ok(built) => built,
                Err(err) => {
                    warn!(source = %id, "cannot start source: {err:#}");
                    outcome.failed.push((id, format!("{err:#}")));
                    continue;
                }
            };
            if let Some(old) = running.remove(&id) {
                // The new source already took over the webhook route.
                self.stop(old, false).await;
            }
            let handle = built.start(self.tx.clone());
            info!(source = %id, "source started");
            running.insert(
                id.clone(),
                Running {
                    id: want.id,
                    fingerprint: want.fingerprint,
                    handle,
                },
            );
            outcome.started.push(id);
        }
        outcome
    }

    /// Stop a source's task and drop what it reported. `drop_route` also
    /// removes its webhook route (not wanted when a new source took it over).
    async fn stop(&self, old: Running, drop_route: bool) {
        old.handle.abort();
        if drop_route && old.id.kind == gitea::KIND {
            self.webhooks.remove(&gitea::webhook::path(&old.id.name));
        }
        info!(source = %old.id, "source stopped");
        FactSink::forget(old.id, &self.tx).await;
    }
}

#[cfg(test)]
mod tests;
