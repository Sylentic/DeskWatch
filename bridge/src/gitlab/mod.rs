//! GitLab source: open merge requests and pipelines from gitlab.com or a
//! self-managed GitLab, turned into the same CI facts as Gitea and GitHub.
//! Merge requests count as the open PRs.
//!
//! Like GitHub, this source only polls outbound:
//!
//! - every `poll_s` (60 s): open merge requests and the latest pipelines of
//!   each project. The result is also the source's health: a refused token or
//!   an unreachable server shows as the `warn` badge.
//! - every `job_poll_s` (5 s), only while a pipeline is running: that
//!   pipeline's jobs. GitLab jobs have no steps, so the progress bar counts
//!   stages: the step is the stage (or `stage: job` when one job runs), and the
//!   bar is the share of stages that are finished. A running pipeline is one
//!   "Running now" card, however many jobs run in parallel.
//! - every `runner_poll_s` (5 min), only when `runners` is set: the runners of
//!   those projects, groups or the instance. This does not touch the source's
//!   health, because a token without the runner right (a plain 403) must not
//!   grey out the PR and pipeline widgets.
//!
//! Failures back off (60 s, 2 min, 5 min).

pub mod api;
pub mod payload;
pub mod runners;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::alerts::{IssueWatch, NewIssue};
use crate::ci::{OpenPull, RepoPulls, Run, RunJob, RunStatus, RunningJob, Update};
use crate::config::GitlabConfig;
use crate::gitea::job_kind;
use crate::gitea::payload::unix_seconds;
use crate::model::{JobData, MAX_TITLE_CHARS, truncate};
use crate::source::{self, FactSink, Health, Interrupt, Source, SourceId, unix_now};
use api::HttpApi;
use payload::{Issue, Job, MergeRequest, Pipeline};
use runners::RunnerScope;

/// Adapter type: `[[source.gitlab]]` in the config and `source` on panel pages.
pub const KIND: &str = "gitlab";

/// Shown as the pipeline name when the project does not name its pipelines.
const DEFAULT_PIPELINE: &str = ".gitlab-ci.yml";

/// Poll delays after 1, 2, and 3 or more failed rounds in a row.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

// ---------------------------------------------------------------------------
// The source task
// ---------------------------------------------------------------------------

/// One `[[source.gitlab]]` instance, ready to run.
pub struct Gitlab {
    id: SourceId,
    config: GitlabConfig,
    api: HttpApi,
}

impl Gitlab {
    /// Load the token. Fails when the credential file is missing.
    pub fn new(config: &GitlabConfig) -> Result<Self> {
        let id = SourceId::new(KIND, &config.name);
        let token =
            source::load_secret(&config.token_file).with_context(|| format!("{id}: token_file"))?;
        let api = HttpApi::new(&config.base_url, token).context("GitLab API")?;
        Ok(Self {
            id,
            config: config.clone(),
            api,
        })
    }
}

impl Source for Gitlab {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    async fn run(self, sink: FactSink) -> Result<()> {
        let Gitlab { id, config, api } = self;
        info!(source = %id, base_url = %config.base_url, projects = config.projects.len(), "polling GitLab");
        let poll = Duration::from_secs(config.poll_s);
        let runner_scopes = config.runner_scopes()?;
        let mut state = GitlabState::new(
            id.clone(),
            config.interrupt,
            config.alias,
            config.notify_new_prs,
            unix_now(),
        )
        .with_site(&config.base_url)
        .with_issue_alerts(config.alert_new_issues);

        let mut next_poll = Instant::now();
        let mut failures = 0;
        let mut job_ticker = tokio::time::interval(Duration::from_secs(config.job_poll_s));
        job_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut runner_ticker = tokio::time::interval(Duration::from_secs(config.runner_poll_s));
        runner_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut reported = None;
        loop {
            let updates = tokio::select! {
                // Skipped altogether when no runner scopes are configured.
                _ = runner_ticker.tick(), if !runner_scopes.is_empty() => {
                    poll_runners(&api, &id, &state, &runner_scopes).await
                }
                _ = tokio::time::sleep_until(next_poll) => {
                    let mut updates = Vec::new();
                    for project in &config.projects {
                        updates.extend(poll_project(&api, &mut state, project).await);
                    }
                    failures = match state.health() {
                        Some(Health::Ok) | None => 0,
                        _ => failures + 1,
                    };
                    next_poll = Instant::now() + next_delay(poll, failures);
                    updates
                }
                _ = job_ticker.tick() => {
                    let (updates, pipelines_done) = poll_jobs(&api, &mut state).await;
                    // All jobs of a pipeline finished: fetch its result now
                    // instead of waiting for the next regular poll.
                    if pipelines_done {
                        next_poll = next_poll.min(Instant::now());
                    }
                    updates
                }
            };
            if !sink.facts(updates).await {
                break; // main loop has stopped
            }
            if let Some(health) = state.health()
                && reported != Some(health)
            {
                reported = Some(health);
                sink.health(health).await;
            }
        }
        Ok(())
    }
}

/// How long to wait before the next regular poll.
fn next_delay(poll: Duration, failures: usize) -> Duration {
    if failures == 0 {
        return poll;
    }
    poll.max(BACKOFF[(failures - 1).min(BACKOFF.len() - 1)])
}

/// Poll open merge requests and recent pipelines of one project.
async fn poll_project(api: &HttpApi, state: &mut GitlabState, project: &str) -> Vec<Update> {
    let mut out = Vec::new();
    let (merge_requests, total) = match api.open_merge_requests(project).await {
        Ok(found) => found,
        Err(err) => {
            warn!(%project, "cannot poll GitLab: {err:#}");
            state.set_health(project, Health::from_error(&err));
            return out;
        }
    };
    debug!(%project, open = merge_requests.len(), "polled open merge requests");
    out.extend(state.on_pulls(project, merge_requests, total, unix_now()));

    // New issues only add alerts: a failure here (issues switched off, a
    // token without the right) must not mark the source as broken.
    if state.alert_new_issues {
        match api.recent_issues(project).await {
            Ok(issues) => out.extend(state.on_issues(project, issues)),
            Err(err) => debug!(%project, "cannot poll issues: {err:#}"),
        }
    }

    let pipelines = match api.pipelines(project).await {
        Ok(pipelines) => pipelines,
        Err(err) => {
            warn!(%project, "cannot poll GitLab pipelines: {err:#}");
            state.set_health(project, Health::from_error(&err));
            return out;
        }
    };
    // Name the failing job in the red alert: one request per failed
    // pipeline, only the first time it is seen.
    for pipeline_id in state.need_failed_step(project, &pipelines) {
        let step = match api.failed_jobs(project, pipeline_id).await {
            Ok(jobs) => failing_step(&jobs),
            Err(err) => {
                warn!(%project, pipeline_id, "cannot fetch jobs of failed pipeline: {err:#}");
                None
            }
        };
        state.set_failed_step(project, pipeline_id, step);
    }
    out.extend(state.on_runs(project, pipelines, unix_now()));
    state.set_health(project, Health::Ok);
    out
}

/// Poll the runners of each scope. A scope that fails keeps its last list
/// (the kiosk greys it out when it gets old); a 403 usually means the token
/// lacks the runner right, see docs/runners.md.
async fn poll_runners(
    api: &HttpApi,
    id: &SourceId,
    state: &GitlabState,
    scopes: &[RunnerScope],
) -> Vec<Update> {
    let mut out = Vec::new();
    for scope in scopes {
        match api.runners(scope).await {
            Ok(runners) => {
                debug!(%scope, count = runners.len(), "polled runners");
                out.push(Update::Runners {
                    key: format!("{id}:runners:{scope}"),
                    runners: runners
                        .into_iter()
                        .map(|r| r.into_fact(KIND, state.runners_url(scope)))
                        .collect(),
                });
            }
            Err(err) => warn!(%scope, "cannot poll runners: {err:#}"),
        }
    }
    out
}

/// Poll the jobs of every running pipeline. Returns the updates and whether
/// some pipeline has all its jobs finished.
async fn poll_jobs(api: &HttpApi, state: &mut GitlabState) -> (Vec<Update>, bool) {
    let mut out = Vec::new();
    let mut pipelines_done = false;
    for (project, pipeline_id) in state.watched_pipelines() {
        match api.jobs(&project, pipeline_id).await {
            Ok(jobs) => {
                let (updates, done) = state.on_jobs(&project, pipeline_id, jobs, unix_now());
                out.extend(updates);
                pipelines_done |= done;
            }
            // Keep the last known progress; the next tick tries again.
            Err(err) => warn!(%project, pipeline_id, "cannot poll jobs: {err:#}"),
        }
    }
    (out, pipelines_done)
}

// ---------------------------------------------------------------------------
// Turning API answers into facts
// ---------------------------------------------------------------------------

/// A running pipeline whose jobs are polled for stage progress.
#[derive(Debug)]
struct WatchedPipeline {
    /// Pipeline name, as shown on the job card.
    name: String,
    git_ref: String,
    sha: String,
    /// When the pipeline was created, the start of a card without job times.
    created: u64,
    url: Option<String>,
    /// Is a card for it on screen right now?
    shown: bool,
}

/// State the GitLab source keeps between polls.
#[derive(Debug)]
pub struct GitlabState {
    id: SourceId,
    interrupt: Interrupt,
    alias: BTreeMap<String, String>,
    notify_new_prs: bool,
    /// Raise an alert for each new issue.
    alert_new_issues: bool,
    /// Issue numbers seen, for "new issue" alerts.
    issues: IssueWatch,
    /// Unix seconds when the source started. Pipelines that finished before
    /// then show on the pipelines page and in the badge, but do not take over
    /// the screen: they are old news after a bridge restart.
    since: u64,
    /// MR numbers per project from the last poll. Missing before the first
    /// poll, so existing MRs do not all flash "New PR" at startup.
    known_pulls: HashMap<String, HashSet<u64>>,
    /// Last `Run` sent per pipeline key, so only changes are sent.
    runs: HashMap<String, Run>,
    /// Running pipelines, by project and pipeline id.
    watched: BTreeMap<(String, u64), WatchedPipeline>,
    /// Failing job per failed pipeline (`None`: looked up, none found).
    failed_steps: HashMap<(String, u64), Option<String>>,
    /// Web address of the site, for links to runner pages. Empty: no links.
    site: String,
    /// Result of the last poll per project.
    health: BTreeMap<String, Health>,
}

impl GitlabState {
    pub fn new(
        id: SourceId,
        interrupt: Interrupt,
        alias: BTreeMap<String, String>,
        notify_new_prs: bool,
        since: u64,
    ) -> Self {
        Self {
            id,
            interrupt,
            alias,
            notify_new_prs,
            alert_new_issues: false,
            issues: IssueWatch::default(),
            since,
            known_pulls: HashMap::new(),
            runs: HashMap::new(),
            watched: BTreeMap::new(),
            failed_steps: HashMap::new(),
            site: String::new(),
            health: BTreeMap::new(),
        }
    }

    /// Raise an alert for each new issue (`alert_new_issues`).
    pub fn with_issue_alerts(mut self, on: bool) -> Self {
        self.alert_new_issues = on;
        self
    }

    /// The newest issues of one project from a poll: an alert for each
    /// issue above the highest number seen before.
    pub fn on_issues(&mut self, project: &str, issues: Vec<Issue>) -> Vec<Update> {
        let issues = issues
            .into_iter()
            .map(|i| NewIssue {
                number: i.iid,
                title: i.title,
                url: i.web_url,
                by: i.author.map(|a| a.username),
            })
            .collect();
        let label = source::label(&self.alias, project);
        self.issues
            .check(&self.id.to_string(), KIND, project, &label, issues)
            .into_iter()
            .map(|alert| Update::Alert { alert })
            .collect()
    }

    /// Give the kiosk links to runner pages, from the configured address.
    pub fn with_site(mut self, base_url: &str) -> Self {
        self.site = api::site_root(base_url);
        self
    }

    /// Page with the runners of one scope.
    pub fn runners_url(&self, scope: &RunnerScope) -> Option<String> {
        let path: Vec<&str> = match scope {
            RunnerScope::Instance => vec!["admin", "runners"],
            RunnerScope::Group(path) => vec!["groups", path, "-", "runners"],
            RunnerScope::Project(path) => vec![path, "-", "settings", "ci_cd"],
        };
        source::web_url(&self.site, &path, &[])
    }

    /// Health from the polls: the worst project wins. `None` before the
    /// first poll.
    pub fn health(&self) -> Option<Health> {
        self.health.values().max().copied()
    }

    pub fn set_health(&mut self, project: &str, health: Health) {
        self.health.insert(project.to_string(), health);
    }

    /// Pipelines whose jobs to poll, in a stable order.
    pub fn watched_pipelines(&self) -> Vec<(String, u64)> {
        self.watched.keys().cloned().collect()
    }

    /// Open merge requests of one project from a poll. `total` is the exact
    /// count when GitLab sent one; the list holds at most one page.
    pub fn on_pulls(
        &mut self,
        project: &str,
        pulls: Vec<MergeRequest>,
        total: Option<u32>,
        now: u64,
    ) -> Vec<Update> {
        let mut out = Vec::new();
        let key = self.project_key(project);
        let label = source::label(&self.alias, project);
        let numbers: HashSet<u64> = pulls.iter().map(|p| p.iid).collect();

        // New since the last poll: flash, unless it is the first poll or a draft.
        if let Some(known) = self.known_pulls.get(project)
            && self.notify_new_prs
        {
            for pull in pulls.iter().filter(|p| !known.contains(&p.iid) && !p.draft) {
                out.push(Update::PullOpened {
                    key: key.clone(),
                    project: label.clone(),
                    source: KIND.into(),
                    pull: open_pull(pull, now),
                    notify: true,
                });
            }
        }
        self.known_pulls.insert(project.to_string(), numbers);

        let listed: Vec<OpenPull> = pulls.iter().map(|p| open_pull(p, now)).collect();
        out.push(Update::Pulls {
            key,
            repo: RepoPulls {
                project: label,
                source: KIND.into(),
                total: total.unwrap_or(0).max(listed.len() as u32),
                pulls: listed,
            },
        });
        out
    }

    /// Failed pipelines, latest of their name, whose failing job has not been
    /// looked up yet.
    pub fn need_failed_step(&self, project: &str, pipelines: &[Pipeline]) -> Vec<u64> {
        latest_per_pipeline(pipelines)
            .into_iter()
            .filter(|p| status_of(p) == RunStatus::Failed)
            .filter(|p| !self.failed_steps.contains_key(&(project.to_string(), p.id)))
            .map(|p| p.id)
            .collect()
    }

    pub fn set_failed_step(&mut self, project: &str, pipeline_id: u64, step: Option<String>) {
        self.failed_steps
            .insert((project.to_string(), pipeline_id), step);
    }

    /// Recent pipelines of one project from a poll, newest first.
    pub fn on_runs(&mut self, project: &str, pipelines: Vec<Pipeline>, now: u64) -> Vec<Update> {
        let mut out = Vec::new();

        for pipeline in latest_per_pipeline(&pipelines) {
            let status = status_of(pipeline);
            let name = pipeline_name(pipeline);
            let started = unix_seconds(pipeline.created_at.as_deref()).unwrap_or(now);
            // The list has no finish time; the last update is the closest.
            let finished = matches!(
                status,
                RunStatus::Success | RunStatus::Failed | RunStatus::Neutral
            )
            .then(|| unix_seconds(pipeline.updated_at.as_deref()).unwrap_or(now));
            let fresh = finished.is_none_or(|f| f >= self.since);
            let step = self
                .failed_steps
                .get(&(project.to_string(), pipeline.id))
                .cloned()
                .flatten()
                .filter(|_| status == RunStatus::Failed);
            let fact = Run {
                source: KIND.into(),
                project: source::label(&self.alias, project),
                pipeline: name.clone(),
                git_ref: display_ref(pipeline.git_ref.as_deref().unwrap_or("")),
                status,
                step,
                started,
                finished,
                interrupt: fresh && self.interrupt.allows(project),
                url: pipeline.web_url.as_deref().and_then(source::safe_url),
            };
            let key = format!("{}:{project}:{name}", self.id);
            if self.runs.get(&key) != Some(&fact) {
                if finished.is_some() {
                    info!(source = %self.id, %project, pipeline = %name, ?status, "pipeline finished");
                }
                self.runs.insert(key.clone(), fact.clone());
                out.push(Update::Run { key, run: fact });
            }
        }

        // Watch running pipelines; stop watching the ones that finished or
        // dropped off the list. Quiet projects are watched too, for the
        // kiosk's "Running now" list; `interrupt` only decides about the ESP.
        let running: HashMap<u64, &Pipeline> = pipelines
            .iter()
            .filter(|p| status_of(p) == RunStatus::Running)
            .map(|p| (p.id, p))
            .collect();
        for (id, pipeline) in &running {
            self.watched
                .entry((project.to_string(), *id))
                .or_insert_with(|| WatchedPipeline {
                    name: pipeline_name(pipeline),
                    git_ref: display_ref(pipeline.git_ref.as_deref().unwrap_or("")),
                    sha: pipeline.sha.clone(),
                    created: unix_seconds(pipeline.created_at.as_deref()).unwrap_or(0),
                    url: pipeline.web_url.as_deref().and_then(source::safe_url),
                    shown: false,
                });
        }
        let over: Vec<(String, u64)> = self
            .watched
            .keys()
            .filter(|(p, id)| p == project && !running.contains_key(id))
            .cloned()
            .collect();
        for pipeline_ref in over {
            if let Some(watched) = self.watched.remove(&pipeline_ref)
                && watched.shown
            {
                out.push(Update::JobDone {
                    key: self.job_key(&pipeline_ref.0, pipeline_ref.1),
                });
            }
        }

        // Forget failing jobs of pipelines no longer listed.
        let listed: HashSet<u64> = pipelines.iter().map(|p| p.id).collect();
        self.failed_steps
            .retain(|(p, id), _| p != project || listed.contains(id));
        out
    }

    /// Jobs of a watched pipeline from a poll. Returns the updates and whether
    /// every job has finished (the pipeline result is then worth fetching).
    pub fn on_jobs(
        &mut self,
        project: &str,
        pipeline_id: u64,
        jobs: Vec<Job>,
        now: u64,
    ) -> (Vec<Update>, bool) {
        let mut out = Vec::new();
        let pipeline_ref = (project.to_string(), pipeline_id);
        // A late answer for a pipeline that already finished changes nothing.
        let Some(mut watched) = self.watched.remove(&pipeline_ref) else {
            return (out, false);
        };
        if !jobs.is_empty() {
            let key = self.job_key(project, pipeline_id);
            let data = self.job_data(project, &watched, &jobs, now);
            debug!(%key, step = ?data.step, progress = ?data.progress, "pipeline running");
            watched.shown = true;
            out.push(Update::RunJobs {
                key: key.clone(),
                jobs: run_jobs(&jobs),
            });
            out.push(Update::JobRunning {
                key: key.clone(),
                job: RunningJob {
                    data,
                    interrupt: self.interrupt.allows(project),
                    updated: now,
                    url: watched.url.clone(),
                    group: Some(key),
                },
            });
        }
        let all_done = !jobs.is_empty() && jobs.iter().all(|j| is_finished(&j.status));
        self.watched.insert(pipeline_ref, watched);
        (out, all_done)
    }

    fn job_data(
        &self,
        project: &str,
        watched: &WatchedPipeline,
        jobs: &[Job],
        now: u64,
    ) -> JobData {
        let progress = stage_progress(jobs);
        let running_name = jobs
            .iter()
            .find(|j| j.status == "running")
            .map_or("", |j| j.name.as_str());
        let started = jobs
            .iter()
            .filter_map(|j| unix_seconds(j.started_at.as_deref()))
            .min()
            .or(Some(watched.created).filter(|&c| c > 0))
            .unwrap_or(now);
        JobData {
            source: KIND.into(),
            project: truncate(&source::label(&self.alias, project), MAX_TITLE_CHARS),
            kind: job_kind(&watched.name, &format!("{running_name} {}", progress.stage)),
            pipeline: truncate(&watched.name, MAX_TITLE_CHARS),
            git_ref: truncate(&watched.git_ref, MAX_TITLE_CHARS),
            commit: watched.sha.chars().take(7).collect(),
            step: progress.step.map(|s| truncate(&s, MAX_TITLE_CHARS)),
            step_no: progress.step_no,
            step_count: progress.step_count,
            progress: progress.fraction,
            started,
            others: 0,
        }
    }

    /// Key for a running pipeline's card in the fact store.
    fn job_key(&self, project: &str, pipeline_id: u64) -> String {
        format!("{}:{project}:pipeline:{pipeline_id}", self.id)
    }

    /// Key for a project's merge requests in the fact store.
    fn project_key(&self, project: &str) -> String {
        format!("{}:{project}", self.id)
    }
}

fn open_pull(pull: &MergeRequest, now: u64) -> OpenPull {
    OpenPull {
        number: pull.iid,
        title: pull.title.clone(),
        created: unix_seconds(pull.created_at.as_deref()).unwrap_or(now),
        url: pull.web_url.as_deref().and_then(source::safe_url),
    }
}

/// The newest pipeline of each name, in list order (the API lists newest
/// first). Projects that do not name their pipelines have one, so the
/// latest pipeline of any branch is the one shown, as with GitHub.
fn latest_per_pipeline(pipelines: &[Pipeline]) -> Vec<&Pipeline> {
    let mut seen = HashSet::new();
    pipelines
        .iter()
        .filter(|p| seen.insert(pipeline_name(p)))
        .collect()
}

fn pipeline_name(pipeline: &Pipeline) -> String {
    pipeline
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(DEFAULT_PIPELINE)
        .to_string()
}

/// A branch or tag name for the card. Merge request pipelines run on
/// `refs/merge-requests/<n>/head` and show as `!<n>`.
fn display_ref(git_ref: &str) -> String {
    if let Some(rest) = git_ref.strip_prefix("refs/merge-requests/")
        && let Some((number, _)) = rest.split_once('/')
    {
        return format!("!{number}");
    }
    git_ref
        .strip_prefix("refs/heads/")
        .or_else(|| git_ref.strip_prefix("refs/tags/"))
        .unwrap_or(git_ref)
        .to_string()
}

fn status_of(pipeline: &Pipeline) -> RunStatus {
    pipeline_status(&pipeline.status)
}

/// Map a GitLab pipeline status to the shared run status (plan 8.1).
/// Cancelled and skipped pipelines never raise an alert. A `manual` pipeline
/// stopped at a gate somebody has to open, which reads as waiting.
fn pipeline_status(status: &str) -> RunStatus {
    match status {
        "success" | "success-with-warnings" => RunStatus::Success,
        "failed" => RunStatus::Failed,
        "running" => RunStatus::Running,
        "manual" => RunStatus::Waiting,
        "created" | "waiting_for_resource" | "preparing" | "pending" | "scheduled" => {
            RunStatus::Queued
        }
        // canceled, canceling, skipped, and anything GitLab adds later
        _ => RunStatus::Neutral,
    }
}

/// Every job of a running pipeline in pipeline order, for the kiosk. A
/// running job shows its stage; a failure that is allowed does not count.
fn run_jobs(jobs: &[Job]) -> Vec<RunJob> {
    let mut by_id: Vec<&Job> = jobs.iter().collect();
    by_id.sort_by_key(|j| j.id);
    by_id
        .into_iter()
        .map(|j| {
            let status = match pipeline_status(&j.status) {
                RunStatus::Failed if j.allow_failure => RunStatus::Neutral,
                s => s,
            };
            RunJob {
                name: truncate(&j.name, MAX_TITLE_CHARS),
                status,
                step: Some(truncate(&j.stage, MAX_TITLE_CHARS)).filter(|s| !s.is_empty()),
                progress: None,
                started: unix_seconds(j.started_at.as_deref()),
                finished: unix_seconds(j.finished_at.as_deref()),
                url: j.web_url.as_deref().and_then(source::safe_url),
            }
        })
        .collect()
}

/// Has this job stopped for good? A `manual` job waits for a person and does
/// not hold the pipeline back by itself.
fn is_finished(status: &str) -> bool {
    matches!(
        status,
        "success" | "failed" | "canceled" | "skipped" | "manual"
    )
}

/// Where a running pipeline stands, counted in stages.
#[derive(Debug, Default, PartialEq)]
struct StageProgress {
    /// Name of the current stage.
    stage: String,
    /// `stage`, or `stage: job` when exactly one job of it runs.
    step: Option<String>,
    step_no: Option<u32>,
    step_count: Option<u32>,
    /// Finished stages over all stages.
    fraction: Option<f32>,
}

/// Work out the current stage and progress from the job list. Stages are in
/// the order their first job was created. The current stage is the one with a
/// running job, else the first that is not finished.
fn stage_progress(jobs: &[Job]) -> StageProgress {
    let mut by_id: Vec<&Job> = jobs.iter().collect();
    by_id.sort_by_key(|j| j.id);
    let mut stages: Vec<(&str, Vec<&Job>)> = Vec::new();
    for job in by_id {
        match stages.iter_mut().find(|(name, _)| *name == job.stage) {
            Some((_, list)) => list.push(job),
            None => stages.push((&job.stage, vec![job])),
        }
    }
    if stages.is_empty() {
        return StageProgress::default();
    }
    let finished = |list: &[&Job]| list.iter().all(|j| is_finished(&j.status));
    let done = stages.iter().filter(|(_, list)| finished(list)).count();
    let current = stages
        .iter()
        .position(|(_, list)| list.iter().any(|j| j.status == "running"))
        .or_else(|| stages.iter().position(|(_, list)| !finished(list)));
    let (stage, step) = match current {
        Some(i) => {
            let (name, list) = &stages[i];
            let running: Vec<&&Job> = list.iter().filter(|j| j.status == "running").collect();
            let step = match running.as_slice() {
                [one] => format!("{name}: {}", one.name),
                _ => name.to_string(),
            };
            (name.to_string(), Some(step))
        }
        None => (String::new(), None),
    };
    StageProgress {
        stage,
        step,
        step_no: current.map(|i| i as u32 + 1),
        step_count: Some(stages.len() as u32),
        fraction: Some(done as f32 / stages.len() as f32),
    }
}

/// The first failed job that does not allow failure, for the red alert.
fn failing_step(jobs: &[Job]) -> Option<String> {
    jobs.iter()
        .filter(|j| j.status == "failed" && !j.allow_failure)
        .min_by_key(|j| j.id)
        .map(|j| j.name.clone())
}

#[cfg(test)]
mod tests;
