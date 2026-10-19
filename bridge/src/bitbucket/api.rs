//! Bitbucket Cloud REST API 2.0 client.
//!
//! `base_url` is the API root, `https://api.bitbucket.org/2.0` unless a test
//! points it elsewhere. Requests carry a repository, project or workspace
//! access token as a bearer token. Lists come in pages of at most 100 with an
//! absolute `next` link, which is followed only when it stays on the same host
//! as `base_url`.
//!
//! Nothing is cached. Bitbucket Cloud limits how many requests a token may make
//! per hour, so the source polls gently (see `docs/bitbucket.md`); a `429` shows
//! as "not reachable" and polling backs off.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

use super::payload::{ApiRunner, Page, Pipeline, PullRequest, Step};
use super::runners::RunnerScope;
use crate::source::Secret;

/// Default API root.
pub const BITBUCKET_API: &str = "https://api.bitbucket.org/2.0";

/// Open pull requests per page (the API's default maximum is 100).
const PULLS_PAGE_SIZE: u32 = 50;

/// Most pages read per list, so a broken `next` link cannot loop for ever. With
/// the page size above this caps the open PRs read per repository at 250; the
/// `size` field still gives the exact count when Bitbucket sends it.
const MAX_PAGES: u32 = 5;

/// Recent pipelines fetched per repository.
const PIPELINES_PAGE_SIZE: u32 = 20;

/// Give up on a request after this long, so a hung server cannot stall polling.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A pipeline UUID as a path segment: `{...}` has its braces encoded.
fn encode_uuid(uuid: &str) -> String {
    uuid.replace('{', "%7B").replace('}', "%7D")
}

/// Bitbucket Cloud API over HTTP with a read-only token.
pub struct HttpApi {
    client: reqwest::Client,
    /// API root without trailing slash.
    root: String,
    token: Secret,
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
            root: base_url.trim().trim_end_matches('/').to_string(),
            token,
        })
    }

    /// Can the token see this repository? One cheap request, for the setup
    /// page's connection test.
    pub async fn repository(&self, repo: &str) -> Result<()> {
        let _: serde_json::Value = self
            .get_json(&format!("{}/repositories/{repo}", self.root))
            .await
            .context("repository")?;
        Ok(())
    }

    /// Open pull requests of a repository, and the exact total when Bitbucket
    /// sends one (`size`).
    pub async fn open_pull_requests(&self, repo: &str) -> Result<(Vec<PullRequest>, Option<u32>)> {
        let first = format!(
            "{}/repositories/{repo}/pullrequests?state=OPEN&pagelen={PULLS_PAGE_SIZE}",
            self.root
        );
        let (list, size) = self.all_pages(first).await.context("open pull requests")?;
        Ok((list, size))
    }

    /// The newest pipelines of a repository, newest first.
    pub async fn pipelines(&self, repo: &str) -> Result<Vec<Pipeline>> {
        let url = format!(
            "{}/repositories/{repo}/pipelines?sort=-created_on&pagelen={PIPELINES_PAGE_SIZE}",
            self.root
        );
        let page: Page<Pipeline> = self.get_json(&url).await.context("pipelines")?;
        Ok(page.values)
    }

    /// The steps of one pipeline, in the order they run.
    pub async fn steps(&self, repo: &str, pipeline_uuid: &str) -> Result<Vec<Step>> {
        let url = format!(
            "{}/repositories/{repo}/pipelines/{}/steps?pagelen=100",
            self.root,
            encode_uuid(pipeline_uuid)
        );
        let page: Page<Step> = self.get_json(&url).await.context("pipeline steps")?;
        Ok(page.values)
    }

    /// Every self-hosted runner of a scope. Needs the runner read right.
    pub async fn runners(&self, scope: &RunnerScope) -> Result<Vec<ApiRunner>> {
        let first = format!("{}{}?pagelen=100", self.root, scope.path());
        let (runners, _) = self.all_pages(first).await.context("runners")?;
        Ok(runners)
    }

    /// Read a list, following `next` links up to `MAX_PAGES`.
    async fn all_pages<T: DeserializeOwned>(&self, first: String) -> Result<(Vec<T>, Option<u32>)> {
        let mut out = Vec::new();
        let mut size = None;
        let mut url = Some(first);
        for _ in 0..MAX_PAGES {
            let Some(current) = url.take() else { break };
            let page: Page<T> = self.get_json(&current).await?;
            size = size.or(page.size);
            out.extend(page.values);
            url = page.next.filter(|next| self.same_host(next));
        }
        Ok((out, size))
    }

    /// Does this link stay on the API host? The token is never sent anywhere
    /// else, whatever a page claims as its `next`.
    fn same_host(&self, link: &str) -> bool {
        match (reqwest::Url::parse(&self.root), reqwest::Url::parse(link)) {
            (Ok(root), Ok(link)) => {
                root.scheme() == link.scheme()
                    && root.host_str() == link.host_str()
                    && root.port_or_known_default() == link.port_or_known_default()
            }
            _ => false,
        }
    }

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self
            .client
            .get(url)
            .bearer_auth(self.token.expose())
            .send()
            .await?
            .error_for_status()?;
        let body = response.bytes().await?;
        serde_json::from_slice(&body).context("invalid JSON")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;

    use super::*;
    use crate::source::Health;

    /// What the mock server saw: a short label and the request headers.
    #[derive(Default)]
    pub(crate) struct Seen {
        pub requests: Mutex<Vec<(String, HeaderMap)>>,
    }

    fn json(seen: &Seen, what: &str, headers: HeaderMap, body: &'static str) -> Response {
        seen.requests
            .lock()
            .unwrap()
            .push((what.to_string(), headers));
        ([("content-type", "application/json")], body).into_response()
    }

    /// A mock Bitbucket Cloud answering under `/2.0` with the fixtures written
    /// from the published API description. Returns the API root.
    pub(crate) async fn mock_bitbucket(seen: Arc<Seen>) -> String {
        let repo = "/2.0/repositories/{workspace}/{slug}";
        let app = Router::new()
            .route(
                &format!("{repo}/pullrequests"),
                get(
                    |State(seen): State<Arc<Seen>>, headers: HeaderMap| async move {
                        json(
                            &seen,
                            "pullrequests",
                            headers,
                            include_str!("testdata/pullrequests.json"),
                        )
                    },
                ),
            )
            .route(
                &format!("{repo}/pipelines"),
                get(
                    |State(seen): State<Arc<Seen>>, headers: HeaderMap| async move {
                        json(
                            &seen,
                            "pipelines",
                            headers,
                            include_str!("testdata/pipelines.json"),
                        )
                    },
                ),
            )
            .route(
                &format!("{repo}/pipelines/{{uuid}}/steps"),
                get(
                    |State(seen): State<Arc<Seen>>,
                     Path((_, _, uuid)): Path<(String, String, String)>,
                     headers: HeaderMap| async move {
                        // The failed deploy has its own steps; the running one the others.
                        if uuid == "{aaaa0003-0000-4000-8000-000000000003}" {
                            json(
                                &seen,
                                &format!("steps {uuid}"),
                                headers,
                                include_str!("testdata/failed_steps.json"),
                            )
                        } else {
                            json(
                                &seen,
                                &format!("steps {uuid}"),
                                headers,
                                include_str!("testdata/steps.json"),
                            )
                        }
                    },
                ),
            )
            .route(
                &format!("{repo}/pipelines-config/runners"),
                get(runners_handler),
            )
            .route(
                "/2.0/workspaces/{workspace}/pipelines-config/runners",
                get(runners_handler),
            )
            .with_state(seen);
        format!("{}/2.0", serve(app).await)
    }

    async fn runners_handler(State(seen): State<Arc<Seen>>, headers: HeaderMap) -> Response {
        json(
            &seen,
            "runners",
            headers,
            include_str!("testdata/runners.json"),
        )
    }

    /// Serve `app` on a free port and return `http://127.0.0.1:<port>`.
    pub(crate) async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn api(base_url: &str) -> HttpApi {
        HttpApi::new(base_url, Secret::new("test-token")).unwrap()
    }

    #[tokio::test]
    async fn sends_the_token_as_bearer_and_encodes_the_uuid() {
        let seen = Arc::new(Seen::default());
        let api = api(&format!("{}/", mock_bitbucket(seen.clone()).await));

        let (prs, size) = api.open_pull_requests("your-workspace/demo").await.unwrap();
        assert_eq!((prs.len(), size), (2, Some(2)));
        let pipelines = api.pipelines("your-workspace/demo").await.unwrap();
        assert_eq!(pipelines.len(), 4);
        let steps = api
            .steps("your-workspace/demo", &pipelines[0].uuid)
            .await
            .unwrap();
        assert_eq!(steps.len(), 4);

        let requests = seen.requests.lock().unwrap();
        assert_eq!(requests[0].1["authorization"], "Bearer test-token");
        assert_eq!(
            requests[2].0,
            "steps {aaaa0004-0000-4000-8000-000000000004}"
        );
    }

    #[tokio::test]
    async fn follows_next_only_on_the_same_host() {
        let app = Router::new().route(
            "/2.0/repositories/{w}/{s}/pullrequests",
            get(|| async {
                (
                    [("content-type", "application/json")],
                    "{\"values\":[{\"id\":1}],\"next\":\"https://evil.example/steal\"}",
                )
            }),
        );
        let api = api(&format!("{}/2.0", serve(app).await));
        let (prs, size) = api.open_pull_requests("w/s").await.unwrap();
        assert_eq!(
            (prs.len(), size),
            (1, None),
            "the foreign link was not followed"
        );
    }

    #[tokio::test]
    async fn pages_are_followed_until_there_is_no_next() {
        use axum::extract::Query;
        use std::collections::HashMap;
        let app = Router::new().route(
            "/2.0/repositories/{w}/{s}/pullrequests",
            get(|Query(q): Query<HashMap<String, String>>, headers: HeaderMap| async move {
                let host = headers["host"].to_str().unwrap().to_string();
                let body = if q.get("page").map(String::as_str) == Some("2") {
                    "{\"values\":[{\"id\":3}]}".to_string()
                } else {
                    format!("{{\"values\":[{{\"id\":1}},{{\"id\":2}}],\"next\":\"http://{host}/2.0/repositories/w/s/pullrequests?page=2\"}}")
                };
                ([("content-type", "application/json")], body)
            }),
        );
        let api = api(&format!("{}/2.0", serve(app).await));
        let (prs, _) = api.open_pull_requests("w/s").await.unwrap();
        let ids: Vec<_> = prs.iter().map(|p| p.id).collect();
        assert_eq!(ids, [1, 2, 3]);
    }

    #[tokio::test]
    async fn refused_token_is_auth_failed() {
        let app = Router::new().route(
            "/2.0/repositories/{w}/{s}/pullrequests",
            get(|| async { (StatusCode::UNAUTHORIZED, "{\"type\":\"error\"}") }),
        );
        let api = api(&format!("{}/2.0", serve(app).await));
        let err = api.open_pull_requests("w/s").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
    }

    #[tokio::test]
    async fn unknown_repository_and_rate_limit_are_unreachable() {
        let app = Router::new()
            .route(
                "/2.0/repositories/{w}/{s}",
                get(|| async { (StatusCode::NOT_FOUND, "{\"type\":\"error\"}") }),
            )
            .route(
                "/2.0/repositories/{w}/{s}/pipelines",
                get(|| async { (StatusCode::TOO_MANY_REQUESTS, "Too many requests") }),
            );
        let api = api(&format!("{}/2.0", serve(app).await));
        for err in [
            api.repository("w/gone").await.unwrap_err(),
            api.pipelines("w/s").await.unwrap_err(),
        ] {
            assert_eq!(Health::from_error(&err), Health::Unreachable);
        }
    }

    #[tokio::test]
    async fn unreachable_server() {
        // Nothing listens on port 9 of localhost.
        let err = api("http://127.0.0.1:9/2.0")
            .pipelines("w/s")
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::Unreachable);
    }

    #[tokio::test]
    async fn runners_of_a_repository_and_a_workspace() {
        let seen = Arc::new(Seen::default());
        let api = api(&mock_bitbucket(seen.clone()).await);
        let repo = api
            .runners(&RunnerScope::Repo("your-workspace/demo".into()))
            .await
            .unwrap();
        assert_eq!(repo.len(), 4);
        let workspace = api
            .runners(&RunnerScope::Workspace("your-workspace".into()))
            .await
            .unwrap();
        assert_eq!(workspace[0].name, "build-box-1");
        assert!(
            seen.requests
                .lock()
                .unwrap()
                .iter()
                .all(|(w, _)| w == "runners")
        );
    }

    /// The token lacks the runner right: a plain 403.
    #[tokio::test]
    async fn runner_list_without_the_right_is_auth_failed() {
        let app = Router::new().route(
            "/2.0/workspaces/{w}/pipelines-config/runners",
            get(|| async { (StatusCode::FORBIDDEN, "{\"type\":\"error\"}") }),
        );
        let api = api(&format!("{}/2.0", serve(app).await));
        let err = api
            .runners(&RunnerScope::Workspace("w".into()))
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
    }
}
