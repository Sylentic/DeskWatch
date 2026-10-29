use super::*;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("dw-auth-{}", random_hex(6)));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const PW: &str = "correct horse battery";

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

/// A user whose first-sign-in password change is already done.
fn add(auth: &Auth, name: &str, role: Role, password: &str) {
    auth.add_user(name, role, password).unwrap();
    let mut state = auth.read();
    state.user.iter_mut().for_each(|u| u.must_change = false);
    auth.save(state).unwrap();
}

fn fresh() -> (TempDir, Auth) {
    let d = TempDir::new();
    let auth = Auth::open(Some(&d.0), false).unwrap();
    (d, auth)
}

#[test]
fn off_until_a_password_is_set_and_it_survives_a_reopen() {
    let (d, auth) = fresh();
    assert!(!auth.enabled() && auth.can_save());
    auth.setup_admin("admin", PW).unwrap();
    assert!(auth.enabled());
    assert!(
        auth.setup_admin("other", PW).is_err(),
        "only the first run may set it"
    );
    let again = Auth::open(Some(&d.0), false).unwrap();
    assert!(again.enabled());
    let text = std::fs::read_to_string(d.0.join("auth.toml")).unwrap();
    assert!(text.contains("$argon2id$") && !text.contains(PW));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.0.join("auth.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn without_a_data_folder_login_cannot_be_saved() {
    let auth = Auth::open(None, false).unwrap();
    assert!(!auth.can_save());
    assert!(auth.setup_admin("admin", PW).is_err());
    assert!(!auth.enabled());
}

#[test]
fn password_and_username_rules() {
    let (_d, auth) = fresh();
    assert!(auth.setup_admin("admin", "short").is_err());
    assert!(auth.setup_admin("viewer", PW).is_err());
    assert!(auth.setup_admin("bad name", PW).is_err());
    assert!(!auth.enabled());
}

#[test]
fn login_roles_and_lockout() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    add(&auth, "guest", Role::Viewer, "a different phrase");
    let c = ip("192.168.1.9");
    let (role, token) = auth.login(c, "admin", PW).ok().unwrap();
    assert_eq!(role, Role::Admin);
    assert_eq!(auth.session_role(&token), Some(Role::Admin));
    let (role, _) = auth.login(c, "guest", "a different phrase").ok().unwrap();
    assert_eq!(role, Role::Viewer);
    // Wrong tries count down, then the address is locked, even for the right password.
    for left in (1..MAX_TRIES).rev() {
        match auth.login(c, "admin", "nope nope nope") {
            Err(LoginError::Wrong { left: l }) => assert_eq!(l, left),
            _ => panic!("expected a wrong password"),
        }
    }
    assert!(matches!(
        auth.login(c, "admin", "nope nope nope"),
        Err(LoginError::Locked(_))
    ));
    assert!(matches!(
        auth.login(c, "admin", PW),
        Err(LoginError::Locked(_))
    ));
    // Another address is not affected.
    assert!(auth.login(ip("192.168.1.10"), "admin", PW).is_ok());
    // An unknown username fails like a wrong password.
    assert!(matches!(
        auth.login(ip("192.168.1.11"), "ghost", PW),
        Err(LoginError::Wrong { .. })
    ));
}

#[test]
fn lockout_counts_an_ipv6_slash_64_as_one_client() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    for n in 0..MAX_TRIES {
        let _ = auth.login(ip(&format!("2001:db8:1:2::{n:x}")), "admin", "nope nope nope");
    }
    // Same /64, new address: still locked. Another /64 is not.
    assert!(matches!(
        auth.login(ip("2001:db8:1:2:ffff::1"), "admin", PW),
        Err(LoginError::Locked(_))
    ));
    assert!(auth.login(ip("2001:db8:1:3::1"), "admin", PW).is_ok());
}

#[test]
fn sessions_end_and_reset_forgets_passwords_but_keeps_keys() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let key = auth.create_key("ha", Scope::Alert).unwrap();
    let (_, token) = auth.login(ip("10.0.0.2"), "admin", PW).ok().unwrap();
    auth.end_session(&token);
    assert_eq!(auth.session_role(&token), None);
    let (_, token) = auth.login(ip("10.0.0.2"), "admin", PW).ok().unwrap();
    assert_eq!(auth.end_all_sessions(), 1);
    assert_eq!(auth.session_role(&token), None);
    auth.reset().unwrap();
    assert!(!auth.enabled());
    assert!(auth.key_allows(&key, Scope::Alert));
}

#[test]
fn keys_are_hashed_scoped_and_listed_without_the_secret() {
    let (d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let key = auth.create_key("home assistant", Scope::Alert).unwrap();
    let screen = auth.create_key("pi screen", Scope::Viewer).unwrap();
    assert!(key.starts_with("dwk_"));
    assert!(auth.key_allows(&key, Scope::Alert) && !auth.key_allows(&key, Scope::Viewer));
    assert!(auth.key_allows(&screen, Scope::Viewer));
    assert!(
        auth.create_key("pi screen", Scope::Viewer).is_err(),
        "names are unique"
    );
    let text = std::fs::read_to_string(d.0.join("auth.toml")).unwrap();
    assert!(!text.contains(&key) && !text.contains(&screen));
    assert!(!auth.describe().to_string().contains(&key));
    auth.delete_key("home assistant").unwrap();
    assert!(!auth.key_allows(&key, Scope::Alert));
}

#[test]
fn a_person_can_change_their_own_password() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    assert!(
        auth.change_own_password("admin", "wrong password!!", "another long phrase")
            .is_err()
    );
    auth.change_own_password("admin", PW, "another long phrase")
        .unwrap();
    assert!(auth.login(ip("10.0.0.3"), "admin", PW).is_err());
    assert!(
        auth.login(ip("10.0.0.3"), "admin", "another long phrase")
            .is_ok()
    );
}

#[test]
fn cookie_and_key_parsing() {
    let mut h = HeaderMap::new();
    h.insert(
        header::COOKIE,
        "a=1; dw_session=abc123; b=2".parse().unwrap(),
    );
    assert_eq!(cookie_token(&h).as_deref(), Some("abc123"));
    h.insert(header::AUTHORIZATION, "Bearer dwk_x".parse().unwrap());
    assert_eq!(bearer_key(&h).as_deref(), Some("dwk_x"));
    assert_eq!(strip_key("key=abc&x=1").as_deref(), Some("x=1"));
    assert_eq!(strip_key("key=abc"), None);
    assert_eq!(query_key("x=1&key=abc"), Some("abc"));
    let c = session_cookie("t", true, 60);
    let c = c.to_str().unwrap();
    assert!(c.contains("HttpOnly") && c.contains("SameSite=Lax") && c.contains("Secure"));
}

// ---- through the middleware ------------------------------------------------

fn app(auth: &Auth, need: Need) -> Router {
    Router::new()
        .route("/", get(|| async { "page" }).post(|| async { "wrote" }))
        .layer(middleware::from_fn_with_state(auth.guard(need), enforce))
}

async fn send(app: &Router, method: &str, uri: &str, hdrs: &[(&str, &str)]) -> Response {
    let mut req = HttpRequest::builder().method(method).uri(uri);
    for (k, v) in hdrs {
        req = req.header(*k, *v);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn open_while_no_password_is_set() {
    let (_d, auth) = fresh();
    let app = app(&auth, Need::Admin);
    assert_eq!(send(&app, "POST", "/", &[]).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn guarded_routes_redirect_browsers_and_refuse_scripts() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let app = app(&auth, Need::Viewer);
    let r = send(&app, "GET", "/?a=1", &[("accept", "text/html")]).await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[header::LOCATION], "/login?next=/%3Fa%3D1");
    assert_eq!(
        send(&app, "GET", "/", &[]).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_tab_icon_needs_no_login() {
    // Mounted as in main.rs: the icons next to a guarded page, so the login
    // page and a signed-out tab still show the icon.
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let app = crate::icons::router().merge(app(&auth, Need::Viewer));
    for path in ["/favicon.svg", "/favicon-32.png", "/apple-touch-icon.png"] {
        let r = send(&app, "GET", path, &[("accept", "image/*")]).await;
        assert_eq!(r.status(), StatusCode::OK, "{path}");
    }
    assert_eq!(
        send(&app, "GET", "/", &[]).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn viewer_reads_but_cannot_write_and_writes_need_the_header() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    add(&auth, "guest", Role::Viewer, "a different phrase");
    add(&auth, "anna", Role::Operator, "an operator phrase");
    let (_, viewer) = auth
        .login(ip("10.0.0.4"), "guest", "a different phrase")
        .ok()
        .unwrap();
    let (_, operator) = auth
        .login(ip("10.0.0.4"), "anna", "an operator phrase")
        .ok()
        .unwrap();
    let (_, admin) = auth.login(ip("10.0.0.4"), "admin", PW).ok().unwrap();
    let dash = app(&auth, Need::Viewer);
    let v = format!("dw_session={viewer}");
    let a = format!("dw_session={admin}");
    let o = format!("dw_session={operator}");
    // An operator dismisses (writes to the dashboard) but never opens setup.
    let write = [("cookie", o.as_str()), ("x-deskwatch", "1")];
    assert_eq!(
        send(&dash, "POST", "/", &write).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&app(&auth, Need::Admin), "GET", "/", &[("cookie", &o)])
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&dash, "GET", "/", &[("cookie", &v)]).await.status(),
        StatusCode::OK
    );
    let write = [("cookie", v.as_str()), ("x-deskwatch", "1")];
    assert_eq!(
        send(&dash, "POST", "/", &write).await.status(),
        StatusCode::FORBIDDEN
    );
    // The administrator writes, but only with the custom header.
    assert_eq!(
        send(&dash, "POST", "/", &[("cookie", &a)]).await.status(),
        StatusCode::FORBIDDEN
    );
    let write = [("cookie", a.as_str()), ("x-deskwatch", "1")];
    assert_eq!(
        send(&dash, "POST", "/", &write).await.status(),
        StatusCode::OK
    );
    // The setup page is for the administrator only.
    let setup = app(&auth, Need::Admin);
    assert_eq!(
        send(&setup, "GET", "/", &[("cookie", &v)]).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&setup, "GET", "/", &[("cookie", &a)]).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn alert_key_works_without_a_session_but_a_viewer_key_does_not() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let alert = auth.create_key("ha", Scope::Alert).unwrap();
    let screen = auth.create_key("pi", Scope::Viewer).unwrap();
    let app = app(&auth, Need::AlertKey);
    let bearer = format!("Bearer {alert}");
    assert_eq!(
        send(&app, "POST", "/", &[("authorization", &bearer)])
            .await
            .status(),
        StatusCode::OK
    );
    let bearer = format!("Bearer {screen}");
    assert_eq!(
        send(
            &app,
            "POST",
            "/",
            &[("authorization", &bearer), ("x-deskwatch", "1")]
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, "POST", "/", &[]).await.status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn a_screen_opens_the_page_once_with_its_viewer_key() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let screen = auth.create_key("pi", Scope::Viewer).unwrap();
    let app = app(&auth, Need::Viewer);
    let r = send(
        &app,
        "GET",
        &format!("/?key={screen}"),
        &[("accept", "text/html")],
    )
    .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[header::LOCATION], "/");
    let cookie = r.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    let pair = cookie.split(';').next().unwrap().to_string();
    assert_eq!(
        send(&app, "GET", "/", &[("cookie", &pair)]).await.status(),
        StatusCode::OK
    );
    let bad = send(&app, "GET", "/?key=dwk_nope", &[("accept", "text/html")]).await;
    assert_eq!(bad.status(), StatusCode::SEE_OTHER);
    assert!(
        bad.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("/login")
    );
}

#[test]
fn an_older_file_with_one_admin_and_a_viewer_becomes_users() {
    let d = TempDir::new();
    let admin = hash_password(PW).unwrap();
    let viewer = hash_password("a different phrase").unwrap();
    std::fs::write(
        d.0.join("auth.toml"),
        format!(
            "[admin]\nusername = \"boss\"\nhash = \"{admin}\"\n\n[viewer]\nhash = \"{viewer}\"\n"
        ),
    )
    .unwrap();
    let auth = Auth::open(Some(&d.0), false).unwrap();
    assert!(auth.enabled());
    assert_eq!(
        auth.login(ip("10.0.0.5"), "boss", PW).ok().unwrap().0,
        Role::Admin
    );
    assert_eq!(
        auth.login(ip("10.0.0.5"), "viewer", "a different phrase")
            .ok()
            .unwrap()
            .0,
        Role::Viewer
    );
    // The next write stores the new format only.
    auth.create_key("ha", Scope::Alert).unwrap();
    let text = std::fs::read_to_string(d.0.join("auth.toml")).unwrap();
    assert!(text.contains("[[user]]") && !text.contains("[admin]") && !text.contains("[viewer]"));
}

#[test]
fn users_are_managed_and_the_last_administrator_stays() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    auth.add_user("anna", Role::Operator, "an operator phrase")
        .unwrap();
    assert!(
        auth.add_user("Anna", Role::Viewer, "another phrase here")
            .is_err()
    );
    assert!(
        auth.add_user("viewer", Role::Viewer, "another phrase here")
            .is_err()
    );
    assert!(auth.add_user("bob", Role::Viewer, "short").is_err());
    assert!(
        auth.delete_user("admin").is_err(),
        "the last administrator stays"
    );
    assert!(auth.set_user_role("admin", Role::Viewer).is_err());
    auth.set_user_role("anna", Role::Admin).unwrap();
    auth.set_user_role("admin", Role::Viewer).unwrap();
    auth.delete_user("admin").unwrap();
    assert!(auth.login(ip("10.0.0.6"), "admin", PW).is_err());
    // A deleted or changed user is signed out at once.
    let (_, token) = auth
        .login(ip("10.0.0.6"), "anna", "an operator phrase")
        .ok()
        .unwrap();
    auth.set_user_role("anna", Role::Admin).unwrap();
    assert_eq!(
        auth.session_role(&token),
        None,
        "a new role means signing in again"
    );
    auth.reset_user_password("anna", "a brand new phrase")
        .unwrap();
    assert_eq!(auth.session_role(&token), None);
}

#[tokio::test]
async fn a_chosen_password_must_be_replaced_before_anything_else() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    auth.add_user("anna", Role::Operator, "an operator phrase")
        .unwrap();
    let (_, token) = auth
        .login(ip("10.0.0.7"), "anna", "an operator phrase")
        .ok()
        .unwrap();
    let cookie = format!("dw_session={token}");
    let dash = app(&auth, Need::Viewer);
    let r = send(
        &dash,
        "GET",
        "/",
        &[("cookie", &cookie), ("accept", "text/html")],
    )
    .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[header::LOCATION], "/account");
    assert_eq!(
        send(&dash, "GET", "/", &[("cookie", &cookie)])
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // The account page itself stays open to her, for every role.
    let account = app(&auth, Need::Account);
    assert_eq!(
        send(&account, "GET", "/", &[("cookie", &cookie)])
            .await
            .status(),
        StatusCode::OK
    );
    assert!(
        auth.change_own_password("anna", "wrong one here!", "my own new phrase")
            .is_err()
    );
    assert!(
        auth.change_own_password("anna", "an operator phrase", "an operator phrase")
            .is_err()
    );
    auth.change_own_password("anna", "an operator phrase", "my own new phrase")
        .unwrap();
    assert_eq!(
        send(&dash, "GET", "/", &[("cookie", &cookie)])
            .await
            .status(),
        StatusCode::OK
    );
}

// ---- per-user dashboard layout ---------------------------------------------

async fn layout_call(
    auth: &Auth,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = HttpRequest::builder()
        .method(method)
        .uri(uri)
        .header("cookie", format!("dw_session={cookie}"))
        .header("x-deskwatch", "1")
        .header("content-type", "application/json");
    if body.is_none() {
        req = req.header("content-length", "0");
    }
    let res = auth
        .router()
        .oneshot(
            req.body(Body::from(body.unwrap_or("").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn each_user_keeps_their_own_layout() {
    let (d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    add(&auth, "vera", Role::Viewer, "a viewer phrase!!");
    add(&auth, "anna", Role::Operator, "an operator phrase");
    let vera = auth
        .login(ip("10.0.0.5"), "vera", "a viewer phrase!!")
        .ok()
        .unwrap()
        .1;
    let anna = auth
        .login(ip("10.0.0.5"), "anna", "an operator phrase")
        .ok()
        .unwrap()
        .1;
    let mine = r#"{"order":["jobs#1","hosts#1"],"hidden":["alerts#1"],"span":{"hosts#1":[1,2]},"pos":{"hosts#1":[3,2]}}"#;

    // Nothing saved yet.
    let (s, j) = layout_call(&auth, "GET", "/api/auth/layout", &vera, None).await;
    assert_eq!((s, j["layout"].is_null()), (StatusCode::OK, true));
    // A viewer saves a layout of their own; nobody else sees it.
    let (s, _) = layout_call(&auth, "PUT", "/api/auth/layout", &vera, Some(mine)).await;
    assert_eq!(s, StatusCode::OK);
    let (_, j) = layout_call(&auth, "GET", "/api/auth/layout", &vera, None).await;
    assert_eq!(j["layout"]["hidden"][0], "alerts#1");
    let (_, j) = layout_call(&auth, "GET", "/api/auth/layout", &anna, None).await;
    assert!(j["layout"].is_null());
    // It survives a restart.
    let again = Auth::open(Some(&d.0), false).unwrap();
    let token = again
        .login(ip("10.0.0.6"), "vera", "a viewer phrase!!")
        .ok()
        .unwrap()
        .1;
    let (_, j) = layout_call(&again, "GET", "/api/auth/layout", &token, None).await;
    assert_eq!(j["layout"]["span"]["hosts#1"][1], 2);
    assert_eq!(j["layout"]["pos"]["hosts#1"][0], 3);
    // Sizes out of range are refused; reset gives the default back.
    let bad = r#"{"span":{"hosts#1":[0,99]}}"#;
    let (s, _) = layout_call(&auth, "PUT", "/api/auth/layout", &vera, Some(bad)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let bad = r#"{"pos":{"hosts#1":[1,13]}}"#;
    let (s, _) = layout_call(&auth, "PUT", "/api/auth/layout", &vera, Some(bad)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = layout_call(&auth, "POST", "/api/auth/layout/reset", &vera, None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, j) = layout_call(&auth, "GET", "/api/auth/layout", &vera, None).await;
    assert!(j["layout"].is_null());
}

#[tokio::test]
async fn a_screen_key_has_no_saved_layout() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let screen = auth.new_session(Role::Viewer);
    let (s, _) = layout_call(&auth, "GET", "/api/auth/layout", &screen, None).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[test]
fn a_hash_made_by_an_older_version_still_verifies_and_new_hashes_work() {
    // Made with argon2 0.5.3 (the version before the 0.6 bump), default
    // parameters, for PW. A stored auth.toml must keep working after updates.
    let old = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$69I7qCjDTsQwhQqFvpQJiqEI0lGQvxC84fXE0Ktxca4";
    assert!(verify_password(old, PW));
    assert!(!verify_password(old, "wrong password entirely"));
    assert!(!verify_password("not a hash", PW));

    let new = hash_password(PW).unwrap();
    assert!(new.starts_with("$argon2id$v=19$"));
    assert!(verify_password(&new, PW));
    assert!(!verify_password(&new, "wrong password entirely"));
}

#[tokio::test]
async fn a_new_install_opens_on_the_first_run_page_and_scripts_are_left_alone() {
    let (_d, auth) = fresh();
    let app = app(&auth, Need::Admin);
    let page = [("accept", "text/html")];
    let r = send(&app, "GET", "/", &page).await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[header::LOCATION], "/login");
    // A script (no HTML accept header), a write and the health probe path are untouched.
    assert_eq!(send(&app, "GET", "/", &[]).await.status(), StatusCode::OK);
    assert_eq!(send(&app, "POST", "/", &[]).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_new_install_can_stay_open() {
    // `[auth] required = false`
    let (_d, auth) = fresh();
    auth.configure(false, true);
    let app = app(&auth, Need::Viewer);
    let r = send(&app, "GET", "/", &[("accept", "text/html")]).await;
    assert_eq!(r.status(), StatusCode::OK);
    // "Continue without a login" is remembered across a restart.
    let (d, auth) = fresh();
    auth.continue_without_login().unwrap();
    let r = send(&app_for(&auth), "GET", "/", &[("accept", "text/html")]).await;
    assert_eq!(r.status(), StatusCode::OK);
    let again = Auth::open(Some(&d.0), false).unwrap();
    let r = send(&app_for(&again), "GET", "/", &[("accept", "text/html")]).await;
    assert_eq!(r.status(), StatusCode::OK);
}

fn app_for(auth: &Auth) -> Router {
    app(auth, Need::Viewer)
}

#[tokio::test]
async fn an_existing_install_without_a_password_is_not_redirected() {
    let (d, auth) = fresh();
    std::fs::write(d.0.join("sources.toml"), "").unwrap();
    let again = Auth::open(Some(&d.0), false).unwrap();
    let r = send(&app_for(&again), "GET", "/", &[("accept", "text/html")]).await;
    assert_eq!(r.status(), StatusCode::OK);
    drop(auth);
}

#[tokio::test]
async fn without_a_data_folder_nothing_is_redirected() {
    let auth = Auth::open(None, false).unwrap();
    let r = send(&app_for(&auth), "GET", "/", &[("accept", "text/html")]).await;
    assert_eq!(r.status(), StatusCode::OK);
}

#[test]
fn sessions_survive_a_restart_and_only_a_hash_is_saved() {
    let (d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let (_, token) = auth.login(ip("10.0.0.2"), "admin", PW).ok().unwrap();
    let saved = std::fs::read_to_string(d.0.join("sessions.json")).unwrap();
    assert!(
        !saved.contains(&token),
        "the session id must not be on disk"
    );
    let again = Auth::open(Some(&d.0), false).unwrap();
    assert_eq!(again.session_role(&token), Some(Role::Admin));
    // Signing out is saved too.
    again.end_session(&token);
    let third = Auth::open(Some(&d.0), false).unwrap();
    assert_eq!(third.session_role(&token), None);
}

#[test]
fn stay_signed_in_lasts_thirty_days_and_a_plain_session_seven() {
    let (d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let (_, plain) = auth.login(ip("10.0.0.2"), "admin", PW).ok().unwrap();
    let (_, kept) = auth
        .login_with(ip("10.0.0.2"), "admin", PW, true)
        .ok()
        .unwrap();
    // Eight days later only the remembered one is alive.
    let eight_days = 8 * 24 * 3600;
    for s in auth.0.sessions.lock().unwrap().values_mut() {
        s.seen -= eight_days;
        if s.until != 0 {
            s.until -= eight_days;
        }
    }
    assert_eq!(auth.session_role(&plain), None);
    assert_eq!(auth.session_role(&kept), Some(Role::Admin));
    // 31 days after sign-in it ends, even though it was in use.
    for s in auth.0.sessions.lock().unwrap().values_mut() {
        s.until = now_secs() - 1;
    }
    assert_eq!(auth.session_role(&kept), None);
    drop(d);
}

#[test]
fn expired_sessions_are_not_loaded_and_a_password_change_ends_saved_ones() {
    let (d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    let (_, token) = auth
        .login_with(ip("10.0.0.2"), "admin", PW, true)
        .ok()
        .unwrap();
    auth.end_all_sessions();
    assert_eq!(
        Auth::open(Some(&d.0), false).unwrap().session_role(&token),
        None
    );
    let (_, token) = auth.login(ip("10.0.0.2"), "admin", PW).ok().unwrap();
    for s in auth.0.sessions.lock().unwrap().values_mut() {
        s.seen -= 8 * 24 * 3600;
    }
    auth.save_sessions();
    assert_eq!(
        Auth::open(Some(&d.0), false).unwrap().session_role(&token),
        None
    );
}
