//! Shared source framework: what every data source has in common.
//!
//! A source is one configured instance of an adapter, such as
//! `[[source.gitea]] name = "home"`. It runs as its own task, keeps whatever
//! state it needs, and reports to the main loop through a `FactSink`:
//!
//! - fact updates (`ci::Update`): running jobs, runs, open PRs, notices;
//! - host stats (`fleet::StatsReport`): machines and what is down on them;
//! - its health: working, auth failed, or unreachable.
//!
//! The main loop owns the fact store and the composer, so there are no locks.
//! Sources never decide what is on screen.
//!
//! This module also holds the helpers every source config shares: credential
//! files (`token_file`), the `interrupt` switch and the `alias` map.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::ci::Update;
use crate::fleet::StatsReport;

/// Environment variable systemd sets to the folder holding `LoadCredential=` files.
pub const CREDENTIALS_DIR_ENV: &str = "CREDENTIALS_DIRECTORY";

// ---------------------------------------------------------------------------
// Identity and health
// ---------------------------------------------------------------------------

/// Which source instance something came from: adapter type plus config name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId {
    /// Adapter type, such as `gitea`. Also the `source` field on panel pages.
    pub kind: &'static str,
    /// Instance name from the config, such as `home`.
    pub name: String,
}

impl SourceId {
    pub fn new(kind: &'static str, name: &str) -> Self {
        Self {
            kind,
            name: name.to_string(),
        }
    }
}

/// Printed as `gitea:home`; also the prefix of every fact key the source writes.
impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.name)
    }
}

/// Is a source working? Anything but `Ok` counts in the `warn` badge and
/// greys out the pages built from CI facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    Ok,
    /// The service answered but refused the token (HTTP 401 or 403), for
    /// example an expired PAT.
    AuthFailed,
    /// No answer, a timeout, or a server error.
    Unreachable,
}

/// A service answered with its sign-in page instead of data (Azure DevOps
/// does this for a missing, expired or unauthorised token). Counts as a
/// refused token.
#[derive(Debug)]
pub struct AuthRefused;

impl fmt::Display for AuthRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the service answered with its sign-in page, so the token was not accepted")
    }
}

impl std::error::Error for AuthRefused {}

impl Health {
    /// Classify a failed request. Works for any error chain that contains a
    /// `reqwest::Error` or an `AuthRefused`; everything else counts as
    /// unreachable.
    pub fn from_error(err: &anyhow::Error) -> Self {
        if err.chain().any(|e| e.is::<AuthRefused>()) {
            return Health::AuthFailed;
        }
        let status = err
            .chain()
            .find_map(|e| e.downcast_ref::<reqwest::Error>())
            .and_then(reqwest::Error::status);
        match status {
            Some(s)
                if s == reqwest::StatusCode::UNAUTHORIZED
                    || s == reqwest::StatusCode::FORBIDDEN =>
            {
                Health::AuthFailed
            }
            _ => Health::Unreachable,
        }
    }
}

/// Latest health of every source that has reported one.
#[derive(Debug, Default)]
pub struct HealthBoard {
    sources: BTreeMap<SourceId, Health>,
}

impl HealthBoard {
    /// Record a source's health. Logs only changes, so a source that stays
    /// down does not flood the log.
    pub fn set(&mut self, source: &SourceId, health: Health) {
        let before = self.sources.insert(source.clone(), health);
        if before != Some(health) {
            match health {
                Health::Ok => info!(%source, "source is working"),
                _ => warn!(%source, ?health, "source has a problem"),
            }
        }
    }

    /// Number of sources with a problem, for the `warn` badge.
    pub fn problems(&self) -> u32 {
        self.sources.values().filter(|h| **h != Health::Ok).count() as u32
    }

    /// True when every source is working.
    pub fn all_ok(&self) -> bool {
        self.problems() == 0
    }
}

// ---------------------------------------------------------------------------
// Talking to the main loop
// ---------------------------------------------------------------------------

/// A message from a source to the main loop.
#[derive(Debug)]
pub struct SourceMsg {
    pub source: SourceId,
    pub body: SourceBody,
}

#[derive(Debug)]
pub enum SourceBody {
    Facts(Vec<Update>),
    /// Host stats and what is down, replacing the source's previous report.
    Stats(StatsReport),
    Health(Health),
}

/// A source's handle for reporting to the main loop.
#[derive(Debug, Clone)]
pub struct FactSink {
    source: SourceId,
    tx: mpsc::Sender<SourceMsg>,
}

impl FactSink {
    pub fn new(source: SourceId, tx: mpsc::Sender<SourceMsg>) -> Self {
        Self { source, tx }
    }

    pub fn source(&self) -> &SourceId {
        &self.source
    }

    /// Send fact updates. Returns false once the main loop has stopped.
    pub async fn facts(&self, updates: Vec<Update>) -> bool {
        if updates.is_empty() {
            return true;
        }
        self.send(SourceBody::Facts(updates)).await
    }

    /// Send host stats. Returns false once the main loop has stopped.
    pub async fn stats(&self, report: StatsReport) -> bool {
        self.send(SourceBody::Stats(report)).await
    }

    /// Report health. Returns false once the main loop has stopped.
    pub async fn health(&self, health: Health) -> bool {
        self.send(SourceBody::Health(health)).await
    }

    async fn send(&self, body: SourceBody) -> bool {
        let msg = SourceMsg {
            source: self.source.clone(),
            body,
        };
        self.tx.send(msg).await.is_ok()
    }
}

/// One configured source instance. Built from its config block (which is
/// where secrets are loaded and webhook routes registered, so mistakes stop
/// the bridge at startup), then run as its own task.
pub trait Source: Send + 'static {
    fn id(&self) -> SourceId;

    /// Run forever: poll, or wait for pushed events, and report to `sink`.
    fn run(self, sink: FactSink) -> impl Future<Output = Result<()>> + Send;
}

/// Start a source as a task. Errors are logged and mark the source unreachable.
pub fn spawn(source: impl Source, tx: mpsc::Sender<SourceMsg>) {
    let sink = FactSink::new(source.id(), tx);
    tokio::spawn(async move {
        let id = sink.source().clone();
        if let Err(err) = source.run(sink.clone()).await {
            warn!(source = %id, "source stopped: {err:#}");
            sink.health(Health::Unreachable).await;
        }
    });
}

/// The current time in Unix seconds, the clock every fact uses.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Shared config pieces
// ---------------------------------------------------------------------------

/// A secret read from a credential file. `Debug` prints `***` so it can never
/// end up in a log line.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// Read a secret named in the config, such as `token_file = "gitea-token"`.
///
/// A plain name is a systemd credential: `$CREDENTIALS_DIRECTORY/<name>`, put
/// there by `LoadCredential=` in the unit. An absolute path is read as is,
/// which is handy when running the bridge by hand during development.
/// Surrounding whitespace (the trailing newline of `echo > file`) is removed.
pub fn load_secret(name: &str) -> Result<Secret> {
    let path = credential_path(
        name,
        std::env::var_os(CREDENTIALS_DIR_ENV)
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    )?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read credential {name} at {}", path.display()))?;
    let value = text.trim();
    anyhow::ensure!(!value.is_empty(), "credential {name} is empty");
    Ok(Secret::new(value))
}

fn credential_path(name: &str, credentials_dir: Option<PathBuf>) -> Result<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    anyhow::ensure!(
        !name.is_empty() && !name.contains('/'),
        "credential name {name:?} must be a plain name or an absolute path"
    );
    let dir = credentials_dir.with_context(|| {
        format!(
            "credential {name} needs {CREDENTIALS_DIR_ENV} (set LoadCredential={name}:... in the \
             systemd unit) or an absolute path"
        )
    })?;
    Ok(dir.join(name))
}

/// The `interrupt` setting: may this source's runs take over the screen?
/// Either `true`/`false` for everything, or a list of repositories or
/// pipelines that may.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Interrupt {
    All(bool),
    Only(Vec<String>),
}

impl Interrupt {
    pub fn allows(&self, name: &str) -> bool {
        match self {
            Interrupt::All(all) => *all,
            Interrupt::Only(names) => names.iter().any(|n| n == name),
        }
    }
}

/// Short label for a repository or pipeline on the panel: its `alias` if one
/// is set, otherwise the last part of the name (`owner/repo` shows `repo`).
pub fn label(alias: &BTreeMap<String, String>, full_name: &str) -> String {
    match alias.get(full_name) {
        Some(short) => short.clone(),
        None => full_name
            .rsplit('/')
            .next()
            .unwrap_or(full_name)
            .to_string(),
    }
}

/// Instance names end up in fact keys and webhook URLs, so keep them simple.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_prints() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "***");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn credential_paths() {
        let dir = Some(PathBuf::from("/run/credentials/x"));
        assert_eq!(
            credential_path("gitea-token", dir.clone()).unwrap(),
            PathBuf::from("/run/credentials/x/gitea-token")
        );
        assert_eq!(
            credential_path("/etc/deskwatch/t", None).unwrap(),
            PathBuf::from("/etc/deskwatch/t")
        );
        assert!(credential_path("gitea-token", None).is_err());
        assert!(credential_path("../escape", dir.clone()).is_err());
        assert!(credential_path("", dir).is_err());
    }

    #[test]
    fn load_secret_trims_and_rejects_empty() {
        let dir = std::env::temp_dir().join(format!("deskwatch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");
        std::fs::write(&file, "abc123\n").unwrap();
        assert_eq!(
            load_secret(file.to_str().unwrap()).unwrap().expose(),
            "abc123"
        );
        std::fs::write(&file, "\n").unwrap();
        assert!(load_secret(file.to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn interrupt_parses_bool_or_list() {
        #[derive(Deserialize)]
        struct T {
            interrupt: Interrupt,
        }
        let all: T = toml::from_str("interrupt = true").unwrap();
        assert!(all.interrupt.allows("anything"));
        let some: T = toml::from_str("interrupt = [\"me/a\"]").unwrap();
        assert!(some.interrupt.allows("me/a"));
        assert!(!some.interrupt.allows("me/b"));
    }

    #[test]
    fn labels_use_alias_or_short_name() {
        let alias = BTreeMap::from([("team/service-a".to_string(), "work A".to_string())]);
        assert_eq!(label(&alias, "team/service-a"), "work A");
        assert_eq!(label(&alias, "me/deskwatch"), "deskwatch");
    }

    #[test]
    fn health_board_counts_problems() {
        let mut board = HealthBoard::default();
        let home = SourceId::new("gitea", "home");
        let work = SourceId::new("github", "work");
        board.set(&home, Health::Ok);
        assert!(board.all_ok());
        board.set(&work, Health::AuthFailed);
        assert_eq!(board.problems(), 1);
        assert!(!board.all_ok());
        board.set(&work, Health::Ok);
        assert!(board.all_ok());
        assert_eq!(home.to_string(), "gitea:home");
    }

    #[test]
    fn names_are_simple() {
        assert!(valid_name("home_1-a"));
        assert!(!valid_name(""));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("with space"));
    }
}
