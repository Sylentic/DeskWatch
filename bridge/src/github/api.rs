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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, ETAG, HeaderMap, IF_NONE_MATCH};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex as AsyncMutex;

use super::app::GithubApp;
use super::payload::{Issue, Job, Jobs, PullRequest, WorkflowRun, WorkflowRuns};
use crate::gitea::runners::{ApiRunner, RunnerList, RunnerScope};
use crate::source::{AuthRefused, Secret};

/// Default API root: github.com.
pub const GITHUB_COM: &str = "https://api.github.com";

/// REST API version the payload shapes were written against.
const API_VERSION: &str = "2022-11-28";

/// Most open PRs fetched per repository. GitHub has no total count header,
/// so a repository with more open PRs shows this many.
pub const PULLS_PAGE_SIZE: u32 = 100;

/// Issues fetched per poll for "new issue" alerts. Only the newest matter.
pub const ISSUES_PAGE_SIZE: u32 = 20;

/// Recent runs fetched per repository: enough to find the latest run of
/// each workflow in a busy repository.
const RUNS_PAGE_SIZE: u32 = 20;

/// Self-hosted runners per page (GitHub's maximum) and the most pages read per
/// scope, so a broken `total_count` cannot loop for ever.
const RUNNERS_PAGE_SIZE: u32 = 100;
const MAX_RUNNER_PAGES: u32 = 10;

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

/// An installation token is renewed this long before it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(300);

/// How requests are signed.
enum Auth {
    Token(Secret),
    App {
        app: Box<GithubApp>,
        /// The installation token in use and when to stop using it.
        cached: AsyncMutex<Option<(Secret, Instant)>>,
    },
}

/// The answer of GitHub's installation token endpoint.
#[derive(Deserialize)]
struct InstallationToken {
    token: Option<String>,
    expires_at: Option<String>,
    message: Option<String>,
}

/// GitHub API over HTTP, with a read-only token or as a GitHub App.
pub struct HttpApi {
    client: reqwest::Client,
    /// API root without trailing slash.
    base_url: String,
    auth: Auth,
    /// Last answer per URL. Holds a few entries per repository; job lists
    /// are dropped with `forget_jobs` once their run is over.
    cache: Mutex<HashMap<String, Cached>>,
    /// `None` when the server sends no rate limit headers (often GHES).
    quota: Mutex<Option<Quota>>,
}

impl HttpApi {
    /// Sign in with a fine-grained token.
    pub fn new(base_url: &str, token: Secret) -> Result<Self> {
        Self::build(base_url, Auth::Token(token))
    }

    /// Sign in as a GitHub App installation.
    pub fn with_app(base_url: &str, app: GithubApp) -> Result<Self> {
        let auth = Auth::App {
            app: Box::new(app),
            cached: AsyncMutex::new(None),
        };
        Self::build(base_url, auth)
    }

    fn build(base_url: &str, auth: Auth) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("deskwatch-bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot build HTTP client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            auth,
            cache: Mutex::new(HashMap::new()),
            quota: Mutex::new(None),
        })
    }

    /// The `Authorization` header value. As an App, the installation token is
    /// reused until shortly before it expires (or `renew` asks for a new one).
    async fn authorization(&self, renew: bool) -> Result<String> {
        let (app, cached) = match &self.auth {
            Auth::Token(token) => return Ok(format!("Bearer {}", token.expose())),
            Auth::App { app, cached } => (app, cached),
        };
        let mut cached = cached.lock().await;
        if !renew
            && let Some((token, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(format!("Bearer {}", token.expose()));
        }
        let (token, lifetime) = self.fetch_installation_token(app).await?;
        let until = Instant::now() + lifetime.saturating_sub(TOKEN_MARGIN);
        let header = format!("Bearer {}", token.expose());
        *cached = Some((token, until));
        Ok(header)
    }

    /// Trade the App's signed token for an installation token. GitHub refusing
    /// the App (wrong key, unknown App or installation) is `AuthRefused`;
    /// GitHub being unreachable is an ordinary request error.
    async fn fetch_installation_token(&self, app: &GithubApp) -> Result<(Secret, Duration)> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.base_url, app.installation_id
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(app.jwt(now)?)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .await
            .context("GitHub App token request")?;
        let status = response.status();
        let body = response.bytes().await?;
        let answer: InstallationToken =
            serde_json::from_slice(&body).unwrap_or(InstallationToken {
                token: None,
                expires_at: None,
                message: None,
            });
        if let (true, Some(token)) = (status.is_success(), answer.token) {
            let lifetime = answer
                .expires_at
                .as_deref()
                .and_then(|t| {
                    time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .map(|t| (t - time::OffsetDateTime::now_utc()).whole_seconds())
                .filter(|s| *s > 0)
                .map_or(Duration::from_secs(1800), |s| Duration::from_secs(s as u64));
            return Ok((Secret::new(token), lifetime));
        }
        if status.is_client_error() {
            let why = answer
                .message
                .as_deref()
                .and_then(|m| m.lines().next())
                .unwrap_or("no reason given");
            return Err(anyhow::Error::new(AuthRefused)
                .context(format!("GitHub refused the App ({status}): {why}")));
        }
        anyhow::bail!("GitHub answered HTTP {status} to the App token request")
    }

    /// Can the token see `owner/name`? One cheap request, for the setup
    /// page's connection test.
    pub async fn repo(&self, repo: &str) -> Result<()> {
        let _: serde_json::Value = self
            .get_json(&format!("/repos/{repo}"))
            .await
            .context("repository")?;
        Ok(())
    }

    /// Open pull requests of `owner/name`, newest first.
    pub async fn open_pulls(&self, repo: &str) -> Result<Vec<PullRequest>> {
        let path = format!("/repos/{repo}/pulls?state=open&per_page={PULLS_PAGE_SIZE}");
        self.get_json(&path).await.context("open pulls")
    }

    /// The newest open issues of `owner/name` (pull requests included, the
    /// caller skips them).
    pub async fn recent_issues(&self, repo: &str) -> Result<Vec<Issue>> {
        let path = format!(
            "/repos/{repo}/issues?state=open&sort=created&direction=desc&per_page={ISSUES_PAGE_SIZE}"
        );
        self.get_json(&path).await.context("open issues")
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

    /// Every self-hosted runner of a repository or organisation, all pages.
    /// The runner list is filed under Administration (repository) and
    /// Self-hosted runners (organisation), so a token made for PRs and
    /// Actions gets `403` here.
    pub async fn runners(&self, scope: &RunnerScope) -> Result<Vec<ApiRunner>> {
        let base = scope.path();
        let mut runners = Vec::new();
        for page in 1..=MAX_RUNNER_PAGES {
            let path = format!("{base}?per_page={RUNNERS_PAGE_SIZE}&page={page}");
            let list: RunnerList = self.get_json(&path).await.context("runners")?;
            let empty = list.runners.is_empty();
            runners.extend(list.runners);
            if empty || runners.len() >= list.total_count as usize {
                break;
            }
        }
        Ok(runners)
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

        let send = |authorization: String| {
            let mut request = self
                .client
                .get(&url)
                .header(AUTHORIZATION, authorization)
                .header(ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", API_VERSION);
            if let Some(etag) = &etag {
                request = request.header(IF_NONE_MATCH, etag);
            }
            request.send()
        };
        let mut response = send(self.authorization(false).await?).await?;
        // An installation token can be revoked before it runs out: get a new one once.
        if response.status() == StatusCode::UNAUTHORIZED && matches!(self.auth, Auth::App { .. }) {
            response = send(self.authorization(true).await?).await?;
        }
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
            .route(&format!("{repo}/actions/runners"), get(runners_handler))
            .route("/api/v3/orgs/{org}/actions/runners", get(runners_handler))
            .with_state(seen);
        serve(app).await
    }

    async fn runners_handler(State(seen): State<Arc<Seen>>, headers: HeaderMap) -> Response {
        fixture(
            &seen,
            "runners",
            headers,
            include_str!("testdata/runners.json"),
        )
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

    #[tokio::test]
    async fn runners_of_a_repository_and_an_organisation() {
        let seen = Arc::new(Seen::default());
        let api = api(&mock_github(seen.clone()).await);

        let repo = api
            .runners(&RunnerScope::Repo("your-user/demo".into()))
            .await
            .unwrap();
        let names: Vec<_> = repo.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["build-box-1", "build-box-2", "build-box-3"]);
        // The second poll sends the ETag back and gets the kept list.
        let org = api
            .runners(&RunnerScope::Org("your-org".into()))
            .await
            .unwrap();
        assert_eq!(org.len(), 3);
        api.runners(&RunnerScope::Org("your-org".into()))
            .await
            .unwrap();

        let requests = seen.requests.lock().unwrap();
        assert!(requests.iter().all(|(path, _)| path == "runners"));
        assert_eq!(requests[0].1["authorization"], "Bearer test-token");
        assert!(
            requests[2].1.get("if-none-match").is_some(),
            "ETag sent back"
        );
    }

    /// The token lacks the runner right: a plain 403, not a rate limit.
    #[tokio::test]
    async fn runner_list_without_the_right_is_auth_failed() {
        let app = Router::new().route(
            "/api/v3/orgs/{org}/actions/runners",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    "{\"message\":\"Resource not accessible by personal access token\"}",
                )
            }),
        );
        let api = api(&serve(app).await);
        let err = api
            .runners(&RunnerScope::Org("your-org".into()))
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
    }
    // -- GitHub App sign-in ---------------------------------------------

    use axum::routing::post;
    use base64::Engine as _;
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    use crate::github::app::GithubApp;
    use crate::github::app::tests::{PKCS1, PUBLIC, pem};

    /// A mock GitHub: hands out `inst-<n>` installation tokens for a JWT signed
    /// with the test key, and lets only the token named in `valid` read.
    #[derive(Default)]
    struct AppMock {
        issued: std::sync::atomic::AtomicUsize,
        valid: Mutex<String>,
        /// Answer for the token endpoint: 0 = fine, else this status.
        refuse: std::sync::atomic::AtomicU16,
    }

    async fn app_mock(mock: Arc<AppMock>) -> String {
        let router = Router::new()
            .route(
                "/api/v3/app/installations/678/access_tokens",
                post(
                    |State(mock): State<Arc<AppMock>>, headers: HeaderMap| async move {
                        let refuse = mock.refuse.load(std::sync::atomic::Ordering::SeqCst);
                        if refuse != 0 {
                            return (
                                StatusCode::from_u16(refuse).unwrap(),
                                "{\"message\":\"A JSON web token could not be decoded\"}"
                                    .to_string(),
                            );
                        }
                        let jwt = headers["authorization"]
                            .to_str()
                            .unwrap()
                            .strip_prefix("Bearer ")
                            .unwrap()
                            .to_string();
                        let parts: Vec<&str> = jwt.split('.').collect();
                        let public = base64::engine::general_purpose::STANDARD
                            .decode(PUBLIC)
                            .unwrap();
                        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
                            .decode(parts[2])
                            .unwrap();
                        if UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public)
                            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
                            .is_err()
                        {
                            return (StatusCode::UNAUTHORIZED, "{}".into());
                        }
                        let n = mock
                            .issued
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                            + 1;
                        let expires = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
                        let expires = format!(
                            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                            expires.year(),
                            expires.month() as u8,
                            expires.day(),
                            expires.hour(),
                            expires.minute(),
                            expires.second()
                        );
                        (
                            StatusCode::CREATED,
                            format!("{{\"token\":\"inst-{n}\",\"expires_at\":\"{expires}\"}}"),
                        )
                    },
                ),
            )
            .route(
                "/api/v3/repos/{owner}/{repo}",
                get(
                    |State(mock): State<Arc<AppMock>>, headers: HeaderMap| async move {
                        let want = format!("Bearer {}", mock.valid.lock().unwrap());
                        if headers["authorization"] == want.as_str() {
                            (StatusCode::OK, "{}")
                        } else {
                            (StatusCode::UNAUTHORIZED, "{}")
                        }
                    },
                ),
            )
            .with_state(mock);
        serve(router).await
    }

    fn app_api(base_url: &str, installation: &str) -> HttpApi {
        let key = pem("RSA PRIVATE KEY", PKCS1);
        HttpApi::with_app(
            base_url,
            GithubApp::new("12345", installation, &key).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn app_signs_in_and_reuses_the_installation_token() {
        let mock = Arc::new(AppMock::default());
        *mock.valid.lock().unwrap() = "inst-1".into();
        let api = app_api(&app_mock(mock.clone()).await, "678");
        api.repo("o/a").await.unwrap();
        api.repo("o/b").await.unwrap();
        assert_eq!(mock.issued.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn app_gets_a_new_token_once_when_the_old_one_is_revoked() {
        let mock = Arc::new(AppMock::default());
        *mock.valid.lock().unwrap() = "inst-2".into();
        let api = app_api(&app_mock(mock.clone()).await, "678");
        api.repo("o/a").await.unwrap();
        assert_eq!(mock.issued.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn app_refused_by_github_is_auth_failed_and_names_the_reason_only() {
        let mock = Arc::new(AppMock::default());
        let url = app_mock(mock.clone()).await;
        // An installation the mock does not know.
        let err = app_api(&url, "999").repo("o/a").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
        mock.refuse.store(401, std::sync::atomic::Ordering::SeqCst);
        let err = app_api(&url, "678").repo("o/a").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
        let text = format!("{err:#}");
        assert!(text.contains("could not be decoded"), "{text}");
        assert!(!text.contains("PRIVATE KEY"));
    }
}
