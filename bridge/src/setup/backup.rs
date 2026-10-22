//! Backup of the settings in the data folder: one file to download, one to
//! restore (docs/backup.md).
//!
//! The file is JSON and holds the text of `sources.toml` (sources, broker,
//! alerts, dashboard and update settings), `oauth.toml` (sign-in rules, no
//! secrets) and `layouts.json` (each person's own dashboard layout).
//!
//! Secrets are not in it unless the person locks the file with a password.
//! Then the stored tokens and client secrets (`credentials/`) are encrypted
//! with XChaCha20-Poly1305, with a key made from the password by Argon2id.
//! Passwords, API keys and sessions (`auth.toml`) are never exported.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

use crate::store::Store;

const FORMAT: &str = "deskwatch-backup";
const VERSION: u32 = 1;
/// Binds the ciphertext to this file format.
const AAD: &[u8] = b"deskwatch-backup-v1";
const MIN_PASSWORD: usize = 12;
/// Cost of the key derivation when locking, and the most a file may ask for
/// when opening (so a crafted file cannot use all of the memory).
const KDF_MEMORY_KIB: u32 = 65536;
const KDF_TIME: u32 = 3;
const MAX_MEMORY_KIB: u32 = 262_144;
const MAX_TIME: u32 = 10;
pub const MIN_PASSWORD_LEN: usize = MIN_PASSWORD;

const OAUTH_FILE: &str = "oauth.toml";
const LAYOUTS_FILE: &str = "layouts.json";

/// The file as written and read.
#[derive(Debug, Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    /// The DeskWatch version that made it.
    app: String,
    /// Unix seconds.
    created: u64,
    sources: String,
    #[serde(default)]
    oauth: Option<String>,
    #[serde(default)]
    layouts: Option<String>,
    /// Present only in a file locked with a password.
    #[serde(default)]
    sealed: Option<Sealed>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Sealed {
    kdf: String,
    memory_kib: u32,
    time: u32,
    salt: String,
    nonce: String,
    /// The encrypted JSON of `BTreeMap<name, value>`.
    data: String,
}

/// What a backup holds, after it was opened (and decrypted).
#[derive(Debug, Default)]
pub struct Backup {
    pub created: u64,
    pub app: String,
    pub sources: String,
    pub oauth: Option<String>,
    pub layouts: Option<String>,
    pub secrets: BTreeMap<String, String>,
    /// Locked with a password: opening it needed one.
    pub locked: bool,
}

/// Why a backup could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// It is locked and no password was given.
    NeedsPassword,
    WrongPassword,
    Invalid(anyhow::Error),
}

impl From<anyhow::Error> for OpenError {
    fn from(e: anyhow::Error) -> Self {
        Self::Invalid(e)
    }
}

fn argon(memory_kib: u32, time: u32) -> Result<Argon2<'static>> {
    let params = Params::new(memory_kib, time, 1, Some(32))
        .map_err(|e| anyhow::anyhow!("bad key settings: {e}"))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

fn key(password: &str, salt: &[u8], memory_kib: u32, time: u32) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    argon(memory_kib, time)?
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow::anyhow!("cannot derive the key: {e}"))?;
    Ok(out)
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(bytes)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Make the backup file from what is in the data folder. With a password the
/// stored secrets go in, encrypted; without one nothing secret is included.
/// Blocking when a password is given (Argon2): call from `spawn_blocking`.
pub fn export(store: &Store, password: Option<&str>) -> Result<Vec<u8>> {
    let mut file = File {
        format: FORMAT.into(),
        version: VERSION,
        app: env!("CARGO_PKG_VERSION").into(),
        created: unix_now(),
        sources: store.read_sources()?,
        oauth: store.read_data_file(OAUTH_FILE)?,
        layouts: store.read_data_file(LAYOUTS_FILE)?,
        sealed: None,
    };
    if let Some(password) = password {
        ensure!(
            password.chars().count() >= MIN_PASSWORD,
            "the password needs at least {MIN_PASSWORD} characters"
        );
        let mut secrets = BTreeMap::new();
        for name in store.list_secrets() {
            secrets.insert(name.clone(), store.read_secret(&name)?);
        }
        let salt = random::<16>()?;
        let nonce = random::<24>()?;
        let key = key(password, &salt, KDF_MEMORY_KIB, KDF_TIME)?;
        let cipher = XChaCha20Poly1305::new((&key).into());
        let plain = serde_json::to_vec(&secrets)?;
        let data = cipher
            .encrypt(
                &XNonce::try_from(nonce.as_slice()).map_err(|_| anyhow::anyhow!("bad nonce"))?,
                Payload {
                    msg: &plain,
                    aad: AAD,
                },
            )
            .map_err(|_| anyhow::anyhow!("cannot encrypt the secrets"))?;
        file.sealed = Some(Sealed {
            kdf: "argon2id".into(),
            memory_kib: KDF_MEMORY_KIB,
            time: KDF_TIME,
            salt: B64.encode(salt),
            nonce: B64.encode(nonce),
            data: B64.encode(data),
        });
    }
    Ok(serde_json::to_vec_pretty(&file)?)
}

/// Read a backup file. A locked one needs the password (Argon2, blocking).
pub fn open(bytes: &[u8], password: Option<&str>) -> Result<Backup, OpenError> {
    let file: File = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("this is not a DeskWatch backup file"))?;
    if file.format != FORMAT {
        return Err(anyhow::anyhow!("this is not a DeskWatch backup file").into());
    }
    if file.version > VERSION {
        return Err(anyhow::anyhow!(
            "this backup comes from a newer DeskWatch (file version {}); update this install first",
            file.version
        )
        .into());
    }
    let mut backup = Backup {
        created: file.created,
        app: file.app,
        sources: file.sources,
        oauth: file.oauth,
        layouts: file.layouts,
        secrets: BTreeMap::new(),
        locked: file.sealed.is_some(),
    };
    if let Some(sealed) = file.sealed {
        let password = password
            .filter(|p| !p.is_empty())
            .ok_or(OpenError::NeedsPassword)?;
        if sealed.kdf != "argon2id" || sealed.memory_kib > MAX_MEMORY_KIB || sealed.time > MAX_TIME
        {
            return Err(anyhow::anyhow!(
                "the backup uses key settings this version does not accept"
            )
            .into());
        }
        let salt = B64.decode(&sealed.salt).context("the backup is damaged")?;
        let nonce = B64.decode(&sealed.nonce).context("the backup is damaged")?;
        let data = B64.decode(&sealed.data).context("the backup is damaged")?;
        if nonce.len() != 24 || salt.len() < 8 {
            return Err(anyhow::anyhow!("the backup is damaged").into());
        }
        let key = key(password, &salt, sealed.memory_kib, sealed.time)?;
        let plain = XChaCha20Poly1305::new((&key).into())
            .decrypt(
                &XNonce::try_from(nonce.as_slice()).map_err(|_| anyhow::anyhow!("bad nonce"))?,
                Payload {
                    msg: &data,
                    aad: AAD,
                },
            )
            // The tag does not match: wrong password, or the file was changed.
            .map_err(|_| OpenError::WrongPassword)?;
        backup.secrets = serde_json::from_slice(&plain).context("the backup is damaged")?;
    }
    Ok(backup)
}

/// What a backup contains, in words for the pop-up. Checks every part on the
/// way, so a backup that would not load is refused before anything changes.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub app: String,
    pub created: u64,
    pub locked: bool,
    pub sources: Vec<(String, usize)>,
    pub mqtt: bool,
    pub dashboard: bool,
    pub alerts: bool,
    pub layouts: Option<usize>,
    pub providers: Vec<&'static str>,
    pub secrets: usize,
}

/// Check the backup against `base` (the config `bridge.toml` gives) and
/// describe it.
pub fn check(backup: &Backup, base: &crate::config::Config) -> Result<Summary> {
    let config = base
        .clone()
        .with_overlay(&backup.sources)
        .context("the sources in the backup are not valid")?;
    let table: toml::Table = toml::from_str(&backup.sources).context("sources are not valid")?;
    let providers = match &backup.oauth {
        Some(text) => crate::auth::oauth::check_text(text)?,
        None => Vec::new(),
    };
    let layouts = match &backup.layouts {
        Some(text) => Some(crate::auth::check_layouts_text(text)?),
        None => None,
    };
    for name in backup.secrets.keys() {
        crate::source::valid_name(name)
            .then_some(())
            .with_context(|| format!("the backup holds a secret with a bad name: {name:?}"))?;
    }
    // Only what the backup itself sets, not what bridge.toml already has.
    let own = super::source_ids(base);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for id in super::source_ids(&config) {
        if own.contains(&id) {
            continue;
        }
        let kind = id.split(':').next().unwrap_or_default().to_string();
        *kinds.entry(kind).or_default() += 1;
    }
    Ok(Summary {
        app: backup.app.clone(),
        created: backup.created,
        locked: backup.locked,
        sources: kinds.into_iter().collect(),
        mqtt: table.contains_key("mqtt"),
        dashboard: table.contains_key("kiosk"),
        alerts: table.contains_key("alerts"),
        layouts,
        providers,
        secrets: backup.secrets.len(),
    })
}

/// Write a checked backup into the data folder. The bridge needs a restart
/// afterwards to use it. Files the backup does not hold are left as they are.
pub fn restore(store: &Store, backup: &Backup) -> Result<()> {
    if backup.sources.trim().is_empty() && backup.oauth.is_none() && backup.layouts.is_none() {
        bail!("the backup is empty");
    }
    // Secrets first: sources that name them find them as soon as they load.
    for (name, value) in &backup.secrets {
        store.write_secret(name, value)?;
    }
    store.write_sources(&backup.sources)?;
    if let Some(text) = &backup.oauth {
        store.write_data_file(OAUTH_FILE, text)?;
    }
    if let Some(text) = &backup.layouts {
        store.write_data_file(LAYOUTS_FILE, text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
