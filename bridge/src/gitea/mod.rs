//! Gitea source: turns Gitea Actions and pull request events into CI facts.
//!
//! Data comes in three ways, all ending up as a `GiteaEvent` on one channel
//! that the main loop feeds to `GiteaSource::apply`:
//!
//! - Webhooks (`webhook.rs`): instant job start/end, run results and PR changes.
//! - Job polling (`api.rs`): Gitea webhooks fire per job, not per step, so
//!   while a job runs the bridge polls its steps for the progress bar.
//! - PR polling (`api.rs`): a safety net that resets the open PR count, in
//!   case a webhook was missed or the bridge was down.
//!
//! The webhook settings needed in Gitea are in the README.

pub mod api;
pub mod payload;
pub mod webhook;

use std::collections::{HashMap, HashSet};

use tracing::{debug, info};

use crate::ci::{CiFacts, OpenPull, RepoPulls, RunningJob};
use crate::model::{
    AlertData, AlertStatus, JobData, JobKind, MAX_TITLE_CHARS, NoticeData, truncate,
};
use payload::{
    Job, PullRequestEvent, WorkflowJobEvent, WorkflowRunEvent, unix_seconds, workflow_file,
};

/// Value of `source` in every page this module produces.
pub const SOURCE: &str = "gitea";

/// Something the Gitea source learned, from a webhook or a poll.
#[derive(Debug)]
pub enum GiteaEvent {
    WorkflowRun(WorkflowRunEvent),
    WorkflowJob(WorkflowJobEvent),
    PullRequest(PullRequestEvent),
    /// Fresh job state from the jobs API, for step progress.
    JobPolled {
        repo: String,
        job: Job,
    },
    /// Open PRs of one repository from the API, or `None` if the poll failed.
    PullsPolled {
        repo: String,
        pulls: Option<api::OpenPulls>,
    },
}

/// A job the bridge is tracking, so the poller knows what to ask for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobRef {
    /// `owner/name`.
    pub repo: String,
    pub id: u64,
}

/// State the Gitea source keeps between events.
#[derive(Debug, Default)]
pub struct GiteaSource {
    /// May Gitea jobs take over the screen? From `gitea.interrupt`.
    interrupt: bool,
    /// Workflow file per run id, learned from `workflow_run` events, so job
    /// screens can show `deploy.yml` instead of just the job name.
    run_files: HashMap<u64, String>,
    /// Running jobs and the run they belong to.
    running: HashMap<JobRef, u64>,
    /// First failing step per run, from `workflow_job` events, for the red alert.
    failed_steps: HashMap<u64, String>,
    /// Repositories whose last PR poll failed.
    failed_pull_polls: HashSet<String>,
}

impl GiteaSource {
    pub fn new(interrupt: bool) -> Self {
        Self {
            interrupt,
            ..Default::default()
        }
    }

    /// Jobs to poll for step progress, in a stable order.
    pub fn running_jobs(&self) -> Vec<JobRef> {
        let mut jobs: Vec<JobRef> = self.running.keys().cloned().collect();
        jobs.sort();
        jobs
    }

    /// Apply one event to the facts. `now` is Unix seconds.
    pub fn apply(&mut self, event: GiteaEvent, facts: &mut CiFacts, now: u64) {
        match event {
            GiteaEvent::WorkflowRun(event) => self.on_run(event, facts, now),
            GiteaEvent::WorkflowJob(event) => {
                self.on_job(&event.repository.full_name, event.workflow_job, facts, now)
            }
            GiteaEvent::JobPolled { repo, job } => {
                // Only refresh jobs still tracked: a late poll result must not
                // bring back a job whose `completed` webhook already arrived.
                let tracked = JobRef {
                    repo: repo.clone(),
                    id: job.id,
                };
                if self.running.contains_key(&tracked) {
                    self.on_job(&repo, job, facts, now);
                }
            }
            GiteaEvent::PullRequest(event) => on_pull_request(event, facts, now),
            GiteaEvent::PullsPolled { repo, pulls } => {
                match pulls {
                    Some(pulls) => {
                        self.failed_pull_polls.remove(&repo);
                        facts.set_pulls(&repo_key(&repo), repo_pulls(&repo, pulls));
                    }
                    None => {
                        self.failed_pull_polls.insert(repo);
                    }
                }
                facts.pulls_stale = !self.failed_pull_polls.is_empty();
            }
        }
    }

    fn on_run(&mut self, event: WorkflowRunEvent, facts: &mut CiFacts, now: u64) {
        let run = event.workflow_run;
        let repo = &event.repository;
        let file = workflow_file(&run.path);
        let pipeline = if file.is_empty() {
            run.display_title.clone()
        } else {
            file.to_string()
        };
        self.run_files.insert(run.id, pipeline.clone());

        if event.action != "completed" {
            return;
        }
        self.run_files.remove(&run.id);
        let failed_step = self.failed_steps.remove(&run.id);

        // Any job of this run still on screen has finished too.
        let done: Vec<JobRef> = self
            .running
            .iter()
            .filter(|(_, run_id)| **run_id == run.id)
            .map(|(job, _)| job.clone())
            .collect();
        for job in done {
            self.running.remove(&job);
            facts.job_done(&job_key(&job));
        }

        let Some(status) = alert_status(run.conclusion.as_deref()) else {
            info!(repo = %repo.full_name, %pipeline, conclusion = ?run.conclusion, "run ended without an alert");
            return;
        };
        info!(repo = %repo.full_name, %pipeline, ?status, "run finished");
        let data = AlertData {
            status,
            source: SOURCE.into(),
            project: truncate(&repo.name, MAX_TITLE_CHARS),
            pipeline: truncate(&pipeline, MAX_TITLE_CHARS),
            step: failed_step
                .filter(|_| status == AlertStatus::Failed)
                .map(|s| truncate(&s, MAX_TITLE_CHARS)),
            started: unix_seconds(run.started_at.as_deref()).unwrap_or(now),
            finished: unix_seconds(run.completed_at.as_deref()).unwrap_or(now),
            others: 0,
        };
        let key = format!("{SOURCE}:{}:{pipeline}", repo.full_name);
        facts.run_finished(&key, data, now);
    }

    fn on_job(&mut self, repo: &str, job: Job, facts: &mut CiFacts, now: u64) {
        let job_ref = JobRef {
            repo: repo.to_string(),
            id: job.id,
        };
        let key = job_key(&job_ref);

        match job.status.as_str() {
            "in_progress" => {
                self.running.insert(job_ref, job.run_id);
                let project = repo.rsplit('/').next().unwrap_or(repo);
                let data = self.job_data(project, &job, now);
                debug!(%key, step = ?data.step, progress = ?data.progress, "job running");
                let running = RunningJob {
                    data,
                    interrupt: self.interrupt,
                    updated: now,
                };
                facts.job_running(&key, running);
            }
            "completed" => {
                self.running.remove(&job_ref);
                facts.job_done(&key);
                if alert_status(job.conclusion.as_deref()) == Some(AlertStatus::Failed)
                    && let Some(step) = failing_step(&job)
                {
                    self.failed_steps.entry(job.run_id).or_insert(step);
                }
            }
            // queued / waiting: nothing to show until a runner picks it up.
            _ => {}
        }
    }

    fn job_data(&self, project: &str, job: &Job, now: u64) -> JobData {
        let pipeline = self
            .run_files
            .get(&job.run_id)
            .cloned()
            .unwrap_or_else(|| job.name.clone());
        let steps = job.steps.as_deref().unwrap_or_default();
        let progress = step_progress(steps);
        JobData {
            source: SOURCE.into(),
            project: truncate(project, MAX_TITLE_CHARS),
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
}

fn on_pull_request(event: PullRequestEvent, facts: &mut CiFacts, now: u64) {
    let repo = &event.repository;
    let pull = &event.pull_request;
    let key = repo_key(&repo.full_name);
    match event.action.as_str() {
        "opened" | "reopened" => {
            let open = OpenPull {
                number: pull.number,
                title: pull.title.clone(),
                created: unix_seconds(pull.created_at.as_deref()).unwrap_or(now),
            };
            let is_new = facts.pull_opened(&key, &repo.name, SOURCE, open);
            // Flash only for brand-new PRs, not reopens or redelivered webhooks.
            if is_new && event.action == "opened" {
                let sub = format!("{} #{}: {}", repo.name, pull.number, pull.title);
                facts.notice(
                    NoticeData {
                        text: "New PR".into(),
                        sub: truncate(&sub, MAX_TITLE_CHARS),
                        source: SOURCE.into(),
                    },
                    now,
                );
            }
        }
        "closed" => facts.pull_closed(&key, pull.number),
        _ => {}
    }
}

/// Key for a job in `CiFacts`.
fn job_key(job: &JobRef) -> String {
    format!("{SOURCE}:{}:{}", job.repo, job.id)
}

/// Key for a repository's PRs in `CiFacts`.
fn repo_key(full_name: &str) -> String {
    format!("{SOURCE}:{full_name}")
}

fn repo_pulls(full_name: &str, polled: api::OpenPulls) -> RepoPulls {
    let pulls = polled
        .pulls
        .into_iter()
        .map(|p| OpenPull {
            number: p.number,
            title: p.title,
            created: unix_seconds(p.created_at.as_deref()).unwrap_or(0),
        })
        .collect();
    RepoPulls {
        project: full_name.rsplit('/').next().unwrap_or(full_name).into(),
        source: SOURCE.into(),
        pulls,
        total: polled.total,
    }
}

/// Map a Gitea conclusion to an alert. Cancelled and skipped runs show nothing:
/// someone stopped them on purpose.
fn alert_status(conclusion: Option<&str>) -> Option<AlertStatus> {
    match conclusion? {
        "success" => Some(AlertStatus::Success),
        "failure" | "timed_out" => Some(AlertStatus::Failed),
        _ => None,
    }
}

/// Deploy if the workflow file or job name mentions "deploy", otherwise build.
fn job_kind(pipeline: &str, job_name: &str) -> JobKind {
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
        .find(|s| alert_status(s.conclusion.as_deref()) == Some(AlertStatus::Failed))
        .map(|s| s.name.clone())
}

/// Step progress of a running job.
#[derive(Debug, Default, PartialEq)]
struct StepProgress {
    step: Option<String>,
    step_no: Option<u32>,
    step_count: Option<u32>,
    fraction: Option<f32>,
}

/// Work out the current step and progress from the step list. With no steps
/// (job just picked up, or webhook without steps) everything is `None` and
/// the panel shows a spinner.
fn step_progress(steps: &[payload::Step]) -> StepProgress {
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

    fn top(facts: &CiFacts, now: u64) -> Option<(Level, ScreenData)> {
        let candidates = facts.candidates();
        pick(&candidates, now).map(|c| (c.level, c.data.clone()))
    }

    #[test]
    fn job_lifecycle_with_progress_and_failed_alert() {
        let mut source = GiteaSource::new(true);
        let mut facts = CiFacts::default();
        let now = 1_791_400_000;

        source.apply(run_event("in_progress", None), &mut facts, now);
        source.apply(job_event("in_progress", None, json!(null)), &mut facts, now);
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
        source.apply(
            GiteaEvent::JobPolled {
                repo: "me/demo".into(),
                job: polled,
            },
            &mut facts,
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
        source.apply(
            job_event("completed", Some("failure"), steps),
            &mut facts,
            now + 60,
        );
        assert!(source.running_jobs().is_empty());
        source.apply(
            run_event("completed", Some("failure")),
            &mut facts,
            now + 61,
        );
        let Some((level, ScreenData::Alert(alert))) = top(&facts, now + 61) else {
            panic!("expected an alert screen");
        };
        assert_eq!(level, Level::AlertFailed);
        assert_eq!(alert.step.as_deref(), Some("cargo test"));
        assert_eq!(alert.pipeline, "deploy.yml");
        assert_eq!(alert.finished - alert.started, 300);
    }

    #[test]
    fn late_poll_does_not_revive_finished_job() {
        let mut source = GiteaSource::new(true);
        let mut facts = CiFacts::default();
        source.apply(job_event("in_progress", None, json!([])), &mut facts, 100);
        source.apply(
            job_event("completed", Some("success"), json!([])),
            &mut facts,
            110,
        );
        let stale: Job = serde_json::from_value(job_json("in_progress", None, json!([]))).unwrap();
        source.apply(
            GiteaEvent::JobPolled {
                repo: "me/demo".into(),
                job: stale,
            },
            &mut facts,
            111,
        );
        assert!(facts.candidates().is_empty());
    }

    #[test]
    fn success_flashes_and_cancel_shows_nothing() {
        let mut source = GiteaSource::new(true);
        let mut facts = CiFacts::default();
        source.apply(run_event("completed", Some("success")), &mut facts, 100);
        assert_eq!(top(&facts, 100).unwrap().0, Level::AlertSuccess);
        assert!(top(&facts, 110).is_none(), "flash is gone after 10 s");

        let mut facts = CiFacts::default();
        source.apply(run_event("completed", Some("cancelled")), &mut facts, 100);
        assert!(facts.candidates().is_empty());
    }

    #[test]
    fn pending_and_queued_jobs_wait_for_a_runner() {
        // Gitea 28 adds `pending`; like `queued` it is not on screen yet.
        let mut source = GiteaSource::new(true);
        let mut facts = CiFacts::default();
        for status in ["queued", "waiting", "pending"] {
            source.apply(job_event(status, None, json!(null)), &mut facts, 100);
        }
        assert!(facts.candidates().is_empty());
        assert!(source.running_jobs().is_empty());
    }

    #[test]
    fn no_interrupt_keeps_jobs_off_screen() {
        let mut source = GiteaSource::new(false);
        let mut facts = CiFacts::default();
        source.apply(job_event("in_progress", None, json!([])), &mut facts, 100);
        assert!(facts.candidates().is_empty());
        // Still polled, so progress is ready if interrupts are switched on later.
        assert_eq!(source.running_jobs().len(), 1);
    }

    #[test]
    fn pull_request_events_flash_and_count() {
        let mut source = GiteaSource::new(true);
        let mut facts = CiFacts::default();
        let event = |action: &str| {
            let value = json!({
                "action": action,
                "pull_request": { "number": 43, "title": "add invoice export" },
                "repository": repo()
            });
            GiteaEvent::PullRequest(serde_json::from_value(value).unwrap())
        };
        source.apply(event("opened"), &mut facts, 100);
        assert_eq!(facts.open_pull_count(), 1);
        let Some((Level::Notice, ScreenData::Notice(notice))) = top(&facts, 100) else {
            panic!("expected a notice");
        };
        assert_eq!(notice.sub, "demo #43: add invoice export");

        source.apply(event("closed"), &mut facts, 200);
        assert_eq!(facts.open_pull_count(), 0);
    }

    #[test]
    fn failed_pull_poll_marks_page_stale_and_keeps_count() {
        let mut source = GiteaSource::new(true);
        let mut facts = CiFacts::default();
        let pulls = api::OpenPulls {
            pulls: vec![],
            total: 2,
        };
        let polled = |pulls| GiteaEvent::PullsPolled {
            repo: "me/demo".into(),
            pulls,
        };
        source.apply(polled(Some(pulls)), &mut facts, 100);
        assert_eq!(facts.open_pull_count(), 2);
        source.apply(polled(None), &mut facts, 160);
        assert!(facts.pulls_stale);
        assert_eq!(facts.open_pull_count(), 2);
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
}
