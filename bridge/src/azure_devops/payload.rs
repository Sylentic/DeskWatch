//! Azure DevOps REST API JSON shapes (API version 7.1): the three answers the
//! bridge polls. Only the fields the bridge uses are listed; serde ignores the
//! rest. Everything has a default, so an odd value degrades to "unknown"
//! instead of failing the whole poll.

use serde::Deserialize;

/// List answers wrap the items as `{ "count": 2, "value": [...] }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct List<T> {
    pub value: Vec<T>,
}

impl<T> Default for List<T> {
    fn default() -> Self {
        Self { value: Vec::new() }
    }
}

/// One item of `GET {project}/_apis/build/builds`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Build {
    pub id: u64,
    /// `inProgress`, `notStarted`, `completed`, `cancelling`, `postponed` or `none`.
    pub status: Option<String>,
    /// `succeeded`, `partiallySucceeded`, `failed` or `canceled`, once completed.
    pub result: Option<String>,
    pub queue_time: Option<String>,
    pub start_time: Option<String>,
    pub finish_time: Option<String>,
    /// Full ref, such as `refs/heads/main` or `refs/pull/12/merge`.
    pub source_branch: Option<String>,
    pub source_version: Option<String>,
    pub definition: Definition,
}

/// The pipeline a build belongs to.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Definition {
    pub id: u64,
    pub name: String,
}

/// One item of `GET {project}/_apis/git/pullrequests`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PullRequest {
    pub pull_request_id: u64,
    pub title: String,
    pub is_draft: bool,
    pub creation_date: Option<String>,
    pub repository: Repository,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Repository {
    pub name: String,
}

/// One discussion of `GET .../pullRequests/{id}/threads`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Thread {
    pub id: u64,
    pub is_deleted: bool,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Comment {
    pub id: u64,
    pub author: Identity,
    pub content: String,
    /// `text` for people; `system` for votes, pushes and status changes.
    pub comment_type: String,
    pub is_deleted: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Identity {
    pub id: String,
    pub display_name: String,
}

/// `GET _apis/connectionData`: who the token belongs to.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConnectionData {
    pub authenticated_user: Identity,
}

/// `GET {project}/_apis/build/builds/{id}/timeline`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Timeline {
    pub records: Vec<Record>,
}

/// One timeline record: a stage, a job inside it, a task inside that, or an
/// approval check.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub parent_id: Option<String>,
    /// `Stage`, `Phase`, `Job`, `Task`, `Checkpoint`, `Checkpoint.Approval`, ...
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    /// `pending`, `inProgress` or `completed`.
    pub state: Option<String>,
    /// `succeeded`, `succeededWithIssues`, `failed`, `canceled`, `skipped` or `abandoned`.
    pub result: Option<String>,
    pub order: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_parse() {
        let builds: List<Build> =
            serde_json::from_str(include_str!("testdata/builds.json")).unwrap();
        assert_eq!(builds.value.len(), 5);
        assert_eq!(builds.value[1].definition.name, "deploy-infra");
        assert_eq!(builds.value[1].status.as_deref(), Some("inProgress"));

        let pulls: List<PullRequest> =
            serde_json::from_str(include_str!("testdata/pulls.json")).unwrap();
        assert_eq!(pulls.value.len(), 3);
        assert!(pulls.value[2].is_draft);

        let timeline: Timeline =
            serde_json::from_str(include_str!("testdata/timeline_running.json")).unwrap();
        assert!(timeline.records.iter().any(|r| r.kind == "Stage"));
    }

    #[test]
    fn nulls_do_not_fail_the_poll() {
        let build: Build = serde_json::from_str(
            r#"{"id":1,"status":"notStarted","result":null,"startTime":null,"sourceBranch":null,
                "definition":{"id":3,"name":"x"}}"#,
        )
        .unwrap();
        assert!(build.start_time.is_none());
        let record: Record =
            serde_json::from_str(r#"{"id":"a","parentId":null,"type":"Task","state":null}"#)
                .unwrap();
        assert!(record.state.is_none());
    }
}

/// One item of `GET _apis/distributedtask/pools`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Pool {
    pub id: u64,
    pub name: String,
    /// Microsoft-hosted pools are never listed: they cannot be down.
    pub is_hosted: bool,
}

/// One item of `GET _apis/distributedtask/pools/{id}/agents`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Agent {
    pub name: String,
    /// `online` or `offline`.
    pub status: String,
    /// `false` when an administrator disabled the agent. Missing counts as enabled.
    pub enabled: Option<bool>,
    /// Present (with `includeAssignedRequest=true`) while the agent runs a job.
    /// Only its presence matters, so the content is not read.
    pub assigned_request: Option<serde::de::IgnoredAny>,
}

/// One item of `GET _apis/distributedtask/pools/{id}/jobrequests`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct JobRequest {
    /// Set once an agent took the job.
    pub assign_time: Option<String>,
    /// Set once the job is over.
    pub finish_time: Option<String>,
}

impl JobRequest {
    /// Assumption, see docs/runners.md: a request nobody took and nobody
    /// finished is waiting for an agent.
    pub fn is_queued(&self) -> bool {
        self.assign_time.is_none() && self.finish_time.is_none()
    }
}
