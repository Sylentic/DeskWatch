//! Gitea source: turns Gitea Actions and pull request events into CI facts.
//!
//! Data comes in three ways, all ending up as a `GiteaEvent` on one channel
//! inside the source's task, where `GiteaState::apply` turns them into fact
//! updates for the main loop:
//!
//! - Webhooks (`webhook.rs`): instant job start/end, run results and PR changes.
//! - Job polling (`api.rs`): Gitea webhooks fire per job, not per step, so
//!   while a job runs the bridge polls its steps for the progress bar.
//! - PR polling (`api.rs`): a safety net that resets the open PR count, in
//!   case a webhook was missed or the bridge was down. Its result is also the
//!   source's health: a refused token or an unreachable Gitea shows as the
//!   `warn` badge.
//!
//! The webhook settings needed in Gitea are in docs/install.md.

pub mod api;
pub mod payload;
pub mod runners;
pub mod webhook;

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info};

use crate::alerts::{self, AlertMessage};
use crate::ci::{OpenPull, RepoPulls, Run, RunJob, RunStatus, RunningJob, Update};
use crate::config::GiteaConfig;
use crate::hooks::Hooks;
use crate::model::{JobData, JobKind, MAX_TITLE_CHARS, truncate};
use crate::source::{self, FactSink, Health, Interrupt, Source, SourceId, unix_now};
use payload::{
    IssueEvent, Job, PullRequestEvent, WorkflowJobEvent, WorkflowRunEvent, unix_seconds,
    workflow_file,
};

/// Adapter type: `[[source.gitea]]` in the config and `source` on the `[[kiosk.panel]]` blocks.
pub const KIND: &str = "gitea";

/// Something the Gitea source learned, from a webhook or a poll.
#[derive(Debug)]
pub enum GiteaEvent {
    WorkflowRun(WorkflowRunEvent),
    WorkflowJob(WorkflowJobEvent),
    PullRequest(PullRequestEvent),
    Issue(IssueEvent),
    /// Fresh job state from the jobs API, for step progress.
    JobPolled {
        repo: String,
        job: Job,
    },
    /// Open PRs of one repository from the API, or why the poll failed.
    PullsPolled {
        repo: String,
        pulls: Result<api::OpenPulls, Health>,
    },
    /// Every runner of one scope, from the runners API.
    RunnersPolled {
        scope: runners::RunnerScope,
        runners: Vec<runners::ApiRunner>,
    },
}

/// A job the bridge is tracking, so the poller knows what to ask for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobRef {
    /// `owner/name`.
    pub repo: String,
    pub id: u64,
}

// ---------------------------------------------------------------------------
// The source task
// ---------------------------------------------------------------------------

/// One `[[source.gitea]]` instance, ready to run.
pub struct Gitea {
    id: SourceId,
    config: GiteaConfig,
    api: api::HttpApi,
    events_tx: mpsc::Sender<GiteaEvent>,
    events_rx: mpsc::Receiver<GiteaEvent>,
}

impl Gitea {
    /// Load the credentials and register the webhook route on the shared
    /// listener. Fails when a credential file is missing.
    pub fn new(config: &GiteaConfig, hooks: &mut Hooks) -> Result<Self> {
        let id = SourceId::new(KIND, &config.name);
        let secret = source::load_secret(&config.webhook_secret_file)
            .with_context(|| format!("{id}: webhook_secret_file"))?;
        let token = match &config.token_file {
            Some(file) => {
                Some(source::load_secret(file).with_context(|| format!("{id}: token_file"))?)
            }
            None => {
                info!(source = %id, "no token_file, polling Gitea anonymously (public repos only)");
                None
            }
        };
        let api = api::HttpApi::new(&config.base_url, token).context("Gitea API")?;

        let (events_tx, events_rx) = mpsc::channel(64);
        let path = webhook::path(&config.name);
        hooks.add(&path, webhook::router(&path, secret, events_tx.clone()));
        Ok(Self {
            id,
            config: config.clone(),
            api,
            events_tx,
            events_rx,
        })
    }
}

impl Source for Gitea {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    async fn run(self, sink: FactSink) -> Result<()> {
        let Gitea {
            id,
            config,
            api,
            events_tx,
            mut events_rx,
        } = self;

        // The poller follows the jobs the state says are running.
        let (jobs_tx, jobs_rx) = watch::channel(Vec::new());
        let polling = api::Polling {
            repos: config.repos.clone(),
            runner_scopes: config.runner_scopes()?,
            job: Duration::from_secs(config.job_poll_s),
            pulls: Duration::from_secs(config.poll_s),
            runners: Duration::from_secs(config.runner_poll_s),
        };
        let _poller = source::AbortOnDrop(tokio::spawn(api::poll_loop(
            api, polling, jobs_rx, events_tx,
        )));

        let mut state = GiteaState::new(id, config.interrupt, config.alias)
            .with_web(&config.base_url)
            .with_issue_alerts(config.alert_new_issues);
        let mut reported = None;
        while let Some(event) = events_rx.recv().await {
            if !sink.facts(state.apply(event, unix_now())).await {
                break; // main loop has stopped
            }
            let running = state.running_jobs();
            jobs_tx.send_if_modified(|jobs: &mut Vec<JobRef>| {
                let changed = *jobs != running;
                *jobs = running;
                changed
            });
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

// ---------------------------------------------------------------------------
// Turning events into facts
// ---------------------------------------------------------------------------

/// State the Gitea source keeps between events.
#[derive(Debug)]
pub struct GiteaState {
    id: SourceId,
    /// Which repositories may take over the screen.
    interrupt: Interrupt,
    /// Short labels for repositories.
    alias: BTreeMap<String, String>,
    /// Workflow file per run id, learned from `workflow_run` events, so job
    /// screens can show `deploy.yml` instead of just the job name.
    run_files: HashMap<u64, String>,
    /// Running jobs and the run they belong to.
    running: HashMap<JobRef, u64>,
    /// Every job seen per unfinished run, by job id, for the kiosk's list.
    run_jobs: HashMap<u64, BTreeMap<u64, RunJob>>,
    /// First failing step per run, from `workflow_job` events, for the red alert.
    failed_steps: HashMap<u64, String>,
    /// Web address of the instance, for links on the kiosk. Empty: no links.
    web: String,
    /// Result of the last PR poll per repository.
    polls: BTreeMap<String, Health>,
    /// Raise an alert for each new issue.
    alert_new_issues: bool,
}

impl GiteaState {
    pub fn new(id: SourceId, interrupt: Interrupt, alias: BTreeMap<String, String>) -> Self {
        Self {
            id,
            interrupt,
            alias,
            run_files: HashMap::new(),
            running: HashMap::new(),
            run_jobs: HashMap::new(),
            failed_steps: HashMap::new(),
            web: String::new(),
            polls: BTreeMap::new(),
            alert_new_issues: false,
        }
    }

    /// Raise an alert for each new issue (`alert_new_issues`).
    pub fn with_issue_alerts(mut self, on: bool) -> Self {
        self.alert_new_issues = on;
        self
    }

    /// Give the kiosk links to this instance's web pages (`base_url`).
    pub fn with_web(mut self, base_url: &str) -> Self {
        self.web = base_url.to_string();
        self
    }

    /// Web page of a repository: `{web}/{owner/repo}/{tail...}`.
    fn repo_url(&self, repo: &str, tail: &[&str]) -> Option<String> {
        let mut path = vec![repo];
        path.extend_from_slice(tail);
        source::web_url(&self.web, &path, &[])
    }

    /// Page with the runners of one scope (settings of the repo, org or
    /// user, or the admin area).
    fn runners_url(&self, scope: &runners::RunnerScope) -> Option<String> {
        use runners::RunnerScope::*;
        let path: Vec<&str> = match scope {
            Admin => vec!["-", "admin", "actions", "runners"],
            User => vec!["user", "settings", "actions", "runners"],
            Org(org) => vec!["org", org, "settings", "actions", "runners"],
            Repo(repo) => vec![repo, "settings", "actions", "runners"],
        };
        source::web_url(&self.web, &path, &[])
    }

    /// Jobs to poll for step progress, in a stable order.
    pub fn running_jobs(&self) -> Vec<JobRef> {
        let mut jobs: Vec<JobRef> = self.running.keys().cloned().collect();
        jobs.sort();
        jobs
    }

    /// Health from the PR polls: the worst repository wins. `None` until the
    /// first poll, or when no repositories are polled.
    pub fn health(&self) -> Option<Health> {
        self.polls.values().max().copied()
    }

    /// Turn one event into fact updates. `now` is Unix seconds.
    pub fn apply(&mut self, event: GiteaEvent, now: u64) -> Vec<Update> {
        let mut out = Vec::new();
        match event {
            GiteaEvent::WorkflowRun(event) => self.on_run(event, now, &mut out),
            GiteaEvent::WorkflowJob(event) => self.on_job(
                &event.repository.full_name,
                event.workflow_job,
                now,
                &mut out,
            ),
            GiteaEvent::JobPolled { repo, job } => {
                // Only refresh jobs still tracked: a late poll result must not
                // bring back a job whose `completed` webhook already arrived.
                let tracked = JobRef {
                    repo: repo.clone(),
                    id: job.id,
                };
                if self.running.contains_key(&tracked) {
                    self.on_job(&repo, job, now, &mut out);
                }
            }
            GiteaEvent::PullRequest(event) => self.on_pull_request(event, now, &mut out),
            GiteaEvent::Issue(event) => self.on_issue(event, &mut out),
            GiteaEvent::RunnersPolled { scope, runners } => out.push(Update::Runners {
                key: format!("{}:runners:{scope}", self.id),
                runners: runners
                    .into_iter()
                    .map(|runner| runner.into_fact(KIND, self.runners_url(&scope)))
                    .collect(),
            }),
            GiteaEvent::PullsPolled { repo, pulls } => match pulls {
                Ok(pulls) => {
                    out.push(Update::Pulls {
                        key: self.repo_key(&repo),
                        repo: self.repo_pulls(&repo, pulls),
                    });
                    self.polls.insert(repo, Health::Ok);
                }
                // Keep the last known PRs; the page is greyed out meanwhile.
                Err(health) => {
                    self.polls.insert(repo, health);
                }
            },
        }
        out
    }

    fn on_run(&mut self, event: WorkflowRunEvent, now: u64, out: &mut Vec<Update>) {
        let run = event.workflow_run;
        let repo = &event.repository;
        let file = workflow_file(&run.path);
        let pipeline = if file.is_empty() {
            run.display_title.clone()
        } else {
            file.to_string()
        };

        let status = run_status(&run.status, run.conclusion.as_deref());
        let finished = event.action == "completed" || run.status == "completed";
        let mut step = None;
        if finished {
            self.run_files.remove(&run.id);
            step = self.failed_steps.remove(&run.id);
            if self.run_jobs.remove(&run.id).is_some() {
                out.push(Update::RunJobs {
                    key: self.run_key(&repo.full_name, run.id),
                    jobs: Vec::new(),
                });
            }
            // Any job of this run still on screen has finished too.
            let done: Vec<JobRef> = self
                .running
                .iter()
                .filter(|(_, run_id)| **run_id == run.id)
                .map(|(job, _)| job.clone())
                .collect();
            for job in done {
                self.running.remove(&job);
                out.push(Update::JobDone {
                    key: self.job_key(&job),
                });
            }
            info!(source = %self.id, repo = %repo.full_name, %pipeline, ?status, "run finished");
        } else {
            self.run_files.insert(run.id, pipeline.clone());
        }

        // Gitea's own address when it sends one, else the run by its number,
        // else the repository's Actions list.
        let run_url = run
            .html_url
            .as_deref()
            .and_then(source::safe_url)
            .or_else(|| match run.run_number {
                0 => self.repo_url(&repo.full_name, &["actions"]),
                n => self.repo_url(&repo.full_name, &["actions", "runs", &n.to_string()]),
            });
        out.push(Update::Run {
            key: format!("{}:{}:{pipeline}", self.id, repo.full_name),
            run: Run {
                source: KIND.into(),
                project: source::label(&self.alias, &repo.full_name),
                pipeline,
                git_ref: run.head_branch.clone(),
                status,
                step,
                started: unix_seconds(run.started_at.as_deref()).unwrap_or(now),
                finished: finished
                    .then(|| unix_seconds(run.completed_at.as_deref()).unwrap_or(now)),
                interrupt: self.interrupt.allows(&repo.full_name),
                url: run_url,
            },
        });
    }

    fn on_job(&mut self, repo: &str, job: Job, now: u64, out: &mut Vec<Update>) {
        let job_ref = JobRef {
            repo: repo.to_string(),
            id: job.id,
        };
        let key = self.job_key(&job_ref);
        let group = self.run_key(repo, job.run_id);
        let jobs = self.run_jobs.entry(job.run_id).or_default();
        jobs.insert(job.id, run_job(&job));
        let jobs: Vec<RunJob> = jobs.values().cloned().collect();
        // A run whose `completed` webhook got lost must not stay forever.
        while self.run_jobs.len() > 64 {
            let Some(&oldest) = self.run_jobs.keys().min() else {
                break;
            };
            self.run_jobs.remove(&oldest);
        }
        out.push(Update::RunJobs {
            key: group.clone(),
            jobs,
        });

        match job.status.as_str() {
            "in_progress" | "running" => {
                self.running.insert(job_ref, job.run_id);
                let data = self.job_data(repo, &job, now);
                debug!(%key, step = ?data.step, progress = ?data.progress, "job running");
                let url = job
                    .html_url
                    .as_deref()
                    .and_then(source::safe_url)
                    .or_else(|| self.repo_url(repo, &["actions"]));
                let job = RunningJob {
                    data,
                    interrupt: self.interrupt.allows(repo),
                    updated: now,
                    url,
                    group: Some(group),
                };
                out.push(Update::JobRunning { key, job });
            }
            "completed" => {
                self.running.remove(&job_ref);
                out.push(Update::JobDone { key });
                if run_status("completed", job.conclusion.as_deref()) == RunStatus::Failed
                    && let Some(step) = failing_step(&job)
                {
                    self.failed_steps.entry(job.run_id).or_insert(step);
                }
            }
            // queued / waiting / pending: nothing to show until a runner picks it up.
            _ => {}
        }
    }

    fn job_data(&self, repo: &str, job: &Job, now: u64) -> JobData {
        let pipeline = self
            .run_files
            .get(&job.run_id)
            .cloned()
            .unwrap_or_else(|| job.name.clone());
        let steps = job.steps.as_deref().unwrap_or_default();
        let progress = step_progress(steps);
        JobData {
            source: KIND.into(),
            project: truncate(&source::label(&self.alias, repo), MAX_TITLE_CHARS),
            kind: job_kind(&pipeline, &job.name),
            pipeline: truncate(&pipeline, MAX_TITLE_CHARS),
            git_ref: truncate(&job.head_branch, MAX_TITLE_CHARS),
            commit: job.head_sha.chars().take(7).collect(),
            step: progress.step.map(|s| truncate(&s, MAX_TITLE_CHARS)),
            step_no: progress.step_no,
            step_count: progress.step_count,
            progress: progress.fraction,
            started: unix_seconds(job.started_at.as_deref()).unwrap_or(now),
            others: 0,
        }
    }

    /// A newly opened issue goes to the alerts list. Edits, closes and
    /// comments are ignored.
    fn on_issue(&self, event: IssueEvent, out: &mut Vec<Update>) {
        if !self.alert_new_issues || event.action != "opened" {
            return;
        }
        let repo = &event.repository.full_name;
        let issue = event.issue;
        let by = issue
            .user
            .map(|u| format!(" by {}", u.login))
            .unwrap_or_default();
        let url = issue
            .html_url
            .or_else(|| self.repo_url(repo, &["issues", &issue.number.to_string()]));
        out.push(Update::Alert {
            alert: AlertMessage::activity(
                alerts::activity_id(&format!("{}:issue:{repo}#{}", self.id, issue.number)),
                issue.title,
                format!(
                    "New issue #{} in {}{by}",
                    issue.number,
                    source::label(&self.alias, repo)
                ),
                KIND,
                url,
            ),
        });
    }

    fn on_pull_request(&self, event: PullRequestEvent, now: u64, out: &mut Vec<Update>) {
        let repo = &event.repository;
        let pull = &event.pull_request;
        let key = self.repo_key(&repo.full_name);
        match event.action.as_str() {
            "opened" | "reopened" => out.push(Update::PullOpened {
                key,
                project: source::label(&self.alias, &repo.full_name),
                source: KIND.into(),
                pull: OpenPull {
                    number: pull.number,
                    title: pull.title.clone(),
                    created: unix_seconds(pull.created_at.as_deref()).unwrap_or(now),
                    url: self.repo_url(&repo.full_name, &["pulls", &pull.number.to_string()]),
                },
                // Flash only for brand-new PRs, not reopens.
                notify: event.action == "opened",
            }),
            "closed" => out.push(Update::PullClosed {
                key,
                number: pull.number,
            }),
            _ => {}
        }
    }

    /// Key for a job in the fact store.
    fn job_key(&self, job: &JobRef) -> String {
        format!("{}:{}:{}", self.id, job.repo, job.id)
    }

    /// Key for the job list of a run, shared by its jobs.
    fn run_key(&self, repo: &str, run_id: u64) -> String {
        format!("{}:{repo}:run:{run_id}", self.id)
    }

    /// Key for a repository's PRs in the fact store.
    fn repo_key(&self, full_name: &str) -> String {
        format!("{}:{full_name}", self.id)
    }

    fn repo_pulls(&self, full_name: &str, polled: api::OpenPulls) -> RepoPulls {
        let pulls = polled
            .pulls
            .into_iter()
            .map(|p| OpenPull {
                number: p.number,
                title: p.title,
                created: unix_seconds(p.created_at.as_deref()).unwrap_or(0),
                url: self.repo_url(full_name, &["pulls", &p.number.to_string()]),
            })
            .collect();
        RepoPulls {
            project: source::label(&self.alias, full_name),
            source: KIND.into(),
            pulls,
            total: polled.total,
        }
    }
}

/// Map a Gitea run or job status to the shared run status. Cancelled and
/// skipped runs are neutral: someone stopped them on purpose.
fn run_status(status: &str, conclusion: Option<&str>) -> RunStatus {
    match status {
        "completed" => match conclusion {
            Some("success") => RunStatus::Success,
            Some("failure" | "timed_out") => RunStatus::Failed,
            _ => RunStatus::Neutral,
        },
        "in_progress" | "running" => RunStatus::Running,
        // requested, queued, waiting, pending (28+)
        _ => RunStatus::Queued,
    }
}

/// One job of a run for the kiosk's list.
fn run_job(job: &Job) -> RunJob {
    let status = run_status(&job.status, job.conclusion.as_deref());
    let steps = job.steps.as_deref().unwrap_or_default();
    let progress = step_progress(steps);
    let step = match status {
        RunStatus::Running => progress.step,
        RunStatus::Failed => failing_step(job),
        _ => None,
    };
    RunJob {
        name: truncate(&job.name, MAX_TITLE_CHARS),
        status,
        step: step.map(|s| truncate(&s, MAX_TITLE_CHARS)),
        progress: progress.fraction.filter(|_| status == RunStatus::Running),
        started: unix_seconds(job.started_at.as_deref()),
        finished: unix_seconds(job.completed_at.as_deref()),
        url: job.html_url.as_deref().and_then(source::safe_url),
    }
}

/// Deploy if the workflow file or job name mentions "deploy", otherwise build.
/// Shared with the GitHub source.
pub(crate) fn job_kind(pipeline: &str, job_name: &str) -> JobKind {
    let mentions = |s: &str| s.to_lowercase().contains("deploy");
    if mentions(pipeline) || mentions(job_name) {
        JobKind::Deploy
    } else {
        JobKind::Build
    }
}

fn failing_step(job: &Job) -> Option<String> {
    job.steps
        .as_deref()?
        .iter()
        .find(|s| run_status("completed", s.conclusion.as_deref()) == RunStatus::Failed)
        .map(|s| s.name.clone())
}

/// Step progress of a running job.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct StepProgress {
    pub step: Option<String>,
    pub step_no: Option<u32>,
    pub step_count: Option<u32>,
    pub fraction: Option<f32>,
}

/// Work out the current step and progress from the step list. With no steps
/// (job just picked up, or webhook without steps) everything is `None` and
/// the panel shows a spinner. GitHub jobs list steps the same way, so the
/// GitHub source uses this too.
pub(crate) fn step_progress(steps: &[payload::Step]) -> StepProgress {
    if steps.is_empty() {
        return StepProgress::default();
    }
    let count = steps.len();
    let done = steps.iter().filter(|s| s.status == "completed").count();
    // Current step: the one running, else the first one not finished yet.
    let current = steps
        .iter()
        .position(|s| s.status == "in_progress")
        .or_else(|| steps.iter().position(|s| s.status != "completed"));
    StepProgress {
        step: current.map(|i| steps[i].name.clone()),
        step_no: current.map(|i| i as u32 + 1),
        step_count: Some(count as u32),
        fraction: Some(done as f32 / count as f32),
    }
}
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ci::CiFacts;
    use crate::model::{Level, ScreenData, pick};

    fn repo() -> serde_json::Value {
        json!({ "full_name": "me/demo", "name": "demo" })
    }

    fn run_event(action: &str, conclusion: Option<&str>) -> GiteaEvent {
        let value = json!({
            "action": action,
            "workflow_run": {
                "id": 2, "display_title": "Fix things", "path": "deploy.yml@refs/heads/main",
                "head_sha": "a1b2c3d4e5f6", "head_branch": "main",
                "status": if action == "completed" { "completed" } else { "in_progress" },
                "conclusion": conclusion,
                "started_at": "2026-10-08T18:00:00Z", "completed_at": "2026-10-08T18:05:00Z"
            },
            "repository": repo()
        });
        GiteaEvent::WorkflowRun(serde_json::from_value(value).unwrap())
    }

    fn job_json(
        status: &str,
        conclusion: Option<&str>,
        steps: serde_json::Value,
    ) -> serde_json::Value {
        json!({
            "id": 7, "run_id": 2, "name": "build", "head_sha": "a1b2c3d4e5f6",
            "head_branch": "main", "status": status, "conclusion": conclusion,
            "started_at": "2026-10-08T18:00:10Z", "steps": steps
        })
    }

    fn job_event(status: &str, conclusion: Option<&str>, steps: serde_json::Value) -> GiteaEvent {
        let value = json!({
            "action": status,
            "workflow_job": job_json(status, conclusion, steps),
            "repository": repo()
        });
        GiteaEvent::WorkflowJob(serde_json::from_value(value).unwrap())
    }

    #[test]
    fn jobs_of_one_run_are_listed_together() {
        let mut state = state(true);
        let mut facts = CiFacts::default();
        feed(&mut state, &mut facts, run_event("requested", None), 10);
        let job = |id: u64, name: &str, status: &str, conclusion: Option<&str>| {
            let mut job = job_json(status, conclusion, json!([]));
            job["id"] = json!(id);
            job["name"] = json!(name);
            GiteaEvent::WorkflowJob(
                serde_json::from_value(
                    json!({ "action": status, "workflow_job": job, "repository": repo() }),
                )
                .unwrap(),
            )
        };
        feed(
            &mut state,
            &mut facts,
            job(7, "stable", "in_progress", None),
            11,
        );
        feed(
            &mut state,
            &mut facts,
            job(8, "msrv", "in_progress", None),
            12,
        );
        feed(&mut state, &mut facts, job(9, "docker", "queued", None), 13);
        feed(
            &mut state,
            &mut facts,
            job(8, "msrv", "completed", Some("success")),
            20,
        );

        let group = facts.jobs()[0].1.group.clone().unwrap();
        let rows: Vec<_> = facts
            .run_jobs(&group)
            .unwrap()
            .iter()
            .map(|j| (j.name.clone(), j.status))
            .collect();
        assert_eq!(
            rows,
            [
                ("stable".to_string(), RunStatus::Running),
                ("msrv".to_string(), RunStatus::Success),
                ("docker".to_string(), RunStatus::Queued),
            ]
        );

        // The run completes: jobs and list are gone.
        feed(
            &mut state,
            &mut facts,
            run_event("completed", Some("success")),
            30,
        );
        assert!(facts.jobs().is_empty());
        assert!(facts.run_jobs(&group).is_none());
    }

    #[test]
    fn polled_runners_replace_the_scope_in_the_fact_store() {
        let mut state = state(true);
        let mut facts = CiFacts::default();
        let polled = |names: &[&str]| GiteaEvent::RunnersPolled {
            scope: runners::RunnerScope::Org("team".into()),
            runners: names
                .iter()
                .map(|name| runners::ApiRunner {
                    name: name.to_string(),
                    status: "idle".into(),
                    ..Default::default()
                })
                .collect(),
        };
        feed(&mut state, &mut facts, polled(&["a", "b"]), 10);
        assert_eq!(facts.runners().len(), 2);
        // `b` was removed from Gitea: the next poll drops it.
        feed(&mut state, &mut facts, polled(&["a"]), 40);
        let names: Vec<_> = facts
            .runners()
            .iter()
            .map(|(r, _)| r.name.clone())
            .collect();
        assert_eq!(names, ["a"]);
        assert_eq!(facts.runners()[0].0.source, "gitea");
    }

    fn state(interrupt: bool) -> GiteaState {
        GiteaState::new(
            SourceId::new(KIND, "home"),
            Interrupt::All(interrupt),
            BTreeMap::new(),
        )
    }

    /// Apply an event and its updates, as the source task and main loop do.
    fn feed(state: &mut GiteaState, facts: &mut CiFacts, event: GiteaEvent, now: u64) {
        for update in state.apply(event, now) {
            facts.apply(update, now);
        }
    }

    fn top(facts: &CiFacts, now: u64) -> Option<(Level, ScreenData)> {
        let candidates = facts.candidates();
        pick(&candidates, now).map(|c| (c.level, c.data.clone()))
    }

    #[test]
    fn job_lifecycle_with_progress_and_failed_alert() {
        let mut source = state(true);
        let mut facts = CiFacts::default();
        let now = 1_791_400_000;

        feed(&mut source, &mut facts, run_event("in_progress", None), now);
        feed(
            &mut source,
            &mut facts,
            job_event("in_progress", None, json!(null)),
            now,
        );
        let (level, ScreenData::Job(job)) = top(&facts, now).unwrap() else {
            panic!("expected a job screen");
        };
        assert_eq!(level, Level::Job);
        assert_eq!(job.pipeline, "deploy.yml");
        assert_eq!(job.kind, JobKind::Deploy);
        assert_eq!(job.commit, "a1b2c3d");
        assert_eq!(job.progress, None);
        assert_eq!(source.running_jobs().len(), 1);

        // A poll brings step progress.
        let steps = json!([
            { "name": "Set up job", "status": "completed", "conclusion": "success" },
            { "name": "cargo test", "status": "in_progress" },
            { "name": "Complete job", "status": "queued" },
            { "name": "Post", "status": "queued" }
        ]);
        let polled: Job = serde_json::from_value(job_json("in_progress", None, steps)).unwrap();
        feed(
            &mut source,
            &mut facts,
            GiteaEvent::JobPolled {
                repo: "me/demo".into(),
                job: polled,
            },
            now + 5,
        );
        let Some((_, ScreenData::Job(job))) = top(&facts, now + 5) else {
            panic!("expected a job screen");
        };
        assert_eq!(job.step.as_deref(), Some("cargo test"));
        assert_eq!((job.step_no, job.step_count), (Some(2), Some(4)));
        assert_eq!(job.progress, Some(0.25));

        // The job fails, then the run completes: red alert naming the step.
        let steps = json!([
            { "name": "Set up job", "status": "completed", "conclusion": "success" },
            { "name": "cargo test", "status": "completed", "conclusion": "failure" }
        ]);
        feed(
            &mut source,
            &mut facts,
            job_event("completed", Some("failure"), steps),
            now + 60,
        );
        assert!(source.running_jobs().is_empty());
        feed(
            &mut source,
            &mut facts,
            run_event("completed", Some("failure")),
            now + 61,
        );
        let Some((level, ScreenData::Alert(alert))) = top(&facts, now + 61) else {
            panic!("expected an alert screen");
        };
        assert_eq!(level, Level::AlertFailed);
        assert_eq!(alert.step.as_deref(), Some("cargo test"));
        assert_eq!(alert.pipeline.as_deref(), Some("deploy.yml"));
        assert_eq!(alert.finished, Some(alert.started + 300));
    }

    #[test]
    fn late_poll_does_not_revive_finished_job() {
        let mut source = state(true);
        let mut facts = CiFacts::default();
        feed(
            &mut source,
            &mut facts,
            job_event("in_progress", None, json!([])),
            100,
        );
        feed(
            &mut source,
            &mut facts,
            job_event("completed", Some("success"), json!([])),
            110,
        );
        let stale: Job = serde_json::from_value(job_json("in_progress", None, json!([]))).unwrap();
        feed(
            &mut source,
            &mut facts,
            GiteaEvent::JobPolled {
                repo: "me/demo".into(),
                job: stale,
            },
            111,
        );
        assert!(facts.candidates().is_empty());
    }

    #[test]
    fn success_flashes_and_cancel_shows_nothing() {
        let mut source = state(true);
        let mut facts = CiFacts::default();
        feed(
            &mut source,
            &mut facts,
            run_event("completed", Some("success")),
            100,
        );
        assert_eq!(top(&facts, 100).unwrap().0, Level::AlertSuccess);
        assert!(top(&facts, 110).is_none(), "flash is gone after 10 s");

        let mut facts = CiFacts::default();
        feed(
            &mut source,
            &mut facts,
            run_event("completed", Some("cancelled")),
            100,
        );
        assert!(facts.candidates().is_empty());
    }

    #[test]
    fn pending_and_queued_jobs_wait_for_a_runner() {
        // Gitea 28 adds `pending`; like `queued` it is not on screen yet.
        let mut source = state(true);
        let mut facts = CiFacts::default();
        for status in ["queued", "waiting", "pending"] {
            feed(
                &mut source,
                &mut facts,
                job_event(status, None, json!(null)),
                100,
            );
        }
        assert!(facts.candidates().is_empty());
        assert!(source.running_jobs().is_empty());
    }

    #[test]
    fn no_interrupt_keeps_jobs_off_screen() {
        let mut source = state(false);
        let mut facts = CiFacts::default();
        feed(
            &mut source,
            &mut facts,
            job_event("in_progress", None, json!([])),
            100,
        );
        assert!(facts.candidates().is_empty());
        // Still polled, so progress is ready if interrupts are switched on later.
        assert_eq!(source.running_jobs().len(), 1);
    }

    #[test]
    fn pull_request_events_flash_and_count() {
        let mut source = state(true);
        let mut facts = CiFacts::default();
        let event = |action: &str| {
            let value = json!({
                "action": action,
                "pull_request": { "number": 43, "title": "add invoice export" },
                "repository": repo()
            });
            GiteaEvent::PullRequest(serde_json::from_value(value).unwrap())
        };
        feed(&mut source, &mut facts, event("opened"), 100);
        assert_eq!(facts.open_pull_count(), 1);
        let Some((Level::Notice, ScreenData::Notice(notice))) = top(&facts, 100) else {
            panic!("expected a notice");
        };
        assert_eq!(notice.sub, "demo #43: add invoice export");

        feed(&mut source, &mut facts, event("closed"), 200);
        assert_eq!(facts.open_pull_count(), 0);
    }

    #[test]
    fn pull_polls_set_health_and_keep_count_on_failure() {
        let mut source = state(true);
        let mut facts = CiFacts::default();
        assert_eq!(source.health(), None, "no poll yet");
        let pulls = api::OpenPulls {
            pulls: vec![],
            total: 2,
        };
        let polled = |repo: &str, pulls| GiteaEvent::PullsPolled {
            repo: repo.into(),
            pulls,
        };
        feed(&mut source, &mut facts, polled("me/demo", Ok(pulls)), 100);
        assert_eq!(facts.open_pull_count(), 2);
        assert_eq!(source.health(), Some(Health::Ok));

        let other = api::OpenPulls::default();
        feed(&mut source, &mut facts, polled("me/other", Ok(other)), 100);
        feed(
            &mut source,
            &mut facts,
            polled("me/demo", Err(Health::AuthFailed)),
            160,
        );
        assert_eq!(source.health(), Some(Health::AuthFailed), "worst repo wins");
        assert_eq!(facts.open_pull_count(), 2);
    }

    #[test]
    fn runs_fill_the_pipeline_rows() {
        let mut source = state(true);
        let mut facts = CiFacts::default();
        feed(&mut source, &mut facts, run_event("in_progress", None), 100);
        let page = facts.pipeline_rows();
        assert_eq!(page.count, 1);
        assert_eq!(page.rows[0].text, "deploy.yml");
        assert_eq!(page.rows[0].sub, "demo main");
        assert_eq!(page.rows[0].status, crate::model::RowStatus::Running);

        // Same pipeline, finished: one row, now green.
        feed(
            &mut source,
            &mut facts,
            run_event("completed", Some("success")),
            200,
        );
        let page = facts.pipeline_rows();
        assert_eq!(page.count, 1);
        assert_eq!(page.rows[0].status, crate::model::RowStatus::Ok);
    }

    #[test]
    fn interrupt_list_and_alias_per_repo() {
        let mut source = GiteaState::new(
            SourceId::new(KIND, "home"),
            Interrupt::Only(vec!["me/other".into()]),
            BTreeMap::from([("me/demo".to_string(), "Demo".to_string())]),
        );
        let mut facts = CiFacts::default();
        feed(
            &mut source,
            &mut facts,
            job_event("in_progress", None, json!([])),
            100,
        );
        assert!(facts.candidates().is_empty(), "me/demo may not interrupt");

        // A failure of a quiet repo counts in the badge and names the alias.
        feed(
            &mut source,
            &mut facts,
            run_event("completed", Some("failure")),
            200,
        );
        assert!(facts.candidates().is_empty());
        assert_eq!(facts.failed_count(), 1);
        assert_eq!(facts.pipeline_rows().rows[0].sub, "Demo main");
    }

    #[test]
    fn kiosk_links_point_at_the_instance() {
        let mut source = state(true).with_web("https://git.example/");
        let run = |extra: serde_json::Value| {
            let mut run = json!({
                "id": 2, "path": "deploy.yml@refs/heads/main", "head_branch": "main",
                "status": "in_progress", "started_at": "2026-10-08T18:00:00Z"
            });
            run.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let value =
                json!({ "action": "in_progress", "workflow_run": run, "repository": repo() });
            GiteaEvent::WorkflowRun(serde_json::from_value(value).unwrap())
        };
        let url_of = |updates: Vec<Update>| match updates.into_iter().next() {
            Some(Update::Run { run, .. }) => run.url,
            other => panic!("expected a run, got {other:?}"),
        };
        // No address from Gitea: the repository's Actions page.
        assert_eq!(
            url_of(source.apply(run(json!({})), 100)).as_deref(),
            Some("https://git.example/me/demo/actions")
        );
        // With the run number: that run.
        assert_eq!(
            url_of(source.apply(run(json!({ "run_number": 12 })), 100)).as_deref(),
            Some("https://git.example/me/demo/actions/runs/12")
        );
        // Gitea's own address wins, but only when it is http(s).
        assert_eq!(
            url_of(source.apply(
                run(json!({ "html_url": "https://git.example/me/demo/actions/runs/9" })),
                100
            ))
            .as_deref(),
            Some("https://git.example/me/demo/actions/runs/9")
        );
        assert_eq!(
            url_of(source.apply(
                run(json!({ "html_url": "javascript:alert(1)", "run_number": 3 })),
                100
            ))
            .as_deref(),
            Some("https://git.example/me/demo/actions/runs/3")
        );

        // A running job links to its job page, else the Actions page.
        let updates = source.apply(job_event("in_progress", None, json!([])), 100);
        let Some(Update::JobRunning { job, .. }) = updates
            .iter()
            .find(|u| matches!(u, Update::JobRunning { .. }))
        else {
            panic!("expected a running job");
        };
        assert_eq!(
            job.url.as_deref(),
            Some("https://git.example/me/demo/actions")
        );

        // PRs, from a webhook and from a poll.
        let pr = GiteaEvent::PullRequest(
            serde_json::from_value(json!({
                "action": "opened",
                "pull_request": { "number": 43, "title": "x" },
                "repository": repo()
            }))
            .unwrap(),
        );
        let updates = source.apply(pr, 100);
        let Some(Update::PullOpened { pull, .. }) = updates.first() else {
            panic!("expected a PR");
        };
        assert_eq!(
            pull.url.as_deref(),
            Some("https://git.example/me/demo/pulls/43")
        );
        let polled = api::OpenPulls {
            pulls: vec![serde_json::from_value(json!({ "number": 7, "title": "y" })).unwrap()],
            total: 1,
        };
        assert_eq!(
            source.repo_pulls("me/demo", polled).pulls[0].url.as_deref(),
            Some("https://git.example/me/demo/pulls/7")
        );

        // Runner pages per scope.
        let scope = |text: &str| source.runners_url(&runners::RunnerScope::parse(text).unwrap());
        assert_eq!(
            scope("admin").as_deref(),
            Some("https://git.example/-/admin/actions/runners")
        );
        assert_eq!(
            scope("user").as_deref(),
            Some("https://git.example/user/settings/actions/runners")
        );
        assert_eq!(
            scope("org:team").as_deref(),
            Some("https://git.example/org/team/settings/actions/runners")
        );
        assert_eq!(
            scope("repo:me/demo").as_deref(),
            Some("https://git.example/me/demo/settings/actions/runners")
        );
    }

    #[test]
    fn without_a_web_address_there_are_no_links() {
        let mut source = state(true);
        let updates = source.apply(run_event("in_progress", None), 100);
        let Some(Update::Run { run, .. }) = updates.first() else {
            panic!("expected a run");
        };
        assert_eq!(run.url, None);
    }

    #[test]
    fn step_progress_edge_cases() {
        assert_eq!(step_progress(&[]), StepProgress::default());
        let all_done: Vec<payload::Step> = (0..2)
            .map(|_| payload::Step {
                status: "completed".into(),
                ..Default::default()
            })
            .collect();
        let progress = step_progress(&all_done);
        assert_eq!(progress.fraction, Some(1.0));
        assert_eq!(progress.step, None);
    }

    #[test]
    fn issue_webhook_raises_an_alert_when_opened() {
        let mut source = state(true)
            .with_web("https://git.example")
            .with_issue_alerts(true);
        let event = |action: &str| {
            let value = json!({
                "action": action,
                "issue": { "number": 7, "title": "login loops", "user": { "login": "dana" } },
                "repository": repo()
            });
            webhook::parse_event("issues", value.to_string().as_bytes())
                .unwrap()
                .unwrap()
        };
        let updates = source.apply(event("opened"), 100);
        let [Update::Alert { alert }] = updates.as_slice() else {
            panic!("one alert expected, got {updates:?}");
        };
        assert_eq!(alert.title.as_deref(), Some("login loops"));
        assert_eq!(
            alert.message.as_deref(),
            Some("New issue #7 in demo by dana")
        );
        assert_eq!(
            alert.url.as_deref(),
            Some("https://git.example/me/demo/issues/7")
        );
        assert!(source.apply(event("closed"), 110).is_empty());

        let mut quiet = state(true);
        assert!(
            quiet.apply(event("opened"), 100).is_empty(),
            "off by default in the state"
        );
    }
}
