//! The built-in login (docs/auth.md).
//!
//! One administrator (username and password) and, if wanted, one viewer
//! password, kept as Argon2id hashes in `auth.toml` in the data folder (mode
//! 0600). Until an administrator password is set the bridge behaves as before
//! (local network only, see `access.rs`). Once it is set, the dashboard, the
//! live WebSocket, the setup page and every write need a session; scripts use
//! API keys instead of a password. Webhooks keep their own signature check and
//! `/healthz` stays open.
//!
//! Sessions live in memory (a restart signs everyone out) and idle out after
//! seven days. The cookie is HttpOnly, SameSite=Lax and, over HTTPS, Secure.
//! Every write also needs the header `X-DeskWatch: 1`, which a page on another
//! site cannot send without a CORS preflight the bridge never allows; the
//! `Origin` check stays too. Wrong passwords are rate limited per client
//! address.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::kiosk::asset;
use crate::kiosk::origin;

mod layouts;
pub mod oauth;

const AUTH_FILE: &str = "auth.toml";
const COOKIE: &str = "dw_session";
const SESSION_IDLE: Duration = Duration::from_secs(7 * 24 * 3600);
const MIN_PASSWORD: usize = 12;
const MAX_PASSWORD: usize = 256;
const MAX_TRIES: u32 = 5;
const LOCK_BASE: Duration = Duration::from_secs(300);
const LOCK_MAX: Duration = Duration::from_secs(3600);
const MAX_KEYS: usize = 32;
const MAX_USERS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The dashboard, read only.
    Viewer,
    /// The dashboard, and dismissing or muting what it shows.
    Operator,
    /// Everything: setup, users and keys.
    Admin,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "viewer" => Some(Role::Viewer),
            "operator" => Some(Role::Operator),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// What a key may do: send alerts, or open the dashboard (a screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Alert,
    Viewer,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Persist {
    /// The accounts with a password. Older files have `[admin]` and `[viewer]`
    /// instead; `open` turns those into entries here.
    #[serde(default)]
    user: Vec<User>,
    #[serde(default)]
    key: Vec<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admin: Option<Admin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    viewer: Option<Viewer>,
}

impl Persist {
    /// Turn the single administrator and viewer password of earlier versions
    /// into users. Nothing changes for the people who sign in.
    fn migrate(&mut self) {
        let now = now_secs();
        if let Some(a) = self.admin.take() {
            self.user.push(User {
                name: a.username,
                role: Role::Admin,
                hash: a.hash,
                created: now,
                must_change: false,
            });
        }
        if let Some(v) = self.viewer.take() {
            self.user.push(User {
                name: "viewer".to_string(),
                role: Role::Viewer,
                hash: v.hash,
                created: now,
                must_change: false,
            });
        }
    }

    fn has_admin(&self) -> bool {
        self.user.iter().any(|u| u.role == Role::Admin)
    }

    fn find(&self, name: &str) -> Option<&User> {
        self.user.iter().find(|u| u.name == name)
    }
}

/// A local account.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    name: String,
    role: Role,
    hash: String,
    #[serde(default)]
    created: u64,
    /// Set when an administrator chose the password: the person replaces it
    /// at the first sign-in.
    #[serde(default)]
    must_change: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Admin {
    username: String,
    hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Viewer {
    hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Key {
    name: String,
    scope: Scope,
    /// Hex SHA-256 of the key; the key itself is shown once and never stored.
    hash: String,
    created: u64,
}

struct Session {
    role: Role,
    /// The local account, if this is not a provider sign-in.
    user: Option<String>,
    seen: Instant,
    /// Shown in the page header (the name at the provider), if not a local account.
    label: Option<String>,
    idle: Duration,
}

struct Lock {
    fails: u32,
    until: Option<Instant>,
    level: u32,
}

struct Inner {
    path: Option<PathBuf>,
    state: RwLock<Persist>,
    enabled: Arc<AtomicBool>,
    sessions: Mutex<HashMap<String, Session>>,
    locks: Mutex<HashMap<IpAddr, Lock>>,
    oauth: oauth::OAuth,
    layouts: layouts::Layouts,
    tls: bool,
}

/// Handle on the login state; cheap to clone.
#[derive(Clone)]
pub struct Auth(Arc<Inner>);

pub enum LoginError {
    /// Too many wrong tries: wait this long.
    Locked(Duration),
    Wrong {
        left: u32,
    },
}

fn hasher() -> Argon2<'static> {
    // Cheap parameters in tests only; the hash string carries its own, so
    // verifying works with whatever made it.
    #[cfg(test)]
    let params = Params::new(8, 1, 1, None).expect("params");
    #[cfg(not(test))]
    let params = Params::DEFAULT;
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

fn hash_password(password: &str) -> Result<String> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).expect("the system has no random source");
    let salt = SaltString::encode_b64(&raw).map_err(|e| anyhow::anyhow!("salt: {e}"))?;
    hasher()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("cannot hash the password: {e}"))
}

fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

fn check_password(password: &str) -> Result<()> {
    let len = password.chars().count();
    if len < MIN_PASSWORD {
        bail!("the password needs at least {MIN_PASSWORD} characters (a few words work well)");
    }
    if len > MAX_PASSWORD {
        bail!("the password is over {MAX_PASSWORD} characters");
    }
    Ok(())
}

fn check_username(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-@".contains(c));
    if !ok || name.eq_ignore_ascii_case("viewer") {
        bail!("the username may use letters, digits and . _ - @ (up to 64), and not be \"viewer\"");
    }
    Ok(())
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the system has no random source");
    hex::encode(buf)
}

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn now_secs() -> u64 {
    crate::source::unix_now()
}

impl Auth {
    /// Read `auth.toml` from the data folder. Without a folder login cannot
    /// be saved, so it stays off.
    pub fn open(dir: Option<&Path>, tls: bool) -> Result<Self> {
        let path = dir.map(|d| d.join(AUTH_FILE));
        let state = match &path {
            Some(p) => match std::fs::read_to_string(p) {
                Ok(text) => toml::from_str(&text)
                    .with_context(|| format!("{} is not valid", p.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persist::default(),
                Err(e) => return Err(e).with_context(|| format!("cannot read {}", p.display())),
            },
            None => Persist::default(),
        };
        let mut state = state;
        state.migrate();
        let enabled = Arc::new(AtomicBool::new(state.has_admin()));
        Ok(Self(Arc::new(Inner {
            path,
            state: RwLock::new(state),
            enabled,
            sessions: Mutex::default(),
            locks: Mutex::default(),
            oauth: oauth::OAuth::open(dir)?,
            layouts: layouts::Layouts::open(dir),
            tls,
        })))
    }

    /// Is a password set, so that everything needs a login?
    pub fn enabled(&self) -> bool {
        self.0.enabled.load(Ordering::Relaxed)
    }

    /// The same answer as a shared flag (for `access.rs`).
    pub fn enabled_flag(&self) -> Arc<AtomicBool> {
        self.0.enabled.clone()
    }

    /// Can a password be saved (is there a data folder)?
    pub fn can_save(&self) -> bool {
        self.0.path.is_some()
    }

    fn read(&self) -> Persist {
        self.0
            .state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn save(&self, next: Persist) -> Result<()> {
        let path = self
            .0
            .path
            .as_ref()
            .context("there is no data folder to save to")?;
        let text = toml::to_string_pretty(&next)?;
        crate::store::write_atomic(path, text.as_bytes(), 0o600)?;
        self.0.enabled.store(next.has_admin(), Ordering::Relaxed);
        *self.0.state.write().unwrap_or_else(|e| e.into_inner()) = next;
        Ok(())
    }

    /// First run: create the administrator. Only while there is none.
    pub fn setup_admin(&self, username: &str, password: &str) -> Result<()> {
        let mut state = self.read();
        if state.has_admin() {
            bail!("an administrator password is already set");
        }
        check_username(username)?;
        check_password(password)?;
        state.user.retain(|u| u.name != username);
        state.user.push(User {
            name: username.to_string(),
            role: Role::Admin,
            hash: hash_password(password)?,
            created: now_secs(),
            must_change: false,
        });
        self.save(state)
    }

    /// Remove every local account (the "forgot my password" path, on the
    /// host). API keys stay.
    pub fn reset(&self) -> Result<()> {
        let mut state = self.read();
        state.user.clear();
        self.end_all_sessions();
        self.save(state)
    }

    /// Set a password from the command line: the account becomes (or stays)
    /// an administrator.
    pub fn set_admin_password(&self, username: &str, password: &str) -> Result<()> {
        let mut state = self.read();
        check_username(username)?;
        check_password(password)?;
        let hash = hash_password(password)?;
        match state.user.iter_mut().find(|u| u.name == username) {
            Some(u) => {
                u.hash = hash;
                u.role = Role::Admin;
                u.must_change = false;
            }
            None => state.user.push(User {
                name: username.to_string(),
                role: Role::Admin,
                hash,
                created: now_secs(),
                must_change: false,
            }),
        }
        self.end_all_sessions();
        self.save(state)
    }

    /// Add a local account with a password chosen by the administrator,
    /// which the person must replace at the first sign-in. Blocking (Argon2).
    pub fn add_user(&self, name: &str, role: Role, password: &str) -> Result<()> {
        let mut state = self.read();
        check_username(name)?;
        check_password(password)?;
        if state.user.len() >= MAX_USERS {
            bail!("at most {MAX_USERS} users");
        }
        if state.user.iter().any(|u| u.name.eq_ignore_ascii_case(name)) {
            bail!("a user with that name exists");
        }
        state.user.push(User {
            name: name.to_string(),
            role,
            hash: hash_password(password)?,
            created: now_secs(),
            must_change: true,
        });
        self.save(state)
    }

    /// Give a user another role. The last administrator cannot be lowered.
    pub fn set_user_role(&self, name: &str, role: Role) -> Result<()> {
        let mut state = self.read();
        let user = state
            .user
            .iter_mut()
            .find(|u| u.name == name)
            .context("no user with that name")?;
        user.role = role;
        if !state.has_admin() {
            bail!("there must be at least one administrator");
        }
        self.end_sessions_of(name);
        self.save(state)
    }

    /// Replace a password as an administrator; the person changes it again at
    /// the next sign-in. Blocking (Argon2).
    pub fn reset_user_password(&self, name: &str, password: &str) -> Result<()> {
        let mut state = self.read();
        check_password(password)?;
        let hash = hash_password(password)?;
        let user = state
            .user
            .iter_mut()
            .find(|u| u.name == name)
            .context("no user with that name")?;
        user.hash = hash;
        user.must_change = true;
        self.end_sessions_of(name);
        self.save(state)
    }

    /// Remove a user. The last administrator cannot be removed.
    pub fn delete_user(&self, name: &str) -> Result<()> {
        let mut state = self.read();
        let before = state.user.len();
        state.user.retain(|u| u.name != name);
        if state.user.len() == before {
            bail!("no user with that name");
        }
        if !state.has_admin() {
            bail!("there must be at least one administrator");
        }
        self.end_sessions_of(name);
        self.save(state)
    }

    /// A person changes their own password. Blocking (Argon2).
    pub fn change_own_password(&self, name: &str, current: &str, new: &str) -> Result<()> {
        let mut state = self.read();
        let user = state
            .user
            .iter_mut()
            .find(|u| u.name == name)
            .context("this sign-in has no local password")?;
        if !verify_password(&user.hash, current) {
            bail!("the current password is wrong");
        }
        check_password(new)?;
        if new == current {
            bail!("choose a different password");
        }
        user.hash = hash_password(new)?;
        user.must_change = false;
        self.save(state)
    }

    fn lock_left(&self, ip: IpAddr) -> Option<Duration> {
        let locks = self.0.locks.lock().unwrap_or_else(|e| e.into_inner());
        let until = locks.get(&ip)?.until?;
        until.checked_duration_since(Instant::now())
    }

    fn fail(&self, ip: IpAddr) -> LoginError {
        let mut locks = self.0.locks.lock().unwrap_or_else(|e| e.into_inner());
        if locks.len() > 2000 {
            let now = Instant::now();
            locks.retain(|_, l| l.until.is_some_and(|u| u > now));
        }
        let lock = locks.entry(ip).or_insert(Lock {
            fails: 0,
            until: None,
            level: 0,
        });
        lock.fails += 1;
        if lock.fails >= MAX_TRIES {
            let wait = (LOCK_BASE * 2u32.pow(lock.level.min(4))).min(LOCK_MAX);
            lock.level += 1;
            lock.fails = 0;
            lock.until = Some(Instant::now() + wait);
            warn!(client = %ip, "too many wrong passwords, sign-in paused for {} s", wait.as_secs());
            return LoginError::Locked(wait);
        }
        LoginError::Wrong {
            left: MAX_TRIES - lock.fails,
        }
    }

    /// Check a username and password. Returns the role and a session token.
    /// Blocking (Argon2): call from `spawn_blocking`.
    pub fn login(
        &self,
        ip: IpAddr,
        username: &str,
        password: &str,
    ) -> Result<(Role, String), LoginError> {
        if let Some(left) = self.lock_left(ip) {
            return Err(LoginError::Locked(left));
        }
        let state = self.read();
        // Always do one hash check, so a wrong username takes as long as a wrong password.
        let dummy = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let found = match state.find(username) {
            Some(u) => verify_password(&u.hash, password).then_some(u),
            None => {
                let _ = verify_password(dummy, password);
                None
            }
        };
        let role = found.map(|u| u.role);
        let name = found.map(|u| u.name.clone());
        match role {
            Some(role) => {
                self.0
                    .locks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&ip);
                info!(client = %ip, role = role.name(), "signed in");
                Ok((
                    role,
                    self.new_local_session(role, name.expect("found with the role")),
                ))
            }
            None => {
                warn!(client = %ip, "failed sign-in");
                Err(self.fail(ip))
            }
        }
    }

    fn new_session(&self, role: Role) -> String {
        self.new_session_with(role, None, None, SESSION_IDLE)
    }

    fn new_local_session(&self, role: Role, user: String) -> String {
        self.new_session_with(role, Some(user), None, SESSION_IDLE)
    }

    fn new_session_with(
        &self,
        role: Role,
        user: Option<String>,
        label: Option<String>,
        idle: Duration,
    ) -> String {
        let token = random_hex(32);
        let mut sessions = self.0.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        sessions.retain(|_, s| now.duration_since(s.seen) < s.idle);
        sessions.insert(
            token.clone(),
            Session {
                role,
                user,
                seen: now,
                label,
                idle,
            },
        );
        token
    }

    /// The sign-in providers (GitHub, Microsoft).
    pub fn oauth(&self) -> &oauth::OAuth {
        &self.0.oauth
    }

    fn session_label(&self, token: &str) -> Option<String> {
        let sessions = self.0.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.get(token)?.label.clone()
    }

    /// The role of a live session, refreshing its idle timer.
    pub fn session_role(&self, token: &str) -> Option<Role> {
        let mut sessions = self.0.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions.get_mut(token)?;
        let now = Instant::now();
        if now.duration_since(session.seen) >= session.idle {
            sessions.remove(token);
            return None;
        }
        session.seen = now;
        Some(session.role)
    }

    fn end_session(&self, token: &str) {
        self.0
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
    }

    fn end_sessions_of(&self, user: &str) {
        self.0
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, s| s.user.as_deref() != Some(user));
    }

    /// The local account behind a session and whether it must change its
    /// password first. `None` for a provider sign-in or a screen key.
    fn session_user(&self, token: &str) -> Option<(String, bool)> {
        let name = self
            .0
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(token)?
            .user
            .clone()?;
        let must = self.read().find(&name).is_some_and(|u| u.must_change);
        Some((name, must))
    }

    fn end_all_sessions(&self) -> usize {
        let mut sessions = self.0.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let n = sessions.len();
        sessions.clear();
        n
    }

    fn session_count(&self) -> usize {
        self.0
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Does `key` belong to a key with this scope?
    pub fn key_allows(&self, key: &str, scope: Scope) -> bool {
        let hash = sha256_hex(key);
        self.read()
            .key
            .iter()
            .any(|k| k.scope == scope && k.hash == hash)
    }

    /// Open a viewer session for a screen that presents a viewer key.
    fn session_for_key(&self, key: &str) -> Option<String> {
        self.key_allows(key, Scope::Viewer)
            .then(|| self.new_session(Role::Viewer))
    }

    /// Create a key and return it. This is the only time it can be read.
    pub fn create_key(&self, name: &str, scope: Scope) -> Result<String> {
        let mut state = self.read();
        let name = name.trim();
        if name.is_empty() || name.len() > 40 || name.chars().any(char::is_control) {
            bail!("give the key a short name (up to 40 characters)");
        }
        if state.key.iter().any(|k| k.name == name) {
            bail!("a key with that name exists");
        }
        if state.key.len() >= MAX_KEYS {
            bail!("at most {MAX_KEYS} keys");
        }
        let key = format!("dwk_{}", random_hex(24));
        state.key.push(Key {
            name: name.to_string(),
            scope,
            hash: sha256_hex(&key),
            created: now_secs(),
        });
        self.save(state)?;
        Ok(key)
    }

    pub fn delete_key(&self, name: &str) -> Result<()> {
        let mut state = self.read();
        let before = state.key.len();
        state.key.retain(|k| k.name != name);
        if state.key.len() == before {
            bail!("no key with that name");
        }
        self.save(state)
    }

    fn describe(&self) -> Value {
        let state = self.read();
        json!({
            "users": state.user.iter().map(|u| json!({
                "name": u.name,
                "role": u.role.name(),
                "created": u.created,
                "must_change": u.must_change,
            })).collect::<Vec<_>>(),
            "sessions": self.session_count(),
            "keys": state.key.iter().map(|k| json!({
                "name": k.name,
                "scope": k.scope,
                "created": k.created,
            })).collect::<Vec<_>>(),
        })
    }

    fn secure_cookie(&self, headers: &HeaderMap) -> bool {
        self.0.tls
            || headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("https"))
    }
}

fn session_cookie(token: &str, secure: bool, max_age: u64) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie is ascii")
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
}

fn bearer_key(headers: &HeaderMap) -> Option<String> {
    if let Some(rest) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(rest.trim().to_string());
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
}

/// What a guarded route needs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The dashboard: any session reads, an operator or administrator writes.
    Viewer,
    /// The setup page and the account settings.
    Admin,
    /// `POST /api/alert`: an alert key, or the administrator.
    AlertKey,
    /// The person's own account page: any signed-in user, reading or
    /// writing, even before a first password change.
    Account,
}

#[derive(Clone)]
pub struct Guard {
    auth: Auth,
    need: Need,
}

impl Auth {
    pub fn guard(&self, need: Need) -> Guard {
        Guard {
            auth: self.clone(),
            need,
        }
    }
}

fn wants_html(headers: &HeaderMap, path: &str) -> bool {
    !path.starts_with("/api/")
        && headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/html"))
}

fn login_redirect(path_and_query: &str) -> Response {
    let next: String = path_and_query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    Redirect::to(&format!("/login?next={next}")).into_response()
}

/// Remove `key=...` from a query string; `None` if nothing is left.
fn strip_key(query: &str) -> Option<String> {
    let rest: Vec<&str> = query
        .split('&')
        .filter(|p| !p.starts_with("key="))
        .collect();
    (!rest.is_empty()).then(|| rest.join("&"))
}

fn query_key(query: &str) -> Option<&str> {
    query.split('&').find_map(|p| p.strip_prefix("key="))
}

/// Middleware: with a password set, let a request through only with the
/// right credentials; without one, change nothing.
pub async fn enforce(State(guard): State<Guard>, request: Request, next: Next) -> Response {
    let auth = &guard.auth;
    if !auth.enabled() {
        return next.run(request).await;
    }
    let headers = request.headers();
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let reading = matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    let key = bearer_key(headers);
    let alert_key_ok = guard.need == Need::AlertKey
        && key
            .as_deref()
            .is_some_and(|k| auth.key_allows(k, Scope::Alert));
    // A write needs the custom header, except a script with an alert key
    // (curl and Home Assistant cannot be a browser's cross-site request).
    if !reading && !alert_key_ok && !headers.contains_key("x-deskwatch") {
        return (StatusCode::FORBIDDEN, "missing X-DeskWatch header").into_response();
    }
    if alert_key_ok {
        return next.run(request).await;
    }
    let role = cookie_token(headers).and_then(|t| auth.session_role(&t));
    // A screen opens the page once with `?key=<viewer key>`: it gets a
    // viewer session and is sent on to the same page without the key.
    let screen_session = (role.is_none() && guard.need == Need::Viewer && reading)
        .then(|| {
            request
                .uri()
                .query()
                .and_then(query_key)
                .and_then(|k| auth.session_for_key(k))
        })
        .flatten();
    if let Some(token) = screen_session {
        let target = match request.uri().query().and_then(strip_key) {
            Some(q) => format!("{path}?{q}"),
            None => path.clone(),
        };
        let mut response = Redirect::to(&target).into_response();
        response.headers_mut().append(
            header::SET_COOKIE,
            session_cookie(&token, auth.secure_cookie(request.headers()), 7 * 24 * 3600),
        );
        return response;
    }
    let Some(role) = role else {
        if reading && wants_html(headers, &path) && guard.need != Need::AlertKey {
            let pq = request
                .uri()
                .path_and_query()
                .map_or(path.clone(), |p| p.to_string());
            return login_redirect(&pq);
        }
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    };
    let needed = match guard.need {
        Need::Admin | Need::AlertKey => Role::Admin,
        Need::Viewer if reading => Role::Viewer,
        Need::Viewer => Role::Operator,
        Need::Account => Role::Viewer,
    };
    if role < needed {
        let who = if needed == Role::Admin {
            "this needs the administrator"
        } else {
            "this needs an operator or administrator"
        };
        return (StatusCode::FORBIDDEN, who).into_response();
    }
    // A password an administrator chose must be replaced before anything else.
    if guard.need != Need::Account
        && cookie_token(headers)
            .and_then(|t| auth.session_user(&t))
            .is_some_and(|(_, must)| must)
    {
        if reading && wants_html(headers, &path) {
            return Redirect::to("/account").into_response();
        }
        return (
            StatusCode::FORBIDDEN,
            "choose a new password first (open /account)",
        )
            .into_response();
    }
    next.run(request).await
}

/// The account routes only mean something once a password exists.
async fn require_enabled(State(auth): State<Auth>, request: Request, next: Next) -> Response {
    if !auth.enabled() {
        return text(StatusCode::CONFLICT, "no password is set yet");
    }
    next.run(request).await
}

// ---- routes ---------------------------------------------------------------

const LOGIN_HTML: &str = include_str!("../auth/login.html");
const LOGIN_JS: &str = include_str!("../auth/login.js");
const LOGIN_CSS: &str = include_str!("../auth/login.css");
const ACCOUNT_HTML: &str = include_str!("../auth/account.html");
const ACCOUNT_JS: &str = include_str!("../auth/account.js");

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

fn client(request_peer: Option<SocketAddr>, headers: &HeaderMap) -> Option<IpAddr> {
    crate::access::client_ip(request_peer?.ip(), headers)
}

fn text(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("hash task")
}

async fn status(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    let role = cookie_token(&headers).and_then(|t| auth.session_role(&t));
    let state = if !auth.enabled() {
        if auth.can_save() { "first_run" } else { "open" }
    } else if role.is_some() {
        "signed_in"
    } else {
        "login"
    };
    let (local, must_change) = match cookie_token(&headers).and_then(|t| auth.session_user(&t)) {
        Some((n, m)) => (Some(n), m),
        None => (None, false),
    };
    let mut body = json!({
        "state": state,
        "role": role.map(Role::name),
        "min_password": MIN_PASSWORD,
        "providers": if auth.enabled() {
            auth.oauth().enabled().iter().map(|p| p.id()).collect::<Vec<_>>()
        } else {
            Vec::new()
        },
        "user": cookie_token(&headers).and_then(|t| auth.session_label(&t)),
        "name": local,
        "must_change": must_change,
    });
    if role == Some(Role::Admin) {
        body["account"] = auth.describe();
    }
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn started(
    auth: &Auth,
    role: Role,
    token: &str,
    headers: &HeaderMap,
    must_change: bool,
) -> Response {
    let mut response = Json(json!({ "ok": true, "role": role.name(), "must_change": must_change }))
        .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        session_cookie(token, auth.secure_cookie(headers), 7 * 24 * 3600),
    );
    response
}

async fn setup_admin(
    State(auth): State<Auth>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Credentials>,
) -> Response {
    if !origin::same_origin(&headers) {
        return text(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    // Only a machine on the local network may claim a fresh install.
    if !client(Some(peer.0), &headers).is_some_and(crate::access::is_local) {
        return text(
            StatusCode::FORBIDDEN,
            "the first password can only be set from the local network (or with `deskwatch-bridge auth set-password` on the host)",
        );
    }
    if !auth.can_save() {
        return text(
            StatusCode::CONFLICT,
            "login needs a data folder (docs/data-dir.md)",
        );
    }
    let a = auth.clone();
    let (user, pass) = (body.username.clone(), body.password.clone());
    match blocking(move || a.setup_admin(&user, &pass)).await {
        Ok(()) => {
            info!("administrator password set from the browser");
            let token = auth.new_session(Role::Admin);
            started(&auth, Role::Admin, &token, &headers, false)
        }
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn login(
    State(auth): State<Auth>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Credentials>,
) -> Response {
    if !origin::same_origin(&headers) || !headers.contains_key("x-deskwatch") {
        return text(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    let Some(ip) = client(Some(peer.0), &headers) else {
        return text(
            StatusCode::FORBIDDEN,
            "cannot tell where this request comes from",
        );
    };
    if !auth.enabled() {
        return text(StatusCode::CONFLICT, "no password is set yet");
    }
    let a = auth.clone();
    let result = blocking(move || a.login(ip, &body.username, &body.password)).await;
    match result {
        Ok((role, token)) => {
            let must = auth.session_user(&token).is_some_and(|(_, m)| m);
            started(&auth, role, &token, &headers, must)
        }
        Err(LoginError::Locked(wait)) => {
            let mins = wait.as_secs().div_ceil(60).max(1);
            let mut r = text(
                StatusCode::TOO_MANY_REQUESTS,
                format!("too many wrong tries, wait {mins} minute(s)"),
            );
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(wait.as_secs()));
            r
        }
        Err(LoginError::Wrong { left }) => text(
            StatusCode::UNAUTHORIZED,
            format!("wrong username or password, {left} tries left"),
        ),
    }
}

async fn logout(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_token(&headers) {
        auth.end_session(&token);
    }
    let mut response = Json(json!({ "ok": true })).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        session_cookie("", auth.secure_cookie(&headers), 0),
    );
    response
}

async fn logout_all(State(auth): State<Auth>) -> Response {
    let n = auth.end_all_sessions();
    info!("all sessions ended ({n})");
    Json(json!({ "ok": true, "ended": n })).into_response()
}

#[derive(Deserialize)]
struct NewUser {
    name: String,
    role: String,
    password: String,
}

const BAD_ROLE: &str = "the role is viewer, operator or admin";

async fn add_user(State(auth): State<Auth>, Json(body): Json<NewUser>) -> Response {
    let Some(role) = Role::parse(&body.role) else {
        return text(StatusCode::BAD_REQUEST, BAD_ROLE);
    };
    let a = auth.clone();
    let name = body.name.trim().to_string();
    let logged = name.clone();
    match blocking(move || a.add_user(&name, role, &body.password)).await {
        Ok(()) => {
            info!(user = %logged, role = role.name(), "user added");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct UserRole {
    name: String,
    role: String,
}

async fn set_user_role(
    State(auth): State<Auth>,
    headers: HeaderMap,
    Json(body): Json<UserRole>,
) -> Response {
    let Some(role) = Role::parse(&body.role) else {
        return text(StatusCode::BAD_REQUEST, BAD_ROLE);
    };
    if is_self(&auth, &headers, &body.name) && role != Role::Admin {
        return text(StatusCode::BAD_REQUEST, "you cannot lower your own role");
    }
    match auth.set_user_role(&body.name, role) {
        Ok(()) => {
            info!(user = %body.name, role = role.name(), "user role changed");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct UserPassword {
    name: String,
    password: String,
}

async fn reset_user_password(State(auth): State<Auth>, Json(body): Json<UserPassword>) -> Response {
    let a = auth.clone();
    match blocking(move || a.reset_user_password(&body.name, &body.password)).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn delete_user(
    State(auth): State<Auth>,
    headers: HeaderMap,
    Json(body): Json<NameBody>,
) -> Response {
    if is_self(&auth, &headers, &body.name) {
        return text(StatusCode::BAD_REQUEST, "you cannot remove yourself");
    }
    match auth.delete_user(&body.name) {
        Ok(()) => {
            info!(user = %body.name, "user removed");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

fn is_self(auth: &Auth, headers: &HeaderMap, name: &str) -> bool {
    cookie_token(headers)
        .and_then(|t| auth.session_user(&t))
        .is_some_and(|(n, _)| n == name)
}

#[derive(Deserialize)]
struct PasswordBody {
    current: String,
    new: String,
}

/// Any signed-in local user changes their own password.
async fn change_password(
    State(auth): State<Auth>,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
) -> Response {
    let Some((name, _)) = cookie_token(&headers).and_then(|t| auth.session_user(&t)) else {
        return text(
            StatusCode::BAD_REQUEST,
            "this sign-in has no local password to change",
        );
    };
    let a = auth.clone();
    let who = name.clone();
    match blocking(move || a.change_own_password(&who, &body.current, &body.new)).await {
        Ok(()) => {
            info!(user = %name, "password changed");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct KeyBody {
    name: String,
    scope: Option<Scope>,
}

async fn create_key(State(auth): State<Auth>, Json(body): Json<KeyBody>) -> Response {
    let scope = body.scope.unwrap_or(Scope::Alert);
    match auth.create_key(&body.name, scope) {
        Ok(key) => {
            info!(name = %body.name, "API key created");
            Json(json!({ "ok": true, "key": key })).into_response()
        }
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn delete_key(State(auth): State<Auth>, Json(body): Json<NameBody>) -> Response {
    match auth.delete_key(&body.name) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

// ---- sign in with GitHub or Microsoft ----------------------------------------

const OAUTH_COOKIE: &str = "dw_oauth";
const OAUTH_IDLE: Duration = Duration::from_secs(24 * 3600);

/// A path on this site, never an address on another one.
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\") => {
            n.to_string()
        }
        _ => "/".to_string(),
    }
}

fn oauth_cookie(value: &str, secure: bool, max_age: u64) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{OAUTH_COOKIE}={value}; Path=/auth; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie is ascii")
}

fn cookie_named(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.to_string())
}

fn login_error(code: &str) -> Response {
    Redirect::to(&format!("/login?error={code}")).into_response()
}

async fn oauth_start(
    State(auth): State<Auth>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(provider) = oauth::Provider::parse(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !auth.enabled() || !auth.oauth().enabled().contains(&provider) {
        return login_error("failed");
    }
    let next = safe_next(query.get("next").map(String::as_str));
    match auth.oauth().begin(provider, &next) {
        Ok((url, state)) => {
            let mut response = Redirect::to(&url).into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                oauth_cookie(&state, auth.secure_cookie(&headers), 600),
            );
            response
        }
        Err(e) => {
            warn!("cannot start a {name} sign-in: {e:#}");
            login_error("failed")
        }
    }
}

async fn oauth_callback(
    State(auth): State<Auth>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(provider) = oauth::Provider::parse(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let secure = auth.secure_cookie(&headers);
    let clear = oauth_cookie("", secure, 0);
    let mut response = if query.contains_key("error") {
        login_error("denied")
    } else {
        match (query.get("code"), query.get("state")) {
            (Some(code), Some(state)) if auth.enabled() => {
                let cookie = cookie_named(&headers, OAUTH_COOKIE);
                match auth
                    .oauth()
                    .finish(provider, code, state, cookie.as_deref())
                    .await
                {
                    Ok((role, label, next)) => {
                        info!(provider = provider.id(), %label, role = role.name(), "signed in with a provider");
                        let token = auth.new_session_with(role, None, Some(label), OAUTH_IDLE);
                        let mut r = Redirect::to(&next).into_response();
                        r.headers_mut().append(
                            header::SET_COOKIE,
                            session_cookie(&token, secure, OAUTH_IDLE.as_secs()),
                        );
                        r
                    }
                    Err(failure) => {
                        match &failure {
                            oauth::Failure::Failed(why) => {
                                warn!(provider = provider.id(), "sign-in failed: {why}");
                            }
                            other => info!(
                                provider = provider.id(),
                                "sign-in refused: {}",
                                other.code()
                            ),
                        }
                        login_error(failure.code())
                    }
                }
            }
            _ => login_error("expired"),
        }
    };
    response.headers_mut().append(header::SET_COOKIE, clear);
    response
}

/// The provider settings for the setup page: never a secret, only whether one is set.
async fn oauth_get(State(auth): State<Auth>) -> Response {
    let o = auth.oauth();
    let c = o.config();
    let base = c.public_url.clone().unwrap_or_default();
    let logins = |l: &[oauth::GhUser]| l.iter().map(|u| u.login.clone()).collect::<Vec<_>>();
    Json(json!({
        "public_url": c.public_url,
        "github": c.github.as_ref().map(|g| json!({
            "client_id": g.client_id,
            "secret_set": o.secret_set(oauth::Provider::Github),
            "users": logins(&g.users),
            "orgs": g.orgs,
            "teams": g.teams,
            "operator_users": logins(&g.operator_users),
            "operator_teams": g.operator_teams,
            "admin_users": logins(&g.admin_users),
            "admin_teams": g.admin_teams,
        })),
        "microsoft": c.microsoft.as_ref().map(|m| json!({
            "tenant": m.tenant,
            "client_id": m.client_id,
            "secret_set": o.secret_set(oauth::Provider::Microsoft),
            "groups": m.groups,
            "operator_groups": m.operator_groups,
            "admin_groups": m.admin_groups,
        })),
        "redirect": {
            "github": format!("{}/auth/callback/github", base.trim_end_matches('/')),
            "microsoft": format!("{}/auth/callback/microsoft", base.trim_end_matches('/')),
        },
    }))
    .into_response()
}

#[derive(Deserialize)]
struct GithubBody {
    client_id: String,
    secret: Option<String>,
    #[serde(default)]
    users: Vec<String>,
    #[serde(default)]
    orgs: Vec<String>,
    #[serde(default)]
    teams: Vec<String>,
    #[serde(default)]
    operator_users: Vec<String>,
    #[serde(default)]
    operator_teams: Vec<String>,
    #[serde(default)]
    admin_users: Vec<String>,
    #[serde(default)]
    admin_teams: Vec<String>,
}

#[derive(Deserialize)]
struct MicrosoftBody {
    tenant: String,
    client_id: String,
    secret: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    operator_groups: Vec<String>,
    #[serde(default)]
    admin_groups: Vec<String>,
}

#[derive(Deserialize)]
struct OauthBody {
    public_url: Option<String>,
    github: Option<GithubBody>,
    microsoft: Option<MicrosoftBody>,
}

fn clean(list: Vec<String>) -> Vec<String> {
    list.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

async fn oauth_save(State(auth): State<Auth>, Json(body): Json<OauthBody>) -> Response {
    let o = auth.oauth().clone();
    let current = o.config();
    let mut github_secret = None;
    let mut microsoft_secret = None;
    let mut config = oauth::Config {
        public_url: body
            .public_url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty()),
        ..Default::default()
    };
    if let Some(g) = body.github {
        let known = current.github.clone().unwrap_or_default();
        let resolve =
            async |logins: Vec<String>, known: &[oauth::GhUser]| -> Result<Vec<oauth::GhUser>> {
                let mut out = Vec::new();
                for login in clean(logins) {
                    out.push(o.resolve_github_user(&login, known).await?);
                }
                Ok(out)
            };
        let users = match resolve(g.users, &known.users).await {
            Ok(u) => u,
            Err(e) => return text(StatusCode::BAD_REQUEST, format!("{e:#}")),
        };
        let operator_users = match resolve(g.operator_users, &known.operator_users).await {
            Ok(u) => u,
            Err(e) => return text(StatusCode::BAD_REQUEST, format!("{e:#}")),
        };
        let admin_users = match resolve(g.admin_users, &known.admin_users).await {
            Ok(u) => u,
            Err(e) => return text(StatusCode::BAD_REQUEST, format!("{e:#}")),
        };
        github_secret = g.secret;
        config.github = Some(oauth::Github {
            client_id: g.client_id.trim().to_string(),
            users,
            orgs: clean(g.orgs),
            teams: clean(g.teams),
            operator_users,
            operator_teams: clean(g.operator_teams),
            admin_users,
            admin_teams: clean(g.admin_teams),
        });
    }
    if let Some(m) = body.microsoft {
        microsoft_secret = m.secret;
        config.microsoft = Some(oauth::Microsoft {
            tenant: m.tenant.trim().to_string(),
            client_id: m.client_id.trim().to_string(),
            groups: clean(m.groups),
            operator_groups: clean(m.operator_groups),
            admin_groups: clean(m.admin_groups),
        });
    }
    match o.save(
        config,
        github_secret.as_deref(),
        microsoft_secret.as_deref(),
    ) {
        Ok(()) => {
            info!("sign-in providers changed");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => text(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

impl Auth {
    /// The routes: the login page and its API (open), and the account
    /// settings (administrator only).
    pub fn router(&self) -> Router {
        let open = Router::new()
            .route(
                "/login",
                get(|| asset("text/html; charset=utf-8", LOGIN_HTML)),
            )
            .route(
                "/login.js",
                get(|| asset("text/javascript; charset=utf-8", LOGIN_JS)),
            )
            .route(
                "/login.css",
                get(|| asset("text/css; charset=utf-8", LOGIN_CSS)),
            )
            .route("/api/auth/status", get(status))
            .route("/api/auth/setup", post(setup_admin))
            .route("/api/auth/login", post(login))
            .route("/api/auth/logout", post(logout))
            .route("/auth/login/{provider}", get(oauth_start))
            .route("/auth/callback/{provider}", get(oauth_callback));
        let admin = Router::new()
            .route("/api/auth/logout-all", post(logout_all))
            .route("/api/auth/user", post(add_user))
            .route("/api/auth/user/role", post(set_user_role))
            .route("/api/auth/user/password", post(reset_user_password))
            .route("/api/auth/user/delete", post(delete_user))
            .route("/api/auth/key", post(create_key))
            .route("/api/auth/key/delete", post(delete_key))
            .route("/api/auth/oauth", get(oauth_get).post(oauth_save))
            .route_layer(middleware::from_fn_with_state(
                self.guard(Need::Admin),
                enforce,
            ))
            .route_layer(middleware::from_fn_with_state(
                self.clone(),
                require_enabled,
            ));
        let account = Router::new()
            .route(
                "/account",
                get(|| asset("text/html; charset=utf-8", ACCOUNT_HTML)),
            )
            .route(
                "/account.js",
                get(|| asset("text/javascript; charset=utf-8", ACCOUNT_JS)),
            )
            .route("/api/auth/password", post(change_password))
            .route(
                "/api/auth/layout",
                get(layouts::get_layout).put(layouts::put_layout),
            )
            .route("/api/auth/layout/reset", post(layouts::reset_layout))
            .route_layer(middleware::from_fn_with_state(
                self.guard(Need::Account),
                enforce,
            ))
            .route_layer(middleware::from_fn_with_state(
                self.clone(),
                require_enabled,
            ));
        open.merge(admin).merge(account).with_state(self.clone())
    }
}

/// `deskwatch-bridge auth reset|set-password [--user NAME] [CONFIG]`: the
/// host-side way to set or forget the administrator password. The new
/// password is read from the first line of standard input.
pub fn run_cli(args: Vec<String>) -> Result<()> {
    const HELP: &str = "usage: deskwatch-bridge auth reset [CONFIG]\n       \
        printf '%s\\n' 'a long password' | deskwatch-bridge auth set-password [--user NAME] [CONFIG]\n\n\
        reset forgets all local users and their passwords (API keys stay); the next visit\n\
        to the page shows the first-run screen. Restart the bridge afterwards.";
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        bail!("{HELP}");
    };
    let mut user = "admin".to_string();
    let mut config_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--user" => user = args.next().context("--user needs a name")?,
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            _ if config_path.is_none() => config_path = Some(PathBuf::from(arg)),
            _ => bail!("{HELP}"),
        }
    }
    let path = crate::config::Config::path(crate::config::Config::path_given(config_path));
    let config = crate::config::Config::load(&path)?;
    let dir = config.data_dir().context(
        "login needs a data folder: set [data] dir or DESKWATCH_DATA_DIR (docs/data-dir.md)",
    )?;
    let auth = Auth::open(Some(&dir), false)?;
    match command.as_str() {
        "reset" => {
            auth.reset()?;
            println!(
                "The passwords are removed. Restart the bridge; the page then asks for a new one."
            );
        }
        "set-password" => {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let password = line.trim_end_matches(['\r', '\n']);
            auth.set_admin_password(&user, password)?;
            println!("The administrator password for {user} is set. Restart the bridge.");
        }
        _ => bail!("{HELP}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
