//! Gitea runners: which lists to read and how a runner becomes a `Runner` fact.
//!
//! Gitea answers `{ "runners": [...], "total_count": n }` on four endpoints,
//! one per scope. The fields used here (`name`, `status`, `busy`, `disabled`,
//! `labels`) were read from the 28.1.0 swagger; see docs/runners.md.

use std::fmt;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::ci::{Runner, RunnerStatus};
use crate::model::{MAX_TITLE_CHARS, truncate};

/// Most labels kept per runner. A runner with more is cut, the page shows two.
const MAX_LABELS: usize = 8;

/// Which runners to list: the `runners = [...]` entries of a Gitea source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunnerScope {
    /// Every runner of the instance. Needs an administrator token.
    Admin,
    /// The runners of the user the token belongs to.
    User,
    /// The runners of one organisation.
    Org(String),
    /// The runners of one repository (`owner/name`).
    Repo(String),
}

impl RunnerScope {
    /// Parse `admin`, `user`, `org:<name>` or `repo:<owner>/<name>`.
    pub fn parse(text: &str) -> Result<Self> {
        let scope = match text.split_once(':') {
            None if text == "admin" => RunnerScope::Admin,
            None if text == "user" => RunnerScope::User,
            Some(("org", org)) if !org.is_empty() && !org.contains('/') => {
                RunnerScope::Org(org.to_string())
            }
            Some(("repo", repo)) if repo.split('/').count() == 2 && !repo.starts_with('/') => {
                RunnerScope::Repo(repo.to_string())
            }
            _ => bail!(
                "runners entry {text:?} must be admin, user, org:<name> or repo:<owner>/<name>"
            ),
        };
        Ok(scope)
    }

    /// The REST path, below `/api/v1`.
    pub fn path(&self) -> String {
        match self {
            RunnerScope::Admin => "/admin/actions/runners".into(),
            RunnerScope::User => "/user/actions/runners".into(),
            RunnerScope::Org(org) => format!("/orgs/{org}/actions/runners"),
            RunnerScope::Repo(repo) => format!("/repos/{repo}/actions/runners"),
        }
    }
}

/// Printed like it is configured; also the end of the fact key.
impl fmt::Display for RunnerScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunnerScope::Admin => f.write_str("admin"),
            RunnerScope::User => f.write_str("user"),
            RunnerScope::Org(org) => write!(f, "org:{org}"),
            RunnerScope::Repo(repo) => write!(f, "repo:{repo}"),
        }
    }
}

/// One page of `GET .../actions/runners`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RunnerList {
    pub runners: Vec<ApiRunner>,
    pub total_count: u32,
}

/// One runner, as Gitea lists it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiRunner {
    pub name: String,
    /// `idle`, `active` or `offline` on 28.x. Older builds said `online`.
    pub status: String,
    pub busy: bool,
    pub disabled: bool,
    pub labels: Vec<ApiLabel>,
}

/// A label is an object on 28.x; a plain string is accepted too.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ApiLabel {
    Name(String),
    Object { name: String },
}

impl ApiLabel {
    fn name(&self) -> &str {
        match self {
            ApiLabel::Name(name) | ApiLabel::Object { name } => name,
        }
    }
}

impl ApiRunner {
    /// The shared fact. A status nobody knows counts as offline: showing a
    /// runner as idle when we cannot tell would hide an outage.
    pub fn into_fact(self, source: &str) -> Runner {
        let status = match self.status.as_str() {
            "offline" => RunnerStatus::Offline,
            "active" | "busy" => RunnerStatus::Busy,
            "idle" | "online" if self.busy => RunnerStatus::Busy,
            "idle" | "online" => RunnerStatus::Idle,
            _ => RunnerStatus::Offline,
        };
        Runner {
            source: source.into(),
            name: truncate(&self.name, MAX_TITLE_CHARS),
            status,
            disabled: self.disabled,
            labels: self
                .labels
                .iter()
                .map(|l| truncate(l.name(), MAX_TITLE_CHARS))
                .take(MAX_LABELS)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_parse_and_print_like_the_config() {
        for text in ["admin", "user", "org:team", "repo:me/demo"] {
            assert_eq!(RunnerScope::parse(text).unwrap().to_string(), text);
        }
        for bad in [
            "",
            "org:",
            "org:a/b",
            "repo:demo",
            "repo:a/b/c",
            "team",
            "user:x",
        ] {
            assert!(
                RunnerScope::parse(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn scopes_map_to_the_four_endpoints() {
        let path = |s: &str| RunnerScope::parse(s).unwrap().path();
        assert_eq!(path("admin"), "/admin/actions/runners");
        assert_eq!(path("user"), "/user/actions/runners");
        assert_eq!(path("org:team"), "/orgs/team/actions/runners");
        assert_eq!(path("repo:me/demo"), "/repos/me/demo/actions/runners");
    }

    #[test]
    fn runner_list_maps_to_facts() {
        let list: RunnerList = serde_json::from_str(include_str!("testdata/runners.json")).unwrap();
        assert_eq!(list.total_count, 4);
        let facts: Vec<_> = list
            .runners
            .into_iter()
            .map(|r| r.into_fact("gitea"))
            .collect();
        let status: Vec<_> = facts.iter().map(|r| (r.status, r.disabled)).collect();
        assert_eq!(
            status,
            [
                (RunnerStatus::Idle, false),
                (RunnerStatus::Busy, false),
                (RunnerStatus::Offline, false),
                (RunnerStatus::Offline, true),
            ]
        );
        assert_eq!(facts[1].labels, ["ubuntu-latest", "docker"]);
        assert!(facts[2].labels.is_empty());
        assert_eq!(facts[0].source, "gitea");
    }

    #[test]
    fn busy_flag_wins_and_unknown_status_is_offline() {
        let runner = |json: &str| -> RunnerStatus {
            serde_json::from_str::<ApiRunner>(json)
                .unwrap()
                .into_fact("gitea")
                .status
        };
        assert_eq!(
            runner(r#"{"status":"online","busy":true}"#),
            RunnerStatus::Busy
        );
        assert_eq!(runner(r#"{"status":"online"}"#), RunnerStatus::Idle);
        assert_eq!(runner(r#"{"status":"mystery"}"#), RunnerStatus::Offline);
        assert_eq!(runner(r#"{}"#), RunnerStatus::Offline);
    }

    #[test]
    fn labels_may_be_plain_strings() {
        let runner: ApiRunner =
            serde_json::from_str(r#"{"status":"idle","labels":["a",{"name":"b"}]}"#).unwrap();
        assert_eq!(runner.into_fact("gitea").labels, ["a", "b"]);
    }
}
