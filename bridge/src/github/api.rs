//! GitHub REST API client with ETag caching and rate limit tracking.
//!
//! Every answer is kept with its `ETag`. The next request for the same URL
//! sends it as `If-None-Match`; a `304 Not Modified` returns the kept body.
//! On github.com a 304 does not count against the rate limit, so polling an
//! idle repository every minute is nearly free.
//!
//! `base_url` is the API root, which differs per kind of host:
//! `https://api.github.com`, `https://ghe.example.com/api/v3` (Enterprise
//! Server) or `https://api.your-subdomain.ghe.com` (GHE.com).

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, ETAG, HeaderMap, IF_NONE_MATCH};
use serde::de::DeserializeOwned;

use super::payload::{Job, Jobs, PullRequest, WorkflowRun, WorkflowRuns};
use crate::source::Secret;

/// Default API root: github.com.
pub const GITHUB_COM: &str = "https://api.github.com";

/// REST API version the payload shapes were written against.
const API_VERSION: &str = "2022-11-28";

/// Most open PRs fetched per repository. GitHub has no total count header,
/// so a repository with more open PRs shows this many.
pub const PULLS_PAGE_SIZE: u32 = 100;

/// Recent runs fetched per repository: enough to find the latest run of
/// each workflow in a busy repository.
const RUNS_PAGE_SIZE: u32 = 20;

/// Give up on a request after this long, so a hung server cannot stall polling.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The rate limit ran out (403 or 429 with no requests left). Counts as
/// unreachable, not as a refused token.
#[derive(Debug)]
pub struct RateLimited;

impl fmt::Display for RateLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GitHub rate limit exhausted")
    }
}

impl std::error::Error for RateLimited {}

/// An answer kept for `If-None-Match`.
struct Cached {
    etag: String,
    body: Vec<u8>,
}

/// Requests left in the current rate limit window, from the last answer.
#[derive(Debug, Clone, Copy)]
struct Quota {
    remaining: u32,
    limit: u32,
}

/// GitHub API over HTTP with a read-only token.
pub struct HttpApi {
    client: reqwest::Client,
    /// API root without trailing slash.
    base_url: String,
    token: Secret,
    /// Last answer per URL. Holds a few entries per repository; job lists
    /// are dropped with `forget_jobs` once their run is over.
    cache: Mutex<HashMap<String, Cached>>,
    /// `None` when the server sends no rate limit headers (often GHES).
    quota: Mutex<Option<Quota>>,
}

impl HttpApi {
    pub fn new(base_url: &str, token: Secret) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("deskwatch-bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot build HTTP client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            cache: Mutex::new(HashMap::new()),
            quota: Mutex::new(None),
        })
    }

    /// Open pull requests of `owner/name`, newest first.
    pub async fn open_pulls(&self, repo: &str) -> Result<Vec<PullRequest>> {
        let path = format!("/repos/{repo}/pulls?state=open&per_page={PULLS_PAGE_SIZE}");
        self.get_json(&path).await.context("open pulls")
    }

    /// Recent workflow runs of `owner/name`, newest first.
    pub async fn runs(&self, repo: &str) -> Result<Vec<WorkflowRun>> {
        let path = format!("/repos/{repo}/actions/runs?per_page={RUNS_PAGE_SIZE}");
        let runs: WorkflowRuns = self.get_json(&path).await.context("workflow runs")?;
        Ok(runs.workflow_runs)
    }

    /// Jobs of one run, with their steps.
    pub async fn jobs(&self, repo: &str, run_id: u64) -> Result<Vec<Job>> {
        let jobs: Jobs = self
            .get_json(&jobs_path(repo, run_id))
            .await
            .context("run jobs")?;
        Ok(jobs.jobs)
    }

    /// Drop the kept job list of a finished run.
    pub fn forget_jobs(&self, repo: &str, run_id: u64) {
        let url = format!("{}{}", self.base_url, jobs_path(repo, run_id));
        self.cache.lock().unwrap().remove(&url);
    }

    /// True when less than 10 % of the rate limit is left, so the poller
    /// should slow down until the window resets.
    pub fn low_on_quota(&self) -> bool {
        self.quota
            .lock()
            .unwrap()
            .is_some_and(|q| q.remaining.saturating_mul(10) < q.limit)
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.base_url);
        let etag = self.cache.lock().unwrap().get(&url).map(|c| c.etag.clone());

        let mut request = self
            .client
            .get(&url)
            .bearer_auth(self.token.expose())
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send().await?;
        let rate_limited = self.note_quota(response.headers());

        if response.status() == StatusCode::NOT_MODIFIED {
            let cache = self.cache.lock().unwrap();
            let cached = cache
                .get(&url)
                .context("304 for an answer that was not kept")?;
            return serde_json::from_slice(&cached.body).context("invalid kept JSON");
        }
        if rate_limited
            && matches!(
                response.status(),
                StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
            )
        {
            return Err(RateLimited.into());
        }

        let response = response.error_for_status()?;
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = response.bytes().await?.to_vec();
        let value = serde_json::from_slice(&body).context("invalid JSON")?;
        if let Some(etag) = etag {
            self.cache
                .lock()
                .unwrap()
                .insert(url, Cached { etag, body });
        }
        Ok(value)
    }

    /// Remember the rate limit headers. Returns true when none are left.
    fn note_quota(&self, headers: &HeaderMap) -> bool {
        let number = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u32>().ok())
        };
        let (Some(remaining), Some(limit)) =
            (number("x-ratelimit-remaining"), number("x-ratelimit-limit"))
        else {
            return false;
        };
        *self.quota.lock().unwrap() = Some(Quota { remaining, limit });
        remaining == 0
    }
}

fn jobs_path(repo: &str, run_id: u64) -> String {
    format!("/repos/{repo}/actions/runs/{run_id}/jobs?per_page=50")
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;

    use super::*;
    use crate::source::Health;

    /// What the mock server saw, so tests can check the request headers.
    #[derive(Default)]
    pub(crate) struct Seen {
        pub requests: Mutex<Vec<(String, HeaderMap)>>,
    }

    /// Answer like GitHub: the fixture with an ETag, or 304 when the client
    /// already has it. Rate limit headers say 4,000 of 5,000 left.
    fn fixture(seen: &Seen, path: &str, headers: HeaderMap, body: &'static str) -> Response {
        seen.requests
            .lock()
            .unwrap()
            .push((path.to_string(), headers.clone()));
        let etag = format!("\"{:x}\"", body.len());
        let mut response =
            if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
                StatusCode::NOT_MODIFIED.into_response()
            } else {
                ([("content-type", "application/json")], body).into_response()
            };
        let h = response.headers_mut();
        h.insert("etag", HeaderValue::from_str(&etag).unwrap());
        h.insert("x-ratelimit-limit", HeaderValue::from_static("5000"));
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("4000"));
        response
    }

    /// A mock GitHub Enterprise Server answering under `/api/v3` with the
    /// sanitised fixtures. Returns the API root to use as `base_url`.
    pub(crate) async fn mock_github(seen: Arc<Seen>) -> String {
        let repo = "/api/v3/repos/{owner}/{repo}";
        let app = Router::new()
            .route(
                &format!("{repo}/pulls"),
                get(
                    |State(seen): State<Arc<Seen>>, headers: HeaderMap| async move {
                        fixture(&seen, "pulls", headers, include_str!("testdata/pulls.json"))
                    },
                ),
            )
            .route(
                &format!("{repo}/actions/runs"),
                get(
                    |State(seen): State<Arc<Seen>>, headers: HeaderMap| async move {
                        fixture(&seen, "runs", headers, include_str!("testdata/runs.json"))
                    },
                ),
            )
            .route(
                &format!("{repo}/actions/runs/{{run_id}}/jobs"),
                get(
                    |State(seen): State<Arc<Seen>>, headers: HeaderMap| async move {
                        fixture(&seen, "jobs", headers, include_str!("testdata/jobs.json"))
                    },
                ),
            )
            .with_state(seen);
        serve(app).await
    }

    pub(crate) async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/api/v3")
    }

    fn api(base_url: &str) -> HttpApi {
        HttpApi::new(base_url, Secret::new("test-token")).unwrap()
    }

    #[tokio::test]
    async fn sends_token_and_reuses_etag() {
        let seen = Arc::new(Seen::default());
        let base = mock_github(seen.clone()).await;
        let api = api(&format!("{base}/"));

        assert_eq!(api.open_pulls("your-user/demo").await.unwrap().len(), 2);
        // Second poll: 304, same data from the cache.
        let again = api.open_pulls("your-user/demo").await.unwrap();
        assert_eq!(again[0].title, "Add invoice export");
        assert_eq!(api.runs("your-user/demo").await.unwrap().len(), 4);
        assert_eq!(api.jobs("your-user/demo", 1003).await.unwrap()[0].id, 2001);
        assert!(!api.low_on_quota());

        let requests = seen.requests.lock().unwrap();
        let (_, first) = &requests[0];
        assert_eq!(first["authorization"], "Bearer test-token");
        assert_eq!(first["accept"], "application/vnd.github+json");
        assert_eq!(first["x-github-api-version"], API_VERSION);
        assert!(first.get("if-none-match").is_none());
        let (_, second) = &requests[1];
        assert!(second.get("if-none-match").is_some(), "ETag sent back");
    }

    #[tokio::test]
    async fn refused_token_is_auth_failed() {
        let app = Router::new().route(
            "/api/v3/repos/{owner}/{repo}/pulls",
            get(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    "{\"message\":\"Bad credentials\"}",
                )
            }),
        );
        let api = api(&serve(app).await);
        let err = api.open_pulls("your-user/demo").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
    }

    #[tokio::test]
    async fn exhausted_rate_limit_is_not_an_auth_failure() {
        let app = Router::new().route(
            "/api/v3/repos/{owner}/{repo}/pulls",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    [
                        ("x-ratelimit-limit", "5000"),
                        ("x-ratelimit-remaining", "0"),
                    ],
                    "{\"message\":\"API rate limit exceeded\"}",
                )
            }),
        );
        let api = api(&serve(app).await);
        let err = api.open_pulls("your-user/demo").await.unwrap_err();
        assert!(err.chain().any(|e| e.is::<RateLimited>()));
        assert_eq!(Health::from_error(&err), Health::Unreachable);
        assert!(api.low_on_quota());
    }

    #[tokio::test]
    async fn unreachable_server() {
        // Nothing listens on port 9 of localhost.
        let api = api("http://127.0.0.1:9/api/v3");
        let err = api.runs("your-user/demo").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::Unreachable);
    }
}
