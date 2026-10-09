//! The composer: decides which page is on the panel right now.
//!
//! It owns the idle rotation, the latest server stats, the CI facts, the
//! active alerts and the health of every source. Every tick the main loop asks it for the current
//! screen and badges and publishes them when they change. Messages from
//! sources and button presses from the panel come in here too.

use crate::alerts::{AlertMessage, Alerts};
use crate::ci::CiFacts;
use crate::fleet::Fleet;
use crate::model::{
    AlertStatus, Badge, BadgeIcon, Badges, ButtonAction, Level, ListData, Rotation, RotationEntry,
    RowStatus, Screen, ScreenData, StatsData, pick,
};
use crate::source::{Health, HealthBoard, SourceBody, SourceMsg};

pub struct Composer {
    rotation: Rotation,
    /// Unix seconds when the current rotation page came up.
    rotated_at: u64,
    /// Level of the screen returned by the last `screen` call.
    shown: Level,
    stats: Option<StatsData>,
    pub ci: CiFacts,
    pub fleet: Fleet,
    pub alerts: Alerts,
    pub health: HealthBoard,
}

impl Composer {
    pub fn new(rotation: Vec<RotationEntry>, now: u64) -> Self {
        Self {
            rotation: Rotation::new(rotation),
            rotated_at: now,
            shown: Level::Rotation,
            stats: None,
            ci: CiFacts::default(),
            fleet: Fleet::default(),
            alerts: Alerts::default(),
            health: HealthBoard::default(),
        }
    }

    /// Apply a message from a source.
    pub fn apply(&mut self, msg: SourceMsg, now: u64) {
        match msg.body {
            SourceBody::Facts(updates) => {
                for update in updates {
                    self.ci.apply(update, now);
                }
            }
            SourceBody::Stats(report) => self.fleet.apply(&msg.source, report),
            SourceBody::Health(health) => {
                self.health.set(&msg.source, health);
                self.fleet.set_source_ok(&msg.source, health == Health::Ok);
            }
        }
    }

    /// Apply an alert from `deskpanel/alert`.
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

    /// The screen to show at `now`. Also moves the rotation along when the
    /// current page has had its dwell time.
    pub fn screen(&mut self, now: u64) -> Screen {
        self.ci.expire(now);
        self.alerts.expire(now);

        // Interrupts first: critical alert, running job, then alerts, then notices.
        let mut candidates = self.ci.candidates();
        candidates.extend(self.alerts.candidates(now));
        if let Some(top) = pick(&candidates, now) {
            self.shown = top.level;
            return Screen::new(top.level, top.page.clone(), top.data.clone());
        }

        // Back from an interrupt: rotation resumes at the stats page.
        if self.shown != Level::Rotation {
            self.shown = Level::Rotation;
            self.rotation.reset();
            self.rotated_at = now;
        }
        if now.saturating_sub(self.rotated_at) >= self.rotation.current().dwell_s {
            self.next_page(now);
        }

        let page = self.rotation.current().page.clone();
        let (data, stale) = self.page_data(&page);
        let mut screen = Screen::new(Level::Rotation, page, data);
        screen.stale = stale;
        screen.pinned = self.rotation.is_pinned();
        screen.position = self.rotation.position(|p| self.page_empty(p));
        screen
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

    /// React to a button press on whatever was shown last.
    ///
    /// - Short press on an alert or notice dismisses it (all failed alerts at
    ///   once, so one press acknowledges a bad afternoon). Alerts from
    ///   `deskpanel/alert` leave the screen but keep their badge until cleared.
    /// - Short press in rotation shows the next page now.
    /// - Long press in rotation pins or unpins the current page.
    /// - Long press on a running job shows the next running job.
    pub fn on_button(&mut self, action: ButtonAction, now: u64) {
        match (self.shown, action) {
            (Level::Critical, ButtonAction::Short) => self.alerts.acknowledge(Level::Critical),
            (Level::AlertFailed, ButtonAction::Short) => {
                self.ci.dismiss_alerts(AlertStatus::Failed);
                self.alerts.acknowledge(Level::AlertFailed);
            }
            (Level::AlertSuccess, ButtonAction::Short) => {
                self.ci.dismiss_alerts(AlertStatus::Success)
            }
            (Level::Notice, ButtonAction::Short) => {
                self.ci.dismiss_notices();
                self.alerts.acknowledge(Level::Notice);
            }
            (Level::Rotation, ButtonAction::Short) => self.next_page(now),
            (Level::Rotation, ButtonAction::Long) => self.rotation.toggle_pin(),
            (Level::Job, ButtonAction::Long) => self.ci.cycle_jobs(),
            _ => {}
        }
    }

    fn next_page(&mut self, now: u64) {
        // Collect emptiness first: `advance` borrows the rotation mutably.
        let empty: Vec<String> = self
            .rotation
            .pages()
            .filter(|p| self.page_empty(p))
            .map(String::from)
            .collect();
        self.rotation
            .advance(|page| empty.iter().any(|e| e == page) || !known_page(page));
        self.rotated_at = now;
    }

    /// Does a rotation page have nothing to show? Host pages are empty until
    /// their source has reported that host.
    fn page_empty(&self, page: &str) -> bool {
        match page.strip_prefix("stats:") {
            Some(host) => self.fleet.host(host).is_none(),
            None => self.page_data(page).0.is_empty(),
        }
    }

    /// Data for a rotation page, and whether it is stale. CI pages are stale
    /// while any source has a problem, since their data may be out of date.
    fn page_data(&self, page: &str) -> (ScreenData, bool) {
        let ci_stale = !self.health.all_ok();
        match page {
            "stats" => match &self.stats {
                Some(stats) => (ScreenData::Stats(stats.clone()), false),
                None => (ScreenData::Stats(StatsData::default()), true),
            },
            "prs" => (ScreenData::List(self.ci.pulls_page()), ci_stale),
            "pipelines" => (ScreenData::List(self.ci.pipelines_page()), ci_stale),
            "alerts" => (ScreenData::List(self.alerts.page()), false),
            "containers" => (ScreenData::List(self.fleet.down_page()), false),
            host if host.starts_with("stats:") => match self.fleet.host(&host["stats:".len()..]) {
                Some((stats, stale)) => (ScreenData::Stats(stats.clone()), stale),
                None => (ScreenData::Stats(StatsData::default()), true),
            },
            other => (ScreenData::List(ListData::new(other, 0, vec![])), false),
        }
    }
}

/// Pages the composer can fill. Unknown names in the config show as empty lists.
fn known_page(page: &str) -> bool {
    matches!(
        page,
        "stats" | "prs" | "pipelines" | "alerts" | "containers"
    ) || page.starts_with("stats:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{OpenPull, Run, RunStatus, RunningJob, Update};
    use crate::model::{JobData, JobKind, Template};
    use crate::source::{Health, SourceId};

    fn rotation() -> Vec<RotationEntry> {
        let entry = |page: &str, dwell_s, skip_when_empty| RotationEntry {
            page: page.into(),
            dwell_s,
            skip_when_empty,
        };
        vec![
            entry("stats", 20, false),
            entry("prs", 8, true),
            entry("pipelines", 10, true),
        ]
    }

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
            },
        }
    }

    fn pr_opened() -> Update {
        Update::PullOpened {
            key: "r".into(),
            project: "demo".into(),
            source: "gitea".into(),
            pull: OpenPull {
                number: 7,
                title: "fix login".into(),
                created: 1,
            },
            notify: false,
        }
    }

    #[test]
    fn quiet_day_shows_stats_only() {
        let mut composer = Composer::new(rotation(), 0);
        let screen = composer.screen(0);
        assert_eq!(screen.template, Template::Stats);
        assert!(screen.stale, "no stats sampled yet");
        assert_eq!(screen.position, Some([1, 1]));
        // Past the dwell time, every other page is empty, so stats stays.
        assert_eq!(composer.screen(25).page, "stats");
    }

    #[test]
    fn prs_page_joins_rotation_when_there_are_prs() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, pr_opened(), 0);
        assert_eq!(composer.screen(0).position, Some([1, 2]));
        let screen = composer.screen(20);
        assert_eq!(screen.page, "prs");
        assert_eq!(screen.position, Some([2, 2]));
        assert_eq!(composer.badges().items[0].id, "prs");
    }

    #[test]
    fn pipelines_page_joins_rotation_after_a_run() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, run(RunStatus::Running), 0);
        assert_eq!(composer.screen(0).position, Some([1, 2]));
        let screen = composer.screen(20);
        assert_eq!(screen.page, "pipelines");
        let ScreenData::List(list) = &screen.data else {
            panic!("expected a list");
        };
        assert_eq!(list.rows[0].text, "ci.yml");
    }

    #[test]
    fn job_interrupts_then_rotation_restarts_at_stats() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, pr_opened(), 0);
        assert_eq!(composer.screen(20).page, "prs");

        send(&mut composer, running_job("j", 21), 21);
        let screen = composer.screen(21);
        assert_eq!(screen.template, Template::Job);
        assert_eq!(screen.position, None);

        send(&mut composer, Update::JobDone { key: "j".into() }, 22);
        assert_eq!(composer.screen(22).page, "stats");
    }

    #[test]
    fn long_press_on_a_job_shows_the_next_one() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, running_job("old", 10), 10);
        send(&mut composer, running_job("new", 20), 20);
        let pipeline = |screen: Screen| match screen.data {
            ScreenData::Job(job) => job.pipeline,
            other => panic!("expected a job, got {other:?}"),
        };
        assert_eq!(pipeline(composer.screen(21)), "new.yml");
        composer.on_button(ButtonAction::Long, 22);
        assert_eq!(pipeline(composer.screen(22)), "old.yml");
        composer.on_button(ButtonAction::Long, 23);
        assert_eq!(pipeline(composer.screen(23)), "new.yml");
    }

    #[test]
    fn button_dismisses_failed_alert_and_clears_badge() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, run(RunStatus::Failed), 0);
        assert_eq!(composer.screen(1).template, Template::Alert);
        assert_eq!(composer.badges().items[0].id, "failed");

        composer.on_button(ButtonAction::Short, 2);
        assert_eq!(composer.screen(2).template, Template::Stats);
        assert!(composer.badges().items.is_empty());
    }

    #[test]
    fn failed_alert_drops_to_badge_after_ten_minutes() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, run(RunStatus::Failed), 0);
        assert_eq!(composer.screen(599).template, Template::Alert);
        assert_eq!(composer.screen(600).template, Template::Stats);
        assert_eq!(composer.badges().items.len(), 1);
    }

    #[test]
    fn source_problem_shows_warn_badge_and_greys_ci_pages() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, pr_opened(), 0);
        let health = |health| SourceMsg {
            source: SourceId::new("gitea", "home"),
            body: SourceBody::Health(health),
        };
        composer.apply(health(Health::AuthFailed), 0);
        let ids: Vec<String> = composer.badges().items.into_iter().map(|b| b.id).collect();
        assert_eq!(ids, ["prs", "warn"]);
        let screen = composer.screen(20);
        assert_eq!(screen.page, "prs");
        assert!(screen.stale);

        composer.apply(health(Health::Ok), 21);
        assert_eq!(composer.badges().items.len(), 1);
    }

    fn alert(json: &str) -> AlertMessage {
        AlertMessage::parse(json.as_bytes()).unwrap()
    }

    #[test]
    fn critical_alert_beats_a_running_job_and_button_clears_it() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, running_job("j", 0), 0);
        composer.alert(
            alert(r#"{"v":2,"id":"leak","severity":"critical","title":"Water leak"}"#),
            1,
        );
        let screen = composer.screen(1);
        assert_eq!(screen.level, Level::Critical);
        let value = serde_json::to_value(&screen).unwrap();
        assert_eq!(value["level"], 0);
        assert_eq!(value["data"]["status"], "failed");
        assert_eq!(value["data"]["title"], "Water leak");
        assert_eq!(value["data"]["project"], serde_json::Value::Null);
        assert_eq!(composer.badges().items[0].id, "alerts");

        composer.on_button(ButtonAction::Short, 2);
        assert_eq!(composer.screen(2).template, Template::Job);
        assert_eq!(composer.badges().items[0].id, "alerts", "badge stays");

        composer.alert(alert(r#"{"v":2,"id":"leak","clear":true}"#), 3);
        assert!(composer.badges().items.is_empty());
    }

    #[test]
    fn warning_alert_is_amber_and_one_press_clears_red_and_amber() {
        let mut composer = Composer::new(rotation(), 0);
        send(&mut composer, run(RunStatus::Failed), 0);
        composer.alert(
            alert(r#"{"v":2,"id":"door","severity":"warning","title":"Freezer door"}"#),
            1,
        );
        let screen = composer.screen(1);
        assert_eq!(screen.level, Level::AlertFailed);
        let ScreenData::Alert(data) = &screen.data else {
            panic!("expected an alert");
        };
        assert_eq!(data.status, AlertStatus::Warn);

        composer.on_button(ButtonAction::Short, 2);
        assert_eq!(composer.screen(2).template, Template::Stats);
        let ids: Vec<String> = composer.badges().items.into_iter().map(|b| b.id).collect();
        assert_eq!(
            ids,
            ["alerts"],
            "failed run dismissed, warning kept as badge"
        );
    }

    #[test]
    fn alerts_page_joins_rotation() {
        let mut composer = Composer::new(
            vec![
                RotationEntry {
                    page: "stats".into(),
                    dwell_s: 20,
                    skip_when_empty: false,
                },
                RotationEntry {
                    page: "alerts".into(),
                    dwell_s: 8,
                    skip_when_empty: true,
                },
            ],
            0,
        );
        assert_eq!(composer.screen(0).position, Some([1, 1]));
        composer.alert(alert(r#"{"v":2,"id":"washer","title":"Washer done"}"#), 1);
        assert_eq!(composer.screen(1).template, Template::Notice);
        // After the 5 s flash rotation restarts at stats, then shows the alert list.
        assert_eq!(composer.screen(6).page, "stats");
        let screen = composer.screen(26);
        assert_eq!(screen.page, "alerts");
        assert_eq!(screen.position, Some([2, 2]));
    }

    #[test]
    fn long_press_pins_rotation() {
        let mut composer = Composer::new(rotation(), 0);
        composer.screen(0);
        composer.on_button(ButtonAction::Long, 1);
        assert!(composer.screen(1).pinned);
    }
}
