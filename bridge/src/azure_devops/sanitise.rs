//! Turns a real Azure DevOps answer into one that is safe to share.
//!
//! The rule is "unknown means hidden": a string is kept only when it is one
//! of a few known, harmless fields (`status`, `result`, `state`, `type`, ...),
//! a timestamp, a number string such as a build number, or `main`/`master`.
//! Every other string is replaced by a stable placeholder (`definition-1`,
//! `user-2@example.invalid`, `https://example.invalid/pool-1`, ...). The same
//! original value always gets the same placeholder inside one run, so the
//! relations in the data (this build belongs to that pipeline, this agent
//! runs that job) survive. Numbers under id-like keys are renumbered the same
//! way. Booleans, counts and the shape of the JSON stay as they are.
//!
//! Keys that only carry noise or personal data (`_links`, `parameters`,
//! `variables`, ...) are dropped.

use std::collections::HashMap;

use serde_json::{Map, Value};

/// String values that are kept as they are, whatever they hold: the fields
/// the bridge reads to decide what a run, a check or an agent is doing.
const KEEP_STRING_KEYS: &[&str] = &[
    "status",
    "result",
    "state",
    "type",
    "reason",
    "mergeStatus",
    "provisioningState",
    "planType",
];

/// Keys left out completely: links, build parameters and variables, trigger
/// details, identity descriptors and log references.
const DROP_KEYS: &[&str] = &[
    "_links",
    "parameters",
    "templateParameters",
    "triggerInfo",
    "variables",
    "properties",
    "descriptor",
    "avatar",
    "log",
    "logs",
    "orchestrationPlan",
    "plans",
    "validationResults",
];

/// Keys whose value names the thing the object *is*; the placeholder then
/// takes the name of the surrounding object (`definition-1`) instead.
const GENERIC_KEYS: &[&str] = &["name", "displayName", "title", "uniqueName", "description"];

/// Placeholder maker with memory. Use one per capture run.
#[derive(Default)]
pub struct Sanitiser {
    strings: HashMap<String, String>,
    numbers: HashMap<u64, u64>,
    counters: HashMap<String, usize>,
}

impl Sanitiser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sanitise a whole answer. `label` names what the answer lists (`pool`,
    /// `build`, `agent`), for the placeholders of the names inside it.
    pub fn value(&mut self, value: Value, label: &str) -> Value {
        self.walk(value, "", label)
    }

    /// The placeholder for an id: stable per value, 101, 102, ...
    pub fn id(&mut self, id: u64) -> u64 {
        let next = 101 + self.numbers.len() as u64;
        *self.numbers.entry(id).or_insert(next)
    }

    /// The placeholder for a name: stable per value, `<label>-1`, `<label>-2`.
    pub fn name(&mut self, name: &str, label: &str) -> String {
        if let Some(known) = self.strings.get(name) {
            return known.clone();
        }
        let label = slug(label);
        let n = self.counters.entry(label.clone()).or_insert(0);
        *n += 1;
        let placeholder = format!("{label}-{n}");
        self.strings.insert(name.to_string(), placeholder.clone());
        placeholder
    }

    fn walk(&mut self, value: Value, key: &str, label: &str) -> Value {
        match value {
            Value::Object(map) => {
                // A list entry's own `type` (a timeline record) names it better
                // than the list's key does.
                let kind = map.get("type").and_then(Value::as_str).map(slug);
                let child_label = match (kind, key) {
                    (Some(kind), "" | "value" | "records") => kind,
                    (None, "" | "value") => label.to_string(),
                    _ => key.to_string(),
                };
                let mut out = Map::new();
                for (k, v) in map {
                    if DROP_KEYS.contains(&k.as_str()) {
                        continue;
                    }
                    let new_key = self.key(&k);
                    out.insert(new_key, self.walk(v, &k, &child_label));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|v| self.walk(v, key, label))
                    .collect(),
            ),
            Value::String(s) => Value::String(self.string(&s, key, label)),
            Value::Number(n) => match n.as_u64() {
                Some(n) if is_id_key(key) => Value::from(self.id(n)),
                _ => Value::Number(n),
            },
            other => other,
        }
    }

    /// Object keys are fixed names, except when they look like an identifier
    /// or an address.
    fn key(&mut self, key: &str) -> String {
        if is_guid(key) || key.contains('@') || key.contains('/') || key.contains(' ') {
            self.name(key, "key")
        } else {
            key.to_string()
        }
    }

    fn string(&mut self, s: &str, key: &str, label: &str) -> String {
        if s.is_empty() || KEEP_STRING_KEYS.contains(&key) || is_timestamp(s) {
            return s.to_string();
        }
        if is_guid(s) {
            return self.fake(s, |n| format!("00000000-0000-4000-8000-{n:012x}"), "guid");
        }
        if is_sha(s) {
            return self.fake(s, |n| format!("{n:040x}"), "sha");
        }
        if is_number_text(s) {
            return s.to_string();
        }
        if s.starts_with("refs/") {
            return self.git_ref(s);
        }
        if s.starts_with("http://") || s.starts_with("https://") {
            let name = self.name(s, key_label(key, label));
            return format!("https://example.invalid/{name}");
        }
        if s.contains('@') {
            let user = self.name(s, "user");
            return format!("{user}@example.invalid");
        }
        self.name(s, key_label(key, label))
    }

    /// A fake value with a fixed look (a GUID or a commit id), numbered by
    /// the order in which originals were met.
    fn fake(&mut self, original: &str, make: impl Fn(usize) -> String, label: &str) -> String {
        if let Some(known) = self.strings.get(original) {
            return known.clone();
        }
        let n = self.counters.entry(label.to_string()).or_insert(0);
        *n += 1;
        let fake = make(*n);
        self.strings.insert(original.to_string(), fake.clone());
        fake
    }

    /// `refs/heads/feature/login` becomes `refs/heads/branch-1`; `main` and
    /// `master` and the shape of `refs/pull/<n>/merge` stay.
    fn git_ref(&mut self, s: &str) -> String {
        if matches!(s, "refs/heads/main" | "refs/heads/master") {
            return s.to_string();
        }
        if let Some(rest) = s.strip_prefix("refs/pull/") {
            let (number, tail) = rest.split_once('/').unwrap_or((rest, ""));
            if let Ok(number) = number.parse::<u64>() {
                let number = self.id(number);
                return if tail.is_empty() {
                    format!("refs/pull/{number}")
                } else {
                    format!("refs/pull/{number}/{tail}")
                };
            }
        }
        let mut parts = s.splitn(3, '/');
        let (root, kind) = (parts.next().unwrap_or("refs"), parts.next().unwrap_or("x"));
        let name = self.name(s, "branch");
        format!("{root}/{kind}/{name}")
    }
}

/// The label for a placeholder: the key, except for generic keys such as
/// `name`, where the surrounding object says more.
fn key_label<'a>(key: &'a str, label: &'a str) -> &'a str {
    if key.is_empty() || GENERIC_KEYS.contains(&key) {
        label
    } else {
        key
    }
}

/// Lower-case letters and digits only, at most 20, never empty.
fn slug(s: &str) -> String {
    let slug: String = s
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .take(20)
        .collect();
    if slug.is_empty() { "item".into() } else { slug }
}

fn is_id_key(key: &str) -> bool {
    key.to_ascii_lowercase().ends_with("id")
}

fn is_guid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `20261008.4`, `7`, `4.258.1`: a build number or a version, no words.
fn is_number_text(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
}

/// `2026-10-08T18:01:30.1234567Z`, with an optional offset.
fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    (19..=40).contains(&b.len())
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | '.' | 'T' | 'Z' | '+'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn names_emails_and_urls_are_replaced_and_the_shape_stays() {
        let mut s = Sanitiser::new();
        let out = s.value(
            json!({"count": 1, "value": [{
                "id": 4242,
                "buildNumber": "20261008.4",
                "status": "completed",
                "result": "failed",
                "queueTime": "2026-10-08T17:59:50.5Z",
                "sourceBranch": "refs/heads/feature/secret-login",
                "sourceVersion": "abcdef1234567890abcdef1234567890abcdef12",
                "definition": {"id": 11, "name": "Deploy Acme Infra"},
                "project": {"name": "Acme Portal"},
                "repository": {"name": "acme-portal", "type": "TfsGit"},
                "requestedFor": {
                    "displayName": "Jane Doe",
                    "uniqueName": "jane.doe@acme.example",
                    "imageUrl": "https://dev.azure.com/acme/_apis/GraphProfile/x",
                    "id": "5f2c0f6e-9c43-4a41-8f6e-0a7a1c1c9d11"
                },
                "_links": {"self": {"href": "https://dev.azure.com/acme/x"}},
                "parameters": "{\"password\":\"hunter2\"}"
            }]}),
            "build",
        );
        let text = out.to_string();
        for secret in [
            "Acme",
            "acme",
            "Jane",
            "jane",
            "secret-login",
            "hunter2",
            "Deploy",
            "dev.azure.com",
            "abcdef1234",
            "5f2c0f6e",
            "4242",
        ] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        // The structure and the harmless values are untouched.
        let build = &out["value"][0];
        assert_eq!(out["count"], 1);
        assert_eq!(build["status"], "completed");
        assert_eq!(build["result"], "failed");
        assert_eq!(build["buildNumber"], "20261008.4");
        assert_eq!(build["queueTime"], "2026-10-08T17:59:50.5Z");
        assert_eq!(build["repository"]["type"], "TfsGit");
        assert_eq!(build["definition"]["name"], "definition-1");
        assert_eq!(build["project"]["name"], "project-1");
        assert_eq!(
            build["requestedFor"]["uniqueName"],
            "user-1@example.invalid"
        );
        assert_eq!(
            build["requestedFor"]["imageUrl"],
            "https://example.invalid/imageurl-1"
        );
        assert_eq!(build["sourceBranch"], "refs/heads/branch-1");
        assert!(build.get("_links").is_none() && build.get("parameters").is_none());
    }

    #[test]
    fn the_same_value_gets_the_same_placeholder_everywhere() {
        let mut s = Sanitiser::new();
        let a = s.value(json!({"value": [{"id": 12, "name": "Homelab"}]}), "pool");
        let b = s.value(
            json!({"value": [{"poolId": 12, "pool": {"name": "Homelab"}}, {"poolId": 13}]}),
            "agent",
        );
        assert_eq!(a["value"][0]["id"], b["value"][0]["poolId"]);
        assert_eq!(a["value"][0]["name"], b["value"][0]["pool"]["name"]);
        assert_ne!(b["value"][0]["poolId"], b["value"][1]["poolId"]);
        assert_eq!(s.id(12), a["value"][0]["id"].as_u64().unwrap());
    }

    #[test]
    fn timeline_records_keep_their_type_and_state_but_not_their_names() {
        let mut s = Sanitiser::new();
        let out = s.value(
            json!({"records": [{
                "id": "11111111-2222-3333-4444-555555555555",
                "parentId": "11111111-2222-3333-4444-666666666666",
                "type": "Checkpoint.Approval",
                "name": "Approve prod deploy for Jane",
                "state": "inProgress",
                "result": null,
                "order": 2,
                "percentComplete": 0
            }]}),
            "timeline",
        );
        let rec = &out["records"][0];
        assert_eq!(rec["type"], "Checkpoint.Approval");
        assert_eq!(rec["state"], "inProgress");
        assert_eq!(rec["order"], 2);
        assert_eq!(rec["name"], "checkpointapproval-1");
        assert_eq!(rec["id"], "00000000-0000-4000-8000-000000000001");
        assert_eq!(rec["parentId"], "00000000-0000-4000-8000-000000000002");
        assert!(!out.to_string().contains("Jane"));
    }

    #[test]
    fn unknown_strings_are_hidden_by_default() {
        let mut s = Sanitiser::new();
        let out = s.value(
            json!({"workerName": "buildbox-07", "weird": "contact me at x@y.z"}),
            "agent",
        );
        assert_eq!(out["workerName"], "workername-1");
        assert_eq!(out["weird"], "user-1@example.invalid");
    }
}
