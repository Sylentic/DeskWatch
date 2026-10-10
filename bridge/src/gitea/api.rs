//! Gitea REST API: the two reads the bridge polls, behind a small trait.
//!
//! `GiteaApi` keeps the polling loop independent of HTTP details, so tests
//! can use a fake and a Gitea upgrade (1.27 to 28) that changes an endpoint
//! only touches `HttpApi`.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};
use tracing::{debug, warn};

use super::payload::{Job, PullRequest};
use super::runners::{ApiRunner, RunnerList, RunnerScope};
use super::{GiteaEvent, JobRef};
use crate::source::{Health, Secret};

/// Most PRs fetched per repository. The total count comes from a header, so
/// only the page listing is capped.
const PULLS_PAGE_SIZE: u32 = 50;

/// Give up on a request after this long, so a hung Gitea cannot stall polling.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Runners fetched per page. Gitea caps a page at its own maximum (50 unless
/// the admin changed it) and the loop below follows the pages either way.
const RUNNERS_PAGE_SIZE: u32 = 50;

/// Most pages of runners read per scope, so a broken `total_count` cannot
/// loop for ever. 20 pages are 1000 runners.
const MAX_RUNNER_PAGES: u32 = 20;

/// Open pull requests of one repository.
#[derive(Debug, Clone, Default)]
pub struct OpenPulls {
    /// First page of open PRs.
    pub pulls: Vec<PullRequest>,
    /// Total open PRs, which can be more than `pulls.len()`.
    pub total: u32,
}

/// What the bridge reads from Gitea.
pub trait GiteaApi: Send + Sync + 'static {
    /// One Actions job with its steps.
    fn job(&self, repo: &str, id: u64) -> impl Future<Output = Result<Job>> + Send;

    /// Open pull requests of `owner/name`.
    fn open_pulls(&self, repo: &str) -> impl Future<Output = Result<OpenPulls>> + Send;

    /// Every runner of a scope, all pages.
    fn runners(&self, scope: &RunnerScope) -> impl Future<Output = Result<Vec<ApiRunner>>> + Send;
}

/// `GiteaApi` over HTTP with an optional read-only token.
pub struct HttpApi {
    client: reqwest::Client,
    /// Base URL without trailing slash, such as `https://gitea.example.com`.
    base_url: String,
    token: Option<Secret>,
}

impl HttpApi {
    pub fn new(base_url: &str, token: Option<Secret>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("deskwatch-bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot build HTTP client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let request = self.client.get(format!("{}/api/v1{path}", self.base_url));
        match &self.token {
            Some(token) => request.header("Authorization", format!("token {}", token.expose())),
            None => request,
        }
    }
}

impl GiteaApi for HttpApi {
    async fn job(&self, repo: &str, id: u64) -> Result<Job> {
        let path = format!("/repos/{repo}/actions/jobs/{id}");
        let response = self.get(&path).send().await?.error_for_status()?;
        response.json().await.context("invalid job JSON")
    }

    async fn open_pulls(&self, repo: &str) -> Result<OpenPulls> {
        let path = format!("/repos/{repo}/pulls");
        let response = self
            .get(&path)
            .query(&[("state", "open"), ("limit", &PULLS_PAGE_SIZE.to_string())])
            .send()
            .await?
            .error_for_status()?;
        // Gitea puts the full count in X-Total-Count; fall back to the page size.
        let total = response
            .headers()
            .get("x-total-count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let pulls: Vec<PullRequest> = response.json().await.context("invalid pulls JSON")?;
        let total = total.unwrap_or(pulls.len() as u32);
        Ok(OpenPulls { pulls, total })
    }

    async fn runners(&self, scope: &RunnerScope) -> Result<Vec<ApiRunner>> {
        let path = scope.path();
        let mut runners = Vec::new();
        for page in 1..=MAX_RUNNER_PAGES {
            let response = self
                .get(&path)
                .query(&[
                    ("limit", RUNNERS_PAGE_SIZE.to_string()),
                    ("page", page.to_string()),
                ])
                .send()
                .await?
                .error_for_status()?;
            let list: RunnerList = response.json().await.context("invalid runners JSON")?;
            let empty = list.runners.is_empty();
            runners.extend(list.runners);
            if empty || runners.len() >= list.total_count as usize {
                break;
            }
        }
        Ok(runners)
    }
}

/// What to poll and how often.
#[derive(Debug, Clone)]
pub struct Polling {
    /// Repositories (`owner/name`) whose open PRs are counted.
    pub repos: Vec<String>,
    /// Runner lists to read. Empty means runners are not polled.
    pub runner_scopes: Vec<RunnerScope>,
    /// Step progress of running jobs.
    pub job: Duration,
    pub pulls: Duration,
    pub runners: Duration,
}

/// Poll forever and send results to the main loop.
///
/// - Every `job_poll` it fetches each job in `jobs` (kept up to date by the
///   main loop) for step progress. Nothing is fetched while no job runs.
/// - Every `pulls` interval it fetches the open PRs of each repo.
/// - Every `runners` interval it fetches each runner scope, if any are set.
///
/// Requests run one after another, so a slow Gitea never piles them up.
pub async fn poll_loop(
    api: impl GiteaApi,
    polling: Polling,
    jobs: watch::Receiver<Vec<JobRef>>,
    events: mpsc::Sender<GiteaEvent>,
) {
    let mut job_ticker = tokio::time::interval(polling.job);
    let mut pulls_ticker = tokio::time::interval(polling.pulls);
    let mut runners_ticker = tokio::time::interval(polling.runners);
    for ticker in [&mut job_ticker, &mut pulls_ticker, &mut runners_ticker] {
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    }

    loop {
        let event_batch = tokio::select! {
            _ = job_ticker.tick() => {
                // Copy the list so the watch lock is not held across awaits.
                let running = jobs.borrow().clone();
                poll_jobs(&api, &running).await
            }
            _ = pulls_ticker.tick() => poll_pulls(&api, &polling.repos).await,
            _ = runners_ticker.tick() => poll_runners(&api, &polling.runner_scopes).await,
        };
        for event in event_batch {
            if events.send(event).await.is_err() {
                return; // main loop has stopped
            }
        }
    }
}

async fn poll_jobs(api: &impl GiteaApi, jobs: &[JobRef]) -> Vec<GiteaEvent> {
    let mut out = Vec::new();
    for job_ref in jobs {
        match api.job(&job_ref.repo, job_ref.id).await {
            Ok(job) => out.push(GiteaEvent::JobPolled {
                repo: job_ref.repo.clone(),
                job,
            }),
            // Keep the last known progress; the next tick tries again.
            Err(err) => warn!(repo = %job_ref.repo, id = job_ref.id, "cannot poll job: {err:#}"),
        }
    }
    out
}

async fn poll_pulls(api: &impl GiteaApi, repos: &[String]) -> Vec<GiteaEvent> {
    let mut out = Vec::new();
    for repo in repos {
        let pulls = match api.open_pulls(repo).await {
            Ok(pulls) => {
                debug!(%repo, total = pulls.total, "polled open PRs");
                Ok(pulls)
            }
            Err(err) => {
                warn!(%repo, "cannot poll open PRs: {err:#}");
                Err(Health::from_error(&err))
            }
        };
        out.push(GiteaEvent::PullsPolled {
            repo: repo.clone(),
            pulls,
        });
    }
    out
}

async fn poll_runners(api: &impl GiteaApi, scopes: &[RunnerScope]) -> Vec<GiteaEvent> {
    let mut out = Vec::new();
    for scope in scopes {
        match api.runners(scope).await {
            Ok(runners) => {
                debug!(%scope, count = runners.len(), "polled runners");
                out.push(GiteaEvent::RunnersPolled {
                    scope: scope.clone(),
                    runners,
                });
            }
            // Keep the last list: the page greys it out when it gets old. A
            // 403 here usually means the token lacks the scope for this list.
            Err(err) => warn!(%scope, "cannot poll runners: {err:#}"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake Gitea: jobs are always on step 2 of 3; one repo fails.
    struct FakeApi;

    impl GiteaApi for FakeApi {
        async fn job(&self, _repo: &str, id: u64) -> Result<Job> {
            let steps = serde_json::json!([
                { "name": "a", "status": "completed" },
                { "name": "b", "status": "in_progress" },
                { "name": "c", "status": "queued" }
            ]);
            Ok(Job {
                id,
                status: "in_progress".into(),
                steps: serde_json::from_value(steps)?,
                ..Default::default()
            })
        }

        async fn open_pulls(&self, repo: &str) -> Result<OpenPulls> {
            anyhow::ensure!(repo != "me/broken", "HTTP 500");
            Ok(OpenPulls {
                pulls: vec![],
                total: 4,
            })
        }

        async fn runners(&self, scope: &RunnerScope) -> Result<Vec<ApiRunner>> {
            anyhow::ensure!(*scope != RunnerScope::Admin, "HTTP 403");
            Ok(vec![ApiRunner {
                name: "runner-1".into(),
                status: "idle".into(),
                ..Default::default()
            }])
        }
    }

    #[tokio::test(start_paused = true)]
    async fn poll_loop_sends_jobs_and_pulls() {
        let job = JobRef {
            repo: "me/demo".into(),
            id: 7,
        };
        let (_jobs_tx, jobs_rx) = watch::channel(vec![job]);
        let (tx, mut rx) = mpsc::channel(16);
        let repos = vec!["me/demo".to_string(), "me/broken".to_string()];
        let polling = Polling {
            repos,
            runner_scopes: vec![RunnerScope::Admin, RunnerScope::User],
            job: Duration::from_secs(5),
            pulls: Duration::from_secs(60),
            runners: Duration::from_secs(30),
        };
        tokio::spawn(poll_loop(FakeApi, polling, jobs_rx, tx));

        let mut polled_job = false;
        let mut pull_results = Vec::new();
        let mut runner_results = Vec::new();
        while !(polled_job && pull_results.len() == 2 && !runner_results.is_empty()) {
            match rx.recv().await.unwrap() {
                GiteaEvent::JobPolled { job, .. } => {
                    assert_eq!(job.id, 7);
                    polled_job = true;
                }
                GiteaEvent::PullsPolled { repo, pulls } => {
                    pull_results.push((repo, pulls.map(|p| p.total)));
                }
                GiteaEvent::RunnersPolled { scope, runners } => {
                    runner_results.push((scope, runners.len()));
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(
            pull_results,
            [
                ("me/demo".to_string(), Ok(4)),
                ("me/broken".to_string(), Err(Health::Unreachable))
            ]
        );
        // The admin scope fails (403) and is skipped; the user scope arrives.
        assert_eq!(runner_results, [(RunnerScope::User, 1)]);
    }

    /// A throwaway Gitea that serves 3 runners over pages of 2, to check the
    /// path, the token header and that every page is read.
    #[tokio::test]
    async fn http_runners_follow_the_pages() {
        use axum::extract::Query;
        use axum::http::HeaderMap;
        use axum::routing::get;
        use serde_json::json;

        async fn page(
            headers: HeaderMap,
            Query(q): Query<std::collections::HashMap<String, String>>,
        ) -> axum::Json<serde_json::Value> {
            assert_eq!(headers["authorization"], "token t0ken");
            let all = ["a", "b", "c"];
            let page: usize = q["page"].parse().unwrap();
            let names: Vec<_> = all.iter().skip((page - 1) * 2).take(2).collect();
            let runners: Vec<_> = names
                .iter()
                .map(|n| json!({ "name": n, "status": "idle" }))
                .collect();
            axum::Json(json!({ "runners": runners, "total_count": 3 }))
        }

        let app = axum::Router::new().route("/api/v1/orgs/team/actions/runners", get(page));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let api = HttpApi::new(&format!("http://{addr}/"), Some(Secret::new("t0ken"))).unwrap();
        let runners = api.runners(&RunnerScope::Org("team".into())).await.unwrap();
        let names: Vec<_> = runners.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }
}
