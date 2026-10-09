//! Azure DevOps REST API client with a read-only personal access token.
//!
//! Every request is `GET`, authenticated with HTTP Basic: an empty user name
//! and the PAT as the password. The scopes needed are in
//! docs/azure-devops.md. `base_url` is `https://dev.azure.com` for Azure
//! DevOps Services; tests point it at a mock server.
//!
//! A missing or expired token does not always give a 401: Azure DevOps can
//! answer `203` or a 200 with its HTML sign-in page. Both count as a refused
//! token (`AuthRefused`).

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;

use super::payload::{Build, List, PullRequest, Record, Timeline};
use crate::source::{AuthRefused, Secret};

/// Azure DevOps Services.
pub const AZURE_DEVOPS_COM: &str = "https://dev.azure.com";

/// REST API version the payload shapes were written against.
const API_VERSION: &str = "7.1";

/// Most recent builds fetched per project, newest queued first. One request
/// covers queued, running and finished builds; it is enough to find the
/// latest build of each pipeline in all but the busiest projects.
const BUILDS_PAGE_SIZE: u32 = 50;

/// Most open PRs fetched per project. The API has no total count, so a
/// project with more open PRs shows this many.
pub const PULLS_PAGE_SIZE: u32 = 100;

/// Give up on a request after this long, so a hung server cannot stall polling.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Azure DevOps API over HTTP.
pub struct HttpApi {
    client: reqwest::Client,
    base_url: Url,
    organization: String,
    token: Secret,
}

impl HttpApi {
    pub fn new(base_url: &str, organization: &str, token: Secret) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("deskwatch-bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot build HTTP client")?;
        Ok(Self {
            client,
            base_url: Url::parse(base_url).context("base_url is not a valid URL")?,
            organization: organization.to_string(),
            token,
        })
    }

    /// Recent builds of a project, newest first.
    pub async fn builds(&self, project: &str) -> Result<Vec<Build>> {
        let top = BUILDS_PAGE_SIZE.to_string();
        let url = self.url(
            &[project, "_apis", "build", "builds"],
            &[("queryOrder", "queueTimeDescending"), ("$top", &top)],
        )?;
        let builds: List<Build> = self.get_json(url).await.context("builds")?;
        Ok(builds.value)
    }

    /// The stages, jobs and tasks of one build.
    pub async fn timeline(&self, project: &str, build_id: u64) -> Result<Vec<Record>> {
        let id = build_id.to_string();
        let url = self.url(&[project, "_apis", "build", "builds", &id, "timeline"], &[])?;
        let timeline: Timeline = self.get_json(url).await.context("build timeline")?;
        Ok(timeline.records)
    }

    /// Open (active) pull requests of every repository in a project.
    pub async fn pulls(&self, project: &str) -> Result<Vec<PullRequest>> {
        let top = PULLS_PAGE_SIZE.to_string();
        let url = self.url(
            &[project, "_apis", "git", "pullrequests"],
            &[("searchCriteria.status", "active"), ("$top", &top)],
        )?;
        let pulls: List<PullRequest> = self.get_json(url).await.context("open pulls")?;
        Ok(pulls.value)
    }

    /// `{base}/{organization}/{path...}?api-version=7.1&{query}`, with every
    /// segment percent-encoded (project names may contain spaces).
    fn url(&self, path: &[&str], query: &[(&str, &str)]) -> Result<Url> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("base_url cannot hold a path"))?
            .pop_if_empty()
            .push(&self.organization)
            .extend(path);
        url.query_pairs_mut()
            .append_pair("api-version", API_VERSION)
            .extend_pairs(query);
        Ok(url)
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<T> {
        let response = self
            .client
            .get(url)
            .basic_auth("", Some(self.token.expose()))
            .header(ACCEPT, "application/json")
            // Ask for a plain 401 instead of a redirect to the sign-in page.
            .header("X-TFS-FedAuthRedirect", "Suppress")
            .send()
            .await?;

        let html = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"));
        if response.status() == StatusCode::NON_AUTHORITATIVE_INFORMATION || html {
            return Err(AuthRefused.into());
        }
        let body = response.error_for_status()?.bytes().await?;
        serde_json::from_slice(&body).context("invalid JSON")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, RawQuery, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;

    use super::*;
    use crate::source::Health;

    /// What the mock server saw: path, query and headers of each request.
    #[derive(Default)]
    pub(crate) struct Seen {
        pub requests: Mutex<Vec<(String, String, HeaderMap)>>,
    }

    /// `Authorization: Basic base64(":test-pat")`.
    pub(crate) const BASIC_AUTH: &str = "Basic OnRlc3QtcGF0";

    fn answer(
        seen: &Seen,
        name: &str,
        query: Option<String>,
        headers: HeaderMap,
        body: &'static str,
    ) -> ([(&'static str, &'static str); 1], &'static str) {
        seen.requests
            .lock()
            .unwrap()
            .push((name.into(), query.unwrap_or_default(), headers));
        ([("content-type", "application/json")], body)
    }

    /// A mock Azure DevOps answering for organisation `your-org` with the
    /// sanitised fixtures. Returns the base URL.
    pub(crate) async fn mock_azure(seen: Arc<Seen>) -> String {
        let project = "/your-org/{project}/_apis";
        let app = Router::new()
            .route(
                &format!("{project}/build/builds"),
                get(
                    |State(seen): State<Arc<Seen>>, RawQuery(q): RawQuery, h: HeaderMap| async move {
                        answer(&seen, "builds", q, h, include_str!("testdata/builds.json"))
                    },
                ),
            )
            .route(
                &format!("{project}/build/builds/{{id}}/timeline"),
                get(
                    |State(seen): State<Arc<Seen>>,
                     Path((_, id)): Path<(String, u64)>,
                     RawQuery(q): RawQuery,
                     h: HeaderMap| async move {
                        // The failed build gets its own timeline.
                        let body = match id {
                            3003 => include_str!("testdata/timeline_failed.json"),
                            _ => include_str!("testdata/timeline_running.json"),
                        };
                        answer(&seen, "timeline", q, h, body)
                    },
                ),
            )
            .route(
                &format!("{project}/git/pullrequests"),
                get(
                    |State(seen): State<Arc<Seen>>, RawQuery(q): RawQuery, h: HeaderMap| async move {
                        answer(&seen, "pulls", q, h, include_str!("testdata/pulls.json"))
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
        format!("http://{addr}")
    }

    pub(crate) fn api(base_url: &str) -> HttpApi {
        HttpApi::new(base_url, "your-org", Secret::new("test-pat")).unwrap()
    }

    #[tokio::test]
    async fn sends_the_pat_as_basic_auth_and_the_api_version() {
        let seen = Arc::new(Seen::default());
        let base = mock_azure(seen.clone()).await;
        let api = api(&format!("{base}/"));

        assert_eq!(api.builds("project-a").await.unwrap().len(), 5);
        assert_eq!(api.pulls("project-a").await.unwrap().len(), 3);
        assert!(!api.timeline("project-a", 3004).await.unwrap().is_empty());

        let requests = seen.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for (_, query, headers) in requests.iter() {
            assert_eq!(headers["authorization"], BASIC_AUTH);
            assert_eq!(headers["x-tfs-fedauthredirect"], "Suppress");
            assert!(query.contains("api-version=7.1"), "{query}");
        }
        assert!(requests[0].1.contains("%24top=50"), "{}", requests[0].1);
        assert_eq!(requests[1].0, "pulls");
        assert!(requests[1].1.contains("searchCriteria.status=active"));
        assert_eq!(requests[2].0, "timeline");
    }

    #[tokio::test]
    async fn project_names_are_percent_encoded() {
        let api = api("http://127.0.0.1:9");
        let url = api.url(&["My Project", "_apis"], &[]).unwrap();
        assert_eq!(url.path(), "/your-org/My%20Project/_apis");
    }

    #[tokio::test]
    async fn refused_token_is_auth_failed() {
        let app = Router::new().route(
            "/your-org/{project}/_apis/build/builds",
            get(|| async { StatusCode::UNAUTHORIZED }),
        );
        let err = api(&serve(app).await)
            .builds("project-a")
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
    }

    #[tokio::test]
    async fn sign_in_page_is_auth_failed() {
        // 203 with the HTML sign-in page, and a 200 with HTML.
        let app = Router::new()
            .route(
                "/your-org/{project}/_apis/build/builds",
                get(|| async {
                    (
                        StatusCode::NON_AUTHORITATIVE_INFORMATION,
                        [("content-type", "text/html; charset=utf-8")],
                        "<html>Sign in</html>",
                    )
                }),
            )
            .route(
                "/your-org/{project}/_apis/git/pullrequests",
                get(|| async { ([("content-type", "text/html")], "<html>Sign in</html>") }),
            );
        let api = api(&serve(app).await);
        for err in [
            api.builds("project-a").await.unwrap_err(),
            api.pulls("project-a").await.unwrap_err(),
        ] {
            assert_eq!(Health::from_error(&err), Health::AuthFailed);
        }
    }

    #[tokio::test]
    async fn server_errors_and_unreachable_server() {
        let app = Router::new().route(
            "/your-org/{project}/_apis/build/builds",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        let err = api(&serve(app).await)
            .builds("project-a")
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::Unreachable);

        // Nothing listens on port 9 of localhost.
        let err = api("http://127.0.0.1:9")
            .builds("project-a")
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::Unreachable);
    }
}
