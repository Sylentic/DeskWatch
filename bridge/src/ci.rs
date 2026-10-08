//! CI facts shared by every CI source: running jobs, finished runs, open PRs
//! and short notices.
//!
//! Sources (Gitea today, GitHub and Azure DevOps later) write into `CiFacts`;
//! they never decide what is on screen. The composer turns these facts into
//! screen candidates, header badges and the `prs` rotation page.

use std::collections::BTreeMap;

use crate::model::{
    AlertData, AlertStatus, Badge, BadgeIcon, Candidate, JobData, Level, ListData, ListRow,
    MAX_TITLE_CHARS, NoticeData, RowStatus, ScreenData, truncate,
};

/// A running job with no update for this long is assumed lost (missed
/// webhook, bridge restart on the CI side) and dropped. Gitea's default job
/// timeout is 3 hours, so this leaves plenty of room.
pub const JOB_MAX_AGE_S: u64 = 6 * 60 * 60;

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

/// The outcome of a finished run, shown as a red or green alert.
#[derive(Debug, Clone, PartialEq)]
pub struct RunAlert {
    pub data: AlertData,
    /// Unix seconds when the bridge learned about it. Alert timers count from here.
    pub raised_at: u64,
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
    /// Running jobs by a source-chosen key, such as `gitea:owner/repo:42`.
    jobs: BTreeMap<String, RunningJob>,
    /// Latest outcome per pipeline, keyed like `gitea:owner/repo:ci.yml`, so a
    /// new run of the same pipeline replaces the old alert (a green run
    /// clears an earlier red one).
    alerts: BTreeMap<String, RunAlert>,
    /// Open PRs by repository key, such as `gitea:owner/repo`.
    pulls: BTreeMap<String, RepoPulls>,
    /// Short flashes waiting to be shown, with the time they were raised.
    notices: Vec<(NoticeData, u64)>,
    /// True when the last PR poll failed, so the PR page is drawn greyed out.
    pub pulls_stale: bool,
}

impl CiFacts {
    // --- Writes from sources ---------------------------------------------

    /// Add or refresh a running job.
    pub fn job_running(&mut self, key: &str, job: RunningJob) {
        self.jobs.insert(key.to_string(), job);
    }

    /// The job is no longer running (finished, cancelled or lost).
    pub fn job_done(&mut self, key: &str) {
        self.jobs.remove(key);
    }

    pub fn has_job(&self, key: &str) -> bool {
        self.jobs.contains_key(key)
    }

    /// A run finished: show its alert. Replaces the previous alert of the same pipeline.
    pub fn run_finished(&mut self, key: &str, data: AlertData, now: u64) {
        let alert = RunAlert {
            data,
            raised_at: now,
        };
        self.alerts.insert(key.to_string(), alert);
    }

    /// Replace the open PRs of one repository (after a poll).
    pub fn set_pulls(&mut self, key: &str, mut repo: RepoPulls) {
        repo.total = repo.total.max(repo.pulls.len() as u32);
        self.pulls.insert(key.to_string(), repo);
    }

    /// A PR was opened or reopened. Returns true if it was not known yet.
    pub fn pull_opened(&mut self, key: &str, project: &str, source: &str, pull: OpenPull) -> bool {
        let repo = self
            .pulls
            .entry(key.to_string())
            .or_insert_with(|| RepoPulls {
                project: project.to_string(),
                source: source.to_string(),
                ..Default::default()
            });
        if repo.pulls.iter().any(|p| p.number == pull.number) {
            return false;
        }
        repo.pulls.push(pull);
        repo.total += 1;
        true
    }

    /// A PR was closed or merged.
    pub fn pull_closed(&mut self, key: &str, number: u64) {
        if let Some(repo) = self.pulls.get_mut(key) {
            let before = repo.pulls.len();
            repo.pulls.retain(|p| p.number != number);
            // Also count down when the PR was beyond the first polled page.
            if repo.pulls.len() < before || repo.total as usize > repo.pulls.len() {
                repo.total = repo.total.saturating_sub(1);
            }
        }
    }

    /// Queue a short flash, such as "New PR".
    pub fn notice(&mut self, data: NoticeData, now: u64) {
        self.notices.push((data, now));
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

    // --- Housekeeping ----------------------------------------------------

    /// Drop facts that can never be shown again: finished success flashes and
    /// notices, and jobs nobody has mentioned for `JOB_MAX_AGE_S`. Failed
    /// alerts stay until dismissed, because after their screen time they
    /// still count in the header badge.
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
    }

    // --- Reads for the composer ------------------------------------------

    /// Everything that wants to pre-empt the idle rotation. `model::pick`
    /// chooses among them.
    pub fn candidates(&self) -> Vec<Candidate> {
        let mut out = Vec::new();

        // Running jobs: level 1. `others` counts the rest of the interrupting jobs.
        let running: Vec<&RunningJob> = self.jobs.values().filter(|j| j.interrupt).collect();
        let others = running.len().saturating_sub(1) as u32;
        for job in running {
            let mut data = job.data.clone();
            data.others = others;
            out.push(Candidate {
                level: Level::Job,
                page: format!("{}-job", data.source),
                // Newest job wins, as the schema asks, so rank by start time.
                raised_at: data.started,
                data: ScreenData::Job(data),
            });
        }

        // Finished runs: level 2 (failed) or 3 (success).
        for alert in self.alerts.values() {
            let status = alert.data.status;
            let level = match status {
                AlertStatus::Failed => Level::AlertFailed,
                AlertStatus::Success => Level::AlertSuccess,
            };
            let mut data = alert.data.clone();
            data.others = self
                .alerts
                .values()
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

    /// Header badges from CI facts: open PRs and unacknowledged failures.
    pub fn badges(&self) -> Vec<Badge> {
        let failed = self
            .alerts
            .values()
            .filter(|a| a.data.status == AlertStatus::Failed)
            .count() as u32;
        vec![
            Badge {
                id: "prs".into(),
                icon: BadgeIcon::Pr,
                count: self.open_pull_count(),
                status: RowStatus::Open,
            },
            Badge {
                id: "ci-failed".into(),
                icon: BadgeIcon::Pipeline,
                count: failed,
                status: RowStatus::Failed,
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
        rows.sort_by_key(|row| std::cmp::Reverse(row.0));
        let rows = rows.into_iter().map(|(_, row)| row).collect();
        ListData::new("Open PRs", self.open_pull_count(), rows)
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

    fn alert(status: AlertStatus) -> AlertData {
        AlertData {
            status,
            source: "gitea".into(),
            project: "demo".into(),
            pipeline: "ci.yml".into(),
            step: None,
            started: 100,
            finished: 200,
            others: 0,
        }
    }

    fn pull(number: u64, created: u64) -> OpenPull {
        OpenPull {
            number,
            title: format!("pr {number}"),
            created,
        }
    }

    #[test]
    fn newest_job_wins_and_counts_others() {
        let mut facts = CiFacts::default();
        facts.job_running("a", job(100));
        facts.job_running("b", job(200));
        let candidates = facts.candidates();
        let ScreenData::Job(shown) = &pick(&candidates, 300).unwrap().data else {
            panic!("expected a job screen");
        };
        assert_eq!(shown.started, 200);
        assert_eq!(shown.others, 1);
    }

    #[test]
    fn non_interrupting_jobs_stay_off_screen() {
        let mut facts = CiFacts::default();
        let mut quiet = job(100);
        quiet.interrupt = false;
        facts.job_running("work", quiet);
        assert!(facts.candidates().is_empty());
    }

    #[test]
    fn green_run_replaces_red_alert_of_same_pipeline() {
        let mut facts = CiFacts::default();
        facts.run_finished("p", alert(AlertStatus::Failed), 100);
        assert_eq!(facts.badges()[1].count, 1);
        facts.run_finished("p", alert(AlertStatus::Success), 200);
        assert_eq!(facts.badges()[1].count, 0);
        assert_eq!(facts.candidates()[0].level, Level::AlertSuccess);
    }

    #[test]
    fn expire_keeps_failures_until_dismissed() {
        let mut facts = CiFacts::default();
        facts.run_finished("red", alert(AlertStatus::Failed), 0);
        facts.run_finished("green", alert(AlertStatus::Success), 0);
        facts.job_running("old", job(0));
        facts.expire(JOB_MAX_AGE_S);
        // Success flash and the lost job are gone; the failure stays as a badge.
        assert_eq!(facts.candidates().len(), 1);
        assert!(!facts.has_job("old"));
        assert_eq!(facts.badges()[1].count, 1);
        facts.dismiss_alerts(AlertStatus::Failed);
        assert_eq!(facts.badges()[1].count, 0);
    }

    #[test]
    fn pull_events_update_count_and_page() {
        let mut facts = CiFacts::default();
        facts.set_pulls(
            "r",
            RepoPulls {
                project: "demo".into(),
                source: "gitea".into(),
                pulls: vec![pull(1, 10)],
                total: 3,
            },
        );
        assert_eq!(facts.open_pull_count(), 3);
        assert!(facts.pull_opened("r", "demo", "gitea", pull(5, 50)));
        // The same PR again (redelivered webhook) does not count twice.
        assert!(!facts.pull_opened("r", "demo", "gitea", pull(5, 50)));
        assert_eq!(facts.open_pull_count(), 4);
        facts.pull_closed("r", 1);
        assert_eq!(facts.open_pull_count(), 3);

        let page = facts.pulls_page();
        assert_eq!(page.count, 3);
        assert_eq!(page.rows[0].text, "pr 5");
        assert_eq!(page.rows[0].sub, "demo #5");
    }

    #[test]
    fn closing_unknown_pull_in_fully_listed_repo_changes_nothing() {
        let mut facts = CiFacts::default();
        facts.pull_opened("r", "demo", "gitea", pull(1, 10));
        facts.pull_closed("r", 99);
        assert_eq!(facts.open_pull_count(), 1);
    }
}
