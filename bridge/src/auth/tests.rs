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
    auth.set_viewer_password(Some("a different phrase"))
        .unwrap();
    let c = ip("192.168.1.9");
    let (role, token) = auth.login(c, "admin", PW).ok().unwrap();
    assert_eq!(role, Role::Admin);
    assert_eq!(auth.session_role(&token), Some(Role::Admin));
    let (role, _) = auth.login(c, "viewer", "a different phrase").ok().unwrap();
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
fn admin_can_change_the_password() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    assert!(
        auth.change_admin_password("wrong password!!", "another long phrase")
            .is_err()
    );
    auth.change_admin_password(PW, "another long phrase")
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
async fn viewer_reads_but_cannot_write_and_writes_need_the_header() {
    let (_d, auth) = fresh();
    auth.setup_admin("admin", PW).unwrap();
    auth.set_viewer_password(Some("a different phrase"))
        .unwrap();
    let (_, viewer) = auth
        .login(ip("10.0.0.4"), "viewer", "a different phrase")
        .ok()
        .unwrap();
    let (_, admin) = auth.login(ip("10.0.0.4"), "admin", PW).ok().unwrap();
    let dash = app(&auth, Need::Viewer);
    let v = format!("dw_session={viewer}");
    let a = format!("dw_session={admin}");
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
