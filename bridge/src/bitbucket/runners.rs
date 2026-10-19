//! Bitbucket Cloud runners: which lists to read and how a runner becomes a
//! `Runner` fact.
//!
//! Self-hosted Pipelines runners are listed per repository or per workspace.
//! Each has a `state.status` and labels. The list does not say whether a
//! runner is working a step, so an online runner always counts as idle.

use std::fmt;

use anyhow::{Result, bail};

use super::payload::ApiRunner;
use crate::ci::{Runner, RunnerStatus};
use crate::model::{MAX_TITLE_CHARS, truncate};

/// Most labels kept per runner. A runner with more is cut, the page shows two.
const MAX_LABELS: usize = 8;

/// Which runners to list: the `runners = [...]` entries of a Bitbucket source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunnerScope {
    /// The runners of one workspace (its slug).
    Workspace(String),
    /// The runners of one repository (`workspace/repo`).
    Repo(String),
}

impl RunnerScope {
    /// Parse `workspace:<slug>` or `repo:<workspace>/<repo>`.
    pub fn parse(text: &str) -> Result<Self> {
        let scope = match text.split_once(':') {
            Some(("workspace", slug)) if crate::config::valid_repo_part(slug) => {
                RunnerScope::Workspace(slug.to_string())
            }
            Some(("repo", repo)) if crate::config::valid_repo(repo) => {
                RunnerScope::Repo(repo.to_string())
            }
            _ => {
                bail!("runners entry {text:?} must be workspace:<slug> or repo:<workspace>/<repo>")
            }
        };
        Ok(scope)
    }

    /// The REST path, below the API root.
    pub fn path(&self) -> String {
        match self {
            RunnerScope::Workspace(slug) => format!("/workspaces/{slug}/pipelines-config/runners"),
            RunnerScope::Repo(repo) => format!("/repositories/{repo}/pipelines-config/runners"),
        }
    }
}

/// Printed like it is configured; also the end of the fact key.
impl fmt::Display for RunnerScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunnerScope::Workspace(slug) => write!(f, "workspace:{slug}"),
            RunnerScope::Repo(repo) => write!(f, "repo:{repo}"),
        }
    }
}

impl ApiRunner {
    /// The shared fact. A status nobody knows counts as offline: showing a
    /// runner as idle when we cannot tell would hide an outage. `DISABLED` is
    /// switched off on purpose, which is not an outage.
    pub fn into_fact(self, source: &str, url: Option<String>) -> Runner {
        let (status, disabled) = match self.state.status.as_str() {
            "ONLINE" | "ENABLED" => (RunnerStatus::Idle, false),
            "DISABLED" => (RunnerStatus::Offline, true),
            _ => (RunnerStatus::Offline, false),
        };
        let name = if self.name.trim().is_empty() {
            self.uuid.clone()
        } else {
            self.name.clone()
        };
        Runner {
            source: source.into(),
            name: truncate(&name, MAX_TITLE_CHARS),
            status,
            disabled,
            labels: self
                .labels
                .iter()
                .map(|l| truncate(l, MAX_TITLE_CHARS))
                .take(MAX_LABELS)
                .collect(),
            url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::payload::Page;
    use super::*;

    #[test]
    fn scopes_parse_and_print_like_the_config() {
        for text in ["workspace:team", "repo:team/demo"] {
            assert_eq!(RunnerScope::parse(text).unwrap().to_string(), text);
        }
        for bad in [
            "",
            "workspace:",
            "workspace:a/b",
            "repo:demo",
            "repo:a/b/c",
            "team",
            "group:x",
        ] {
            assert!(
                RunnerScope::parse(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn scopes_map_to_the_endpoints() {
        let path = |s: &str| RunnerScope::parse(s).unwrap().path();
        assert_eq!(
            path("workspace:team"),
            "/workspaces/team/pipelines-config/runners"
        );
        assert_eq!(
            path("repo:team/demo"),
            "/repositories/team/demo/pipelines-config/runners"
        );
    }

    #[test]
    fn runner_list_maps_to_facts() {
        let page: Page<ApiRunner> =
            serde_json::from_str(include_str!("testdata/runners.json")).unwrap();
        let facts: Vec<_> = page
            .values
            .into_iter()
            .map(|r| r.into_fact("bitbucket", None))
            .collect();
        let status: Vec<_> = facts.iter().map(|r| (r.status, r.disabled)).collect();
        assert_eq!(
            status,
            [
                (RunnerStatus::Idle, false),
                (RunnerStatus::Offline, false),
                (RunnerStatus::Offline, false),
                (RunnerStatus::Offline, true),
            ]
        );
        assert_eq!(facts[0].labels, ["self.hosted", "linux"]);
        assert_eq!(facts[0].source, "bitbucket");
    }

    #[test]
    fn unknown_status_is_offline_and_a_missing_name_falls_back() {
        let runner: ApiRunner =
            serde_json::from_str(r#"{"uuid":"{x}","state":{"status":"odd"}}"#).unwrap();
        let fact = runner.into_fact("bitbucket", None);
        assert_eq!(fact.status, RunnerStatus::Offline);
        assert_eq!(fact.name, "{x}");
    }
}
