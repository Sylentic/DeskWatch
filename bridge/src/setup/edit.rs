//! Changing `sources.toml` for the setup page: pure functions from the
//! current file text and a requested change to the new file text plus the
//! secrets to store. Nothing here touches the disk or the network, so the
//! rules (which fields may be set, what a secret is called, what is kept) are
//! easy to test.
//!
//! The file is edited as a TOML table, so keys the page does not know about
//! (`interrupt`, `alias`, `runners`, `poll_s`, a hand-written host table, ...)
//! survive an edit. Comments in `sources.toml` do not; that file belongs to
//! the page (docs/data-dir.md).

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use toml::Table;

use crate::source::{Secret, valid_name};

/// What the page may set on a source of one kind.
pub struct Spec {
    pub kind: &'static str,
    /// Text fields.
    pub strings: &'static [&'static str],
    /// Lists of text.
    pub lists: &'static [&'static str],
    /// Yes/no fields.
    pub bools: &'static [&'static str],
    /// Secrets: (form key, config key that names the credential, required when adding).
    pub secrets: &'static [(&'static str, &'static str, bool)],
}

const SPECS: &[Spec] = &[
    Spec {
        kind: "github",
        strings: &["base_url"],
        lists: &["repos"],
        bools: &[],
        secrets: &[("token", "token_file", true)],
    },
    Spec {
        kind: "gitea",
        strings: &["base_url"],
        lists: &["repos"],
        bools: &[],
        secrets: &[
            ("token", "token_file", false),
            ("webhook_secret", "webhook_secret_file", true),
        ],
    },
    Spec {
        kind: "azure_devops",
        strings: &["organization", "base_url"],
        lists: &["projects"],
        bools: &["pull_requests"],
        secrets: &[("token", "token_file", true)],
    },
    Spec {
        kind: "prometheus",
        strings: &["url"],
        lists: &["hosts"],
        bools: &[],
        secrets: &[("token", "token_file", false)],
    },
];

pub fn spec(kind: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.kind == kind)
}

/// The new file and what to store with it.
#[derive(Debug)]
pub struct Plan {
    /// Full text of the new `sources.toml`.
    pub text: String,
    /// Secrets to write, by credential name. Replaces a file of the same name.
    pub secrets: Vec<(String, Secret)>,
}

/// The credential name a page-managed secret gets.
pub fn credential_name(kind: &str, name: &str, key: &str) -> String {
    format!("{kind}-{name}-{key}")
}

/// Parse `sources.toml` (empty text is an empty table).
fn parse(text: &str) -> Result<Table> {
    if text.trim().is_empty() {
        return Ok(Table::new());
    }
    toml::from_str(text).context("sources.toml is not valid TOML")
}

/// The array of sources of `kind`, created if missing.
fn sources<'a>(root: &'a mut Table, kind: &str) -> Result<&'a mut Vec<toml::Value>> {
    let source = root
        .entry("source")
        .or_insert_with(|| toml::Value::Table(Table::new()))
        .as_table_mut()
        .context("`source` in sources.toml is not a table")?;
    source
        .entry(kind)
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .with_context(|| format!("`source.{kind}` in sources.toml is not an array"))
}

fn position(list: &[toml::Value], name: &str) -> Option<usize> {
    list.iter()
        .position(|v| v.get("name").and_then(toml::Value::as_str) == Some(name))
}

/// Add (`create`) or change source `kind`/`name`. `fields` are the values from
/// the form, `secrets` the tokens typed in (empty or missing = keep).
pub fn upsert(
    current: &str,
    kind: &str,
    name: &str,
    create: bool,
    fields: &Map<String, Value>,
    secrets: &BTreeMap<String, String>,
) -> Result<Plan> {
    let spec = spec(kind).with_context(|| format!("unknown source type {kind:?}"))?;
    anyhow::ensure!(
        valid_name(name),
        "the name is letters, digits, '-' and '_' (got {name:?})"
    );
    for key in secrets.keys() {
        anyhow::ensure!(
            spec.secrets.iter().any(|(k, ..)| k == key),
            "{kind} has no secret called {key:?}"
        );
    }
    let mut root = parse(current)?;
    let list = sources(&mut root, kind)?;
    let found = position(list, name);
    let index = match (create, found) {
        (true, Some(_)) => bail!("a {kind} source named {name:?} is already in sources.toml"),
        (true, None) => {
            let mut entry = Table::new();
            entry.insert("name".into(), toml::Value::String(name.into()));
            list.push(toml::Value::Table(entry));
            list.len() - 1
        }
        (false, Some(i)) => i,
        (false, None) => bail!(
            "there is no {kind} source named {name:?} in the data folder \
             (sources from bridge.toml are changed in that file)"
        ),
    };
    let entry = list[index].as_table_mut().expect("entries are tables");

    for (key, value) in fields {
        if spec.strings.contains(&key.as_str()) {
            let text = value
                .as_str()
                .with_context(|| format!("{key} must be text"))?
                .trim();
            anyhow::ensure!(!text.is_empty(), "{key} must not be empty");
            entry.insert(key.clone(), toml::Value::String(text.into()));
        } else if spec.lists.contains(&key.as_str()) {
            let items = value
                .as_array()
                .with_context(|| format!("{key} must be a list"))?;
            let mut out = Vec::new();
            for item in items {
                let text = item
                    .as_str()
                    .with_context(|| format!("{key} must be a list of text"))?
                    .trim();
                if !text.is_empty() {
                    out.push(toml::Value::String(text.into()));
                }
            }
            if let Some(old) = entry.get(key).and_then(toml::Value::as_array)
                && old.iter().any(|v| !v.is_str())
            {
                bail!(
                    "{key} has entries with their own settings; change those in sources.toml by hand"
                );
            }
            entry.insert(key.clone(), toml::Value::Array(out));
        } else if spec.bools.contains(&key.as_str()) {
            let flag = value
                .as_bool()
                .with_context(|| format!("{key} must be true or false"))?;
            entry.insert(key.clone(), toml::Value::Boolean(flag));
        } else {
            bail!("{key:?} cannot be set from the setup page");
        }
    }

    let mut stored = Vec::new();
    for (key, field, required) in spec.secrets {
        let given = secrets
            .get(*key)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        match given {
            Some(value) => {
                let credential = credential_name(kind, name, key);
                anyhow::ensure!(
                    crate::source::credential_status(&credential)
                        != crate::source::CredentialStatus::Mounted,
                    "a mounted credential named {credential} already exists and would win over the \
                     new one; remove or rename that file"
                );
                entry.insert((*field).into(), toml::Value::String(credential.clone()));
                stored.push((credential, Secret::new(value)));
            }
            None if create && *required => bail!("the {key} is required"),
            None => {}
        }
    }
    Ok(Plan {
        text: toml::to_string(&root).context("cannot write sources.toml")?,
        secrets: stored,
    })
}

/// Remove source `kind`/`name`. Returns the new file text and the names of the
/// credentials the source used (the caller deletes those that are the page's own).
pub fn remove(current: &str, kind: &str, name: &str) -> Result<(String, Vec<String>)> {
    let spec = spec(kind).with_context(|| format!("unknown source type {kind:?}"))?;
    let mut root = parse(current)?;
    let list = sources(&mut root, kind)?;
    let index = position(list, name)
        .with_context(|| format!("there is no {kind} source named {name:?} in the data folder"))?;
    let removed = list.remove(index);
    let used = spec
        .secrets
        .iter()
        .filter_map(|(_, field, _)| removed.get(*field)?.as_str().map(str::to_string))
        .collect();
    // Do not leave empty containers behind.
    if list.is_empty()
        && let Some(source) = root.get_mut("source").and_then(toml::Value::as_table_mut)
    {
        source.remove(kind);
    }
    if root
        .get("source")
        .and_then(toml::Value::as_table)
        .is_some_and(Table::is_empty)
    {
        root.remove("source");
    }
    Ok((
        toml::to_string(&root).context("cannot write sources.toml")?,
        used,
    ))
}

#[cfg(test)]
mod tests;
