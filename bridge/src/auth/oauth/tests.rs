use super::*;
use axum::Router;
use axum::extract::{Path as AxPath, State};
use axum::http::{Request as HttpRequest, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, body::Body};
use tower::ServiceExt;

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const APP: &str = "22222222-2222-2222-2222-222222222222";
const G_USERS: &str = "33333333-3333-3333-3333-333333333333";
const G_ADMINS: &str = "44444444-4444-4444-4444-444444444444";

/// What the fake providers answer.
#[derive(Clone, Default)]
struct World {
    gh_id: u64,
    gh_login: String,
    gh_orgs: Vec<String>,
    gh_teams: Vec<(String, String)>,
    ms_tid: String,
    ms_groups: Vec<String>,
    nonce: Arc<Mutex<String>>,
}

type Shared = Arc<Mutex<World>>;

async fn gh_token() -> Json<Value> {
    Json(json!({ "access_token": "gho_test" }))
}

async fn gh_user(State(w): State<Shared>) -> Json<Value> {
    let w = w.lock().unwrap();
    Json(json!({ "id": w.gh_id, "login": w.gh_login }))
}

async fn gh_orgs(State(w): State<Shared>) -> Json<Value> {
    let w = w.lock().unwrap();
    Json(json!(
        w.gh_orgs
            .iter()
            .map(|o| json!({ "login": o }))
            .collect::<Vec<_>>()
    ))
}

async fn gh_teams(State(w): State<Shared>) -> Json<Value> {
    let w = w.lock().unwrap();
    Json(json!(
        w.gh_teams
            .iter()
            .map(|(o, t)| json!({ "slug": t, "organization": { "login": o } }))
            .collect::<Vec<_>>()
    ))
}

async fn gh_lookup(AxPath(login): AxPath<String>) -> axum::response::Response {
    use axum::response::IntoResponse;
    if login == "ghost" {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({ "login": login, "id": 1000 + login.len() as u64 })).into_response()
}

async fn ms_token(State(w): State<Shared>) -> Json<Value> {
    let w = w.lock().unwrap();
    let nonce = w.nonce.lock().unwrap().clone();
    let claims = json!({
        "aud": APP,
        "tid": w.ms_tid,
        "iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"),
        "exp": unix_now() + 3600,
        "nonce": nonce,
        "oid": "oid-1",
        "name": "Mia",
    });
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    Json(json!({ "id_token": format!("e30.{payload}.sig"), "access_token": "ms_access" }))
}

async fn ms_groups(State(w): State<Shared>, Json(body): Json<Value>) -> Json<Value> {
    let w = w.lock().unwrap();
    let asked: Vec<String> = body["groupIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap().to_string())
        .collect();
    let mine: Vec<String> = asked
        .into_iter()
        .filter(|g| w.ms_groups.contains(g))
        .collect();
    Json(json!({ "value": mine }))
}

async fn mock(world: World) -> (Endpoints, Shared) {
    let shared: Shared = Arc::new(Mutex::new(world));
    let app = Router::new()
        .route("/gh/token", post(gh_token))
        .route("/gh/api/user", get(gh_user))
        .route("/gh/api/user/orgs", get(gh_orgs))
        .route("/gh/api/user/teams", get(gh_teams))
        .route("/gh/api/users/{login}", get(gh_lookup))
        .route("/ms/{tenant}/oauth2/v2.0/token", post(ms_token))
        .route("/ms/graph/v1.0/me/checkMemberGroups", post(ms_groups))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");
    (
        Endpoints {
            github_authorize: format!("{base}/gh/authorize"),
            github_token: format!("{base}/gh/token"),
            github_api: format!("{base}/gh/api"),
            ms_login: format!("{base}/ms"),
            ms_graph: format!("{base}/ms/graph"),
        },
        shared,
    )
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("dw-oauth-{}", random_url_safe(6)));
        std::fs::create_dir_all(p.join("credentials")).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn gh_config() -> Github {
    Github {
        client_id: "Iv1.abc".into(),
        users: vec![GhUser {
            login: "alice".into(),
            id: 42,
        }],
        orgs: vec!["Acme".into()],
        teams: vec!["acme/platform".into()],
        admin_users: vec![],
        admin_teams: vec!["acme/admins".into()],
    }
}

fn ms_config() -> Microsoft {
    Microsoft {
        tenant: TENANT.into(),
        client_id: APP.into(),
        groups: vec![G_USERS.into()],
        admin_groups: vec![G_ADMINS.into()],
    }
}

async fn setup(world: World) -> (TempDir, OAuth, Shared) {
    let d = TempDir::new();
    let o = OAuth::open(Some(&d.0)).unwrap();
    let (ep, shared) = mock(world).await;
    o.set_endpoints(ep);
    let config = Config {
        public_url: Some("http://localhost:8787".into()),
        github: Some(gh_config()),
        microsoft: Some(ms_config()),
    };
    o.save(config, Some("gh-secret"), Some("ms-secret"))
        .unwrap();
    (d, o, shared)
}

fn query(url: &str, key: &str) -> String {
    Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or_default()
}

#[test]
fn the_rules_decide_the_role() {
    let g = gh_config();
    let who = |id, orgs: &[&str], teams: &[&str]| GithubIdentity {
        id,
        orgs: orgs.iter().map(|s| s.to_string()).collect(),
        teams: teams.iter().map(|s| s.to_string()).collect(),
    };
    assert_eq!(
        decide_github(&g, &who(42, &[], &[])),
        Some(Role::Viewer),
        "listed user"
    );
    assert_eq!(
        decide_github(&g, &who(7, &["acme"], &[])),
        Some(Role::Viewer),
        "org member"
    );
    assert_eq!(
        decide_github(&g, &who(7, &[], &["acme/platform"])),
        Some(Role::Viewer)
    );
    assert_eq!(
        decide_github(&g, &who(7, &["acme"], &["acme/admins"])),
        Some(Role::Admin)
    );
    assert_eq!(
        decide_github(&g, &who(7, &["other"], &["other/platform"])),
        None
    );
    // A rule list that is empty lets nobody in.
    assert_eq!(
        decide_github(&Github::default(), &who(42, &["acme"], &[])),
        None
    );
    let m = ms_config();
    assert_eq!(
        decide_microsoft(&m, &[G_USERS.to_uppercase()]),
        Some(Role::Viewer)
    );
    assert_eq!(
        decide_microsoft(&m, &[G_ADMINS.into(), G_USERS.into()]),
        Some(Role::Admin)
    );
    assert_eq!(decide_microsoft(&m, &[]), None);
}

#[test]
fn settings_are_checked_before_they_are_saved() {
    let ok = Config {
        public_url: Some("https://deskwatch.example.org".into()),
        github: Some(gh_config()),
        microsoft: Some(ms_config()),
    };
    assert!(ok.validate().is_ok());
    let with_url = |u: &str| Config {
        public_url: Some(u.into()),
        ..ok.clone()
    };
    assert!(
        with_url("http://deskwatch.example.org").validate().is_err(),
        "http is not allowed"
    );
    assert!(with_url("http://localhost:8787").validate().is_ok());
    assert!(with_url("https://x.example.org/path").validate().is_err());
    assert!(
        Config {
            public_url: None,
            ..ok.clone()
        }
        .validate()
        .is_err()
    );
    let mut bad = ok.clone();
    bad.github.as_mut().unwrap().teams = vec!["no-slash".into()];
    assert!(bad.validate().is_err());
    let mut bad = ok.clone();
    bad.microsoft.as_mut().unwrap().tenant = "contoso.com".into();
    assert!(bad.validate().is_err());
    let mut bad = ok;
    bad.microsoft.as_mut().unwrap().groups = vec!["not-a-guid".into()];
    assert!(bad.validate().is_err());
}

#[test]
fn id_token_claims_are_checked() {
    let cfg = ms_config();
    let good = json!({
        "aud": APP, "tid": TENANT, "oid": "o", "nonce": "n", "exp": 2_000_000_000u64,
        "iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"),
    });
    assert!(check_claims(&good, &cfg, "n", 1_000_000_000).is_ok());
    let mut c = good.clone();
    c["aud"] = json!("someone-else");
    assert!(check_claims(&c, &cfg, "n", 1_000_000_000).is_err());
    let mut c = good.clone();
    c["tid"] = json!("99999999-9999-9999-9999-999999999999");
    assert!(check_claims(&c, &cfg, "n", 1_000_000_000).is_err());
    assert!(check_claims(&good, &cfg, "other-nonce", 1_000_000_000).is_err());
    assert!(
        check_claims(&good, &cfg, "n", 3_000_000_000).is_err(),
        "expired"
    );
    assert!(id_token_claims("not-a-token").is_err());
}

#[tokio::test]
async fn github_sign_in_gives_the_right_role() {
    let world = World {
        gh_id: 7,
        gh_login: "bob".into(),
        gh_orgs: vec!["acme".into()],
        gh_teams: vec![("acme".into(), "admins".into())],
        ..Default::default()
    };
    let (_d, o, world) = setup(world).await;
    let (url, state) = o.begin(Provider::Github, "/setup").unwrap();
    assert!(url.contains("/gh/authorize") && url.contains("code_challenge_method=S256"));
    assert_eq!(query(&url, "state"), state);
    assert_eq!(
        query(&url, "redirect_uri"),
        "http://localhost:8787/auth/callback/github"
    );
    assert!(query(&url, "scope").contains("read:org"));
    let (role, label, next) = o
        .finish(Provider::Github, "code", &state, Some(&state))
        .await
        .unwrap();
    assert_eq!(
        (role, label.as_str(), next.as_str()),
        (Role::Admin, "bob (GitHub)", "/setup")
    );
    // The same link cannot be used twice.
    assert!(matches!(
        o.finish(Provider::Github, "code", &state, Some(&state))
            .await,
        Err(Failure::Expired)
    ));
    // An ordinary org member is a viewer; a stranger is refused.
    world.lock().unwrap().gh_teams.clear();
    let (_, state) = o.begin(Provider::Github, "/").unwrap();
    let (role, _, _) = o
        .finish(Provider::Github, "c", &state, Some(&state))
        .await
        .unwrap();
    assert_eq!(role, Role::Viewer);
    {
        let mut w = world.lock().unwrap();
        w.gh_orgs.clear();
    }
    let (_, state) = o.begin(Provider::Github, "/").unwrap();
    assert!(matches!(
        o.finish(Provider::Github, "c", &state, Some(&state)).await,
        Err(Failure::NotAllowed)
    ));
}

#[tokio::test]
async fn a_sign_in_needs_the_cookie_of_the_same_browser() {
    let (_d, o, _) = setup(World::default()).await;
    let (_, state) = o.begin(Provider::Github, "/").unwrap();
    assert!(matches!(
        o.finish(Provider::Github, "c", &state, Some("another-state"))
            .await,
        Err(Failure::Expired)
    ));
    let (_, state) = o.begin(Provider::Github, "/").unwrap();
    assert!(matches!(
        o.finish(Provider::Github, "c", &state, None).await,
        Err(Failure::Expired)
    ));
    // The state of a GitHub start cannot finish a Microsoft sign-in.
    let (_, state) = o.begin(Provider::Github, "/").unwrap();
    assert!(matches!(
        o.finish(Provider::Microsoft, "c", &state, Some(&state))
            .await,
        Err(Failure::Expired)
    ));
    assert!(matches!(
        o.finish(Provider::Github, "c", "unknown", Some("unknown"))
            .await,
        Err(Failure::Expired)
    ));
}

#[tokio::test]
async fn microsoft_sign_in_uses_groups() {
    let world = World {
        ms_tid: TENANT.into(),
        ms_groups: vec![G_USERS.into()],
        ..Default::default()
    };
    let (_d, o, world) = setup(world).await;
    let sign_in = |o: OAuth, world: Shared| async move {
        let (url, state) = o.begin(Provider::Microsoft, "/").unwrap();
        *world.lock().unwrap().nonce.lock().unwrap() = query(&url, "nonce");
        assert!(url.contains(&format!("/ms/{TENANT}/oauth2/v2.0/authorize")));
        o.finish(Provider::Microsoft, "code", &state, Some(&state))
            .await
    };
    let (role, label, _) = sign_in(o.clone(), world.clone()).await.unwrap();
    assert_eq!((role, label.as_str()), (Role::Viewer, "Mia (Microsoft)"));
    world.lock().unwrap().ms_groups = vec![G_ADMINS.into()];
    assert_eq!(
        sign_in(o.clone(), world.clone()).await.unwrap().0,
        Role::Admin
    );
    world.lock().unwrap().ms_groups.clear();
    assert!(matches!(
        sign_in(o.clone(), world.clone()).await,
        Err(Failure::NotAllowed)
    ));
    // A token from another directory is refused.
    {
        let mut w = world.lock().unwrap();
        w.ms_groups = vec![G_USERS.into()];
        w.ms_tid = "99999999-9999-9999-9999-999999999999".into();
    }
    assert!(matches!(sign_in(o, world).await, Err(Failure::Failed(_))));
}

#[tokio::test]
async fn github_logins_are_resolved_to_ids() {
    let (_d, o, _) = setup(World::default()).await;
    let u = o.resolve_github_user("@Carol", &[]).await.unwrap();
    assert_eq!((u.login.as_str(), u.id), ("Carol", 1005));
    // A known login is not asked again.
    let known = [GhUser {
        login: "carol".into(),
        id: 5,
    }];
    assert_eq!(o.resolve_github_user("CAROL", &known).await.unwrap().id, 5);
    assert!(o.resolve_github_user("ghost", &[]).await.is_err());
    assert!(o.resolve_github_user("bad name!", &[]).await.is_err());
}

#[test]
fn secrets_are_kept_write_only_and_removed_with_the_provider() {
    let d = TempDir::new();
    let o = OAuth::open(Some(&d.0)).unwrap();
    let only_github = Config {
        public_url: Some("https://deskwatch.example.org".into()),
        github: Some(gh_config()),
        microsoft: None,
    };
    assert!(
        o.save(only_github.clone(), None, None).is_err(),
        "a secret is needed the first time"
    );
    o.save(only_github.clone(), Some("s3cret"), None).unwrap();
    assert!(o.secret_set(Provider::Github) && !o.secret_set(Provider::Microsoft));
    // Saving again without a secret keeps the old one.
    o.save(only_github, None, None).unwrap();
    assert!(o.secret_set(Provider::Github));
    let toml = std::fs::read_to_string(d.0.join("oauth.toml")).unwrap();
    assert!(!toml.contains("s3cret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.0.join("credentials/oauth-github"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Switching a provider off deletes its secret; the settings survive a reopen.
    let off = Config {
        public_url: Some("https://deskwatch.example.org".into()),
        ..Default::default()
    };
    o.save(off.clone(), None, None).unwrap();
    assert!(!o.secret_set(Provider::Github));
    assert_eq!(OAuth::open(Some(&d.0)).unwrap().config(), off);
}

// ---- through the routes ---------------------------------------------------------

async fn get_path(app: &Router, uri: &str, cookie: Option<&str>) -> axum::response::Response {
    let mut req = HttpRequest::builder().uri(uri);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn the_routes_run_a_whole_sign_in() {
    let d = TempDir::new();
    let auth = crate::auth::Auth::open(Some(&d.0), false).unwrap();
    auth.setup_admin("admin", "correct horse battery").unwrap();
    let (ep, world) = mock(World {
        gh_id: 42,
        gh_login: "alice".into(),
        ..Default::default()
    })
    .await;
    auth.oauth().set_endpoints(ep);
    let config = Config {
        public_url: Some("http://localhost:8787".into()),
        github: Some(gh_config()),
        microsoft: None,
    };
    auth.oauth().save(config, Some("s"), None).unwrap();
    let app = auth.router();

    let status = get_path(&app, "/api/auth/status", None).await;
    let body = axum::body::to_bytes(status.into_body(), 1 << 16)
        .await
        .unwrap();
    let status: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["providers"], json!(["github"]));

    // Start: a redirect to the provider and the cookie that binds it to this browser.
    let r = get_path(&app, "/auth/login/github?next=//evil.example", None).await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let to = r.headers()[header::LOCATION].to_str().unwrap().to_string();
    let state = query(&to, "state");
    let cookie = r.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with(&format!("dw_oauth={state}")) && cookie.contains("HttpOnly"));
    // Another site's address is never followed after the sign-in.
    let pair = format!("dw_oauth={state}");
    let back = get_path(
        &app,
        &format!("/auth/callback/github?code=abc&state={state}"),
        Some(&pair),
    )
    .await;
    assert_eq!(back.status(), StatusCode::SEE_OTHER);
    assert_eq!(back.headers()[header::LOCATION], "/");
    let cookies: Vec<String> = back
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    let session = cookies
        .iter()
        .find(|c| c.starts_with("dw_session="))
        .expect("a session");
    let token = session
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("dw_session=");
    assert_eq!(auth.session_role(token), Some(Role::Viewer));
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("dw_oauth=;") && c.contains("Max-Age=0"))
    );

    // The page header can show who is signed in.
    let who = get_path(
        &app,
        "/api/auth/status",
        Some(&format!("dw_session={token}")),
    )
    .await;
    let body = axum::body::to_bytes(who.into_body(), 1 << 16)
        .await
        .unwrap();
    let who: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(who["user"], "alice (GitHub)");

    // Without the cookie the callback is refused, and so is a person no rule lets in.
    let (_, state) = (
        auth.oauth().begin(Provider::Github, "/").unwrap().0,
        auth.oauth().begin(Provider::Github, "/").unwrap().1,
    );
    let r = get_path(
        &app,
        &format!("/auth/callback/github?code=abc&state={state}"),
        None,
    )
    .await;
    assert_eq!(r.headers()[header::LOCATION], "/login?error=expired");
    world.lock().unwrap().gh_id = 7;
    let (_, state) = auth.oauth().begin(Provider::Github, "/").unwrap();
    let pair = format!("dw_oauth={state}");
    let r = get_path(
        &app,
        &format!("/auth/callback/github?code=abc&state={state}"),
        Some(&pair),
    )
    .await;
    assert_eq!(r.headers()[header::LOCATION], "/login?error=not_allowed");
    let r = get_path(&app, "/auth/callback/github?error=access_denied", None).await;
    assert_eq!(r.headers()[header::LOCATION], "/login?error=denied");
    // A provider that is not set up does not start.
    let r = get_path(&app, "/auth/login/microsoft", None).await;
    assert_eq!(r.headers()[header::LOCATION], "/login?error=failed");
}
