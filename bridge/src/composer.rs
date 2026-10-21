//! The composer: decides what pre-empts the idle kiosk page right now.
//!
//! It owns the latest server stats, the CI facts, the active alerts and the
//! health of every source. Every tick the kiosk asks it for the current
//! interrupt screen (a running job, a failed or successful run, an alert, a
//! notice) and the badges. Messages from sources and alerts come in here too.

use crate::alerts::{AlertMessage, Alerts};
use crate::ci::{CiFacts, Update};
use crate::fleet::Fleet;
use crate::model::{Badge, BadgeIcon, Badges, RowStatus, Screen, StatsData, pick};
use crate::source::{Health, HealthBoard, SourceBody, SourceMsg};

#[derive(Default)]
pub struct Composer {
    stats: Option<StatsData>,
    pub ci: CiFacts,
    pub fleet: Fleet,
    pub alerts: Alerts,
    pub health: HealthBoard,
}

impl Composer {
    /// Apply a message from a source.
    pub fn apply(&mut self, msg: SourceMsg, now: u64) {
        match msg.body {
            SourceBody::Facts(updates) => {
                for update in updates {
                    match update {
                        Update::Alert { alert } => self.alerts.apply(alert, now),
                        update => self.ci.apply(update, now),
                    }
                }
            }
            SourceBody::Stats(report) => self.fleet.apply(&msg.source, report),
            SourceBody::Health(health) => {
                self.health.set(&msg.source, health);
                self.fleet.set_source_ok(&msg.source, health == Health::Ok);
            }
            SourceBody::Forget => {
                self.ci.forget(&format!("{}:", msg.source));
                self.fleet.forget(&msg.source);
                self.health.forget(&msg.source);
            }
        }
    }

    /// Apply an alert from `deskpanel/alert` or `POST /api/alert`.
    pub fn alert(&mut self, msg: AlertMessage, now: u64) {
        self.alerts.apply(msg, now);
    }

    /// Stats of the machine the bridge runs on, once sampled.
    pub fn stats(&self) -> Option<&StatsData> {
        self.stats.as_ref()
    }

    pub fn set_stats(&mut self, stats: StatsData) {
        self.stats = Some(stats);
    }

    /// The interrupt to show at `now`: critical alert, running job, then
    /// failed run or warning, success, notice. `None` means the idle page.
    pub fn screen(&mut self, now: u64) -> Option<Screen> {
        self.ci.expire(now);
        self.alerts.expire(now);

        let mut candidates = self.ci.candidates();
        candidates.extend(self.alerts.candidates(now));
        pick(&candidates, now).map(|top| Screen::new(top.level, top.page.clone(), top.data.clone()))
    }

    /// Header badges for the current facts, in the plan's order: active
    /// alerts, failed runs, open PRs, hosts and containers that are down,
    /// then sources with a problem.
    pub fn badges(&self) -> Badges {
        let warn = Badge {
            id: "warn".into(),
            icon: BadgeIcon::Warn,
            count: self.health.problems(),
            status: RowStatus::Failed,
        };
        let server = Badge {
            id: "server".into(),
            icon: BadgeIcon::Server,
            count: self.fleet.down_count(),
            status: RowStatus::Failed,
        };
        Badges::new(
            [self.alerts.badge()]
                .into_iter()
                .chain(self.ci.badges())
                .chain([server, warn]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{OpenPull, Run, RunStatus, RunningJob, Update};
    use crate::model::{AlertStatus, JobData, JobKind, Level, ScreenData, Template};
    use crate::source::{Health, SourceId};

    fn send(composer: &mut Composer, update: Update, now: u64) {
        let msg = SourceMsg {
            source: SourceId::new("gitea", "home"),
            body: SourceBody::Facts(vec![update]),
        };
        composer.apply(msg, now);
    }

    fn running_job(key: &str, started: u64) -> Update {
        Update::JobRunning {
            key: key.into(),
            job: RunningJob {
                data: JobData {
                    source: "gitea".into(),
                    project: "demo".into(),
                    pipeline: format!("{key}.yml"),
                    kind: JobKind::Deploy,
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
            },
        }
    }

    fn run(status: RunStatus) -> Update {
        Update::Run {
            key: "p".into(),
            run: Run {
                source: "gitea".into(),
                project: "demo".into(),
                pipeline: "ci.yml".into(),
                git_ref: "main".into(),
                status,
                step: Some("cargo test".into()),
                started: 0,
                finished: Some(10),
                interrupt: true,
                url: None,
            },
        }
    }

    fn pr_opened() -> Update {
        pr_opened_in("r")
    }

    fn pr_opened_in(key: &str) -> Update {
        Update::PullOpened {
            key: key.into(),
            project: "demo".into(),
            source: "gitea".into(),
            pull: OpenPull {
                number: 7,
                title: "fix login".into(),
                created: 1,
                url: None,
            },
            notify: false,
        }
    }

    #[test]
    fn quiet_day_has_no_interrupt() {
        let mut composer = Composer::default();
        assert!(composer.screen(0).is_none());
        send(&mut composer, pr_opened(), 0);
        assert!(composer.screen(20).is_none(), "an open PR is a badge only");
        assert_eq!(composer.badges().items[0].id, "prs");
    }

    #[test]
    fn job_interrupts_until_it_is_done() {
        let mut composer = Composer::default();
        send(&mut composer, running_job("j", 21), 21);
        let screen = composer.screen(21).unwrap();
        assert_eq!(screen.template, Template::Job);

        send(&mut composer, Update::JobDone { key: "j".into() }, 22);
        assert!(composer.screen(22).is_none());
    }

    #[test]
    fn the_newest_job_is_shown_and_counts_the_others() {
        let mut composer = Composer::default();
        send(&mut composer, running_job("old", 10), 10);
        send(&mut composer, running_job("new", 20), 20);
        let ScreenData::Job(job) = composer.screen(21).unwrap().data else {
            panic!("expected a job");
        };
        assert_eq!(job.pipeline, "new.yml");
        assert_eq!(job.others, 1);
    }

    #[test]
    fn failed_alert_drops_to_badge_after_ten_minutes() {
        let mut composer = Composer::default();
        send(&mut composer, run(RunStatus::Failed), 0);
        assert_eq!(composer.screen(599).unwrap().template, Template::Alert);
        assert!(composer.screen(600).is_none());
        assert_eq!(composer.badges().items.len(), 1);
        assert_eq!(composer.badges().items[0].id, "failed");
    }

    #[test]
    fn source_problem_shows_warn_badge() {
        let mut composer = Composer::default();
        send(&mut composer, pr_opened(), 0);
        let health = |health| SourceMsg {
            source: SourceId::new("gitea", "home"),
            body: SourceBody::Health(health),
        };
        composer.apply(health(Health::AuthFailed), 0);
        let ids: Vec<String> = composer.badges().items.into_iter().map(|b| b.id).collect();
        assert_eq!(ids, ["prs", "warn"]);

        composer.apply(health(Health::Ok), 21);
        assert_eq!(composer.badges().items.len(), 1);
    }

    #[test]
    fn a_forgotten_source_leaves_the_facts_and_the_badges() {
        let mut composer = Composer::default();
        send(&mut composer, pr_opened_in("gitea:home:o/r"), 0);
        send(&mut composer, running_job("gitea:home:o/r:1", 0), 0);
        let home = |body| SourceMsg {
            source: SourceId::new("gitea", "home"),
            body,
        };
        composer.apply(home(SourceBody::Health(Health::AuthFailed)), 0);
        assert_eq!(composer.ci.open_pull_count(), 1);
        assert!(!composer.health.all_ok());

        // Another source's facts stay.
        let other = SourceMsg {
            source: SourceId::new("gitea", "homelab"),
            body: SourceBody::Facts(vec![running_job("gitea:homelab:o/r:2", 0)]),
        };
        composer.apply(other, 0);

        composer.apply(home(SourceBody::Forget), 1);
        assert_eq!(composer.ci.open_pull_count(), 0);
        assert!(composer.health.all_ok());
        assert_eq!(composer.ci.jobs().len(), 1);
    }

    fn alert(json: &str) -> AlertMessage {
        AlertMessage::parse(json.as_bytes()).unwrap()
    }

    #[test]
    fn critical_alert_beats_a_running_job_until_it_is_cleared() {
        let mut composer = Composer::default();
        send(&mut composer, running_job("j", 0), 0);
        composer.alert(
            alert(r#"{"v":2,"id":"leak","severity":"critical","title":"Water leak"}"#),
            1,
        );
        let screen = composer.screen(1).unwrap();
        assert_eq!(screen.level, Level::Critical);
        let value = serde_json::to_value(&screen).unwrap();
        assert_eq!(value["level"], 0);
        assert_eq!(value["data"]["status"], "failed");
        assert_eq!(value["data"]["title"], "Water leak");
        assert_eq!(value["data"]["project"], serde_json::Value::Null);
        assert_eq!(composer.badges().items[0].id, "alerts");

        composer.alert(alert(r#"{"v":2,"id":"leak","clear":true}"#), 2);
        assert_eq!(composer.screen(2).unwrap().template, Template::Job);
        assert!(composer.badges().items.is_empty());
    }

    #[test]
    fn warning_alert_is_amber_next_to_a_failed_run() {
        let mut composer = Composer::default();
        send(&mut composer, run(RunStatus::Failed), 0);
        composer.alert(
            alert(r#"{"v":2,"id":"door","severity":"warning","title":"Freezer door"}"#),
            1,
        );
        let screen = composer.screen(1).unwrap();
        assert_eq!(screen.level, Level::AlertFailed);
        let ScreenData::Alert(data) = &screen.data else {
            panic!("expected an alert");
        };
        assert_eq!(data.status, AlertStatus::Warn);
        let ids: Vec<String> = composer.badges().items.into_iter().map(|b| b.id).collect();
        assert_eq!(ids, ["alerts", "failed"]);
    }

    #[test]
    fn info_alert_flashes_for_five_seconds() {
        let mut composer = Composer::default();
        composer.alert(alert(r#"{"v":2,"id":"washer","title":"Washer done"}"#), 1);
        assert_eq!(composer.screen(1).unwrap().template, Template::Notice);
        assert!(composer.screen(6).is_none());
        assert_eq!(composer.alerts.views().len(), 1, "stays in the list");
    }
}
