//! Azure DevOps REST API client, read-only.
//!
//! Every request is `GET`. Two ways to sign in (docs/azure-devops-auth.md):
//! a personal access token as HTTP Basic (an empty user name and the PAT as
//! the password), or a Microsoft Entra service principal whose short-lived
//! access token is fetched with the client-credentials flow and sent as a
//! bearer token. The PAT scopes needed are in docs/azure-devops.md. `base_url` is `https://dev.azure.com` for Azure
//! DevOps Services; tests point it at a mock server.
//!
//! A missing or expired token does not always give a 401: Azure DevOps can
//! answer `203` or a 200 with its HTML sign-in page. Both count as a refused
//! token (`AuthRefused`).

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use super::payload::{Agent, Build, JobRequest, List, Pool, PullRequest, Record, Timeline};
use crate::source::{AuthRefused, Secret};

/// Azure DevOps Services.
pub const AZURE_DEVOPS_COM: &str = "https://dev.azure.com";

/// Microsoft Entra, the public cloud sign-in address.
pub const ENTRA_LOGIN: &str = "https://login.microsoftonline.com";

/// The Azure DevOps resource in Entra; `/.default` asks for what the app was
/// given there.
const ENTRA_SCOPE: &str = "499b84ac-1321-427f-aa17-267ca6975798/.default";

/// A cached Entra token is renewed this long before it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(300);

/// A service principal: the app registered in Entra and added to the
/// Azure DevOps organisation as a user.
pub struct EntraApp {
    pub login_url: String,
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: Secret,
}

/// How requests are signed.
enum Auth {
    Pat(Secret),
    Entra {
        app: EntraApp,
        /// The token in use and when to stop using it.
        cached: Mutex<Option<(Secret, Instant)>>,
    },
}

/// The success and error answers of Entra's token endpoint.
#[derive(Deserialize)]
struct TokenAnswer {
    access_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
}

/// The service address without the organisation. Pasting
/// `https://dev.azure.com/<organization>/` into the address field would make
/// every request go to `/<organization>/<organization>/...` and answer 404, so
/// a first path segment equal to the organisation is dropped (any path at all
/// on `dev.azure.com`). Other paths are kept: a proxy may serve it below one.
pub fn clean_base_url(base_url: &str, organization: &str) -> String {
    let Ok(mut url) = Url::parse(base_url.trim()) else {
        return base_url.trim().to_string();
    };
    let first = url.path_segments().and_then(|mut s| s.next()).unwrap_or("");
    let own_org = !first.is_empty() && first.eq_ignore_ascii_case(organization.trim());
    if own_org || (url.host_str() == Some("dev.azure.com") && !first.is_empty()) {
        url.set_path("");
    }
    url.set_query(None);
    url.set_fragment(None);
    url.as_str().trim_end_matches('/').to_string()
}

/// A project name as typed or pasted: `My%20Project` from a browser address
/// becomes `My Project`, so it is not encoded a second time (`%2520`). A name
/// without a valid `%XX` is left alone.
pub fn plain_name(name: &str) -> String {
    let bytes = name.trim().as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| name.trim().to_string())
}

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
    auth: Auth,
}

impl HttpApi {
    /// Sign in with a personal access token.
    pub fn new(base_url: &str, organization: &str, token: Secret) -> Result<Self> {
        Self::build(base_url, organization, Auth::Pat(token))
    }

    /// Sign in as a Microsoft Entra service principal.
    pub fn with_entra(base_url: &str, organization: &str, app: EntraApp) -> Result<Self> {
        anyhow::ensure!(
            Url::parse(&app.login_url).is_ok(),
            "login_url is not a valid URL"
        );
        let cached = Mutex::new(None);
        Self::build(base_url, organization, Auth::Entra { app, cached })
    }

    fn build(base_url: &str, organization: &str, auth: Auth) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("deskwatch-bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot build HTTP client")?;
        Ok(Self {
            client,
            base_url: Url::parse(&clean_base_url(base_url, organization))
                .context("base_url is not a valid URL")?,
            organization: organization.to_string(),
            auth,
        })
    }

    /// The `Authorization` header value. With Entra, a cached token is reused
    /// until shortly before it expires (or `renew` asks for a new one).
    async fn authorization(&self, renew: bool) -> Result<String> {
        let (app, cached) = match &self.auth {
            Auth::Pat(token) => {
                use base64::Engine as _;
                let pair = format!(":{}", token.expose());
                let encoded = base64::engine::general_purpose::STANDARD.encode(pair);
                return Ok(format!("Basic {encoded}"));
            }
            Auth::Entra { app, cached } => (app, cached),
        };
        let mut cached = cached.lock().await;
        if !renew
            && let Some((token, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(format!("Bearer {}", token.expose()));
        }
        let (token, lifetime) = self.fetch_token(app).await?;
        let until = Instant::now() + lifetime.saturating_sub(TOKEN_MARGIN);
        let header = format!("Bearer {}", token.expose());
        *cached = Some((token, until));
        Ok(header)
    }

    /// Ask Entra for an access token (client-credentials flow). A refusal
    /// (wrong secret, unknown app) is `AuthRefused`; Entra being unreachable is
    /// an ordinary request error.
    async fn fetch_token(&self, app: &EntraApp) -> Result<(Secret, Duration)> {
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            app.login_url.trim_end_matches('/'),
            app.tenant_id.trim()
        );
        let response = self
            .client
            .post(url)
            .header(ACCEPT, "application/json")
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", app.client_id.trim()),
                ("client_secret", app.client_secret.expose()),
                ("scope", ENTRA_SCOPE),
            ])
            .send()
            .await
            .context("Microsoft Entra token request")?;
        let status = response.status();
        let body = response.bytes().await?;
        let answer: TokenAnswer = serde_json::from_slice(&body).unwrap_or(TokenAnswer {
            access_token: None,
            expires_in: None,
            error: None,
            error_description: None,
        });
        if let (true, Some(token)) = (status.is_success(), answer.access_token) {
            let lifetime = Duration::from_secs(answer.expires_in.unwrap_or(3600));
            return Ok((Secret::new(token), lifetime));
        }
        if status.is_client_error() {
            // Only the first line of Entra's text: the rest is trace ids.
            let why = answer
                .error_description
                .as_deref()
                .and_then(|d| d.lines().next())
                .or(answer.error.as_deref())
                .unwrap_or("no reason given");
            return Err(anyhow::Error::new(AuthRefused)
                .context(format!("Microsoft Entra refused the app ({status}): {why}")));
        }
        anyhow::bail!("Microsoft Entra answered HTTP {status}")
    }

    /// Recent builds of a project, newest first.
    pub async fn builds(&self, project: &str) -> Result<Vec<Build>> {
        let top = BUILDS_PAGE_SIZE.to_string();
        let project = plain_name(project);
        let url = self.url(
            &[&project, "_apis", "build", "builds"],
            &[("queryOrder", "queueTimeDescending"), ("$top", &top)],
        )?;
        let builds: List<Build> = self.get_json(url).await.context("builds")?;
        Ok(builds.value)
    }

    /// The stages, jobs and tasks of one build.
    pub async fn timeline(&self, project: &str, build_id: u64) -> Result<Vec<Record>> {
        let id = build_id.to_string();
        let project = plain_name(project);
        let url = self.url(
            &[&project, "_apis", "build", "builds", &id, "timeline"],
            &[],
        )?;
        let timeline: Timeline = self.get_json(url).await.context("build timeline")?;
        Ok(timeline.records)
    }

    /// Open (active) pull requests of every repository in a project.
    pub async fn pulls(&self, project: &str) -> Result<Vec<PullRequest>> {
        let top = PULLS_PAGE_SIZE.to_string();
        let project = plain_name(project);
        let url = self.url(
            &[&project, "_apis", "git", "pullrequests"],
            &[("searchCriteria.status", "active"), ("$top", &top)],
        )?;
        let pulls: List<PullRequest> = self.get_json(url).await.context("open pulls")?;
        Ok(pulls.value)
    }

    /// Names of the projects the token can see, for explaining a 404.
    pub async fn projects(&self) -> Result<Vec<String>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Project {
            name: String,
        }
        let url = self.url(&["_apis", "projects"], &[("$top", "100")])?;
        let projects: List<Project> = self.get_json(url).await.context("projects")?;
        Ok(projects.value.into_iter().map(|p| p.name).collect())
    }

    /// Every agent pool of the organisation (organisation level, not per
    /// project). Needs the Agent Pools (Read) scope.
    pub async fn pools(&self) -> Result<Vec<Pool>> {
        let url = self.url(&["_apis", "distributedtask", "pools"], &[])?;
        let pools: List<Pool> = self.get_json(url).await.context("agent pools")?;
        Ok(pools.value)
    }

    /// The agents of one pool, with the job each one is running (if any).
    pub async fn agents(&self, pool_id: u64) -> Result<Vec<Agent>> {
        let id = pool_id.to_string();
        let url = self.url(
            &["_apis", "distributedtask", "pools", &id, "agents"],
            &[("includeAssignedRequest", "true")],
        )?;
        let agents: List<Agent> = self.get_json(url).await.context("pool agents")?;
        Ok(agents.value)
    }

    /// Job requests of one pool that are not finished. Asks for no completed
    /// requests, so the answer stays small on a busy pool.
    pub async fn job_requests(&self, pool_id: u64) -> Result<Vec<JobRequest>> {
        let id = pool_id.to_string();
        let url = self.url(
            &["_apis", "distributedtask", "pools", &id, "jobrequests"],
            &[("completedRequestCount", "0")],
        )?;
        let requests: List<JobRequest> = self.get_json(url).await.context("job requests")?;
        Ok(requests.value)
    }

    /// Any `GET` as raw JSON, for `ado-capture`. The path is relative to the
    /// organisation; the caller passes project names already decoded.
    pub(super) async fn raw(
        &self,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> Result<serde_json::Value> {
        let url = self.url(path, query)?;
        self.get_json(url).await
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

    /// Web page of one agent pool in the organisation settings.
    pub fn pool_url(&self, pool_id: u64) -> Option<String> {
        crate::source::web_url(
            self.base_url.as_str(),
            &[&self.organization, "_settings", "agentpools"],
            &[("poolId", &pool_id.to_string())],
        )
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<T> {
        // An Entra token that Azure DevOps refuses (revoked, or the identity
        // was just added) is renewed once before the call counts as refused.
        let mut renew = false;
        loop {
            let response = self
                .client
                .get(url.clone())
                .header(AUTHORIZATION, self.authorization(renew).await?)
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
            let unauthorised = response.status() == StatusCode::UNAUTHORIZED;
            if matches!(self.auth, Auth::Entra { .. }) && !renew && (unauthorised || html) {
                renew = true;
                continue;
            }
            if response.status() == StatusCode::NON_AUTHORITATIVE_INFORMATION || html {
                return Err(AuthRefused.into());
            }
            let body = response.error_for_status()?.bytes().await?;
            return serde_json::from_slice(&body).context("invalid JSON");
        }
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
            .route(
                "/your-org/_apis/distributedtask/pools",
                get(
                    |State(seen): State<Arc<Seen>>, RawQuery(q): RawQuery, h: HeaderMap| async move {
                        answer(&seen, "pools", q, h, include_str!("testdata/pools.json"))
                    },
                ),
            )
            .route(
                "/your-org/_apis/distributedtask/pools/{id}/agents",
                get(
                    |State(seen): State<Arc<Seen>>,
                     Path(id): Path<u64>,
                     RawQuery(q): RawQuery,
                     h: HeaderMap| async move {
                        let body = match id {
                            12 => include_str!("testdata/agents_12.json"),
                            _ => include_str!("testdata/agents_13.json"),
                        };
                        answer(&seen, "agents", q, h, body)
                    },
                ),
            )
            .route(
                "/your-org/_apis/distributedtask/pools/{id}/jobrequests",
                get(
                    |State(seen): State<Arc<Seen>>,
                     Path(id): Path<u64>,
                     RawQuery(q): RawQuery,
                     h: HeaderMap| async move {
                        let body = match id {
                            12 => include_str!("testdata/jobrequests_12.json"),
                            _ => include_str!("testdata/jobrequests_13.json"),
                        };
                        answer(&seen, "jobrequests", q, h, body)
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

    /// A project name with a space goes out as `%20`, never as a raw space,
    /// a `+` or a double-encoded `%2520`.
    #[tokio::test]
    async fn project_names_with_spaces_are_percent_encoded() {
        let paths = Arc::new(Mutex::new(Vec::new()));
        let seen = paths.clone();
        let app = Router::new().fallback(move |uri: axum::http::Uri| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(uri.path().to_string());
                ([("content-type", "application/json")], r#"{"value":[]}"#)
            }
        });
        let api = api(&serve(app).await);
        api.builds("My Project").await.unwrap();
        api.pulls("My Project").await.unwrap();
        api.timeline("My Project", 7).await.unwrap();
        let paths = paths.lock().unwrap();
        assert_eq!(
            *paths,
            [
                "/your-org/My%20Project/_apis/build/builds",
                "/your-org/My%20Project/_apis/git/pullrequests",
                "/your-org/My%20Project/_apis/build/builds/7/timeline",
            ]
        );
    }

    /// A name pasted from a browser address (`My%20Project`) must not be
    /// encoded twice, and an address that already holds the organisation
    /// must not repeat it. Both gave a 404 from a real organisation.
    #[tokio::test]
    async fn pasted_names_and_addresses_do_not_double_up() {
        assert_eq!(plain_name("My%20Project"), "My Project");
        assert_eq!(plain_name("My Project"), "My Project");
        assert_eq!(plain_name("100%"), "100%");
        assert_eq!(plain_name("a%zzb%2"), "a%zzb%2");
        assert_eq!(plain_name("caf%C3%A9"), "café");

        assert_eq!(
            clean_base_url("https://dev.azure.com/your-org/", "your-org"),
            "https://dev.azure.com"
        );
        assert_eq!(
            clean_base_url("https://dev.azure.com/Your-Org/My%20Project", "your-org"),
            "https://dev.azure.com"
        );
        assert_eq!(
            clean_base_url("https://dev.azure.com/", "your-org"),
            "https://dev.azure.com"
        );
        // A proxy path that is not the organisation stays, on another host.
        assert_eq!(
            clean_base_url("https://proxy.example/azdo/", "your-org"),
            "https://proxy.example/azdo"
        );

        let paths = Arc::new(Mutex::new(Vec::new()));
        let seen = paths.clone();
        let app = Router::new().fallback(move |uri: axum::http::Uri| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(uri.path().to_string());
                ([("content-type", "application/json")], r#"{"value":[]}"#)
            }
        });
        let base = serve(app).await;
        let api = HttpApi::new(&format!("{base}/your-org/"), "your-org", Secret::new("t")).unwrap();
        api.builds("My%20Project").await.unwrap();
        // A bare trailing slash gives a single slash in the path, not `//`.
        let api = HttpApi::new(&format!("{base}/"), "your-org", Secret::new("t")).unwrap();
        api.builds("My Project").await.unwrap();
        assert_eq!(
            *paths.lock().unwrap(),
            [
                "/your-org/My%20Project/_apis/build/builds",
                "/your-org/My%20Project/_apis/build/builds"
            ]
        );
    }

    #[tokio::test]
    async fn pools_and_agents() {
        let seen = Arc::new(Seen::default());
        let api = api(&mock_azure(seen.clone()).await);
        let pools = api.pools().await.unwrap();
        assert_eq!(pools.len(), 3);
        assert!(pools[0].is_hosted && !pools[1].is_hosted);
        let agents = api.agents(12).await.unwrap();
        assert_eq!(agents.len(), 3);
        assert!(agents[1].assigned_request.is_some());
        assert_eq!(agents[2].enabled, Some(false));

        let requests = seen.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .all(|(_, _, h)| h["authorization"] == BASIC_AUTH)
        );
        let agents_query = &requests.iter().find(|(n, ..)| n == "agents").unwrap().1;
        assert!(agents_query.contains("includeAssignedRequest=true"));
        assert!(agents_query.contains("api-version=7.1"));
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

    // -- Microsoft Entra service principal ------------------------------

    /// A mock Entra token endpoint plus an Azure DevOps that accepts only
    /// `good_token`. `tokens` counts token requests and keeps their bodies.
    struct EntraMock {
        base: String,
        tokens: Arc<Mutex<Vec<String>>>,
    }

    /// The nth token handed out is `token-<n>`; `accept` names the one
    /// Azure DevOps lets in. `entra_status` is what the token endpoint answers.
    async fn entra_mock(accept: &'static str, entra_status: StatusCode) -> EntraMock {
        let tokens: Arc<Mutex<Vec<String>>> = Arc::default();
        let app = Router::new()
            .route(
                "/tenant-1/oauth2/v2.0/token",
                axum::routing::post(
                    move |State(tokens): State<Arc<Mutex<Vec<String>>>>, body: String| async move {
                        let n = {
                            let mut seen = tokens.lock().unwrap();
                            seen.push(body);
                            seen.len()
                        };
                        let json = if entra_status.is_success() {
                            format!(r#"{{"access_token":"token-{n}","expires_in":3599}}"#)
                        } else {
                            r#"{"error":"invalid_client","error_description":"AADSTS7000215: Invalid client secret.\r\nTrace ID: x"}"#.into()
                        };
                        (entra_status, [("content-type", "application/json")], json)
                    },
                ),
            )
            .route(
                "/your-org/{project}/_apis/build/builds",
                get(move |h: HeaderMap| async move {
                    if h["authorization"] == format!("Bearer {accept}").as_str() {
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            include_str!("testdata/builds.json"),
                        )
                    } else {
                        (StatusCode::UNAUTHORIZED, [("content-type", "text/plain")], "")
                    }
                }),
            )
            .with_state(tokens.clone());
        EntraMock {
            base: serve(app).await,
            tokens,
        }
    }

    fn entra_api(mock: &EntraMock) -> HttpApi {
        HttpApi::with_entra(
            &mock.base,
            "your-org",
            EntraApp {
                login_url: mock.base.clone(),
                tenant_id: "tenant-1".into(),
                client_id: "client-1".into(),
                client_secret: Secret::new("s3cret&=x"),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn entra_token_is_fetched_once_and_sent_as_bearer() {
        let mock = entra_mock("token-1", StatusCode::OK).await;
        let api = entra_api(&mock);
        assert_eq!(api.builds("project-a").await.unwrap().len(), 5);
        assert_eq!(api.builds("project-a").await.unwrap().len(), 5);
        let tokens = mock.tokens.lock().unwrap();
        assert_eq!(tokens.len(), 1, "the token is cached between polls");
        let body = &tokens[0];
        assert!(body.contains("grant_type=client_credentials"), "{body}");
        assert!(body.contains("client_id=client-1"), "{body}");
        assert!(
            body.contains("scope=499b84ac-1321-427f-aa17-267ca6975798%2F.default"),
            "{body}"
        );
        // The secret is form-encoded, not pasted in raw.
        assert!(body.contains("client_secret=s3cret%26%3Dx"), "{body}");
    }

    #[tokio::test]
    async fn entra_token_is_renewed_once_when_azure_devops_refuses_it() {
        // The first token is refused, the second one is let in.
        let mock = entra_mock("token-2", StatusCode::OK).await;
        let api = entra_api(&mock);
        assert_eq!(api.builds("project-a").await.unwrap().len(), 5);
        assert_eq!(mock.tokens.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn entra_token_refused_for_good_is_auth_failed() {
        let mock = entra_mock("never", StatusCode::OK).await;
        let err = entra_api(&mock).builds("project-a").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
        assert_eq!(mock.tokens.lock().unwrap().len(), 2, "one renewal, no loop");
    }

    #[tokio::test]
    async fn entra_refusing_the_app_is_auth_failed_and_names_the_reason_only() {
        let mock = entra_mock("token-1", StatusCode::BAD_REQUEST).await;
        let err = entra_api(&mock).builds("project-a").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
        let text = format!("{err:#}");
        assert!(text.contains("Invalid client secret"), "{text}");
        assert!(!text.contains("Trace ID"), "{text}");
        assert!(!text.contains("s3cret"), "{text}");
    }

    #[tokio::test]
    async fn entra_unreachable_is_unreachable() {
        let api = HttpApi::with_entra(
            "http://127.0.0.1:9",
            "your-org",
            EntraApp {
                login_url: "http://127.0.0.1:9".into(),
                tenant_id: "t".into(),
                client_id: "c".into(),
                client_secret: Secret::new("s"),
            },
        )
        .unwrap();
        let err = api.builds("p").await.unwrap_err();
        assert_eq!(Health::from_error(&err), Health::Unreachable);
    }
}
