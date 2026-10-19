//! GitLab REST API (v4) client.
//!
//! `base_url` is the address of the GitLab site (`https://gitlab.com` or
//! `https://gitlab.example.com`, with a sub path if GitLab runs under one).
//! `/api/v4` is added here, and accepted when it is already there. Projects
//! are addressed by their path (`group/project`), with the `/` encoded.
//!
//! Requests carry the token in the `PRIVATE-TOKEN` header. Nothing is cached:
//! a project costs two requests per poll, far below the gitlab.com limit.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;

use super::payload::{Job, MergeRequest, Pipeline};
use super::runners::{ApiRunner, RunnerScope};
use crate::source::Secret;

/// Default site: gitlab.com.
pub const GITLAB_COM: &str = "https://gitlab.com";

/// Most open merge requests fetched per project (GitLab's maximum page).
/// The `X-Total` header still gives the exact count.
pub const MRS_PAGE_SIZE: u32 = 100;

/// Recent pipelines fetched per project.
const PIPELINES_PAGE_SIZE: u32 = 20;

/// Runners per page (GitLab's maximum) and the most pages read per scope, so
/// a broken answer cannot loop for ever.
const RUNNERS_PAGE_SIZE: u32 = 100;
const MAX_RUNNER_PAGES: u32 = 5;

/// Most online runners asked whether they are working, per scope and poll.
const MAX_BUSY_LOOKUPS: usize = 50;

/// Give up on a request after this long, so a hung server cannot stall polling.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The API root for a site address: `/api/v4` is added unless it is there.
pub fn api_root(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/api/v4") {
        base.to_string()
    } else {
        format!("{base}/api/v4")
    }
}

/// The site address that goes with a `base_url`: the same without `/api/v4`.
pub fn site_root(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    base.strip_suffix("/api/v4").unwrap_or(base).to_string()
}

/// A project path as the `:id` part of a URL.
fn encode(path: &str) -> String {
    path.replace('/', "%2F")
}

/// GitLab API over HTTP with a read-only token.
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
            root: api_root(base_url),
            token,
        })
    }

    /// Can the token see this project? One cheap request, for the setup
    /// page's connection test.
    pub async fn project(&self, project: &str) -> Result<()> {
        let _: serde_json::Value = self
            .get_json(&format!("/projects/{}", encode(project)))
            .await
            .context("project")?
            .0;
        Ok(())
    }

    /// Open merge requests of a project, newest first, and the exact total
    /// when GitLab sends it (`X-Total`, left out above 10,000).
    pub async fn open_merge_requests(
        &self,
        project: &str,
    ) -> Result<(Vec<MergeRequest>, Option<u32>)> {
        let path = format!(
            "/projects/{}/merge_requests?state=opened&per_page={MRS_PAGE_SIZE}",
            encode(project)
        );
        let (list, headers): (Vec<MergeRequest>, _) =
            self.get_json(&path).await.context("open merge requests")?;
        let total = header_number(&headers, "x-total");
        Ok((list, total))
    }

    /// Recent pipelines of a project, newest first.
    pub async fn pipelines(&self, project: &str) -> Result<Vec<Pipeline>> {
        let path = format!(
            "/projects/{}/pipelines?order_by=id&sort=desc&per_page={PIPELINES_PAGE_SIZE}",
            encode(project)
        );
        Ok(self.get_json(&path).await.context("pipelines")?.0)
    }

    /// Every job of a pipeline (the latest attempt of each).
    pub async fn jobs(&self, project: &str, pipeline_id: u64) -> Result<Vec<Job>> {
        let path = format!(
            "/projects/{}/pipelines/{pipeline_id}/jobs?per_page=100",
            encode(project)
        );
        Ok(self.get_json(&path).await.context("pipeline jobs")?.0)
    }

    /// The failed jobs of a pipeline.
    pub async fn failed_jobs(&self, project: &str, pipeline_id: u64) -> Result<Vec<Job>> {
        let path = format!(
            "/projects/{}/pipelines/{pipeline_id}/jobs?scope[]=failed&per_page=100",
            encode(project)
        );
        Ok(self.get_json(&path).await.context("failed jobs")?.0)
    }

    /// Every runner of a scope, all pages, with `busy` filled in for the
    /// online ones. The runner lists need rights beyond reading code: the
    /// Maintainer role on a project, Owner on a group, an administrator
    /// token for the instance.
    pub async fn runners(&self, scope: &RunnerScope) -> Result<Vec<ApiRunner>> {
        let base = scope.path();
        let mut runners: Vec<ApiRunner> = Vec::new();
        for page in 1..=MAX_RUNNER_PAGES {
            let path = format!("{base}?per_page={RUNNERS_PAGE_SIZE}&page={page}");
            let (list, headers): (Vec<ApiRunner>, _) =
                self.get_json(&path).await.context("runners")?;
            runners.extend(list);
            // `X-Next-Page` is empty on the last page.
            if header_number(&headers, "x-next-page").is_none() {
                break;
            }
        }
        for runner in runners
            .iter_mut()
            .filter(|r| r.status == "online")
            .take(MAX_BUSY_LOOKUPS)
        {
            // A runner that will not say stays "idle": the list already
            // proved it is online, which is what matters for an outage.
            let path = format!("/runners/{}/jobs?status=running&per_page=1", runner.id);
            if let Ok((jobs, _)) = self.get_json::<Vec<serde_json::Value>>(&path).await {
                runner.busy = !jobs.is_empty();
            }
        }
        Ok(runners)
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<(T, HeaderMap)> {
        let url = format!("{}{path}", self.root);
        let response = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", self.token.expose())
            .send()
            .await?
            .error_for_status()?;
        let headers = response.headers().clone();
        let body = response.bytes().await?;
        let value = serde_json::from_slice(&body).context("invalid JSON")?;
        Ok((value, headers))
    }
}

fn header_number(headers: &HeaderMap, name: &str) -> Option<u32> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;

    use super::*;
    use crate::source::Health;

    /// What the mock server saw: the request path and headers.
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

    /// A mock GitLab answering under `/api/v4` with the fixtures written from
    /// the API documentation. Returns the site address to use as `base_url`.
    pub(crate) async fn mock_gitlab(seen: Arc<Seen>) -> String {
        let project = "/api/v4/projects/{id}";
        let app = Router::new()
            .route(
                &format!("{project}/merge_requests"),
                get(
                    |State(seen): State<Arc<Seen>>, Path(id): Path<String>, headers: HeaderMap| async move {
                        let mut response = json(
                            &seen,
                            &format!("mrs {id}"),
                            headers,
                            include_str!("testdata/merge_requests.json"),
                        );
                        response
                            .headers_mut()
                            .insert("x-total", HeaderValue::from_static("137"));
                        response
                    },
                ),
            )
            .route(
                &format!("{project}/pipelines"),
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
                &format!("{project}/pipelines/{{pid}}/jobs"),
                get(
                    |State(seen): State<Arc<Seen>>,
                     Path((_, pid)): Path<(String, u64)>,
                     axum::extract::RawQuery(query): axum::extract::RawQuery,
                     headers: HeaderMap| async move {
                        let failed = query.as_deref().is_some_and(|q| q.contains("scope[]=failed"));
                        if failed {
                            json(
                                &seen,
                                &format!("failed jobs {pid}"),
                                headers,
                                include_str!("testdata/failed_jobs.json"),
                            )
                        } else {
                            json(
                                &seen,
                                &format!("jobs {pid}"),
                                headers,
                                include_str!("testdata/jobs.json"),
                            )
                        }
                    },
                ),
            )
            .route(&format!("{project}/runners"), get(runners_handler))
            .route("/api/v4/groups/{id}/runners", get(runners_handler))
            .route(
                "/api/v4/runners/{id}/jobs",
                get(
                    |State(seen): State<Arc<Seen>>, Path(id): Path<u64>, headers: HeaderMap| async move {
                        // Only runner 12 has a job running.
                        let body = if id == 12 { "[{\"id\":1}]" } else { "[]" };
                        json(&seen, &format!("runner jobs {id}"), headers, body)
                    },
                ),
            )
            .with_state(seen);
        serve(app).await
    }

    async fn runners_handler(State(seen): State<Arc<Seen>>, headers: HeaderMap) -> Response {
        json(
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
        format!("http://{addr}")
    }

    fn api(base_url: &str) -> HttpApi {
        HttpApi::new(base_url, Secret::new("test-token")).unwrap()
    }

    #[test]
    fn api_root_accepts_the_site_or_the_api_address() {
        assert_eq!(api_root("https://gitlab.com"), "https://gitlab.com/api/v4");
        assert_eq!(api_root("https://gitlab.com/"), "https://gitlab.com/api/v4");
        assert_eq!(
            api_root("https://host.example/gitlab/api/v4/"),
            "https://host.example/gitlab/api/v4"
        );
        assert_eq!(
            site_root("https://host.example/gitlab/api/v4"),
            "https://host.example/gitlab"
        );
        assert_eq!(site_root("https://gitlab.com/"), "https://gitlab.com");
    }

    #[tokio::test]
    async fn sends_token_and_encodes_the_project_path() {
        let seen = Arc::new(Seen::default());
        let api = api(&format!("{}/", mock_gitlab(seen.clone()).await));

        let (mrs, total) = api.open_merge_requests("your-group/demo").await.unwrap();
        assert_eq!(mrs.len(), 2);
        assert_eq!(total, Some(137), "exact count from X-Total");
        assert_eq!(api.pipelines("your-group/sub/demo").await.unwrap().len(), 4);
        assert_eq!(api.jobs("your-group/demo", 1004).await.unwrap().len(), 5);
        let failed = api.failed_jobs("your-group/demo", 1003).await.unwrap();
        assert_eq!(failed[1].name, "unit");

        let requests = seen.requests.lock().unwrap();
        assert_eq!(requests[0].0, "mrs your-group/demo");
        assert_eq!(requests[0].1["private-token"], "test-token");
        assert_eq!(requests[1].0, "pipelines");
        assert_eq!(requests[3].0, "failed jobs 1003");
    }

    #[tokio::test]
    async fn refused_token_is_auth_failed() {
        let app = Router::new().route(
            "/api/v4/projects/{id}/merge_requests",
            get(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    "{\"message\":\"401 Unauthorized\"}",
                )
            }),
        );
        let api = api(&serve(app).await);
        let err = api
            .open_merge_requests("your-group/demo")
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
    }

    #[tokio::test]
    async fn unknown_project_and_rate_limit_are_unreachable() {
        let app = Router::new()
            .route(
                "/api/v4/projects/{id}",
                get(|| async {
                    (
                        StatusCode::NOT_FOUND,
                        "{\"message\":\"404 Project Not Found\"}",
                    )
                }),
            )
            .route(
                "/api/v4/projects/{id}/pipelines",
                get(|| async { (StatusCode::TOO_MANY_REQUESTS, "Retry later") }),
            );
        let api = api(&serve(app).await);
        for err in [
            api.project("your-group/gone").await.unwrap_err(),
            api.pipelines("your-group/demo").await.unwrap_err(),
        ] {
            assert_eq!(Health::from_error(&err), Health::Unreachable);
        }
    }

    #[tokio::test]
    async fn unreachable_server() {
        // Nothing listens on port 9 of localhost.
        let api = api("http://127.0.0.1:9");
        let err = api.pipelines("your-group/demo").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::Unreachable);
    }

    #[tokio::test]
    async fn runners_of_a_project_and_a_group_with_busy_lookup() {
        use crate::ci::RunnerStatus;
        let seen = Arc::new(Seen::default());
        let api = api(&mock_gitlab(seen.clone()).await);

        let project = api
            .runners(&RunnerScope::Project("your-group/demo".into()))
            .await
            .unwrap();
        let facts: Vec<_> = project
            .into_iter()
            .map(|r| {
                let f = r.into_fact("gitlab", None);
                (f.name, f.status)
            })
            .collect();
        assert_eq!(
            facts,
            [
                ("build-box-1".to_string(), RunnerStatus::Idle),
                ("build-box-2".to_string(), RunnerStatus::Busy),
                ("gitlab-runner".to_string(), RunnerStatus::Offline),
                ("retired-box".to_string(), RunnerStatus::Offline),
            ]
        );
        let group = api
            .runners(&RunnerScope::Group("your-group".into()))
            .await
            .unwrap();
        assert_eq!(group.len(), 4);

        // Only the two online runners were asked about their jobs.
        let requests = seen.requests.lock().unwrap();
        let lookups = requests
            .iter()
            .filter(|(what, _)| what.starts_with("runner jobs"))
            .count();
        assert_eq!(lookups, 4, "two online runners, for two scopes");
        assert_eq!(requests[0].1["private-token"], "test-token");
    }

    /// The token lacks the runner right: a plain 403.
    #[tokio::test]
    async fn runner_list_without_the_right_is_auth_failed() {
        let app = Router::new().route(
            "/api/v4/groups/{id}/runners",
            get(|| async { (StatusCode::FORBIDDEN, "{\"message\":\"403 Forbidden\"}") }),
        );
        let api = api(&serve(app).await);
        let err = api
            .runners(&RunnerScope::Group("your-group".into()))
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
    }
}
