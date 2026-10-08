//! CI facts shared by every CI source: running jobs, runs per pipeline, open
//! PRs and short notices.
//!
//! Sources (Gitea today, GitHub and Azure DevOps later) send `Update`s; the
//! main loop applies them here. Sources never decide what is on screen. The
//! composer turns these facts into screen candidates, header badges and the
//! `prs` and `pipelines` rotation pages.
//!
//! Every key starts with the source id (`gitea:home:...`), so two instances of
//! the same adapter never overwrite each other.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::model::{
    AlertData, AlertStatus, Badge, BadgeIcon, Candidate, JobData, Level, ListData, ListRow,
    MAX_TITLE_CHARS, NoticeData, RowStatus, ScreenData, truncate,
};

/// A running job with no update for this long is assumed lost (missed
/// webhook, bridge restart on the CI side) and dropped. Gitea's default job
/// timeout is 3 hours, so this leaves plenty of room. Runs still marked
/// running after this long are shown as neutral on the pipelines page.
pub const JOB_MAX_AGE_S: u64 = 6 * 60 * 60;

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
}

/// Status of a run, the same for every CI source (integrations plan 8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunStatus {
    // Declared in pipelines page order: what needs attention first.
    Running,
    Failed,
    Queued,
    Success,
    /// Cancelled, skipped or lost. Never raises an alert.
    Neutral,
}

impl RunStatus {
    fn row_status(self) -> RowStatus {
        match self {
            RunStatus::Running => RowStatus::Running,
            RunStatus::Failed => RowStatus::Failed,
            RunStatus::Success => RowStatus::Ok,
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
}

/// Open pull requests of one repository.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RepoPulls {
    /// Short name shown on the panel, such as `deskwatch`.
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
    /// Job picked with a long press, shown instead of the newest one.
    focus: Option<String>,
    /// Latest run per pipeline, with the Unix seconds it was last updated.
    runs: BTreeMap<String, (Run, u64)>,
    /// Latest outcome per pipeline, keyed like `runs`, so a new run of the
    /// same pipeline replaces the old alert (a green run clears a red one).
    alerts: BTreeMap<String, RunAlert>,
    /// Open PRs by repository key, such as `gitea:home:owner/repo`.
    pulls: BTreeMap<String, RepoPulls>,
    /// Short flashes waiting to be shown, with the time they were raised.
    notices: Vec<(NoticeData, u64)>,
}

impl CiFacts {
    // --- Writes from sources ---------------------------------------------

    /// Apply one update from a source. `now` is Unix seconds.
    pub fn apply(&mut self, update: Update, now: u64) {
        match update {
            Update::JobRunning { key, job } => {
                self.jobs.insert(key, job);
            }
            Update::JobDone { key } => {
                self.jobs.remove(&key);
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

    // --- Button actions --------------------------------------------------

    /// Clear every alert with this status (button press on an alert). A
    /// dismissed failure also leaves the header badge.
    pub fn dismiss_alerts(&mut self, status: AlertStatus) {
        self.alerts.retain(|_, a| a.data.status != status);
    }

    /// Clear pending notices (button press on a notice).
    pub fn dismiss_notices(&mut self) {
        self.notices.clear();
    }

    /// Long press on a job screen: show the next running job, newest first,
    /// wrapping around. Does nothing with fewer than two jobs.
    pub fn cycle_jobs(&mut self) {
        let order = self.screen_jobs();
        if order.len() < 2 {
            return;
        }
        let current = self
            .focus
            .as_ref()
            .and_then(|f| order.iter().position(|(key, _)| *key == f))
            .unwrap_or(0);
        let next = order[(current + 1) % order.len()].0.clone();
        self.focus = Some(next);
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
        if self
            .focus
            .as_ref()
            .is_some_and(|f| !self.jobs.contains_key(f))
        {
            self.focus = None;
        }
        for (run, updated) in self.runs.values_mut() {
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

    /// Everything that wants to pre-empt the idle rotation. `model::pick`
    /// chooses among them.
    pub fn candidates(&self) -> Vec<Candidate> {
        let mut out = Vec::new();

        // Running jobs: level 1. `others` counts the rest of the interrupting
        // jobs. The newest wins, as the schema asks, unless one was picked
        // with a long press.
        let running = self.screen_jobs();
        let others = running.len().saturating_sub(1) as u32;
        for (key, job) in running {
            let mut data = job.data.clone();
            data.others = others;
            let focused = self.focus.as_ref() == Some(key);
            out.push(Candidate {
                level: Level::Job,
                page: format!("{}-job", data.source),
                raised_at: if focused { u64::MAX } else { data.started },
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

    /// The `prs` rotation page: newest PRs first over all repositories.
    pub fn pulls_page(&self) -> ListData {
        let mut rows: Vec<(u64, ListRow)> = self
            .pulls
            .values()
            .flat_map(|repo| {
                repo.pulls.iter().map(|pull| {
                    let row = ListRow {
                        text: truncate(&pull.title, MAX_TITLE_CHARS),
                        sub: truncate(
                            &format!("{} #{}", repo.project, pull.number),
                            MAX_TITLE_CHARS,
                        ),
                        status: RowStatus::Open,
                        source: repo.source.clone(),
                    };
                    (pull.created, row)
                })
            })
            .collect();
        rows.sort_by_key(|row| Reverse(row.0));
        let rows = rows.into_iter().map(|(_, row)| row).collect();
        ListData::new("Open PRs", self.open_pull_count(), rows)
    }

    /// The `pipelines` rotation page: latest run of every pipeline, running
    /// and failed ones first, then the most recently updated.
    pub fn pipelines_page(&self) -> ListData {
        let mut runs: Vec<&(Run, u64)> = self.runs.values().collect();
        runs.sort_by_key(|(run, updated)| (run.status, Reverse(*updated)));
        let rows = runs
            .into_iter()
            .map(|(run, _)| ListRow {
                text: truncate(&run.pipeline, MAX_TITLE_CHARS),
                sub: truncate(&format!("{} {}", run.project, run.git_ref), MAX_TITLE_CHARS),
                status: run.status.row_status(),
                source: run.source.clone(),
            })
            .collect();
        ListData::new("Pipelines", self.runs.len() as u32, rows)
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
        }
    }

    fn pull(number: u64, created: u64) -> OpenPull {
        OpenPull {
            number,
            title: format!("pr {number}"),
            created,
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
    fn long_press_cycles_through_jobs_and_wraps() {
        let mut facts = CiFacts::default();
        running(&mut facts, "a", 100);
        running(&mut facts, "b", 200);
        running(&mut facts, "c", 300);
        assert_eq!(shown_job(&facts).started, 300);
        facts.cycle_jobs();
        assert_eq!(shown_job(&facts).started, 200);
        facts.cycle_jobs();
        assert_eq!(shown_job(&facts).started, 100);
        facts.cycle_jobs();
        assert_eq!(shown_job(&facts).started, 300);

        // The picked job ends: back to the newest.
        facts.cycle_jobs();
        facts.apply(Update::JobDone { key: "b".into() }, 400);
        facts.expire(400);
        assert_eq!(shown_job(&facts).started, 300);
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
        assert_eq!(facts.pipelines_page().rows[0].status, RowStatus::Failed);
    }

    #[test]
    fn expire_keeps_failures_until_dismissed() {
        let mut facts = CiFacts::default();
        finished(&mut facts, "red", run("a", RunStatus::Failed), 0);
        finished(&mut facts, "green", run("b", RunStatus::Success), 0);
        running(&mut facts, "old", 0);
        facts.expire(JOB_MAX_AGE_S);
        // Success flash and the lost job are gone; the failure stays as a badge.
        assert_eq!(facts.candidates().len(), 1);
        assert!(!facts.has_job("old"));
        assert_eq!(facts.failed_count(), 1);
        facts.dismiss_alerts(AlertStatus::Failed);
        assert_eq!(facts.failed_count(), 0);
    }

    #[test]
    fn pipelines_page_orders_by_attention_then_recency() {
        let mut facts = CiFacts::default();
        finished(&mut facts, "a", run("old-green", RunStatus::Success), 10);
        finished(&mut facts, "b", run("new-green", RunStatus::Success), 20);
        finished(&mut facts, "c", run("red", RunStatus::Failed), 5);
        finished(&mut facts, "d", run("busy", RunStatus::Running), 1);
        let page = facts.pipelines_page();
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
        assert_eq!(facts.pipelines_page().count, MAX_PIPELINES as u32);
        facts.expire(JOB_MAX_AGE_S + 100);
        assert!(
            facts
                .pipelines_page()
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

        let page = facts.pulls_page();
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
}
