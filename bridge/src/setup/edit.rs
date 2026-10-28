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
    /// Ways to sign in, for a kind that has more than one (the `auth` field).
    /// The first is the default. Empty: the kind has one way, `strings` and `secrets` above.
    pub modes: &'static [Mode],
}

/// One way to sign in: the extra fields and secrets that go with it.
pub struct Mode {
    /// The value of `auth` in the config.
    pub name: &'static str,
    pub strings: &'static [&'static str],
    pub secrets: &'static [(&'static str, &'static str, bool)],
}

const SPECS: &[Spec] = &[
    Spec {
        kind: "github",
        strings: &["base_url", "container"],
        lists: &["repos"],
        bools: &[],
        secrets: &[],
        modes: &[
            Mode {
                name: "token",
                strings: &[],
                secrets: &[("token", "token_file", true)],
            },
            Mode {
                name: "app",
                strings: &["app_id", "installation_id"],
                secrets: &[("private_key", "private_key_file", true)],
            },
        ],
    },
    Spec {
        kind: "gitlab",
        strings: &["base_url", "container"],
        lists: &["projects"],
        bools: &[],
        secrets: &[("token", "token_file", true)],
        modes: &[],
    },
    Spec {
        kind: "bitbucket",
        strings: &["container"],
        lists: &["repos"],
        bools: &[],
        secrets: &[("token", "token_file", true)],
        modes: &[],
    },
    Spec {
        kind: "gitea",
        strings: &["base_url", "container"],
        lists: &["repos"],
        bools: &[],
        secrets: &[
            ("token", "token_file", false),
            ("webhook_secret", "webhook_secret_file", true),
        ],
        modes: &[],
    },
    Spec {
        kind: "azure_devops",
        strings: &["organization", "base_url", "container"],
        lists: &["projects"],
        bools: &["pull_requests"],
        secrets: &[],
        modes: &[
            Mode {
                name: "pat",
                strings: &[],
                secrets: &[("token", "token_file", true)],
            },
            Mode {
                name: "entra",
                strings: &["tenant_id", "client_id"],
                secrets: &[("client_secret", "client_secret_file", true)],
            },
        ],
    },
    Spec {
        kind: "prometheus",
        strings: &["url"],
        lists: &["hosts"],
        bools: &[],
        secrets: &[("token", "token_file", false)],
        modes: &[],
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

    // The way to sign in: asked for, else the one the source has, else the first.
    let mode = if spec.modes.is_empty() {
        None
    } else {
        let wanted = match fields.get("auth") {
            Some(v) => v.as_str().context("auth must be text")?,
            None => entry
                .get("auth")
                .and_then(toml::Value::as_str)
                .unwrap_or(spec.modes[0].name),
        };
        let mode = spec
            .modes
            .iter()
            .find(|m| m.name == wanted)
            .with_context(|| format!("{kind} cannot sign in with {wanted:?}"))?;
        entry.insert("auth".into(), toml::Value::String(mode.name.into()));
        // What belonged to another way no longer applies.
        for other in spec.modes.iter().filter(|m| m.name != mode.name) {
            for key in other.strings {
                entry.remove(*key);
            }
            for (_, field, _) in other.secrets {
                entry.remove(*field);
            }
        }
        Some(mode)
    };
    let mode_strings: &[&str] = mode.map_or(&[], |m| m.strings);
    let mode_secrets: &[(&str, &str, bool)] = mode.map_or(&[], |m| m.secrets);
    for key in secrets.keys() {
        anyhow::ensure!(
            spec.secrets
                .iter()
                .chain(mode_secrets)
                .any(|(k, ..)| k == key),
            "{kind} has no secret called {key:?}"
        );
    }

    for (key, value) in fields {
        if key == "auth" && mode.is_some() {
            continue;
        }
        if spec.strings.contains(&key.as_str()) || mode_strings.contains(&key.as_str()) {
            let text = value
                .as_str()
                .with_context(|| format!("{key} must be text"))?
                .trim();
            // An empty container switches the feature off for this source.
            if key == "container" && text.is_empty() {
                entry.remove("container");
                continue;
            }
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
    for (key, field, required) in spec.secrets.iter().chain(mode_secrets) {
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
            None if *required && (create || !entry.contains_key(*field)) => {
                bail!("the {key} is required")
            }
            None => {}
        }
    }
    Ok(Plan {
        text: toml::to_string(&root).context("cannot write sources.toml")?,
        secrets: stored,
    })
}

/// The credential the broker password is stored under.
pub const MQTT_PASSWORD: &str = "mqtt-password";

/// Change the broker settings in `[mqtt]` of `sources.toml`. `fields` may hold
/// `enabled`, `tls` (yes/no), `host`, `username` (text; empty = no login) and
/// `port`; `secrets` may hold `password` (empty or missing = keep).
pub fn set_mqtt(
    current: &str,
    fields: &Map<String, Value>,
    secrets: &BTreeMap<String, String>,
) -> Result<Plan> {
    for key in secrets.keys() {
        anyhow::ensure!(key == "password", "MQTT has no secret called {key:?}");
    }
    let mut root = parse(current)?;
    let table = root
        .entry("mqtt")
        .or_insert_with(|| toml::Value::Table(Table::new()))
        .as_table_mut()
        .context("`mqtt` in sources.toml is not a table")?;
    for (key, value) in fields {
        match key.as_str() {
            "enabled" | "tls" => {
                let flag = value
                    .as_bool()
                    .with_context(|| format!("{key} must be true or false"))?;
                table.insert(key.clone(), toml::Value::Boolean(flag));
            }
            "host" => {
                let text = value.as_str().context("host must be text")?.trim();
                anyhow::ensure!(!text.is_empty(), "host must not be empty");
                table.insert(key.clone(), toml::Value::String(text.into()));
            }
            "username" => {
                let text = value.as_str().context("username must be text")?.trim();
                table.insert(key.clone(), toml::Value::String(text.into()));
            }
            "port" => {
                let port = value
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|p| *p > 0)
                    .context("port must be a number from 1 to 65535")?;
                table.insert(key.clone(), toml::Value::Integer(i64::from(port)));
            }
            _ => bail!("{key:?} cannot be set from the setup page"),
        }
    }
    let mut stored = Vec::new();
    if let Some(password) = secrets
        .get("password")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        anyhow::ensure!(
            crate::source::credential_status(MQTT_PASSWORD)
                != crate::source::CredentialStatus::Mounted,
            "a mounted credential named {MQTT_PASSWORD} already exists and would win over the new \
             one; remove or rename that file"
        );
        table.insert(
            "password_file".into(),
            toml::Value::String(MQTT_PASSWORD.into()),
        );
        stored.push((MQTT_PASSWORD.to_string(), Secret::new(password)));
    }
    Ok(Plan {
        text: toml::to_string(&root).context("cannot write sources.toml")?,
        secrets: stored,
    })
}

/// Set `host_view` in `[kiosk]` of `sources.toml`: `detailed` or `compact`.
pub fn set_kiosk(current: &str, host_view: &str) -> Result<String> {
    anyhow::ensure!(
        matches!(host_view, "detailed" | "compact"),
        "host_view must be \"detailed\" or \"compact\""
    );
    let mut root = parse(current)?;
    let table = root
        .entry("kiosk")
        .or_insert_with(|| toml::Value::Table(Table::new()))
        .as_table_mut()
        .context("`kiosk` in sources.toml is not a table")?;
    table.insert("host_view".into(), toml::Value::String(host_view.into()));
    toml::to_string(&root).context("cannot write sources.toml")
}

/// Set the ticker keys of `[alerts]` in `sources.toml`. `fields` may hold
/// `ticker`, `show_source`, `dismiss`, `http_input`, `failed_pill` and `failed_as_alerts` (booleans), `ticker_speed` (`slow`,
/// `normal` or `fast`) and `max_active` (1 to 50); keys it leaves out stay as
/// they are. Anything else is refused.
pub fn set_alerts(
    current: &str,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<String> {
    use serde_json::Value;
    let mut root = parse(current)?;
    let table = root
        .entry("alerts")
        .or_insert_with(|| toml::Value::Table(Table::new()))
        .as_table_mut()
        .context("`alerts` in sources.toml is not a table")?;
    for (key, value) in fields {
        let new = match (key.as_str(), value) {
            (
                "ticker" | "show_source" | "dismiss" | "http_input" | "failed_pill"
                | "failed_as_alerts",
                Value::Bool(on),
            ) => toml::Value::Boolean(*on),
            ("ticker_speed", Value::String(speed))
                if matches!(speed.as_str(), "slow" | "normal" | "fast") =>
            {
                toml::Value::String(speed.clone())
            }
            ("max_active", Value::Number(n)) => {
                let n = n
                    .as_u64()
                    .filter(|n| (1..=crate::config::MAX_ACTIVE_ALERTS as u64).contains(n));
                toml::Value::Integer(n.with_context(|| {
                    format!(
                        "max_active must be a number from 1 to {}",
                        crate::config::MAX_ACTIVE_ALERTS
                    )
                })? as i64)
            }
            _ => bail!("{key:?} is not an alert setting, or its value has the wrong type"),
        };
        table.insert(key.clone(), new);
    }
    toml::to_string(&root).context("cannot write sources.toml")
}

/// Set `[updates] check` in `sources.toml`.
pub fn set_updates(current: &str, check: bool) -> Result<String> {
    let mut root = parse(current)?;
    let table = root
        .entry("updates")
        .or_insert_with(|| toml::Value::Table(Table::new()))
        .as_table_mut()
        .context("`updates` in sources.toml is not a table")?;
    table.insert("check".into(), toml::Value::Boolean(check));
    toml::to_string(&root).context("cannot write sources.toml")
}

/// A host as the Hosts page sends it.
#[derive(Debug, Clone)]
pub struct HostEdit {
    pub instance: String,
    /// The name on the dashboard.
    pub name: String,
    /// What to show; `None` is everything the host reports.
    pub show: Option<Vec<String>>,
}

/// Replace the hosts of Prometheus source `name` in `sources.toml`. What the
/// page does not know about a host (`cadvisor`, `expect_containers`) is kept
/// for hosts that stay. A host named like its default (the instance without
/// its port) and without a `show` list is written as a plain instance.
pub fn set_hosts(current: &str, name: &str, hosts: &[HostEdit]) -> Result<String> {
    let mut root = parse(current)?;
    let list = sources(&mut root, "prometheus")?;
    let index = position(list, name).with_context(|| {
        format!(
            "there is no prometheus source named {name:?} in the data folder \
             (sources from bridge.toml are changed in that file)"
        )
    })?;
    let entry = list[index].as_table_mut().expect("entries are tables");
    let old: Vec<toml::Value> = entry
        .get("hosts")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let instance_of = |v: &toml::Value| {
        v.as_str()
            .or_else(|| v.get("instance").and_then(toml::Value::as_str))
            .map(str::to_string)
    };
    let mut out = Vec::new();
    for host in hosts {
        let instance = host.instance.trim();
        anyhow::ensure!(!instance.is_empty(), "a host needs its address");
        anyhow::ensure!(
            crate::config::valid_host_name(&host.name),
            "the name {:?} must be 1 to 40 characters without spaces at the ends",
            host.name
        );
        for item in host.show.iter().flatten() {
            anyhow::ensure!(
                crate::config::HOST_SHOW.contains(&item.as_str()),
                "a host cannot show {item:?}"
            );
        }
        let mut table = old
            .iter()
            .find(|v| instance_of(v).as_deref() == Some(instance))
            .and_then(|v| v.as_table().cloned())
            .unwrap_or_default();
        table.insert("instance".into(), toml::Value::String(instance.into()));
        table.insert("name".into(), toml::Value::String(host.name.clone()));
        match &host.show {
            Some(show) => table.insert(
                "show".into(),
                toml::Value::Array(
                    show.iter()
                        .map(|s| toml::Value::String(s.clone()))
                        .collect(),
                ),
            ),
            None => table.remove("show"),
        };
        let plain = table.len() == 2 && host.name == crate::config::default_host_name(instance);
        out.push(if plain {
            toml::Value::String(instance.into())
        } else {
            toml::Value::Table(table)
        });
    }
    entry.insert("hosts".into(), toml::Value::Array(out));
    toml::to_string(&root).context("cannot write sources.toml")
}

/// Switch a `bridge.toml` source off (`hide`) or on again. The source stays in
/// `bridge.toml`; `sources.toml` only lists its id under `hidden`.
pub fn set_hidden(current: &str, kind: &str, name: &str, hide: bool) -> Result<String> {
    anyhow::ensure!(spec(kind).is_some(), "unknown source type {kind:?}");
    let id = format!("{kind}:{name}");
    let mut root = parse(current)?;
    let mut list: Vec<toml::Value> = match root.remove("hidden") {
        Some(toml::Value::Array(list)) => list,
        Some(_) => bail!("`hidden` in sources.toml is not a list"),
        None => Vec::new(),
    };
    list.retain(|v| v.as_str() != Some(id.as_str()));
    if hide {
        list.push(toml::Value::String(id));
    }
    if !list.is_empty() {
        root.insert("hidden".into(), toml::Value::Array(list));
    }
    toml::to_string(&root).context("cannot write sources.toml")
}

/// What to take away from the saved broker settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttRemoval {
    /// Only the login: the name is emptied, the stored password goes.
    Login,
    /// Everything the page saved: back to what `bridge.toml` says.
    Settings,
}

/// Remove saved broker settings. Returns the new file text and the credentials
/// the page stored for the broker (the caller deletes those files).
pub fn remove_mqtt(current: &str, what: MqttRemoval) -> Result<(String, Vec<String>)> {
    let mut root = parse(current)?;
    let used = root
        .get("mqtt")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("password_file")?.as_str())
        .filter(|name| *name == MQTT_PASSWORD)
        .map(str::to_string);
    match what {
        MqttRemoval::Settings => {
            root.remove("mqtt");
        }
        MqttRemoval::Login => {
            let table = root
                .entry("mqtt")
                .or_insert_with(|| toml::Value::Table(Table::new()))
                .as_table_mut()
                .context("`mqtt` in sources.toml is not a table")?;
            // An empty name switches the login off, even if bridge.toml has one.
            table.insert("username".into(), toml::Value::String(String::new()));
            table.remove("password_file");
        }
    }
    Ok((
        toml::to_string(&root).context("cannot write sources.toml")?,
        used.into_iter().collect(),
    ))
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
        .chain(spec.modes.iter().flat_map(|m| m.secrets))
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
