//! The data folder: the one place the bridge writes files itself.
//!
//! `bridge.toml` and the credential files that systemd or Docker mount stay
//! hand-managed and read-only. When a data folder is configured (`[data] dir`
//! or `DESKWATCH_DATA_DIR`), it holds two more things, which a setup page can
//! change later (docs/data-dir.md):
//!
//! - `sources.toml`: `[[source.*]]` blocks in the same format as `bridge.toml`,
//!   added to the ones there;
//! - `credentials/<name>`: one secret per file, mode 0600, named by a plain
//!   `token_file = "<name>"` just like the mounted ones. A mounted credential
//!   with the same name wins.
//!
//! Every write goes to a temporary file in the same folder and is renamed
//! over the target, so a crash or a full disk never leaves half a file. Secrets
//! are write-only: nothing here reads one back for display, and nothing logs one.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

use crate::source::valid_name;

const SOURCES_FILE: &str = "sources.toml";
const CREDENTIALS_DIR: &str = "credentials";

/// The data folder's credentials, which `source::load_secret` falls back to.
/// Set once at startup.
static CREDENTIALS: OnceLock<PathBuf> = OnceLock::new();

/// The folder `load_secret` also looks in, if a data folder is in use.
pub fn credentials_dir() -> Option<&'static Path> {
    CREDENTIALS.get().map(PathBuf::as_path)
}

/// Handle on the data folder.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Use `dir`, creating it and `credentials/` if needed (the credentials
    /// folder private to the bridge's user), and prove it is writable. A
    /// folder the bridge cannot write to stops it at startup with the reason,
    /// instead of failing later on the first save.
    pub fn open(dir: &Path) -> Result<Self> {
        let store = Self {
            dir: dir.to_path_buf(),
        };
        create_private_dir(&store.credentials())
            .with_context(|| format!("cannot create the data folder {}", dir.display()))?;
        let probe = store.dir.join(".write-test");
        std::fs::write(&probe, b"").with_context(|| {
            format!(
                "the data folder {} is not writable by the bridge (in Docker: the volume must be \
                 owned by uid 10001, see docs/data-dir.md)",
                dir.display()
            )
        })?;
        let _ = std::fs::remove_file(&probe);
        // A second open() in one process (tests) keeps the first folder; the
        // bridge opens exactly one.
        let _ = CREDENTIALS.set(store.credentials());
        Ok(store)
    }

    fn credentials(&self) -> PathBuf {
        self.dir.join(CREDENTIALS_DIR)
    }

    /// The text of `sources.toml`, or an empty overlay when there is none yet.
    pub fn read_sources(&self) -> Result<String> {
        let path = self.dir.join(SOURCES_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Replace `sources.toml`. The caller has already checked the text with
    /// `Config::with_overlay`; this only writes it safely.
    pub fn write_sources(&self, text: &str) -> Result<()> {
        write_atomic(&self.dir.join(SOURCES_FILE), text.as_bytes(), 0o644)
    }

    /// Store (or replace) a secret under a plain name. Surrounding whitespace
    /// is removed, like `load_secret` does when reading.
    pub fn write_secret(&self, name: &str, value: &str) -> Result<()> {
        check_name(name)?;
        let value = value.trim();
        anyhow::ensure!(!value.is_empty(), "the secret {name} is empty");
        write_atomic(&self.credentials().join(name), value.as_bytes(), 0o600)
            .with_context(|| format!("cannot store the secret {name}"))
    }

    /// Is there a secret with this name? Says nothing about its value.
    pub fn has_secret(&self, name: &str) -> bool {
        check_name(name).is_ok() && self.credentials().join(name).is_file()
    }

    /// The names of the stored secrets, sorted. Never their values.
    pub fn list_secrets(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.credentials())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| check_name(n).is_ok())
            .collect();
        names.sort();
        names
    }

    /// Delete a secret. A missing one is fine.
    pub fn remove_secret(&self, name: &str) -> Result<()> {
        check_name(name)?;
        match std::fs::remove_file(self.credentials().join(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("cannot remove the secret {name}")),
        }
    }
}

fn check_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        valid_name(name),
        "a secret name is letters, digits, '-' and '_' (got {name:?})"
    );
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// Write `bytes` to a temporary file next to `path` (created with `mode`
/// from the start, so the content is never readable by others, even for a
/// moment), flush it to disk, then rename it over `path`.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests;
