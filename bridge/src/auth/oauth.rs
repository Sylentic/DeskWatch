//! Sign in with GitHub or Microsoft (docs/auth.md, "Sign in with GitHub or
//! Microsoft").
//!
//! Authorization code flow with PKCE (S256), a random `state` that is also
//! bound to the browser by a short-lived cookie, and for Microsoft a `nonce`.
//! Nobody gets in by default: a person must match an allow rule (GitHub user,
//! organisation or team; Microsoft group), and a second list of administrator
//! rules decides who gets the `admin` role instead of `viewer`. The identity
//! is the provider's immutable id (GitHub numeric user id, Microsoft `tid` and
//! `oid`), never an e-mail address. The roles are read again at every sign-in.
//!
//! The settings (no secrets) are in `oauth.toml` in the data folder; each
//! client secret is a credential file (`credentials/oauth-github`,
//! `credentials/oauth-microsoft`, mode 0600, never shown again).
//!
//! The Microsoft ID token comes straight from the token endpoint over TLS, so
//! (as OpenID Connect Core 3.1.3.7 allows) its signature is not checked; its
//! issuer, audience, tenant, expiry and nonce are.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::Role;

const OAUTH_FILE: &str = "oauth.toml";
const PENDING_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 200;
const MAX_LIST: usize = 50;
/// `checkMemberGroups` takes at most 20 group ids.
const MAX_GROUPS: usize = 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// Where people reach DeskWatch, e.g. `https://deskwatch.example.org`.
    pub public_url: Option<String>,
    pub github: Option<Github>,
    pub microsoft: Option<Microsoft>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GhUser {
    pub login: String,
    pub id: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Github {
    pub client_id: String,
    pub users: Vec<GhUser>,
    pub orgs: Vec<String>,
    /// `org/team-slug`
    pub teams: Vec<String>,
    pub admin_users: Vec<GhUser>,
    pub admin_teams: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Microsoft {
    /// Directory (tenant) id, a GUID.
    pub tenant: String,
    pub client_id: String,
    pub groups: Vec<String>,
    pub admin_groups: Vec<String>,
}

/// Where the providers live; tests point these at a local mock.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub github_authorize: String,
    pub github_token: String,
    pub github_api: String,
    pub ms_login: String,
    pub ms_graph: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            github_authorize: "https://github.com/login/oauth/authorize".into(),
            github_token: "https://github.com/login/oauth/access_token".into(),
            github_api: "https://api.github.com".into(),
            ms_login: "https://login.microsoftonline.com".into(),
            ms_graph: "https://graph.microsoft.com".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Github,
    Microsoft,
}

impl Provider {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "github" => Some(Self::Github),
            "microsoft" => Some(Self::Microsoft),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Microsoft => "microsoft",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Microsoft => "Microsoft",
        }
    }
}

/// Why a sign-in did not work; the page shows a fixed sentence for each.
#[derive(Debug)]
pub enum Failure {
    /// The person said no at the provider, or the provider refused.
    Denied,
    /// Signed in fine, but no rule lets this person in.
    NotAllowed,
    /// The link was old, reused or not started here.
    Expired,
    /// Anything else (details go to the log).
    Failed(String),
}

impl Failure {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::NotAllowed => "not_allowed",
            Self::Expired => "expired",
            Self::Failed(_) => "failed",
        }
    }
}

impl<E: std::fmt::Display> From<E> for Failure {
    fn from(e: E) -> Self {
        Self::Failed(e.to_string())
    }
}

struct Pending {
    provider: Provider,
    verifier: String,
    nonce: String,
    next: String,
    at: Instant,
}

struct Inner {
    dir: Option<PathBuf>,
    config: RwLock<Config>,
    endpoints: RwLock<Endpoints>,
    pending: Mutex<HashMap<String, Pending>>,
    http: reqwest::Client,
}

#[derive(Clone)]
pub struct OAuth(Arc<Inner>);

fn random_url_safe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the system has no random source");
    URL_SAFE_NO_PAD.encode(buf)
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn valid_guid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(n, p)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

impl Config {
    /// Check a configuration before it is saved.
    pub fn validate(&self) -> Result<()> {
        if let Some(url) = &self.public_url {
            let parsed = Url::parse(url).context("the public address is not a web address")?;
            let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
            if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
                bail!("the public address must start with https:// (http:// only for localhost)");
            }
            if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
                bail!("the public address is just the site, like https://deskwatch.example.org");
            }
        } else if self.github.is_some() || self.microsoft.is_some() {
            bail!("fill in the public address first: the providers send people back to it");
        }
        if let Some(g) = &self.github {
            if g.client_id.trim().is_empty() || g.client_id.len() > 100 {
                bail!("GitHub needs a client id");
            }
            for team in g.teams.iter().chain(&g.admin_teams) {
                let ok = team
                    .split_once('/')
                    .is_some_and(|(o, t)| valid_name(o) && valid_name(t));
                if !ok {
                    bail!("a GitHub team is written organisation/team-name, not \"{team}\"");
                }
            }
            if g.orgs.iter().any(|o| !valid_name(o)) {
                bail!("a GitHub organisation name may use letters, digits, - . _");
            }
            if [&g.users, &g.admin_users]
                .iter()
                .any(|l| l.len() > MAX_LIST)
                || [&g.orgs, &g.teams, &g.admin_teams]
                    .iter()
                    .any(|l| l.len() > MAX_LIST)
            {
                bail!("at most {MAX_LIST} entries per list");
            }
        }
        if let Some(m) = &self.microsoft {
            if !valid_guid(&m.tenant) {
                bail!(
                    "the Directory (tenant) id is a GUID like 00000000-0000-0000-0000-000000000000"
                );
            }
            if !valid_guid(&m.client_id) {
                bail!("the Application (client) id is a GUID");
            }
            if m.groups
                .iter()
                .chain(&m.admin_groups)
                .any(|g| !valid_guid(g))
            {
                bail!("a Microsoft group is written as its Object Id (a GUID)");
            }
            if m.groups.len() + m.admin_groups.len() > MAX_GROUPS {
                bail!("at most {MAX_GROUPS} groups in total");
            }
        }
        Ok(())
    }
}

/// Who an identity is, as far as the rules care. All names lower case.
#[derive(Debug, Default)]
pub struct GithubIdentity {
    pub id: u64,
    pub orgs: Vec<String>,
    pub teams: Vec<String>,
}

fn lower(list: &[String]) -> Vec<String> {
    list.iter().map(|s| s.to_ascii_lowercase()).collect()
}

fn any_in(have: &[String], want: &[String]) -> bool {
    let want = lower(want);
    have.iter().any(|h| want.contains(h))
}

/// The role a GitHub person gets, or `None` if no rule lets them in.
pub fn decide_github(cfg: &Github, who: &GithubIdentity) -> Option<Role> {
    let admin =
        cfg.admin_users.iter().any(|u| u.id == who.id) || any_in(&who.teams, &cfg.admin_teams);
    if admin {
        return Some(Role::Admin);
    }
    let allowed = cfg.users.iter().any(|u| u.id == who.id)
        || any_in(&who.orgs, &cfg.orgs)
        || any_in(&who.teams, &cfg.teams);
    allowed.then_some(Role::Viewer)
}

/// The role a Microsoft person gets, given the groups (of those the rules
/// name) they are in.
pub fn decide_microsoft(cfg: &Microsoft, member_of: &[String]) -> Option<Role> {
    let member_of: Vec<String> = member_of.iter().map(|g| g.to_ascii_lowercase()).collect();
    if any_in(&member_of, &cfg.admin_groups) {
        return Some(Role::Admin);
    }
    any_in(&member_of, &cfg.groups).then_some(Role::Viewer)
}

impl OAuth {
    pub fn open(dir: Option<&Path>) -> Result<Self> {
        let config = match dir.map(|d| d.join(OAUTH_FILE)) {
            Some(p) => match std::fs::read_to_string(&p) {
                Ok(text) => toml::from_str(&text)
                    .with_context(|| format!("{} is not valid", p.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
                Err(e) => return Err(e).with_context(|| format!("cannot read {}", p.display())),
            },
            None => Config::default(),
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("deskwatch-bridge/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self(Arc::new(Inner {
            dir: dir.map(Path::to_path_buf),
            config: RwLock::new(config),
            endpoints: RwLock::new(Endpoints::default()),
            pending: Mutex::default(),
            http,
        })))
    }

    #[cfg(test)]
    pub fn set_endpoints(&self, e: Endpoints) {
        *self.0.endpoints.write().unwrap() = e;
    }

    pub fn config(&self) -> Config {
        self.0
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn secret_path(&self, p: Provider) -> Option<PathBuf> {
        Some(
            self.0
                .dir
                .as_ref()?
                .join("credentials")
                .join(format!("oauth-{}", p.id())),
        )
    }

    pub fn secret_set(&self, p: Provider) -> bool {
        self.secret_path(p).is_some_and(|f| f.is_file())
    }

    fn read_secret(&self, p: Provider) -> Result<String> {
        let path = self.secret_path(p).context("no data folder")?;
        Ok(std::fs::read_to_string(path)
            .context("the client secret is not set")?
            .trim()
            .to_string())
    }

    /// Providers that are fully set up (public address, client id, secret).
    pub fn enabled(&self) -> Vec<Provider> {
        let c = self.config();
        let mut out = Vec::new();
        if c.public_url.is_some() {
            if c.github.is_some() && self.secret_set(Provider::Github) {
                out.push(Provider::Github);
            }
            if c.microsoft.is_some() && self.secret_set(Provider::Microsoft) {
                out.push(Provider::Microsoft);
            }
        }
        out
    }

    /// Save new settings. A `secret` of `Some(..)` replaces the stored client
    /// secret of that provider; a provider that is switched off loses its secret.
    pub fn save(
        &self,
        config: Config,
        github_secret: Option<&str>,
        microsoft_secret: Option<&str>,
    ) -> Result<()> {
        config.validate()?;
        let dir = self.0.dir.as_ref().context("login needs a data folder")?;
        for (provider, on, secret) in [
            (Provider::Github, config.github.is_some(), github_secret),
            (
                Provider::Microsoft,
                config.microsoft.is_some(),
                microsoft_secret,
            ),
        ] {
            let path = self.secret_path(provider).expect("dir is set");
            if on {
                match secret.map(str::trim).filter(|s| !s.is_empty()) {
                    Some(s) => crate::store::write_atomic(&path, s.as_bytes(), 0o600)?,
                    None if !path.is_file() => {
                        bail!("{} needs a client secret", provider.label())
                    }
                    None => {}
                }
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
        let text = toml::to_string_pretty(&config)?;
        crate::store::write_atomic(&dir.join(OAUTH_FILE), text.as_bytes(), 0o600)?;
        *self.0.config.write().unwrap_or_else(|e| e.into_inner()) = config;
        Ok(())
    }

    /// The numeric id of a GitHub login, so a rule keeps working when the
    /// login is renamed (and a re-used login does not inherit it).
    pub async fn resolve_github_user(&self, login: &str, known: &[GhUser]) -> Result<GhUser> {
        let login = login.trim().trim_start_matches('@');
        if !valid_name(login) {
            bail!("\"{login}\" is not a GitHub user name");
        }
        if let Some(u) = known.iter().find(|u| u.login.eq_ignore_ascii_case(login)) {
            return Ok(u.clone());
        }
        let api = self
            .0
            .endpoints
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .github_api
            .clone();
        let answer = self
            .0
            .http
            .get(format!("{api}/users/{login}"))
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .with_context(|| format!("cannot ask GitHub about {login}"))?;
        if answer.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("GitHub has no user \"{login}\"");
        }
        let json: Value = answer.error_for_status()?.json().await?;
        Ok(GhUser {
            login: json["login"].as_str().unwrap_or(login).to_string(),
            id: json["id"].as_u64().context("GitHub sent no user id")?,
        })
    }

    fn redirect_uri(&self, p: Provider) -> Result<String> {
        let base = self.config().public_url.context("no public address")?;
        Ok(format!(
            "{}/auth/callback/{}",
            base.trim_end_matches('/'),
            p.id()
        ))
    }

    /// Start a sign-in: the address to send the browser to, and the state
    /// that goes in the cookie.
    pub fn begin(&self, p: Provider, next: &str) -> Result<(String, String)> {
        let cfg = self.config();
        let ep = self
            .0
            .endpoints
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let state = random_url_safe(24);
        let verifier = random_url_safe(48);
        let nonce = random_url_safe(16);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let redirect = self.redirect_uri(p)?;
        let url = match p {
            Provider::Github => {
                let g = cfg.github.context("GitHub is not set up")?;
                Url::parse_with_params(
                    &ep.github_authorize,
                    &[
                        ("client_id", g.client_id.as_str()),
                        ("redirect_uri", &redirect),
                        ("scope", "read:user read:org"),
                        ("state", &state),
                        ("code_challenge", &challenge),
                        ("code_challenge_method", "S256"),
                    ],
                )?
            }
            Provider::Microsoft => {
                let m = cfg.microsoft.context("Microsoft is not set up")?;
                Url::parse_with_params(
                    &format!("{}/{}/oauth2/v2.0/authorize", ep.ms_login, m.tenant),
                    &[
                        ("client_id", m.client_id.as_str()),
                        ("response_type", "code"),
                        ("redirect_uri", &redirect),
                        ("response_mode", "query"),
                        ("scope", "openid profile User.Read"),
                        ("state", &state),
                        ("nonce", &nonce),
                        ("code_challenge", &challenge),
                        ("code_challenge_method", "S256"),
                    ],
                )?
            }
        };
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        pending.retain(|_, v| now.duration_since(v.at) < PENDING_TTL);
        if pending.len() >= MAX_PENDING {
            bail!("too many sign-ins in progress, try again in a minute");
        }
        pending.insert(
            state.clone(),
            Pending {
                provider: p,
                verifier,
                nonce,
                next: next.to_string(),
                at: now,
            },
        );
        Ok((url.to_string(), state))
    }

    /// Finish a sign-in. `cookie_state` is what the browser sent back in its
    /// cookie; it must equal the `state` of the address. Returns the role, a
    /// label for the page header and where to go next.
    pub async fn finish(
        &self,
        p: Provider,
        code: &str,
        state: &str,
        cookie_state: Option<&str>,
    ) -> Result<(Role, String, String), Failure> {
        let pending = {
            let mut map = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(state)
        };
        let pending = pending
            .filter(|x| x.provider == p && x.at.elapsed() < PENDING_TTL)
            .ok_or(Failure::Expired)?;
        if cookie_state != Some(state) {
            return Err(Failure::Expired);
        }
        let cfg = self.config();
        let ep = self
            .0
            .endpoints
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let redirect = self.redirect_uri(p)?;
        let secret = self.read_secret(p)?;
        let (role, label) = match p {
            Provider::Github => {
                let g = cfg.github.ok_or("GitHub is not set up")?;
                self.github(&g, &ep, &secret, code, &redirect, &pending.verifier)
                    .await?
            }
            Provider::Microsoft => {
                let m = cfg.microsoft.ok_or("Microsoft is not set up")?;
                self.microsoft(&m, &ep, &secret, code, &redirect, &pending)
                    .await?
            }
        };
        Ok((role, label, pending.next))
    }

    async fn github(
        &self,
        cfg: &Github,
        ep: &Endpoints,
        secret: &str,
        code: &str,
        redirect: &str,
        verifier: &str,
    ) -> Result<(Role, String), Failure> {
        let token: Value = self
            .0
            .http
            .post(&ep.github_token)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", cfg.client_id.as_str()),
                ("client_secret", secret),
                ("code", code),
                ("redirect_uri", redirect),
                ("code_verifier", verifier),
            ])
            .send()
            .await?
            .json()
            .await?;
        let access = token["access_token"]
            .as_str()
            .ok_or_else(|| Failure::Failed(format!("no token: {}", token["error"])))?;
        let get = |path: &str| {
            let url = format!("{}{path}", ep.github_api);
            let req = self
                .0
                .http
                .get(url)
                .bearer_auth(access)
                .header("Accept", "application/vnd.github+json");
            async move { req.send().await?.error_for_status()?.json::<Value>().await }
        };
        let user = get("/user").await?;
        let id = user["id"].as_u64().ok_or("GitHub sent no user id")?;
        let login = user["login"].as_str().unwrap_or("?").to_string();
        // The lists need the read:org scope; if GitHub refuses them (the
        // organisation has not approved the app) they count as empty.
        let orgs = get("/user/orgs?per_page=100").await.unwrap_or(Value::Null);
        let teams = get("/user/teams?per_page=100").await.unwrap_or(Value::Null);
        let who = GithubIdentity {
            id,
            orgs: orgs
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| Some(o["login"].as_str()?.to_ascii_lowercase()))
                .collect(),
            teams: teams
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| {
                    Some(
                        format!(
                            "{}/{}",
                            t["organization"]["login"].as_str()?,
                            t["slug"].as_str()?
                        )
                        .to_ascii_lowercase(),
                    )
                })
                .collect(),
        };
        let role = decide_github(cfg, &who).ok_or(Failure::NotAllowed)?;
        Ok((role, format!("{login} (GitHub)")))
    }

    async fn microsoft(
        &self,
        cfg: &Microsoft,
        ep: &Endpoints,
        secret: &str,
        code: &str,
        redirect: &str,
        pending: &Pending,
    ) -> Result<(Role, String), Failure> {
        let token: Value = self
            .0
            .http
            .post(format!("{}/{}/oauth2/v2.0/token", ep.ms_login, cfg.tenant))
            .form(&[
                ("client_id", cfg.client_id.as_str()),
                ("client_secret", secret),
                ("code", code),
                ("redirect_uri", redirect),
                ("grant_type", "authorization_code"),
                ("code_verifier", pending.verifier.as_str()),
                ("scope", "openid profile User.Read"),
            ])
            .send()
            .await?
            .json()
            .await?;
        let id_token = token["id_token"]
            .as_str()
            .ok_or_else(|| Failure::Failed(format!("no id token: {}", token["error"])))?;
        let access = token["access_token"].as_str().ok_or("no access token")?;
        let claims = id_token_claims(id_token)?;
        check_claims(&claims, cfg, &pending.nonce, unix_now())?;
        let name = claims["name"]
            .as_str()
            .or(claims["preferred_username"].as_str())
            .unwrap_or("?")
            .to_string();
        let wanted: Vec<&String> = cfg.groups.iter().chain(&cfg.admin_groups).collect();
        let member_of: Vec<String> = if wanted.is_empty() {
            Vec::new()
        } else {
            let answer: Value = self
                .0
                .http
                .post(format!("{}/v1.0/me/checkMemberGroups", ep.ms_graph))
                .bearer_auth(access)
                .json(&json!({ "groupIds": wanted }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            answer["value"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|g| g.as_str().map(str::to_string))
                .collect()
        };
        let role = decide_microsoft(cfg, &member_of).ok_or(Failure::NotAllowed)?;
        Ok((role, format!("{name} (Microsoft)")))
    }
}

fn unix_now() -> u64 {
    crate::source::unix_now()
}

/// The claims of an ID token (not its signature, see the module comment).
pub fn id_token_claims(token: &str) -> Result<Value, Failure> {
    let payload = token.split('.').nth(1).ok_or("the ID token is malformed")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| "the ID token is malformed")?;
    Ok(serde_json::from_slice(&bytes).map_err(|_| "the ID token is malformed")?)
}

/// Issuer, audience, tenant, expiry and nonce of a Microsoft ID token.
pub fn check_claims(claims: &Value, cfg: &Microsoft, nonce: &str, now: u64) -> Result<(), Failure> {
    let bad = |what: &str| Err(Failure::Failed(format!("the ID token {what}")));
    if claims["aud"].as_str() != Some(cfg.client_id.as_str()) {
        return bad("is for another application");
    }
    if !claims["tid"]
        .as_str()
        .is_some_and(|t| t.eq_ignore_ascii_case(&cfg.tenant))
    {
        return bad("is from another directory");
    }
    let expected_iss = format!(
        "https://login.microsoftonline.com/{}/v2.0",
        cfg.tenant.to_ascii_lowercase()
    );
    if !claims["iss"]
        .as_str()
        .is_some_and(|i| i.to_ascii_lowercase() == expected_iss)
    {
        return bad("has the wrong issuer");
    }
    if claims["exp"].as_u64().is_none_or(|e| e + 60 < now) {
        return bad("has expired");
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return bad("has the wrong nonce");
    }
    if claims["oid"].as_str().is_none_or(str::is_empty) {
        return bad("has no object id");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
