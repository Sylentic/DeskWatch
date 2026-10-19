//! GitLab runners: which lists to read and how a runner becomes a `Runner`
//! fact.
//!
//! The runner lists answer a plain JSON array (`id`, `description`, `status`,
//! `paused`, `runner_type`). They do not say whether a runner is working, so
//! the API client asks `GET /runners/:id/jobs?status=running` for the online
//! ones. The tag list is only in the per-runner detail and is not fetched.

use std::fmt;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::ci::{Runner, RunnerStatus};
use crate::config::valid_gitlab_path;
use crate::model::{MAX_TITLE_CHARS, truncate};

/// Which runners to list: the `runners = [...]` entries of a GitLab source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunnerScope {
    /// Every runner of the instance. Needs an administrator token.
    Instance,
    /// The runners of one group (`group` or `group/subgroup`).
    Group(String),
    /// The runners of one project (`group/name`).
    Project(String),
}

impl RunnerScope {
    /// Parse `instance`, `group:<path>` or `project:<group>/<name>`.
    pub fn parse(text: &str) -> Result<Self> {
        let scope = match text.split_once(':') {
            None if text == "instance" => RunnerScope::Instance,
            Some(("group", path)) if valid_gitlab_path(path, 1) => {
                RunnerScope::Group(path.to_string())
            }
            Some(("project", path)) if valid_gitlab_path(path, 2) => {
                RunnerScope::Project(path.to_string())
            }
            _ => bail!(
                "runners entry {text:?} must be instance, group:<path> or project:<group>/<name>"
            ),
        };
        Ok(scope)
    }

    /// The REST path, below `/api/v4`. A group or project path has its `/`
    /// encoded, as GitLab wants it in place of the numeric id.
    pub fn path(&self) -> String {
        match self {
            RunnerScope::Instance => "/runners/all".into(),
            RunnerScope::Group(path) => format!("/groups/{}/runners", path.replace('/', "%2F")),
            RunnerScope::Project(path) => {
                format!("/projects/{}/runners", path.replace('/', "%2F"))
            }
        }
    }
}

/// Printed like it is configured; also the end of the fact key.
impl fmt::Display for RunnerScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunnerScope::Instance => f.write_str("instance"),
            RunnerScope::Group(path) => write!(f, "group:{path}"),
            RunnerScope::Project(path) => write!(f, "project:{path}"),
        }
    }
}

/// One runner, as GitLab lists it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiRunner {
    pub id: u64,
    pub description: Option<String>,
    pub name: Option<String>,
    /// `online`, `offline`, `stale` or `never_contacted`.
    pub status: String,
    /// Switched off by an administrator or the project owner.
    pub paused: bool,
    /// `instance_type`, `group_type` or `project_type`.
    pub runner_type: Option<String>,
    /// Filled in by the API client from the running jobs; not in the list.
    #[serde(skip)]
    pub busy: bool,
}

impl ApiRunner {
    /// The shared fact. Only an `online` runner is up; a status nobody knows
    /// counts as offline, because showing a runner as idle when we cannot tell
    /// would hide an outage.
    pub fn into_fact(self, source: &str, url: Option<String>) -> Runner {
        let status = match self.status.as_str() {
            "online" if self.busy => RunnerStatus::Busy,
            "online" => RunnerStatus::Idle,
            _ => RunnerStatus::Offline,
        };
        let name = [&self.description, &self.name]
            .into_iter()
            .flatten()
            .map(|n| n.trim())
            .find(|n| !n.is_empty())
            .map_or_else(|| format!("runner {}", self.id), str::to_string);
        // The list has no tags; the kind of runner is the one label it has.
        let labels = self
            .runner_type
            .as_deref()
            .and_then(|t| t.strip_suffix("_type"))
            .map(|t| vec![t.to_string()])
            .unwrap_or_default();
        Runner {
            source: source.into(),
            name: truncate(&name, MAX_TITLE_CHARS),
            status,
            disabled: self.paused,
            labels,
            url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_parse_and_print_like_the_config() {
        for text in [
            "instance",
            "group:team",
            "group:team/sub",
            "project:me/demo",
        ] {
            assert_eq!(RunnerScope::parse(text).unwrap().to_string(), text);
        }
        for bad in [
            "",
            "group:",
            "project:demo",
            "project:/demo",
            "project:a//b",
            "admin",
            "instance:x",
            "repo:a/b",
        ] {
            assert!(
                RunnerScope::parse(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn scopes_map_to_the_endpoints_with_encoded_paths() {
        let path = |s: &str| RunnerScope::parse(s).unwrap().path();
        assert_eq!(path("instance"), "/runners/all");
        assert_eq!(path("group:team/sub"), "/groups/team%2Fsub/runners");
        assert_eq!(path("project:me/demo"), "/projects/me%2Fdemo/runners");
    }

    #[test]
    fn runner_list_maps_to_facts() {
        let list: Vec<ApiRunner> =
            serde_json::from_str(include_str!("testdata/runners.json")).unwrap();
        let facts: Vec<_> = list
            .into_iter()
            .enumerate()
            .map(|(i, mut r)| {
                r.busy = i == 1;
                r.into_fact("gitlab", None)
            })
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
        assert_eq!(facts[2].name, "gitlab-runner", "falls back to the name");
        assert_eq!(facts[0].labels, ["project"]);
        assert_eq!(facts[3].labels, ["instance"]);
    }

    #[test]
    fn unknown_status_is_offline() {
        let runner: ApiRunner = serde_json::from_str(r#"{"id":7,"status":"odd"}"#).unwrap();
        let fact = runner.into_fact("gitlab", None);
        assert_eq!(fact.status, RunnerStatus::Offline);
        assert_eq!(fact.name, "runner 7");
    }
}
