//! CI facts shared by every CI source: running jobs, runs per pipeline, open
//! PRs and short notices.
//!
//! Sources (Gitea, GitHub, Azure DevOps) send `Update`s; the
//! main loop applies them here. Sources never decide what is on screen. The
//! composer turns these facts into screen candidates and header badges; the
//! kiosk reads the rest straight from here.
//!
//! Every key starts with the source id (`gitea:home:...`), so two instances of
//! the same adapter never overwrite each other.

use crate::alerts::AlertMessage;
use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::model::{
    AlertData, AlertStatus, Badge, BadgeIcon, Candidate, JobData, Level, MAX_TITLE_CHARS,
    NoticeData, RowStatus, ScreenData, truncate,
};

/// A running job with no update for this long is assumed lost (missed
/// webhook, bridge restart on the CI side) and dropped. Gitea's default job
/// timeout is 3 hours, so this leaves plenty of room. Runs still marked
/// running after this long are shown as neutral on the pipelines page.
pub const JOB_MAX_AGE_S: u64 = 6 * 60 * 60;

/// A runner list nobody has refreshed for this long (the source stopped
/// asking, or the token lost its scope) is dropped. Until then the kiosk
/// greys it out after a few missed polls.
pub const RUNNER_MAX_AGE_S: u64 = 60 * 60;

/// Most pipelines kept for the `pipelines` page. The least recently updated
/// one is dropped first.
pub const MAX_PIPELINES: usize = 20;

/// A change reported by a source.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// A job started or made progress.
    JobRunning { key: String, job: RunningJob },
    /// The job is no longer running (finished, cancelled or lost).
    JobDone { key: String },
    /// Every job of one running run (its parallel jobs, stages or steps),
    /// for the kiosk's "Running now" list. `key` is the `group` of the run's
    /// running jobs. Replaces what was known; an empty list clears it.
    RunJobs { key: String, jobs: Vec<RunJob> },
    /// The latest run of a pipeline changed. `key` names the pipeline, so a
    /// new run replaces the old one. A finished run also raises its alert.
    Run { key: String, run: Run },
    /// The open PRs of one repository, from a poll. Replaces what was known.
    Pulls { key: String, repo: RepoPulls },
    /// A PR was opened or reopened. `notify` asks for a "New PR" flash if the
    /// PR was not known yet.
    PullOpened {
        key: String,
        project: String,
        source: String,
        pull: OpenPull,
        notify: bool,
    },
    /// A PR was closed or merged.
    PullClosed { key: String, number: u64 },
    /// A short flash that is not about a PR, such as a deploy waiting for
    /// approval.
    Notice { notice: NoticeData },
    /// A new issue or comment, for the alerts list. The composer hands it to
    /// the alerts; the CI facts never see it.
    Alert { alert: AlertMessage },
    /// Every runner of one scope (a user, an organisation, a repository, a
    /// pool). Replaces what was known for `key`, so a runner that was removed
    /// disappears and an empty list clears the scope.
    Runners { key: String, runners: Vec<Runner> },
    /// How many jobs wait for an agent in one pool (Azure DevOps). Replaces
    /// what was known for `key`; zero clears it.
    QueuedJobs {
        key: String,
        source: String,
        pool: String,
        queued: u32,
    },
}

/// What a runner (Gitea runner, GitHub self-hosted runner, Azure DevOps
/// agent) is doing, the same for every CI source (integrations plan 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunnerStatus {
    // Declared in widget order: what needs attention first.
    Offline,
    Busy,
    Idle,
}

impl RunnerStatus {
    /// Lowercase name used by the kiosk page.
    pub fn name(self) -> &'static str {
        match self {
            RunnerStatus::Offline => "offline",
            RunnerStatus::Busy => "busy",
            RunnerStatus::Idle => "idle",
        }
    }
}

/// One runner, as a CI source reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runner {
    /// Adapter type, such as `gitea`.
    pub source: String,
    pub name: String,
    pub status: RunnerStatus,
    /// Switched off by an administrator. Not an outage, so it is shown
    /// neutral and never counts as offline.
    pub disabled: bool,
    pub labels: Vec<String>,
    /// Web page of the runner list (its scope or pool) on the source, if known.
    pub url: Option<String>,
}

/// The runners of one scope with the Unix seconds they were last refreshed,
/// so the page can grey out a list that stopped updating.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunnerSet {
    runners: Vec<Runner>,
    updated: u64,
}

/// Jobs waiting for an agent in one pool, with the time it was refreshed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedJobs {
    pub source: String,
    pub pool: String,
    pub queued: u32,
}

/// A job that is running right now.
#[derive(Debug, Clone, PartialEq)]
pub struct RunningJob {
    /// Screen data; `others` is filled in by `CiFacts::candidates`.
    pub data: JobData,
    /// May this job take over the screen (level 1)? Off for sources that
    /// should only show up as badges, such as work pipelines.
    pub interrupt: bool,
    /// Unix seconds of the last webhook or poll that mentioned this job.
    pub updated: u64,
    /// Web page of the job or its run on the source, if known.
    pub url: Option<String>,
    /// The run this job belongs to, the key of its `Update::RunJobs`. Jobs
    /// of one run share it, so the kiosk lists them under one heading.
    pub group: Option<String>,
}

/// One job of a running run: a parallel job (GitHub, Gitea, Azure DevOps), a
/// job of a stage (GitLab) or a step (Bitbucket).
#[derive(Debug, Clone, PartialEq)]
pub struct RunJob {
    pub name: String,
    pub status: RunStatus,
    /// Current step while it runs; failing step once it failed.
    pub step: Option<String>,
    /// 0.0 to 1.0 while it runs, if known.
    pub progress: Option<f32>,
    pub started: Option<u64>,
    pub finished: Option<u64>,
    /// Web page of the job on the source, if known.
    pub url: Option<String>,
}

/// Status of a run, the same for every CI source (integrations plan 8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunStatus {
    // Declared in pipelines page order: what needs attention first.
    Running,
    Failed,
    /// Started but waiting for someone to approve it (Azure DevOps
    /// environment approvals). Shown as a `review` row.
    Waiting,
    Queued,
    Success,
    /// Cancelled, skipped or lost. Never raises an alert.
    Neutral,
}

impl RunStatus {
    /// Lowercase name used by the kiosk page.
    pub fn name(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Failed => "failed",
            RunStatus::Waiting => "waiting",
            RunStatus::Queued => "queued",
            RunStatus::Success => "success",
            RunStatus::Neutral => "neutral",
        }
    }

    #[cfg(test)]
    fn row_status(self) -> RowStatus {
        match self {
            RunStatus::Running => RowStatus::Running,
            RunStatus::Failed => RowStatus::Failed,
            RunStatus::Success => RowStatus::Ok,
            RunStatus::Waiting => RowStatus::Review,
            RunStatus::Queued | RunStatus::Neutral => RowStatus::Neutral,
        }
    }

    fn alert(self) -> Option<AlertStatus> {
        match self {
            RunStatus::Failed => Some(AlertStatus::Failed),
            RunStatus::Success => Some(AlertStatus::Success),
            _ => None,
        }
    }
}

/// The latest run of one pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Adapter type, such as `gitea`.
    pub source: String,
    /// Short project label, after `alias`.
    pub project: String,
    /// Pipeline or workflow file, such as `ci.yml`.
    pub pipeline: String,
    pub git_ref: String,
    pub status: RunStatus,
    /// First failing step, for the red alert.
    pub step: Option<String>,
    pub started: u64,
    pub finished: Option<u64>,
    /// May this run's outcome take over the screen? A failure still counts in
    /// the `failed` badge when it may not.
    pub interrupt: bool,
    /// Web page of the run on the source, if known.
    pub url: Option<String>,
}

/// The outcome of a finished run, shown as a red or green alert.
#[derive(Debug, Clone, PartialEq)]
struct RunAlert {
    data: AlertData,
    interrupt: bool,
    /// Unix seconds when the bridge learned about it. Alert timers count from here.
    raised_at: u64,
}

/// One open pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPull {
    pub number: u64,
    pub title: String,
    /// Unix seconds, used to list the newest first.
    pub created: u64,
    /// Web page of the pull request on the source, if known.
    pub url: Option<String>,
}

/// Open pull requests of one repository.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RepoPulls {
    /// Short name shown on the kiosk page, such as `deskwatch`.
    pub project: String,
    pub source: String,
    pub pulls: Vec<OpenPull>,
    /// Total open PRs. Can be more than `pulls.len()` when the poll only
    /// fetched the first page.
    pub total: u32,
}

/// Everything the CI sources know right now.
#[derive(Debug, Default)]
pub struct CiFacts {
    /// Running jobs by a source-chosen key, such as `gitea:home:owner/repo:42`.
    jobs: BTreeMap<String, RunningJob>,
    /// Latest run per pipeline, with the Unix seconds it was last updated.
    runs: BTreeMap<String, (Run, u64)>,
    /// Latest outcome per pipeline, keyed like `runs`, so a new run of the
    /// same pipeline replaces the old alert (a green run clears a red one).
    alerts: BTreeMap<String, RunAlert>,
    /// Open PRs by repository key, such as `gitea:home:owner/repo`.
    pulls: BTreeMap<String, RepoPulls>,
    /// Short flashes waiting to be shown, with the time they were raised.
    notices: Vec<(NoticeData, u64)>,
    /// Runners by scope key, such as `gitea:home:runners:org:my-org`.
    runners: BTreeMap<String, RunnerSet>,
    /// Jobs waiting for an agent by pool key, with the time they were refreshed.
    queued: BTreeMap<String, (QueuedJobs, u64)>,
    /// Job lists of running runs by group key, with the time they were refreshed.
    run_jobs: BTreeMap<String, (Vec<RunJob>, u64)>,
}

impl CiFacts {
    // --- Writes from sources ---------------------------------------------

    /// Drop every fact whose key starts with `prefix` (`gitea:home:`), after
    /// the source that wrote them was removed or replaced.
    pub fn forget(&mut self, prefix: &str) {
        self.jobs.retain(|key, _| !key.starts_with(prefix));
        self.runs.retain(|key, _| !key.starts_with(prefix));
        self.alerts.retain(|key, _| !key.starts_with(prefix));
        self.pulls.retain(|key, _| !key.starts_with(prefix));
        self.runners.retain(|key, _| !key.starts_with(prefix));
        self.queued.retain(|key, _| !key.starts_with(prefix));
        self.run_jobs.retain(|key, _| !key.starts_with(prefix));
    }

    /// Apply one update from a source. `now` is Unix seconds.
    pub fn apply(&mut self, update: Update, now: u64) {
        match update {
            Update::JobRunning { key, job } => {
                self.jobs.insert(key, job);
            }
            Update::JobDone { key } => {
                if let Some(group) = self.jobs.remove(&key).and_then(|j| j.group)
                    && !self.jobs.values().any(|j| j.group.as_ref() == Some(&group))
                {
                    self.run_jobs.remove(&group);
                }
            }
            Update::RunJobs { key, jobs } if jobs.is_empty() => {
                self.run_jobs.remove(&key);
            }
            Update::RunJobs { key, jobs } => {
                self.run_jobs.insert(key, (jobs, now));
            }
            Update::Run { key, run } => self.run(key, run, now),
            Update::Pulls { key, mut repo } => {
                repo.total = repo.total.max(repo.pulls.len() as u32);
                self.pulls.insert(key, repo);
            }
            Update::PullOpened {
                key,
                project,
                source,
                pull,
                notify,
            } => self.pull_opened(key, project, source, pull, notify, now),
            Update::PullClosed { key, number } => self.pull_closed(&key, number),
            Update::Notice { notice } => self.notices.push((notice, now)),
            Update::Alert { .. } => {}
            Update::QueuedJobs {
                key,
                source,
                pool,
                queued: 0,
            } => {
                let _ = (source, pool);
                self.queued.remove(&key);
            }
            Update::QueuedJobs {
                key,
                source,
                pool,
                queued,
            } => {
                self.queued.insert(
                    key,
                    (
                        QueuedJobs {
                            source,
                            pool,
                            queued,
                        },
                        now,
                    ),
                );
            }
            Update::Runners { key, runners } if runners.is_empty() => {
                self.runners.remove(&key);
            }
            Update::Runners { key, runners } => {
                self.runners.insert(
                    key,
                    RunnerSet {
                        runners,
                        updated: now,
                    },
                );
            }
        }
    }

    fn run(&mut self, key: String, run: Run, now: u64) {
        // A redelivered webhook for the same finished run must not flash again.
        let repeat = self.runs.get(&key).is_some_and(|(old, _)| *old == run);
        if let Some(status) = run.status.alert()
            && !repeat
        {
            let data = AlertData {
                status,
                source: run.source.clone(),
                project: Some(truncate(&run.project, MAX_TITLE_CHARS)),
                pipeline: Some(truncate(&run.pipeline, MAX_TITLE_CHARS)),
                step: run
                    .step
                    .as_deref()
                    .filter(|_| status == AlertStatus::Failed)
                    .map(|s| truncate(s, MAX_TITLE_CHARS)),
                title: None,
                message: None,
                started: run.started,
                finished: Some(run.finished.unwrap_or(now)),
                others: 0,
            };
            let alert = RunAlert {
                data,
                interrupt: run.interrupt,
                raised_at: now,
            };
            self.alerts.insert(key.clone(), alert);
        }
        self.runs.insert(key, (run, now));
        while self.runs.len() > MAX_PIPELINES {
            let oldest = self.runs.iter().min_by_key(|(_, (_, at))| *at);
            let Some((key, _)) = oldest else { break };
            let key = key.clone();
            self.runs.remove(&key);
        }
    }

    fn pull_opened(
        &mut self,
        key: String,
        project: String,
        source: String,
        pull: OpenPull,
        notify: bool,
        now: u64,
    ) {
        let repo = self.pulls.entry(key).or_insert_with(|| RepoPulls {
            project: project.clone(),
            source: source.clone(),
            ..Default::default()
        });
        if repo.pulls.iter().any(|p| p.number == pull.number) {
            return; // redelivered webhook, or already seen by a poll
        }
        repo.total += 1;
        if notify {
            let sub = format!("{project} #{}: {}", pull.number, pull.title);
            let notice = NoticeData {
                text: "New PR".into(),
                sub: truncate(&sub, MAX_TITLE_CHARS),
                source,
            };
            self.notices.push((notice, now));
        }
        repo.pulls.push(pull);
    }

    fn pull_closed(&mut self, key: &str, number: u64) {
        if let Some(repo) = self.pulls.get_mut(key) {
            let before = repo.pulls.len();
            repo.pulls.retain(|p| p.number != number);
            // Also count down when the PR was beyond the first polled page.
            if repo.pulls.len() < before || repo.total as usize > repo.pulls.len() {
                repo.total = repo.total.saturating_sub(1);
            }
        }
    }

    pub fn has_job(&self, key: &str) -> bool {
        self.jobs.contains_key(key)
    }

    // --- Housekeeping ----------------------------------------------------

    /// Drop facts that can never be shown again: finished success flashes and
    /// notices, and jobs nobody has mentioned for `JOB_MAX_AGE_S`. Failed
    /// alerts stay until dismissed or replaced by a green run, because after
    /// their screen time they still count in the header badge.
    pub fn expire(&mut self, now: u64) {
        let timed_out = |level: Level, raised_at: u64| {
            level
                .max_duration()
                .is_some_and(|max| now.saturating_sub(raised_at) >= max.as_secs())
        };
        self.alerts.retain(|_, a| {
            a.data.status == AlertStatus::Failed || !timed_out(Level::AlertSuccess, a.raised_at)
        });
        self.notices
            .retain(|(_, raised_at)| !timed_out(Level::Notice, *raised_at));
        self.jobs
            .retain(|_, j| now.saturating_sub(j.updated) < JOB_MAX_AGE_S);
        self.runners
            .retain(|_, set| now.saturating_sub(set.updated) < RUNNER_MAX_AGE_S);
        self.queued
            .retain(|_, (_, updated)| now.saturating_sub(*updated) < RUNNER_MAX_AGE_S);
        // A list whose run has no running job left, and that nobody refreshed
        // for a while (all jobs queued, or a missed "done"), goes.
        let jobs = &self.jobs;
        self.run_jobs.retain(|key, (_, updated)| {
            jobs.values().any(|j| j.group.as_ref() == Some(key))
                || now.saturating_sub(*updated) < RUNNER_MAX_AGE_S
        });
        for (run, updated) in self.runs.values_mut() {
            // A run waiting for approval can wait for days, and a polled
            // source resends it when anything changes, so it never expires.
            let active = matches!(run.status, RunStatus::Running | RunStatus::Queued);
            if active && now.saturating_sub(*updated) >= JOB_MAX_AGE_S {
                run.status = RunStatus::Neutral;
            }
        }
    }

    // --- Reads for the composer ------------------------------------------

    /// Jobs that may take over the screen, newest first.
    fn screen_jobs(&self) -> Vec<(&String, &RunningJob)> {
        let mut jobs: Vec<_> = self.jobs.iter().filter(|(_, j)| j.interrupt).collect();
        jobs.sort_by_key(|(key, job)| (Reverse(job.data.started), *key));
        jobs
    }

    /// Everything that wants to pre-empt the idle page. `model::pick`
    /// chooses among them.
    pub fn candidates(&self) -> Vec<Candidate> {
        let mut out = Vec::new();

        // Running jobs: level 1. `others` counts the rest of the interrupting
        // jobs. The newest wins.
        let running = self.screen_jobs();
        let others = running.len().saturating_sub(1) as u32;
        for (_, job) in running {
            let mut data = job.data.clone();
            data.others = others;
            out.push(Candidate {
                level: Level::Job,
                page: format!("{}-job", data.source),
                raised_at: data.started,
                data: ScreenData::Job(data),
            });
        }

        // Finished runs that may interrupt: level 2 (failed) or 3 (success).
        let shown = || self.alerts.values().filter(|a| a.interrupt);
        for alert in shown() {
            let status = alert.data.status;
            let level = match status {
                // CI runs never raise `Warn`; it shares level 2 with failures.
                AlertStatus::Failed | AlertStatus::Warn => Level::AlertFailed,
                AlertStatus::Success => Level::AlertSuccess,
            };
            let mut data = alert.data.clone();
            data.others = shown()
                .filter(|a| a.data.status == status)
                .count()
                .saturating_sub(1) as u32;
            out.push(Candidate {
                level,
                page: format!("{}-alert", data.source),
                raised_at: alert.raised_at,
                data: ScreenData::Alert(data),
            });
        }

        // Notices: level 4.
        for (notice, raised_at) in &self.notices {
            out.push(Candidate {
                level: Level::Notice,
                page: format!("{}-notice", notice.source),
                raised_at: *raised_at,
                data: ScreenData::Notice(notice.clone()),
            });
        }
        out
    }

    /// Every running job with its key, newest first (for the kiosk page).
    pub fn jobs(&self) -> Vec<(&String, &RunningJob)> {
        let mut jobs: Vec<_> = self.jobs.iter().collect();
        jobs.sort_by_key(|(_, job)| Reverse(job.data.started));
        jobs
    }

    /// The job list of a running run, by the `group` of its jobs.
    pub fn run_jobs(&self, group: &str) -> Option<&[RunJob]> {
        self.run_jobs.get(group).map(|(jobs, _)| jobs.as_slice())
    }

    /// Latest run of every pipeline with the time it was last updated,
    /// running and failed ones first, then the most recently updated.
    pub fn runs(&self) -> Vec<(&String, &Run, u64)> {
        let mut runs: Vec<_> = self
            .runs
            .iter()
            .map(|(key, (run, updated))| (key, run, *updated))
            .collect();
        runs.sort_by_key(|(_, run, updated)| (run.status, Reverse(*updated)));
        runs
    }

    /// Every known runner with the time its scope was last refreshed: offline
    /// first, then busy, then idle, each group by name. Overlapping scopes
    /// (the admin list plus an organisation) list a runner twice, so the
    /// docs say to pick one.
    pub fn runners(&self) -> Vec<(&Runner, u64)> {
        let mut runners: Vec<_> = self
            .runners
            .values()
            .flat_map(|set| set.runners.iter().map(|r| (r, set.updated)))
            .collect();
        // A disabled runner is parked, so it goes last whatever it reports.
        runners.sort_by_key(|(r, _)| (r.disabled, r.status, r.name.clone()));
        runners
    }

    /// Pools with jobs waiting for an agent, with the time they were
    /// refreshed, busiest first.
    pub fn queued_jobs(&self) -> Vec<(&QueuedJobs, u64)> {
        let mut pools: Vec<_> = self.queued.values().map(|(q, t)| (q, *t)).collect();
        pools.sort_by_key(|(q, _)| (std::cmp::Reverse(q.queued), q.pool.clone()));
        pools
    }

    /// Runners that should be up but are not (disabled ones do not count).
    pub fn offline_runner_count(&self) -> u32 {
        self.runners()
            .iter()
            .filter(|(r, _)| !r.disabled && r.status == RunnerStatus::Offline)
            .count() as u32
    }

    /// Open PRs per repository.
    pub fn pulls(&self) -> impl Iterator<Item = &RepoPulls> {
        self.pulls.values()
    }

    /// Total open PRs over all repositories.
    pub fn open_pull_count(&self) -> u32 {
        self.pulls.values().map(|r| r.total).sum()
    }

    /// Unacknowledged failed runs, on screen or not.
    pub fn failed_count(&self) -> u32 {
        self.alerts
            .values()
            .filter(|a| a.data.status == AlertStatus::Failed)
            .count() as u32
    }

    /// The failed runs behind the `failed` badge, oldest key first: key, the
    /// alert and the pipeline's latest run when it is still known (it carries
    /// the branch and the web address). The kiosk lists them so they can be
    /// looked at and dismissed.
    pub fn failed_runs(&self) -> Vec<(&String, &AlertData, Option<&Run>)> {
        self.alerts
            .iter()
            .filter(|(_, a)| a.data.status == AlertStatus::Failed)
            .map(|(key, a)| (key, &a.data, self.runs.get(key).map(|(run, _)| run)))
            .collect()
    }

    /// Header badges from CI facts, in the plan's order: failures, then PRs.
    pub fn badges(&self) -> Vec<Badge> {
        vec![
            Badge {
                id: "failed".into(),
                icon: BadgeIcon::Pipeline,
                count: self.failed_count(),
                status: RowStatus::Failed,
            },
            Badge {
                id: "prs".into(),
                icon: BadgeIcon::Pr,
                count: self.open_pull_count(),
                status: RowStatus::Open,
            },
        ]
    }
}

/// What the tests of the sources check: one row per pipeline or open PR.
#[cfg(test)]
pub(crate) struct TestRow {
    pub text: String,
    pub sub: String,
    pub status: RowStatus,
}

#[cfg(test)]
pub(crate) struct TestRows {
    /// Total, which can be more than the rows listed for PRs.
    pub count: u32,
    pub rows: Vec<TestRow>,
}

#[cfg(test)]
impl CiFacts {
    /// Pipelines as the kiosk lists them: `runs()` order, one row each.
    pub(crate) fn pipeline_rows(&self) -> TestRows {
        let rows = self
            .runs()
            .into_iter()
            .map(|(_, run, _)| TestRow {
                text: run.pipeline.clone(),
                sub: format!("{} {}", run.project, run.git_ref),
                status: run.status.row_status(),
            })
            .collect();
        TestRows {
            count: self.runs.len() as u32,
            rows,
        }
    }

    /// Open PRs, newest first.
    pub(crate) fn pull_rows(&self) -> TestRows {
        let mut rows: Vec<(u64, TestRow)> = self
            .pulls()
            .flat_map(|repo| {
                repo.pulls.iter().map(|pull| {
                    let row = TestRow {
                        text: pull.title.clone(),
                        sub: format!("{} #{}", repo.project, pull.number),
                        status: RowStatus::Open,
                    };
                    (pull.created, row)
                })
            })
            .collect();
        rows.sort_by_key(|row| Reverse(row.0));
        TestRows {
            count: self.open_pull_count(),
            rows: rows.into_iter().map(|(_, row)| row).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{JobKind, pick};

    fn job(started: u64) -> RunningJob {
        RunningJob {
            data: JobData {
                source: "gitea".into(),
                project: "demo".into(),
                pipeline: "ci.yml".into(),
                kind: JobKind::Build,
                git_ref: "main".into(),
                commit: "a1b2c3d".into(),
                step: None,
                step_no: None,
                step_count: None,
                progress: None,
                started,
                others: 0,
            },
            interrupt: true,
            updated: started,
            url: None,
            group: None,
        }
    }

    fn run(pipeline: &str, status: RunStatus) -> Run {
        Run {
            source: "gitea".into(),
            project: "demo".into(),
            pipeline: pipeline.into(),
            git_ref: "main".into(),
            status,
            step: Some("cargo test".into()),
            started: 100,
            finished: Some(200),
            interrupt: true,
            url: None,
        }
    }

    fn pull(number: u64, created: u64) -> OpenPull {
        OpenPull {
            number,
            title: format!("pr {number}"),
            created,
            url: None,
        }
    }

    fn running(facts: &mut CiFacts, key: &str, started: u64) {
        let update = Update::JobRunning {
            key: key.into(),
            job: job(started),
        };
        facts.apply(update, started);
    }

    fn finished(facts: &mut CiFacts, key: &str, run: Run, now: u64) {
        facts.apply(
            Update::Run {
                key: key.into(),
                run,
            },
            now,
        );
    }

    fn opened(facts: &mut CiFacts, pull: OpenPull, notify: bool) {
        let update = Update::PullOpened {
            key: "r".into(),
            project: "demo".into(),
            source: "gitea".into(),
            pull,
            notify,
        };
        facts.apply(update, 0);
    }

    fn shown_job(facts: &CiFacts) -> JobData {
        match &pick(&facts.candidates(), 300).unwrap().data {
            ScreenData::Job(job) => job.clone(),
            other => panic!("expected a job screen, got {other:?}"),
        }
    }

    #[test]
    fn newest_job_wins_and_counts_others() {
        let mut facts = CiFacts::default();
        running(&mut facts, "a", 100);
        running(&mut facts, "b", 200);
        let shown = shown_job(&facts);
        assert_eq!(shown.started, 200);
        assert_eq!(shown.others, 1);
    }

    #[test]
    fn non_interrupting_jobs_stay_off_screen() {
        let mut facts = CiFacts::default();
        let mut quiet = job(100);
        quiet.interrupt = false;
        facts.apply(
            Update::JobRunning {
                key: "work".into(),
                job: quiet,
            },
            100,
        );
        assert!(facts.candidates().is_empty());
    }

    #[test]
    fn green_run_replaces_red_alert_of_same_pipeline() {
        let mut facts = CiFacts::default();
        finished(&mut facts, "p", run("ci.yml", RunStatus::Failed), 100);
        assert_eq!(facts.failed_count(), 1);
        finished(&mut facts, "p", run("ci.yml", RunStatus::Success), 200);
        assert_eq!(facts.failed_count(), 0);
        assert_eq!(facts.candidates()[0].level, Level::AlertSuccess);
    }

    #[test]
    fn redelivered_run_does_not_flash_again() {
        let mut facts = CiFacts::default();
        finished(&mut facts, "p", run("ci.yml", RunStatus::Success), 100);
        finished(&mut facts, "p", run("ci.yml", RunStatus::Success), 105);
        assert!(
            pick(&facts.candidates(), 110).is_none(),
            "flash counts from the first"
        );
    }

    #[test]
    fn quiet_failure_counts_in_badge_but_stays_off_screen() {
        let mut facts = CiFacts::default();
        let mut work = run("deploy", RunStatus::Failed);
        work.interrupt = false;
        finished(&mut facts, "w", work, 100);
        assert!(facts.candidates().is_empty());
        assert_eq!(facts.failed_count(), 1);
        assert_eq!(facts.pipeline_rows().rows[0].status, RowStatus::Failed);
    }

    #[test]
    fn expire_keeps_failures() {
        let mut facts = CiFacts::default();
        finished(&mut facts, "red", run("a", RunStatus::Failed), 0);
        finished(&mut facts, "green", run("b", RunStatus::Success), 0);
        running(&mut facts, "old", 0);
        facts.expire(JOB_MAX_AGE_S);
        // Success flash and the lost job are gone; the failure stays as a badge.
        assert_eq!(facts.candidates().len(), 1);
        assert!(!facts.has_job("old"));
        assert_eq!(facts.failed_count(), 1);
    }

    #[test]
    fn pipeline_rows_orders_by_attention_then_recency() {
        let mut facts = CiFacts::default();
        finished(&mut facts, "a", run("old-green", RunStatus::Success), 10);
        finished(&mut facts, "b", run("new-green", RunStatus::Success), 20);
        finished(&mut facts, "c", run("red", RunStatus::Failed), 5);
        finished(&mut facts, "d", run("busy", RunStatus::Running), 1);
        let page = facts.pipeline_rows();
        let names: Vec<&str> = page.rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(names, ["busy", "red", "new-green", "old-green"]);
        assert_eq!(page.rows[0].status, RowStatus::Running);
        assert_eq!(page.rows[0].sub, "demo main");
        assert_eq!(page.count, 4);
    }

    #[test]
    fn pipelines_keep_the_most_recent_and_lost_runs_go_neutral() {
        let mut facts = CiFacts::default();
        for i in 0..(MAX_PIPELINES as u64 + 3) {
            finished(&mut facts, &format!("p{i}"), run("x", RunStatus::Queued), i);
        }
        assert_eq!(facts.pipeline_rows().count, MAX_PIPELINES as u32);
        facts.expire(JOB_MAX_AGE_S + 100);
        assert!(
            facts
                .pipeline_rows()
                .rows
                .iter()
                .all(|r| r.status == RowStatus::Neutral)
        );
    }

    #[test]
    fn pull_events_update_count_page_and_notice() {
        let mut facts = CiFacts::default();
        let repo = RepoPulls {
            project: "demo".into(),
            source: "gitea".into(),
            pulls: vec![pull(1, 10)],
            total: 3,
        };
        facts.apply(
            Update::Pulls {
                key: "r".into(),
                repo,
            },
            0,
        );
        assert_eq!(facts.open_pull_count(), 3);
        opened(&mut facts, pull(5, 50), true);
        // The same PR again (redelivered webhook) does not count or flash twice.
        opened(&mut facts, pull(5, 50), true);
        assert_eq!(facts.open_pull_count(), 4);
        assert_eq!(facts.candidates().len(), 1);
        let ScreenData::Notice(notice) = &facts.candidates()[0].data else {
            panic!("expected a notice");
        };
        assert_eq!(notice.sub, "demo #5: pr 5");

        facts.apply(
            Update::PullClosed {
                key: "r".into(),
                number: 1,
            },
            0,
        );
        assert_eq!(facts.open_pull_count(), 3);

        let page = facts.pull_rows();
        assert_eq!(page.count, 3);
        assert_eq!(page.rows[0].text, "pr 5");
        assert_eq!(page.rows[0].sub, "demo #5");
    }

    #[test]
    fn closing_unknown_pull_in_fully_listed_repo_changes_nothing() {
        let mut facts = CiFacts::default();
        opened(&mut facts, pull(1, 10), false);
        assert!(facts.candidates().is_empty(), "no flash without notify");
        facts.apply(
            Update::PullClosed {
                key: "r".into(),
                number: 99,
            },
            0,
        );
        assert_eq!(facts.open_pull_count(), 1);
    }

    fn runner(name: &str, status: RunnerStatus, disabled: bool) -> Runner {
        Runner {
            source: "gitea".into(),
            name: name.into(),
            status,
            disabled,
            labels: vec!["ubuntu-latest".into()],
            url: None,
        }
    }

    fn runners(facts: &mut CiFacts, key: &str, list: Vec<Runner>, now: u64) {
        facts.apply(
            Update::Runners {
                key: key.into(),
                runners: list,
            },
            now,
        );
    }

    #[test]
    fn runners_sort_by_attention_and_disabled_go_last() {
        let mut facts = CiFacts::default();
        runners(
            &mut facts,
            "gitea:home:runners:user",
            vec![
                runner("c-idle", RunnerStatus::Idle, false),
                runner("b-busy", RunnerStatus::Busy, false),
                runner("z-parked", RunnerStatus::Offline, true),
                runner("a-down", RunnerStatus::Offline, false),
            ],
            10,
        );
        let names: Vec<_> = facts
            .runners()
            .iter()
            .map(|(r, _)| r.name.clone())
            .collect();
        assert_eq!(names, ["a-down", "b-busy", "c-idle", "z-parked"]);
        // Only the runner that should be up counts as offline.
        assert_eq!(facts.offline_runner_count(), 1);
    }

    #[test]
    fn queued_jobs_replace_clear_and_expire() {
        let mut facts = CiFacts::default();
        let queue = |pool: &str, queued| Update::QueuedJobs {
            key: format!("azure_devops:work:queue:pool:{pool}"),
            source: "azure_devops".into(),
            pool: pool.into(),
            queued,
        };
        facts.apply(queue("a", 1), 1000);
        facts.apply(queue("b", 4), 1000);
        facts.apply(queue("a", 2), 1010);
        let pools: Vec<_> = facts
            .queued_jobs()
            .iter()
            .map(|(q, _)| (q.pool.as_str(), q.queued))
            .collect();
        assert_eq!(pools, [("b", 4), ("a", 2)], "busiest first, a replaced");
        facts.apply(queue("b", 0), 1020);
        assert_eq!(facts.queued_jobs().len(), 1, "zero clears");
        facts.expire(1010 + RUNNER_MAX_AGE_S);
        assert!(facts.queued_jobs().is_empty(), "old numbers are dropped");
    }

    #[test]
    fn runner_update_replaces_its_scope_and_empty_clears_it() {
        let mut facts = CiFacts::default();
        runners(
            &mut facts,
            "gitea:home:runners:user",
            vec![
                runner("a", RunnerStatus::Idle, false),
                runner("b", RunnerStatus::Idle, false),
            ],
            10,
        );
        runners(
            &mut facts,
            "gitea:home:runners:org:team",
            vec![runner("c", RunnerStatus::Busy, false)],
            10,
        );
        assert_eq!(facts.runners().len(), 3);
        // The next poll of the user scope no longer has `b`.
        runners(
            &mut facts,
            "gitea:home:runners:user",
            vec![runner("a", RunnerStatus::Busy, false)],
            40,
        );
        assert_eq!(facts.runners().len(), 2);
        runners(&mut facts, "gitea:home:runners:org:team", vec![], 40);
        assert_eq!(facts.runners().len(), 1);
    }

    #[test]
    fn runner_lists_expire_when_nobody_refreshes_them() {
        let mut facts = CiFacts::default();
        runners(
            &mut facts,
            "gitea:home:runners:user",
            vec![runner("a", RunnerStatus::Idle, false)],
            100,
        );
        facts.expire(100 + RUNNER_MAX_AGE_S - 1);
        assert_eq!(facts.runners().len(), 1);
        facts.expire(100 + RUNNER_MAX_AGE_S);
        assert!(facts.runners().is_empty());
    }
}
