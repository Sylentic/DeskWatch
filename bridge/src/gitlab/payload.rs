//! GitLab REST API (v4) JSON shapes: the lists the bridge polls. Only the
//! fields the bridge uses are listed; serde ignores the rest. Everything has
//! a default and nullable fields are `Option`, so an odd value degrades to
//! "unknown" instead of failing the whole poll.
//!
//! The same shapes come from gitlab.com and from self-managed GitLab.

use serde::Deserialize;

/// One item of `GET /projects/:id/merge_requests?state=opened`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MergeRequest {
    /// Number inside the project (`!42`), not the global `id`.
    pub iid: u64,
    pub title: String,
    pub draft: bool,
    pub created_at: Option<String>,
    /// Web page of the merge request.
    pub web_url: Option<String>,
}

/// One item of `GET /projects/:id/pipelines`, newest first.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Pipeline {
    pub id: u64,
    /// Pipeline name (`workflow:name`), only on recent GitLab versions and
    /// only when the project sets one.
    pub name: Option<String>,
    /// `created`, `waiting_for_resource`, `preparing`, `pending`, `running`,
    /// `success`, `failed`, `canceled`, `skipped`, `manual`, `scheduled`, ...
    pub status: String,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub sha: String,
    pub created_at: Option<String>,
    /// For a finished pipeline, about when it finished.
    pub updated_at: Option<String>,
    /// Web page of the pipeline.
    pub web_url: Option<String>,
}

/// One item of `GET /projects/:id/pipelines/:pipeline_id/jobs` (latest
/// attempt of each job).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub stage: String,
    /// `created`, `pending`, `running`, `success`, `failed`, `canceled`,
    /// `skipped`, `manual`, `scheduled`, ...
    pub status: String,
    /// A failed job that does not fail the pipeline.
    pub allow_failure: bool,
    pub started_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_parse() {
        let mrs: Vec<MergeRequest> =
            serde_json::from_str(include_str!("testdata/merge_requests.json")).unwrap();
        assert_eq!(mrs.len(), 2);
        assert!(mrs[1].draft);

        let pipelines: Vec<Pipeline> =
            serde_json::from_str(include_str!("testdata/pipelines.json")).unwrap();
        assert_eq!(pipelines.len(), 4);
        assert_eq!(pipelines[0].git_ref.as_deref(), Some("main"));

        let jobs: Vec<Job> = serde_json::from_str(include_str!("testdata/jobs.json")).unwrap();
        assert_eq!(jobs.len(), 5);
    }

    #[test]
    fn nulls_do_not_fail_the_poll() {
        let pipeline: Pipeline = serde_json::from_str(
            r#"{"id":1,"name":null,"ref":null,"status":"pending","updated_at":null}"#,
        )
        .unwrap();
        assert!(pipeline.git_ref.is_none());
        let job: Job =
            serde_json::from_str(r#"{"id":2,"started_at":null,"status":"created"}"#).unwrap();
        assert!(job.stage.is_empty());
    }
}
