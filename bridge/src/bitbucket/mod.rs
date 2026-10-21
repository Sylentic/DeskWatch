//! Bitbucket Cloud source: open pull requests and Pipelines runs from
//! bitbucket.org, turned into the same CI facts as the other sources.
//!
//! Like GitHub and GitLab, this source only polls outbound:
//!
//! - every `poll_s` (120 s): open pull requests and the newest pipelines of
//!   each repository. The result is also the source's health: a refused token
//!   or an unreachable server shows as the `warn` badge.
//! - every `job_poll_s` (10 s), only while a pipeline is in progress: that
//!   pipeline's steps, for the step progress bar. A running pipeline is one
//!   "Running now" card; the bar is the share of finished steps.
//! - every `runner_poll_s` (5 min), only when `runners` is set: the
//!   self-hosted runners of those repositories and workspaces. This does not
//!   touch the source's health, because a token without the runner right (a
//!   plain 403) must not grey out the PR and pipeline widgets.
//!
//! Bitbucket Cloud limits the requests a token may make per hour, so the
//! defaults are gentler than for the other sources. Failures back off (2, 3,
//! then 5 minutes).

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
use crate::config::BitbucketConfig;
use crate::gitea::job_kind;
use crate::gitea::payload::unix_seconds;
use crate::model::{JobData, MAX_TITLE_CHARS, truncate};
use crate::source::{self, FactSink, Health, Interrupt, Source, SourceId, unix_now};
use api::HttpApi;
use payload::{Issue, Pipeline, PullRequest, Step};
use runners::RunnerScope;

/// Adapter type: `[[source.bitbucket]]` in the config and `source` on panel pages.
pub const KIND: &str = "bitbucket";

/// Shown as the pipeline name when it is not a named custom pipeline.
const DEFAULT_PIPELINE: &str = "bitbucket-pipelines.yml";

/// Poll delays after 1, 2, and 3 or more failed rounds in a row.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(120),
    Duration::from_secs(180),
    Duration::from_secs(300),
];

// ---------------------------------------------------------------------------
// The source task
// ---------------------------------------------------------------------------

/// One `[[source.bitbucket]]` instance, ready to run.
pub struct Bitbucket {
    id: SourceId,
    config: BitbucketConfig,
    api: HttpApi,
}

impl Bitbucket {
    /// Load the token. Fails when the credential file is missing.
    pub fn new(config: &BitbucketConfig) -> Result<Self> {
        let id = SourceId::new(KIND, &config.name);
        let token =
            source::load_secret(&config.token_file).with_context(|| format!("{id}: token_file"))?;
        let api = HttpApi::new(&config.base_url, token).context("Bitbucket API")?;
        Ok(Self {
            id,
            config: config.clone(),
            api,
        })
    }
}

impl Source for Bitbucket {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    async fn run(self, sink: FactSink) -> Result<()> {
        let Bitbucket { id, config, api } = self;
        info!(source = %id, repos = config.repos.len(), "polling Bitbucket");
        let poll = Duration::from_secs(config.poll_s);
        let runner_scopes = config.runner_scopes()?;
        let mut state = BitbucketState::new(
            id.clone(),
            config.interrupt,
            config.alias,
            config.notify_new_prs,
            unix_now(),
        )
        .with_api_base(&config.base_url)
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
                    for repo in &config.repos {
                        updates.extend(poll_repo(&api, &mut state, repo).await);
                    }
                    failures = match state.health() {
                        Some(Health::Ok) | None => 0,
                        _ => failures + 1,
                    };
                    next_poll = Instant::now() + next_delay(poll, failures);
                    updates
                }
                _ = job_ticker.tick() => {
                    let (updates, pipelines_done) = poll_steps(&api, &mut state).await;
                    // Every step of a pipeline finished: fetch its result now
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

/// Poll open pull requests and recent pipelines of one repository.
async fn poll_repo(api: &HttpApi, state: &mut BitbucketState, repo: &str) -> Vec<Update> {
    let mut out = Vec::new();
    let (pulls, total) = match api.open_pull_requests(repo).await {
        Ok(found) => found,
        Err(err) => {
            warn!(%repo, "cannot poll Bitbucket: {err:#}");
            state.set_health(repo, Health::from_error(&err));
            return out;
        }
    };
    debug!(%repo, open = pulls.len(), "polled open pull requests");
    out.extend(state.on_pulls(repo, pulls, total, unix_now()));

    // New issues only add alerts: a failure here (issues switched off, a
    // token without the right) must not mark the source as broken.
    if state.alert_new_issues {
        match api.recent_issues(repo).await {
            Ok(issues) => out.extend(state.on_issues(repo, issues)),
            Err(err) => debug!(%repo, "cannot poll issues: {err:#}"),
        }
    }

    let pipelines = match api.pipelines(repo).await {
        Ok(pipelines) => pipelines,
        Err(err) => {
            warn!(%repo, "cannot poll Bitbucket pipelines: {err:#}");
            state.set_health(repo, Health::from_error(&err));
            return out;
        }
    };
    // Name the failing step in the red alert: one request per failed
    // pipeline, only the first time it is seen.
    for (build, uuid) in state.need_failed_step(repo, &pipelines) {
        let step = match api.steps(repo, &uuid).await {
            Ok(steps) => failing_step(&steps),
            Err(err) => {
                warn!(%repo, build, "cannot fetch steps of failed pipeline: {err:#}");
                None
            }
        };
        state.set_failed_step(repo, build, step);
    }
    out.extend(state.on_runs(repo, pipelines, unix_now()));
    state.set_health(repo, Health::Ok);
    out
}

/// Poll the runners of each scope. A scope that fails keeps its last list
/// (the kiosk greys it out when it gets old); a 403 usually means the token
/// lacks the runner right, see docs/runners.md.
async fn poll_runners(
    api: &HttpApi,
    id: &SourceId,
    state: &BitbucketState,
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

/// Poll the steps of every pipeline in progress. Returns the updates and
/// whether some pipeline has all its steps finished.
async fn poll_steps(api: &HttpApi, state: &mut BitbucketState) -> (Vec<Update>, bool) {
    let mut out = Vec::new();
    let mut pipelines_done = false;
    for (repo, build, uuid) in state.watched_pipelines() {
        match api.steps(&repo, &uuid).await {
            Ok(steps) => {
                let (updates, done) = state.on_steps(&repo, build, steps, unix_now());
                out.extend(updates);
                pipelines_done |= done;
            }
            // Keep the last known progress; the next tick tries again.
            Err(err) => warn!(%repo, build, "cannot poll steps: {err:#}"),
        }
    }
    (out, pipelines_done)
}

// ---------------------------------------------------------------------------
// Turning API answers into facts
// ---------------------------------------------------------------------------

/// A pipeline in progress whose steps are polled for step progress.
#[derive(Debug)]
struct WatchedPipeline {
    /// UUID with braces, for the steps request.
    uuid: String,
    /// Pipeline name, as shown on the job card.
    name: String,
    git_ref: String,
    sha: String,
    /// When the pipeline was created, the start of a card without step times.
    created: u64,
    url: Option<String>,
    /// Is a card for it on screen right now?
    shown: bool,
}

/// State the Bitbucket source keeps between polls.
#[derive(Debug)]
pub struct BitbucketState {
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
    /// PR numbers per repository from the last poll. Missing before the first
    /// poll, so existing PRs do not all flash "New PR" at startup.
    known_pulls: HashMap<String, HashSet<u64>>,
    /// Last `Run` sent per pipeline key, so only changes are sent.
    runs: HashMap<String, Run>,
    /// Pipelines in progress, by repository and build number.
    watched: BTreeMap<(String, u64), WatchedPipeline>,
    /// Failing step per failed pipeline (`None`: looked up, none found).
    failed_steps: HashMap<(String, u64), Option<String>>,
    /// Web address of the site (bitbucket.org), for links. Empty: no links.
    web: String,
    /// Result of the last poll per repository.
    health: BTreeMap<String, Health>,
}

impl BitbucketState {
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
            web: String::new(),
            health: BTreeMap::new(),
        }
    }

    /// Raise an alert for each new issue (`alert_new_issues`).
    pub fn with_issue_alerts(mut self, on: bool) -> Self {
        self.alert_new_issues = on;
        self
    }

    /// The newest issues of one repo from a poll: an alert for each
    /// issue above the highest number seen before.
    pub fn on_issues(&mut self, repo: &str, issues: Vec<Issue>) -> Vec<Update> {
        let issues = issues
            .into_iter()
            .map(|i| NewIssue {
                number: i.id,
                title: i.title,
                url: i.links.html.href,
                by: i.reporter.map(|a| a.display_name),
            })
            .collect();
        let label = source::label(&self.alias, repo);
        self.issues
            .check(&self.id.to_string(), KIND, repo, &label, issues)
            .into_iter()
            .map(|alert| Update::Alert { alert })
            .collect()
    }

    /// Give the cards links to the pipeline pages, from the API address.
    pub fn with_api_base(mut self, api_base: &str) -> Self {
        self.web = web_base(api_base).unwrap_or_default();
        self
    }

    /// Page with the runners of one scope. Bitbucket has none that is known
    /// to be stable, so there is no link.
    pub fn runners_url(&self, _scope: &RunnerScope) -> Option<String> {
        None
    }

    /// Health from the polls: the worst repository wins. `None` before the
    /// first poll.
    pub fn health(&self) -> Option<Health> {
        self.health.values().max().copied()
    }

    pub fn set_health(&mut self, repo: &str, health: Health) {
        self.health.insert(repo.to_string(), health);
    }

    /// Pipelines whose steps to poll, as (repository, build number, UUID).
    pub fn watched_pipelines(&self) -> Vec<(String, u64, String)> {
        self.watched
            .iter()
            .map(|((repo, build), w)| (repo.clone(), *build, w.uuid.clone()))
            .collect()
    }

    /// Open pull requests of one repository from a poll. `total` is the exact
    /// count when Bitbucket sent one; the list holds a few pages at most.
    pub fn on_pulls(
        &mut self,
        repo: &str,
        pulls: Vec<PullRequest>,
        total: Option<u32>,
        now: u64,
    ) -> Vec<Update> {
        let mut out = Vec::new();
        let key = self.repo_key(repo);
        let label = source::label(&self.alias, repo);
        let numbers: HashSet<u64> = pulls.iter().map(|p| p.id).collect();

        // New since the last poll: flash, unless it is the first poll or a draft.
        if let Some(known) = self.known_pulls.get(repo)
            && self.notify_new_prs
        {
            for pull in pulls.iter().filter(|p| !known.contains(&p.id) && !p.draft) {
                out.push(Update::PullOpened {
                    key: key.clone(),
                    project: label.clone(),
                    source: KIND.into(),
                    pull: open_pull(pull, now),
                    notify: true,
                });
            }
        }
        self.known_pulls.insert(repo.to_string(), numbers);

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

    /// Failed pipelines, latest of their name, whose failing step has not been
    /// looked up yet, as (build number, UUID).
    pub fn need_failed_step(&self, repo: &str, pipelines: &[Pipeline]) -> Vec<(u64, String)> {
        latest_per_pipeline(pipelines)
            .into_iter()
            .filter(|p| status_of(p) == RunStatus::Failed)
            .filter(|p| {
                !self
                    .failed_steps
                    .contains_key(&(repo.to_string(), p.build_number))
            })
            .map(|p| (p.build_number, p.uuid.clone()))
            .collect()
    }

    pub fn set_failed_step(&mut self, repo: &str, build: u64, step: Option<String>) {
        self.failed_steps.insert((repo.to_string(), build), step);
    }

    /// Recent pipelines of one repository from a poll, newest first.
    pub fn on_runs(&mut self, repo: &str, pipelines: Vec<Pipeline>, now: u64) -> Vec<Update> {
        let mut out = Vec::new();

        for pipeline in latest_per_pipeline(&pipelines) {
            let status = status_of(pipeline);
            let name = pipeline_name(pipeline);
            let started = unix_seconds(pipeline.created_on.as_deref()).unwrap_or(now);
            let finished = matches!(
                status,
                RunStatus::Success | RunStatus::Failed | RunStatus::Neutral
            )
            .then(|| unix_seconds(pipeline.completed_on.as_deref()).unwrap_or(now));
            let fresh = finished.is_none_or(|f| f >= self.since);
            let step = self
                .failed_steps
                .get(&(repo.to_string(), pipeline.build_number))
                .cloned()
                .flatten()
                .filter(|_| status == RunStatus::Failed);
            let fact = Run {
                source: KIND.into(),
                project: source::label(&self.alias, repo),
                pipeline: name.clone(),
                git_ref: git_ref_of(pipeline),
                status,
                step,
                started,
                finished,
                interrupt: fresh && self.interrupt.allows(repo),
                url: self.pipeline_url(repo, pipeline.build_number),
            };
            let key = format!("{}:{repo}:{name}", self.id);
            if self.runs.get(&key) != Some(&fact) {
                if finished.is_some() {
                    info!(source = %self.id, %repo, pipeline = %name, ?status, "pipeline finished");
                }
                self.runs.insert(key.clone(), fact.clone());
                out.push(Update::Run { key, run: fact });
            }
        }

        // Watch pipelines in progress; stop watching the ones that finished or
        // dropped off the list. A paused pipeline waits for a person and is
        // not polled. Quiet repositories are watched too, for the kiosk's
        // "Running now" list; `interrupt` only decides about the ESP.
        let running: HashMap<u64, &Pipeline> = pipelines
            .iter()
            .filter(|p| status_of(p) == RunStatus::Running)
            .map(|p| (p.build_number, p))
            .collect();
        for (build, pipeline) in &running {
            let url = self.pipeline_url(repo, *build);
            self.watched
                .entry((repo.to_string(), *build))
                .or_insert_with(|| WatchedPipeline {
                    uuid: pipeline.uuid.clone(),
                    name: pipeline_name(pipeline),
                    git_ref: git_ref_of(pipeline),
                    sha: sha_of(pipeline),
                    created: unix_seconds(pipeline.created_on.as_deref()).unwrap_or(0),
                    url,
                    shown: false,
                });
        }
        let over: Vec<(String, u64)> = self
            .watched
            .keys()
            .filter(|(r, build)| r == repo && !running.contains_key(build))
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

        // Forget failing steps of pipelines no longer listed.
        let listed: HashSet<u64> = pipelines.iter().map(|p| p.build_number).collect();
        self.failed_steps
            .retain(|(r, build), _| r != repo || listed.contains(build));
        out
    }

    /// Steps of a watched pipeline from a poll. Returns the updates and whether
    /// every step has finished (the pipeline result is then worth fetching).
    pub fn on_steps(
        &mut self,
        repo: &str,
        build: u64,
        steps: Vec<Step>,
        now: u64,
    ) -> (Vec<Update>, bool) {
        let mut out = Vec::new();
        let pipeline_ref = (repo.to_string(), build);
        // A late answer for a pipeline that already finished changes nothing.
        let Some(mut watched) = self.watched.remove(&pipeline_ref) else {
            return (out, false);
        };
        if !steps.is_empty() {
            let key = self.job_key(repo, build);
            let data = self.job_data(repo, &watched, &steps, now);
            debug!(%key, step = ?data.step, progress = ?data.progress, "pipeline running");
            watched.shown = true;
            out.push(Update::RunJobs {
                key: key.clone(),
                jobs: run_jobs(&steps),
            });
            out.push(Update::JobRunning {
                key: key.clone(),
                job: RunningJob {
                    data,
                    interrupt: self.interrupt.allows(repo),
                    updated: now,
                    url: watched.url.clone(),
                    group: Some(key),
                },
            });
        }
        let all_done = !steps.is_empty() && steps.iter().all(|s| is_finished(&s.state.name));
        self.watched.insert(pipeline_ref, watched);
        (out, all_done)
    }

    fn job_data(&self, repo: &str, watched: &WatchedPipeline, steps: &[Step], now: u64) -> JobData {
        let progress = step_progress(steps);
        let started = steps
            .iter()
            .filter_map(|s| unix_seconds(s.started_on.as_deref()))
            .min()
            .or(Some(watched.created).filter(|&c| c > 0))
            .unwrap_or(now);
        JobData {
            source: KIND.into(),
            project: truncate(&source::label(&self.alias, repo), MAX_TITLE_CHARS),
            kind: job_kind(&watched.name, progress.step.as_deref().unwrap_or("")),
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

    /// Web page of a pipeline run.
    fn pipeline_url(&self, repo: &str, build: u64) -> Option<String> {
        source::web_url(
            &self.web,
            &[repo, "pipelines", "results", &build.to_string()],
            &[],
        )
    }

    /// Key for a running pipeline's card in the fact store.
    fn job_key(&self, repo: &str, build: u64) -> String {
        format!("{}:{repo}:pipeline:{build}", self.id)
    }

    /// Key for a repository's pull requests in the fact store.
    fn repo_key(&self, repo: &str) -> String {
        format!("{}:{repo}", self.id)
    }
}

/// The site address that goes with an API address: `api.bitbucket.org` is
/// `bitbucket.org`. `None` for anything else, so no wrong link is made.
fn web_base(api_base: &str) -> Option<String> {
    let url = reqwest::Url::parse(api_base.trim()).ok()?;
    let site = url
        .host_str()?
        .strip_prefix("api.")
        .filter(|s| *s == "bitbucket.org")?;
    Some(format!("{}://{site}", url.scheme()))
}

fn open_pull(pull: &PullRequest, now: u64) -> OpenPull {
    OpenPull {
        number: pull.id,
        title: pull.title.clone(),
        created: unix_seconds(pull.created_on.as_deref()).unwrap_or(now),
        url: pull.links.html.href.as_deref().and_then(source::safe_url),
    }
}

/// The newest pipeline of each name, in list order (the API lists newest
/// first when sorted by `-created_on`).
fn latest_per_pipeline(pipelines: &[Pipeline]) -> Vec<&Pipeline> {
    let mut seen = HashSet::new();
    pipelines
        .iter()
        .filter(|p| seen.insert(pipeline_name(p)))
        .collect()
}

/// A custom pipeline shows by its name (`deploy-prod`); every other pipeline
/// is the one defined in the repository's `bitbucket-pipelines.yml`.
fn pipeline_name(pipeline: &Pipeline) -> String {
    pipeline
        .target
        .selector
        .as_ref()
        .filter(|s| s.kind.as_deref() == Some("custom"))
        .and_then(|s| s.pattern.as_deref())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(DEFAULT_PIPELINE)
        .to_string()
}

fn git_ref_of(pipeline: &Pipeline) -> String {
    pipeline.target.ref_name.clone().unwrap_or_default()
}

fn sha_of(pipeline: &Pipeline) -> String {
    pipeline
        .target
        .commit
        .as_ref()
        .map(|c| c.hash.clone())
        .unwrap_or_default()
}

fn status_of(pipeline: &Pipeline) -> RunStatus {
    let state = &pipeline.state;
    pipeline_status(
        &state.name,
        state.stage.as_ref().map(|s| s.name.as_str()),
        state.result.as_ref().map(|r| r.name.as_str()),
    )
}

/// Map a Bitbucket pipeline state to the shared run status (plan 8.1). A
/// stopped or expired pipeline never raises an alert. A pipeline paused at a
/// manual step waits for a person, which reads as waiting.
fn pipeline_status(state: &str, stage: Option<&str>, result: Option<&str>) -> RunStatus {
    match state {
        "PENDING" => RunStatus::Queued,
        "IN_PROGRESS" if stage == Some("PAUSED") => RunStatus::Waiting,
        "IN_PROGRESS" => RunStatus::Running,
        "COMPLETED" => match result {
            Some("SUCCESSFUL") => RunStatus::Success,
            Some("FAILED" | "ERROR") => RunStatus::Failed,
            _ => RunStatus::Neutral,
        },
        _ => RunStatus::Neutral,
    }
}

/// Every step of a running pipeline, for the kiosk. Parallel steps run at
/// the same time and show up as several running ones.
fn run_jobs(steps: &[Step]) -> Vec<RunJob> {
    steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let state = &step.state;
            RunJob {
                name: truncate(&step_label(step, i), MAX_TITLE_CHARS),
                status: pipeline_status(
                    &state.name,
                    state.stage.as_ref().map(|s| s.name.as_str()),
                    state.result.as_ref().map(|r| r.name.as_str()),
                ),
                step: None,
                progress: None,
                started: unix_seconds(step.started_on.as_deref()),
                finished: unix_seconds(step.completed_on.as_deref()),
                url: None,
            }
        })
        .collect()
}

/// Has this step stopped for good?
fn is_finished(state: &str) -> bool {
    state == "COMPLETED"
}

/// Where a pipeline in progress stands, counted in steps.
#[derive(Debug, Default, PartialEq)]
struct StepProgress {
    step: Option<String>,
    step_no: Option<u32>,
    step_count: Option<u32>,
    /// Finished steps over all steps.
    fraction: Option<f32>,
}

/// The step's name, or "step N" when the API does not give one.
fn step_label(step: &Step, index: usize) -> String {
    step.name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| format!("step {}", index + 1), str::to_string)
}

/// Work out the current step and progress from the step list. The current
/// step is the one in progress, else the first that is not finished.
fn step_progress(steps: &[Step]) -> StepProgress {
    if steps.is_empty() {
        return StepProgress::default();
    }
    let done = steps.iter().filter(|s| is_finished(&s.state.name)).count();
    let current = steps
        .iter()
        .position(|s| s.state.name == "IN_PROGRESS")
        .or_else(|| steps.iter().position(|s| !is_finished(&s.state.name)));
    StepProgress {
        step: current.map(|i| step_label(&steps[i], i)),
        step_no: current.map(|i| i as u32 + 1),
        step_count: Some(steps.len() as u32),
        fraction: Some(done as f32 / steps.len() as f32),
    }
}

/// The first step that failed, for the red alert.
fn failing_step(steps: &[Step]) -> Option<String> {
    steps
        .iter()
        .enumerate()
        .find(|(_, s)| {
            s.state.name == "COMPLETED"
                && matches!(
                    s.state.result.as_ref().map(|r| r.name.as_str()),
                    Some("FAILED" | "ERROR")
                )
        })
        .map(|(i, s)| step_label(s, i))
}

#[cfg(test)]
mod tests;
