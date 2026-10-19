use axum::Router;
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use std::collections::HashMap;

use super::*;

const GOOD: &str = "good-token-value";

/// Serve `app` on a free local port and return its address.
pub(crate) async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

pub(crate) struct Temp(pub std::path::PathBuf);
impl Temp {
    pub fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("deskwatch-probe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A credential file with `value`; returns its absolute path as TOML text.
    pub fn token(&self, name: &str, value: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, value).unwrap();
        format!("{:?}", path.to_str().unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn bearer_ok(headers: &HeaderMap, scheme: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("{scheme} {GOOD}"))
}

pub(crate) fn gitea_mock() -> Router {
    Router::new()
        .route(
            "/api/v1/version",
            get(|| async { "{\"version\":\"1.22\"}" }),
        )
        .route(
            "/api/v1/user",
            get(|h: HeaderMap| async move {
                if bearer_ok(&h, "token") {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            }),
        )
        .route("/api/v1/repos/o/r", get(|| async { "{}" }))
}

async fn report_for(config_text: &str, id: &str) -> Report {
    let config = Config::from_toml(config_text).unwrap();
    source(&config, id).await.expect("source exists")
}

fn check<'a>(report: &'a Report, name: &str) -> &'a Check {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check {name:?} in {:?}", report.checks))
}

#[tokio::test]
async fn gitea_passes_with_a_good_token_and_names_a_missing_repo() {
    let temp = Temp::new("gitea");
    let base = serve(gitea_mock()).await;
    let text = format!(
        "[[source.gitea]]\nname = \"home\"\nbase_url = {base:?}\nwebhook_secret_file = {}\n\
         token_file = {}\nrepos = [\"o/r\"]\n",
        temp.token("hook", "h"),
        temp.token("tok", GOOD)
    );
    let report = report_for(&text, "gitea:home").await;
    assert!(report.ok, "{report:?}");
    assert!(check(&report, "token").ok && check(&report, "server").ok);
    assert!(
        check(&report, "webhook secret")
            .message
            .contains("cannot be checked")
    );

    let text = text.replace("\"o/r\"", "\"o/r\", \"o/gone\"");
    let report = report_for(&text, "gitea:home").await;
    assert!(!report.ok);
    let gone = check(&report, "repository o/gone");
    assert!(!gone.ok && gone.message.contains("not found"), "{gone:?}");
    assert!(
        check(&report, "repository o/r").ok,
        "other checks still run"
    );
}

#[tokio::test]
async fn a_refused_token_says_what_to_look_at_and_never_repeats_the_token() {
    let temp = Temp::new("refused");
    let base = serve(gitea_mock()).await;
    let bad = "bad-token-xyz";
    let text = format!(
        "[[source.gitea]]\nname = \"home\"\nbase_url = {base:?}\nwebhook_secret_file = {}\n\
         token_file = {}\nrepos = [\"o/r\"]\n",
        temp.token("hook", "hook-secret-value"),
        temp.token("tok", bad)
    );
    let report = report_for(&text, "gitea:home").await;
    assert!(!report.ok);
    let token = check(&report, "token");
    assert!(
        token.message.contains("401") && token.message.contains("read:repository"),
        "{token:?}"
    );
    let json = serde_json::to_string(&report).unwrap();
    for secret in [bad, "hook-secret-value"] {
        assert!(!json.contains(secret), "{secret} leaked into the report");
    }
}

#[tokio::test]
async fn gitlab_checks_each_project_with_the_token() {
    let temp = Temp::new("gitlab");
    let app = Router::new().route(
        "/api/v4/projects/{id}",
        get(
            |h: HeaderMap, axum::extract::Path(id): axum::extract::Path<String>| async move {
                if h.get("private-token").and_then(|v| v.to_str().ok()) != Some(GOOD) {
                    (StatusCode::UNAUTHORIZED, "")
                } else if id == "g/p" {
                    (StatusCode::OK, "{}")
                } else {
                    (StatusCode::NOT_FOUND, "")
                }
            },
        ),
    );
    let base = serve(app).await;
    let make = |token: &str| {
        format!(
            "[[source.gitlab]]\nname = \"main\"\nbase_url = {base:?}\ntoken_file = {}\n\
             projects = [\"g/p\", \"g/other\"]\n",
            temp.token("gl", token)
        )
    };
    let report = report_for(&make(GOOD), "gitlab:main").await;
    assert!(check(&report, "project g/p").ok);
    assert!(
        check(&report, "project g/other")
            .message
            .contains("not found")
    );
    assert!(!report.ok);

    let report = report_for(&make("expired"), "gitlab:main").await;
    let c = check(&report, "project g/p");
    assert!(
        !c.ok && c.message.contains("401") && c.message.contains("read_api"),
        "{c:?}"
    );
}

#[tokio::test]
async fn github_checks_each_repository_with_the_token() {
    let temp = Temp::new("github");
    let app = Router::new().route(
        "/repos/{owner}/{name}",
        get(|h: HeaderMap, axum::extract::Path((o, n)): axum::extract::Path<(String, String)>| async move {
            if !bearer_ok(&h, "Bearer") {
                (StatusCode::UNAUTHORIZED, "")
            } else if (o.as_str(), n.as_str()) == ("o", "r") {
                (StatusCode::OK, "{}")
            } else {
                (StatusCode::NOT_FOUND, "")
            }
        }),
    );
    let base = serve(app).await;
    let make = |token: &str| {
        format!(
            "[[source.github]]\nname = \"main\"\nbase_url = {base:?}\ntoken_file = {}\n\
             repos = [\"o/r\", \"o/other\"]\n",
            temp.token("gh", token)
        )
    };
    let report = report_for(&make(GOOD), "github:main").await;
    assert!(check(&report, "repository o/r").ok);
    assert!(
        check(&report, "repository o/other")
            .message
            .contains("not found")
    );
    assert!(!report.ok);

    let report = report_for(&make("expired"), "github:main").await;
    let c = check(&report, "repository o/r");
    assert!(
        !c.ok && c.message.contains("401") && c.message.contains("fine-grained"),
        "{c:?}"
    );
}

#[tokio::test]
async fn prometheus_reports_hosts_it_does_not_know() {
    let app = Router::new().route(
        "/api/v1/query",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            let query = q.get("query").cloned().unwrap_or_default();
            let known = query == "up" || query.contains("a:9100");
            let result = if known {
                r#"[{"metric":{"instance":"a:9100"},"value":[0,"1"]}]"#
            } else {
                "[]"
            };
            format!(r#"{{"status":"success","data":{{"resultType":"vector","result":{result}}}}}"#)
        }),
    );
    let base = serve(app).await;
    let text = format!(
        "[[source.prometheus]]\nname = \"p\"\nurl = {base:?}\nhosts = [\"a:9100\", \"b:9100\"]\n"
    );
    let report = report_for(&text, "prometheus:p").await;
    assert!(check(&report, "server").ok);
    assert!(check(&report, "host a").ok);
    let b = check(&report, "host b");
    assert!(!b.ok && b.message.contains("Targets page"), "{b:?}");
    assert_eq!(escape_label("a\"b\\c\nd"), "a\\\"b\\\\c d");
}

#[tokio::test]
async fn a_missing_credential_is_a_failed_check_not_a_crash() {
    let text = "[[source.github]]\nname = \"main\"\ntoken_file = \"/nonexistent/dir/token\"\nrepos = [\"o/r\"]\n";
    let report = report_for(text, "github:main").await;
    assert!(!report.ok);
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].name, "token");
}

#[tokio::test]
async fn unknown_sources_are_none() {
    let config = Config::from_toml("").unwrap();
    assert!(source(&config, "github:nope").await.is_none());
    assert!(source(&config, "nonsense").await.is_none());
    assert!(source(&config, "mqtt").await.is_none());
}

#[tokio::test]
async fn mqtt_test_reports_an_unreachable_or_disabled_broker() {
    let off = Config::from_toml("[mqtt]\nenabled = false\n").unwrap();
    let report = mqtt(&off.mqtt).await;
    assert!(!report.ok && report.checks[0].message.contains("switched off"));

    // A port nobody listens on: bind, note the port, drop the listener.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let config =
        Config::from_toml(&format!("[mqtt]\nhost = \"127.0.0.1\"\nport = {port}\n")).unwrap();
    let report = mqtt(&config.mqtt).await;
    assert!(!report.ok);
    assert!(report.checks[0].name.contains(&port.to_string()));
}

#[test]
fn long_errors_are_shortened() {
    let err = anyhow::anyhow!("x".repeat(1000));
    let text = explain(&err, "");
    assert!(text.chars().count() <= MAX_MESSAGE + 1, "{}", text.len());
    assert!(text.ends_with('…'));
}

#[test]
fn repeated_causes_are_not_printed_twice() {
    let err = anyhow::anyhow!("Connection refused (os error 111)")
        .context("I/O: Connection refused (os error 111)");
    assert_eq!(chain_text(&err), "I/O: Connection refused (os error 111)");
    let err = anyhow::anyhow!("inner").context("outer");
    assert_eq!(chain_text(&err), "outer: inner");
}

/// A project the server does not know: the check says HTTP 404 and the path
/// that was asked for (a name with a space, percent-encoded), and lists the
/// projects the token does see.
#[tokio::test]
async fn azure_devops_unknown_project_lists_the_visible_ones() {
    let app = Router::new()
        .route(
            "/your-org/_apis/projects",
            get(|| async { r#"{"value":[{"name":"My Project"},{"name":"Other"}]}"# }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND });
    let base = serve(app).await;
    let dir = Temp::new("azdo-404");
    let token = dir.token("azdo", GOOD);
    let report = report_for(
        &format!(
            "[[source.azure_devops]]\nname = \"work\"\norganization = \"your-org\"\n\
             base_url = \"{base}\"\nprojects = [\"My Projekt\"]\ntoken_file = {token}\n"
        ),
        "azure_devops:work",
    )
    .await;
    let builds = check(&report, "project My Projekt: builds");
    assert!(!builds.ok);
    assert!(builds.message.contains("HTTP 404"), "{builds:?}");
    assert!(
        builds
            .message
            .contains("/your-org/My%20Projekt/_apis/build/builds"),
        "{builds:?}"
    );
    assert!(
        builds
            .message
            .contains("Projects this token can see: My Project, Other."),
        "{builds:?}"
    );
    assert!(!builds.message.contains(GOOD));
}
