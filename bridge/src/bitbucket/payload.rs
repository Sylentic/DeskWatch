//! Bitbucket Cloud REST API 2.0 JSON shapes: the lists the bridge polls. Only
//! the fields the bridge uses are listed; serde ignores the rest. Everything
//! has a default and nullable fields are `Option`, so an odd value degrades to
//! "unknown" instead of failing the whole poll.
//!
//! Names and enums follow the API's published swagger description
//! (`https://api.bitbucket.org/swagger.json`).

use serde::Deserialize;

/// One page of any list: `values`, a `next` link when there is more, and
/// `size` (the total) when Bitbucket bothers to compute it.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Page<T> {
    pub values: Vec<T>,
    pub next: Option<String>,
    pub size: Option<u32>,
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            next: None,
            size: None,
        }
    }
}

/// `{ "href": "..." }`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Href {
    pub href: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Links {
    /// Web page of the object.
    pub html: Href,
}

/// One item of `GET /repositories/{workspace}/{repo_slug}/pullrequests`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PullRequest {
    /// Number inside the repository.
    pub id: u64,
    pub title: String,
    pub draft: bool,
    pub created_on: Option<String>,
    pub links: Links,
}

/// A state or result object; only its `name` matters.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Named {
    pub name: String,
}

/// State of a pipeline or a step. `PENDING`, `IN_PROGRESS` (with a `stage` of
/// `RUNNING` or `PAUSED`) or `COMPLETED` (with a `result`). A step also knows
/// `READY`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct State {
    pub name: String,
    pub stage: Option<Named>,
    /// `SUCCESSFUL`, `FAILED`, `ERROR`, `STOPPED`, `EXPIRED`, `NOT_RUN`.
    pub result: Option<Named>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Selector {
    /// `branches`, `tags`, `bookmarks`, `default` or `custom`.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// The name of the matching pipeline definition.
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Commit {
    pub hash: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Target {
    pub ref_name: Option<String>,
    pub commit: Option<Commit>,
    pub selector: Option<Selector>,
}

/// One item of `GET /repositories/{workspace}/{repo_slug}/pipelines`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Pipeline {
    /// `{...}` with the braces.
    pub uuid: String,
    pub build_number: u64,
    pub state: State,
    pub target: Target,
    pub created_on: Option<String>,
    /// Not set while the pipeline is still going.
    pub completed_on: Option<String>,
}

/// One item of `GET .../pipelines/{uuid}/steps`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Step {
    pub uuid: String,
    /// The step's name from `bitbucket-pipelines.yml`. The published swagger
    /// does not list it, so it is optional.
    pub name: Option<String>,
    pub state: State,
    pub started_on: Option<String>,
}

/// State of a runner.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RunnerState {
    /// `UNREGISTERED`, `ONLINE`, `OFFLINE`, `DISABLED`, `ENABLED`, `UNHEALTHY`.
    pub status: String,
}

/// One item of `GET .../pipelines-config/runners`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiRunner {
    pub uuid: String,
    pub name: String,
    pub labels: Vec<String>,
    pub state: RunnerState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_parse() {
        let prs: Page<PullRequest> =
            serde_json::from_str(include_str!("testdata/pullrequests.json")).unwrap();
        assert_eq!(prs.values.len(), 2);
        assert_eq!(prs.size, Some(2));
        assert!(prs.values[1].draft);
        assert!(prs.values[0].links.html.href.is_some());

        let pipelines: Page<Pipeline> =
            serde_json::from_str(include_str!("testdata/pipelines.json")).unwrap();
        assert_eq!(pipelines.values.len(), 4);
        assert_eq!(pipelines.values[0].state.name, "IN_PROGRESS");
        assert_eq!(pipelines.values[0].target.ref_name.as_deref(), Some("main"));

        let steps: Page<Step> = serde_json::from_str(include_str!("testdata/steps.json")).unwrap();
        assert_eq!(steps.values.len(), 4);

        let runners: Page<ApiRunner> =
            serde_json::from_str(include_str!("testdata/runners.json")).unwrap();
        assert_eq!(runners.values.len(), 4);
    }

    #[test]
    fn nulls_and_gaps_do_not_fail_the_poll() {
        let pipeline: Pipeline =
            serde_json::from_str(r#"{"uuid":"{a}","state":{"name":"PENDING","stage":null}}"#)
                .unwrap();
        assert_eq!(pipeline.state.name, "PENDING");
        let step: Step =
            serde_json::from_str(r#"{"uuid":"{s}","state":{"name":"READY"}}"#).unwrap();
        assert!(step.name.is_none());
        let page: Page<Step> = serde_json::from_str("{}").unwrap();
        assert!(page.values.is_empty() && page.next.is_none());
    }
}
