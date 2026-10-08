//! Gitea JSON shapes: webhook payloads and the few REST API responses the
//! bridge reads. Only the fields the bridge uses are listed; serde ignores
//! the rest. Everything has a default so a field Gitea leaves out (or renames
//! in a later release) degrades to "unknown" instead of rejecting the event.
//!
//! Checked against Gitea 1.27 (`workflow_run`, `workflow_job`, `pull_request`
//! webhooks and `/actions/jobs/{id}`), which 28.0 keeps.

use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Repository {
    /// `owner/name`.
    pub full_name: String,
    /// Just `name`, shown on the panel.
    pub name: String,
}

/// `X-Gitea-Event: workflow_run`. One per workflow run (a push, a PR, ...).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkflowRunEvent {
    /// `requested`, `in_progress` or `completed`.
    pub action: String,
    pub workflow_run: WorkflowRun,
    pub repository: Repository,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkflowRun {
    pub id: u64,
    /// Commit title or similar, used when the workflow file name is unknown.
    pub display_title: String,
    /// Workflow file, as `deploy.yml@refs/heads/main` or a full path.
    pub path: String,
    pub head_sha: String,
    pub head_branch: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// `X-Gitea-Event: workflow_job`. One per job of a run, on each state change.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkflowJobEvent {
    /// `queued`, `waiting`, `in_progress` or `completed`.
    pub action: String,
    pub workflow_job: Job,
    pub repository: Repository,
}

/// A job, as sent in `workflow_job` webhooks and returned by
/// `GET /repos/{owner}/{repo}/actions/jobs/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: u64,
    pub run_id: u64,
    pub name: String,
    pub head_sha: String,
    pub head_branch: String,
    /// `queued`, `waiting`, `in_progress` or `completed`.
    pub status: String,
    /// `success`, `failure`, `cancelled`, `skipped` once completed.
    pub conclusion: Option<String>,
    pub started_at: Option<String>,
    /// Gitea sends `null` instead of `[]` for a job that has not started.
    pub steps: Option<Vec<Step>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Step {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
}

/// `X-Gitea-Event: pull_request`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PullRequestEvent {
    /// `opened`, `closed`, `reopened`, `edited`, `synchronized`, ...
    pub action: String,
    pub pull_request: PullRequest,
    pub repository: Repository,
}

/// A pull request, as in webhooks and `GET /repos/{owner}/{repo}/pulls`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub created_at: Option<String>,
}

/// Parse a Gitea timestamp into Unix seconds. Gitea uses RFC 3339 with an
/// offset, and `0001-01-01T00:00:00Z` for "never"; both odd values and
/// missing ones become `None`.
pub fn unix_seconds(text: Option<&str>) -> Option<u64> {
    let parsed = OffsetDateTime::parse(text?, &Rfc3339).ok()?;
    u64::try_from(parsed.unix_timestamp())
        .ok()
        .filter(|&t| t > 0)
}

/// Workflow file name from a run's `path`: `ci.yml@refs/heads/main` and
/// `.gitea/workflows/ci.yml` both give `ci.yml`.
pub fn workflow_file(path: &str) -> &str {
    let path = path.split('@').next().unwrap_or(path);
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_with_offset_and_zero_values() {
        assert_eq!(
            unix_seconds(Some("2026-10-08T20:00:00+02:00")),
            Some(1_791_482_400)
        );
        assert_eq!(unix_seconds(Some("0001-01-01T00:00:00Z")), None);
        assert_eq!(unix_seconds(Some("")), None);
        assert_eq!(unix_seconds(None), None);
    }

    #[test]
    fn workflow_file_from_path() {
        assert_eq!(workflow_file("deploy.yml@refs/heads/main"), "deploy.yml");
        assert_eq!(workflow_file(".gitea/workflows/ci.yml"), "ci.yml");
        assert_eq!(workflow_file(""), "");
    }

    #[test]
    fn job_with_null_steps_and_unknown_fields() {
        let job: Job = serde_json::from_str(
            r#"{"id":5,"run_id":2,"name":"test","status":"queued","steps":null,"runner_name":"x"}"#,
        )
        .unwrap();
        assert_eq!(job.id, 5);
        assert!(job.steps.is_none());
        assert!(job.conclusion.is_none());
    }
}
