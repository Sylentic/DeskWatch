//! GitHub REST API JSON shapes: the three lists the bridge polls. Only the
//! fields the bridge uses are listed; serde ignores the rest. Fields GitHub
//! documents as nullable are `Option`, and everything has a default, so an
//! odd value degrades to "unknown" instead of failing the whole poll.
//!
//! The same shapes come from github.com, GitHub Enterprise Server (`/api/v3`)
//! and GHE.com.

use serde::Deserialize;

use crate::gitea::payload::Step;

/// One item of `GET /repos/{owner}/{repo}/pulls?state=open`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub draft: bool,
    pub created_at: Option<String>,
}

/// `GET /repos/{owner}/{repo}/actions/runs`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkflowRuns {
    pub workflow_runs: Vec<WorkflowRun>,
}

/// One workflow run. The list is newest first.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkflowRun {
    pub id: u64,
    /// Workflow name, used when the file name is unknown.
    pub name: Option<String>,
    /// Workflow file, such as `.github/workflows/ci.yml`.
    pub path: String,
    pub head_branch: Option<String>,
    pub head_sha: String,
    /// `requested`, `queued`, `waiting`, `pending`, `in_progress` or `completed`.
    pub status: Option<String>,
    /// `success`, `failure`, `timed_out`, `cancelled`, `skipped`, ... once completed.
    pub conclusion: Option<String>,
    pub created_at: Option<String>,
    /// Start of the latest attempt; a re-run moves it.
    pub run_started_at: Option<String>,
    /// For a completed run, when it finished.
    pub updated_at: Option<String>,
}

/// `GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs` (latest attempt).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Jobs {
    pub jobs: Vec<Job>,
}

/// One job of a run. Steps use the same shape as Gitea's, so the step
/// progress code is shared.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: u64,
    pub run_id: u64,
    pub name: String,
    pub head_sha: String,
    pub head_branch: Option<String>,
    /// `queued`, `waiting`, `pending`, `in_progress` or `completed`.
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at: Option<String>,
    pub steps: Vec<Step>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_parse() {
        let pulls: Vec<PullRequest> =
            serde_json::from_str(include_str!("testdata/pulls.json")).unwrap();
        assert_eq!(pulls.len(), 2);
        assert!(pulls[1].draft);

        let runs: WorkflowRuns = serde_json::from_str(include_str!("testdata/runs.json")).unwrap();
        assert_eq!(runs.workflow_runs.len(), 4);
        assert_eq!(runs.workflow_runs[0].status.as_deref(), Some("in_progress"));

        let jobs: Jobs = serde_json::from_str(include_str!("testdata/jobs.json")).unwrap();
        assert_eq!(jobs.jobs[0].steps.len(), 4);
    }

    #[test]
    fn nulls_do_not_fail_the_poll() {
        let run: WorkflowRun = serde_json::from_str(
            r#"{"id":1,"name":null,"head_branch":null,"status":"queued","conclusion":null}"#,
        )
        .unwrap();
        assert!(run.head_branch.is_none());
        let job: Job =
            serde_json::from_str(r#"{"id":2,"head_branch":null,"status":"queued"}"#).unwrap();
        assert!(job.steps.is_empty());
    }
}
