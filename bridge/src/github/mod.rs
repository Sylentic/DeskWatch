//! GitHub source: open PRs and Actions runs from github.com, GitHub
//! Enterprise Server or GHE.com, turned into the same CI facts as Gitea.
//!
//! GitHub cannot reach a home server without opening it to the internet, so
//! this source only polls outbound (integrations plan section 5):
//!
//! - every `poll_s` (60 s): open PRs and the latest runs of each repository.
//!   Requests carry the last `ETag`, so an idle repository costs nothing
//!   against the rate limit on github.com. The result is also the source's
//!   health: a refused token or an unreachable server shows as the `warn` badge.
//! - every `job_poll_s` (5 s), only while a run is in progress in a repository
//!   that may interrupt: that run's jobs, for the step progress bar.
//!
//! Failures back off (60 s, 2 min, 5 min), and polling slows down when less
//! than 10 % of the rate limit is left.

pub mod api;
pub mod payload;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::ci::{OpenPull, RepoPulls, Run, RunStatus, RunningJob, Update};
use crate::config::GithubConfig;
use crate::gitea::payload::{unix_seconds, workflow_file};
use crate::gitea::{job_kind, step_progress};
use crate::model::{JobData, MAX_TITLE_CHARS, truncate};
use crate::source::{self, FactSink, Health, Interrupt, Source, SourceId, unix_now};
use api::HttpApi;
use payload::{Job, PullRequest, WorkflowRun};

/// Adapter type: `[[source.github]]` in the config and `source` on panel pages.
pub const KIND: &str = "github";

/// Poll delays after 1, 2, and 3 or more failed rounds in a row.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

// ---------------------------------------------------------------------------
// The source task
// ---------------------------------------------------------------------------

/// One `[[source.github]]` instance, ready to run.
pub struct Github {
    id: SourceId,
    config: GithubConfig,
    api: HttpApi,
}

impl Github {
    /// Load the token. Fails when the credential file is missing.
    pub fn new(config: &GithubConfig) -> Result<Self> {
        let id = SourceId::new(KIND, &config.name);
        let token =
            source::load_secret(&config.token_file).with_context(|| format!("{id}: token_file"))?;
        let api = HttpApi::new(&config.base_url, token).context("GitHub API")?;
        Ok(Self {
            id,
            config: config.clone(),
            api,
        })
    }
}

impl Source for Github {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    async fn run(self, sink: FactSink) -> Result<()> {
        let Github { id, config, api } = self;
        info!(source = %id, base_url = %config.base_url, repos = config.repos.len(), "polling GitHub");
        let poll = Duration::from_secs(config.poll_s);
        let mut state = GithubState::new(
            id,
            config.interrupt,
            config.alias,
            config.notify_new_prs,
            unix_now(),
        );

        let mut next_poll = Instant::now();
        let mut failures = 0;
        let mut job_ticker = tokio::time::interval(Duration::from_secs(config.job_poll_s));
        job_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut reported = None;
        loop {
            let updates = tokio::select! {
                _ = tokio::time::sleep_until(next_poll) => {
                    let mut updates = Vec::new();
                    for repo in &config.repos {
                        updates.extend(poll_repo(&api, &mut state, repo).await);
                    }
                    failures = match state.health() {
                        Some(Health::Ok) | None => 0,
                        _ => failures + 1,
                    };
                    next_poll = Instant::now() + next_delay(poll, failures, api.low_on_quota());
                    updates
                }
                _ = job_ticker.tick() => {
                    let (updates, runs_done) = poll_jobs(&api, &mut state).await;
                    // A run's jobs all finished: fetch the run result now
                    // instead of waiting for the next regular poll.
                    if runs_done {
                        next_poll = next_poll.min(Instant::now());
                    }
                    updates
                }
            };
            for (repo, run_id) in state.take_finished_runs() {
                api.forget_jobs(&repo, run_id);
            }
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
fn next_delay(poll: Duration, failures: usize, low_on_quota: bool) -> Duration {
    let mut delay = poll;
    if failures > 0 {
        delay = delay.max(BACKOFF[(failures - 1).min(BACKOFF.len() - 1)]);
    }
    if low_on_quota {
        warn!("GitHub rate limit below 10 %, polling less often");
        delay *= 4;
    }
    delay
}

/// Poll open PRs and recent runs of one repository.
async fn poll_repo(api: &HttpApi, state: &mut GithubState, repo: &str) -> Vec<Update> {
    let mut out = Vec::new();
    let pulls = match api.open_pulls(repo).await {
        Ok(pulls) => pulls,
        Err(err) => {
            warn!(%repo, "cannot poll GitHub: {err:#}");
            state.set_health(repo, Health::from_error(&err));
            return out;
        }
    };
    debug!(%repo, open = pulls.len(), "polled open PRs");
    out.extend(state.on_pulls(repo, pulls, unix_now()));

    let runs = match api.runs(repo).await {
        Ok(runs) => runs,
        Err(err) => {
            warn!(%repo, "cannot poll GitHub runs: {err:#}");
            state.set_health(repo, Health::from_error(&err));
            return out;
        }
    };
    // Name the failing step in the red alert: one jobs request per failed
    // run, only the first time it is seen.
    for run_id in state.need_failed_step(repo, &runs) {
        let step = match api.jobs(repo, run_id).await {
            Ok(jobs) => failing_step(&jobs),
            Err(err) => {
                warn!(%repo, run_id, "cannot fetch jobs of failed run: {err:#}");
                None
            }
        };
        state.set_failed_step(repo, run_id, step);
    }
    out.extend(state.on_runs(repo, runs, unix_now()));
    state.set_health(repo, Health::Ok);
    out
}

/// Poll the jobs of every run in progress. Returns the updates and whether
/// some run has all its jobs finished.
async fn poll_jobs(api: &HttpApi, state: &mut GithubState) -> (Vec<Update>, bool) {
    let mut out = Vec::new();
    let mut runs_done = false;
    for (repo, run_id) in state.watched_runs() {
        match api.jobs(&repo, run_id).await {
            Ok(jobs) => {
                let (updates, done) = state.on_jobs(&repo, run_id, jobs, unix_now());
                out.extend(updates);
                runs_done |= done;
            }
            // Keep the last known progress; the next tick tries again.
            Err(err) => warn!(%repo, run_id, "cannot poll jobs: {err:#}"),
        }
    }
    (out, runs_done)
}

// ---------------------------------------------------------------------------
// Turning API answers into facts
// ---------------------------------------------------------------------------

/// A run in progress whose jobs are polled for step progress.
#[derive(Debug, Default)]
struct WatchedRun {
    /// Workflow file, shown as the pipeline on the job screen.
    pipeline: String,
    /// Jobs on screen right now.
    jobs: HashSet<u64>,
}

/// State the GitHub source keeps between polls.
#[derive(Debug)]
pub struct GithubState {
    id: SourceId,
    interrupt: Interrupt,
    alias: BTreeMap<String, String>,
    notify_new_prs: bool,
    /// Unix seconds when the source started. Runs that finished before then
    /// show on the pipelines page and in the badge, but do not take over
    /// the screen: they are old news after a bridge restart.
    since: u64,
    /// PR numbers per repository from the last poll. Missing before the
    /// first poll, so existing PRs do not all flash "New PR" at startup.
    known_pulls: HashMap<String, HashSet<u64>>,
    /// Last `Run` sent per pipeline key, so only changes are sent.
    runs: HashMap<String, Run>,
    /// Runs in progress, by repository and run id.
    watched: BTreeMap<(String, u64), WatchedRun>,
    /// Runs that stopped being watched since the last call to
    /// `take_finished_runs`, so their kept job lists can be dropped.
    finished: Vec<(String, u64)>,
    /// First failing step per failed run (`None`: looked up, none found).
    failed_steps: HashMap<(String, u64), Option<String>>,
    /// Result of the last poll per repository.
    health: BTreeMap<String, Health>,
}

impl GithubState {
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
            since,
            known_pulls: HashMap::new(),
            runs: HashMap::new(),
            watched: BTreeMap::new(),
            finished: Vec::new(),
            failed_steps: HashMap::new(),
            health: BTreeMap::new(),
        }
    }

    /// Health from the polls: the worst repository wins. `None` before the
    /// first poll.
    pub fn health(&self) -> Option<Health> {
        self.health.values().max().copied()
    }

    pub fn set_health(&mut self, repo: &str, health: Health) {
        self.health.insert(repo.to_string(), health);
    }

    /// Runs whose jobs to poll, in a stable order.
    pub fn watched_runs(&self) -> Vec<(String, u64)> {
        self.watched.keys().cloned().collect()
    }

    /// Runs that stopped being watched since the last call.
    pub fn take_finished_runs(&mut self) -> Vec<(String, u64)> {
        std::mem::take(&mut self.finished)
    }

    /// Open PRs of one repository from a poll.
    pub fn on_pulls(&mut self, repo: &str, pulls: Vec<PullRequest>, now: u64) -> Vec<Update> {
        let mut out = Vec::new();
        let key = self.repo_key(repo);
        let project = source::label(&self.alias, repo);
        let numbers: HashSet<u64> = pulls.iter().map(|p| p.number).collect();

        // New since the last poll: flash, unless it is the first poll or a draft.
        if let Some(known) = self.known_pulls.get(repo)
            && self.notify_new_prs
        {
            for pull in pulls
                .iter()
                .filter(|p| !known.contains(&p.number) && !p.draft)
            {
                out.push(Update::PullOpened {
                    key: key.clone(),
                    project: project.clone(),
                    source: KIND.into(),
                    pull: open_pull(pull, now),
                    notify: true,
                });
            }
        }
        self.known_pulls.insert(repo.to_string(), numbers);

        let pulls: Vec<OpenPull> = pulls.iter().map(|p| open_pull(p, now)).collect();
        out.push(Update::Pulls {
            key,
            repo: RepoPulls {
                project,
                source: KIND.into(),
                total: pulls.len() as u32,
                pulls,
            },
        });
        out
    }

    /// Failed runs, latest of their workflow, whose failing step has not been
    /// looked up yet.
    pub fn need_failed_step(&self, repo: &str, runs: &[WorkflowRun]) -> Vec<u64> {
        latest_per_pipeline(runs)
            .into_iter()
            .filter(|run| status_of(run) == RunStatus::Failed)
            .filter(|run| !self.failed_steps.contains_key(&(repo.to_string(), run.id)))
            .map(|run| run.id)
            .collect()
    }

    pub fn set_failed_step(&mut self, repo: &str, run_id: u64, step: Option<String>) {
        self.failed_steps.insert((repo.to_string(), run_id), step);
    }

    /// Recent runs of one repository from a poll, newest first.
    pub fn on_runs(&mut self, repo: &str, runs: Vec<WorkflowRun>, now: u64) -> Vec<Update> {
        let mut out = Vec::new();

        for run in latest_per_pipeline(&runs) {
            let status = status_of(run);
            let pipeline = pipeline_name(run);
            let started = unix_seconds(run.run_started_at.as_deref())
                .or_else(|| unix_seconds(run.created_at.as_deref()))
                .unwrap_or(now);
            let finished = (run.status.as_deref() == Some("completed"))
                .then(|| unix_seconds(run.updated_at.as_deref()).unwrap_or(now));
            let fresh = finished.is_none_or(|f| f >= self.since);
            let step = self
                .failed_steps
                .get(&(repo.to_string(), run.id))
                .cloned()
                .flatten()
                .filter(|_| status == RunStatus::Failed);
            let fact = Run {
                source: KIND.into(),
                project: source::label(&self.alias, repo),
                pipeline: pipeline.clone(),
                git_ref: run.head_branch.clone().unwrap_or_default(),
                status,
                step,
                started,
                finished,
                interrupt: fresh && self.interrupt.allows(repo),
            };
            let key = format!("{}:{repo}:{pipeline}", self.id);
            if self.runs.get(&key) != Some(&fact) {
                if finished.is_some() {
                    info!(source = %self.id, %repo, %pipeline, ?status, "run finished");
                }
                self.runs.insert(key.clone(), fact.clone());
                out.push(Update::Run { key, run: fact });
            }
        }

        // Watch runs in progress that may take over the screen; stop
        // watching the ones that finished or dropped off the list.
        let in_progress: HashMap<u64, &WorkflowRun> = runs
            .iter()
            .filter(|r| status_of(r) == RunStatus::Running)
            .map(|r| (r.id, r))
            .collect();
        if self.interrupt.allows(repo) {
            for (id, run) in &in_progress {
                self.watched
                    .entry((repo.to_string(), *id))
                    .or_insert_with(|| WatchedRun {
                        pipeline: pipeline_name(run),
                        jobs: HashSet::new(),
                    });
            }
        }
        let over: Vec<(String, u64)> = self
            .watched
            .keys()
            .filter(|(r, id)| r == repo && !in_progress.contains_key(id))
            .cloned()
            .collect();
        for run_ref in over {
            if let Some(watched) = self.watched.remove(&run_ref) {
                for job_id in watched.jobs {
                    out.push(Update::JobDone {
                        key: self.job_key(&run_ref.0, job_id),
                    });
                }
            }
            self.finished.push(run_ref);
        }

        // Forget failing steps of runs no longer listed.
        let listed: HashSet<u64> = runs.iter().map(|r| r.id).collect();
        self.failed_steps
            .retain(|(r, id), _| r != repo || listed.contains(id));
        out
    }

    /// Jobs of a watched run from a poll. Returns the updates and whether
    /// every job has finished (the run result is then worth fetching).
    pub fn on_jobs(
        &mut self,
        repo: &str,
        run_id: u64,
        jobs: Vec<Job>,
        now: u64,
    ) -> (Vec<Update>, bool) {
        let mut out = Vec::new();
        let run_ref = (repo.to_string(), run_id);
        // A late answer for a run that already finished changes nothing.
        let Some(mut watched) = self.watched.remove(&run_ref) else {
            return (out, false);
        };
        let interrupt = self.interrupt.allows(repo);
        for job in &jobs {
            let key = self.job_key(repo, job.id);
            match job.status.as_str() {
                "in_progress" => {
                    watched.jobs.insert(job.id);
                    let data = self.job_data(repo, &watched.pipeline, job, now);
                    debug!(%key, step = ?data.step, progress = ?data.progress, "job running");
                    out.push(Update::JobRunning {
                        key,
                        job: RunningJob {
                            data,
                            interrupt,
                            updated: now,
                        },
                    });
                }
                "completed" => {
                    if watched.jobs.remove(&job.id) {
                        out.push(Update::JobDone { key });
                    }
                    if job_status("completed", job.conclusion.as_deref()) == RunStatus::Failed {
                        let step = failing_step(std::slice::from_ref(job));
                        self.failed_steps.entry(run_ref.clone()).or_insert(step);
                    }
                }
                // queued / waiting / pending: nothing to show until a runner picks it up.
                _ => {}
            }
        }
        let all_done = !jobs.is_empty() && jobs.iter().all(|j| j.status == "completed");
        self.watched.insert(run_ref, watched);
        (out, all_done)
    }

    fn job_data(&self, repo: &str, pipeline: &str, job: &Job, now: u64) -> JobData {
        let progress = step_progress(&job.steps);
        JobData {
            source: KIND.into(),
            project: truncate(&source::label(&self.alias, repo), MAX_TITLE_CHARS),
            kind: job_kind(pipeline, &job.name),
            pipeline: truncate(pipeline, MAX_TITLE_CHARS),
            git_ref: truncate(job.head_branch.as_deref().unwrap_or(""), MAX_TITLE_CHARS),
            commit: job.head_sha.chars().take(7).collect(),
            step: progress.step.map(|s| truncate(&s, MAX_TITLE_CHARS)),
            step_no: progress.step_no,
            step_count: progress.step_count,
            progress: progress.fraction,
            started: unix_seconds(job.started_at.as_deref()).unwrap_or(now),
            others: 0,
        }
    }

    /// Key for a job in the fact store.
    fn job_key(&self, repo: &str, job_id: u64) -> String {
        format!("{}:{repo}:job:{job_id}", self.id)
    }

    /// Key for a repository's PRs in the fact store.
    fn repo_key(&self, repo: &str) -> String {
        format!("{}:{repo}", self.id)
    }
}

fn open_pull(pull: &PullRequest, now: u64) -> OpenPull {
    OpenPull {
        number: pull.number,
        title: pull.title.clone(),
        created: unix_seconds(pull.created_at.as_deref()).unwrap_or(now),
    }
}

/// The newest run of each workflow, in list order (the API lists newest first).
fn latest_per_pipeline(runs: &[WorkflowRun]) -> Vec<&WorkflowRun> {
    let mut seen = HashSet::new();
    runs.iter()
        .filter(|run| seen.insert(pipeline_name(run)))
        .collect()
}

/// Workflow file name, such as `ci.yml`, else the workflow's display name.
fn pipeline_name(run: &WorkflowRun) -> String {
    let file = workflow_file(&run.path);
    if file.is_empty() {
        run.name.clone().unwrap_or_default()
    } else {
        file.to_string()
    }
}

fn status_of(run: &WorkflowRun) -> RunStatus {
    job_status(
        run.status.as_deref().unwrap_or(""),
        run.conclusion.as_deref(),
    )
}

/// Map a GitHub run or job status to the shared run status (plan 8.1).
/// Cancelled, skipped and neutral runs never raise an alert.
fn job_status(status: &str, conclusion: Option<&str>) -> RunStatus {
    match status {
        "completed" => match conclusion {
            Some("success") => RunStatus::Success,
            Some("failure" | "timed_out" | "startup_failure") => RunStatus::Failed,
            _ => RunStatus::Neutral,
        },
        "in_progress" => RunStatus::Running,
        // requested, queued, waiting, pending
        _ => RunStatus::Queued,
    }
}

/// The first failed step over a run's jobs, for the red alert.
fn failing_step(jobs: &[Job]) -> Option<String> {
    jobs.iter()
        .flat_map(|job| &job.steps)
        .find(|step| matches!(step.conclusion.as_deref(), Some("failure" | "timed_out")))
        .map(|step| step.name.clone())
}

#[cfg(test)]
mod tests;
