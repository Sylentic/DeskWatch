//! The composer: decides which page is on the panel right now.
//!
//! It owns the idle rotation, the latest server stats and the CI facts. Every
//! tick the main loop asks it for the current screen and badges and publishes
//! them when they change. Button presses from the panel come in here too.

use crate::ci::CiFacts;
use crate::model::{
    AlertStatus, Badges, ButtonAction, Level, ListData, Rotation, RotationEntry, Screen,
    ScreenData, StatsData, pick,
};

pub struct Composer {
    rotation: Rotation,
    /// Unix seconds when the current rotation page came up.
    rotated_at: u64,
    /// Level of the screen returned by the last `screen` call.
    shown: Level,
    stats: Option<StatsData>,
    pub ci: CiFacts,
}

impl Composer {
    pub fn new(rotation: Vec<RotationEntry>, now: u64) -> Self {
        Self {
            rotation: Rotation::new(rotation),
            rotated_at: now,
            shown: Level::Rotation,
            stats: None,
            ci: CiFacts::default(),
        }
    }

    pub fn set_stats(&mut self, stats: StatsData) {
        self.stats = Some(stats);
    }

    /// The screen to show at `now`. Also moves the rotation along when the
    /// current page has had its dwell time.
    pub fn screen(&mut self, now: u64) -> Screen {
        self.ci.expire(now);

        // Interrupts first: running job, then alerts, then notices.
        let candidates = self.ci.candidates();
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
        screen.position = self.rotation.position(|p| self.page_data(p).0.is_empty());
        screen
    }

    /// Header badges for the current facts.
    pub fn badges(&self) -> Badges {
        Badges::new(self.ci.badges())
    }

    /// React to a button press on whatever was shown last.
    ///
    /// - Short press on an alert or notice dismisses it (all failed alerts at
    ///   once, so one press acknowledges a bad afternoon).
    /// - Short press in rotation shows the next page now.
    /// - Long press in rotation pins or unpins the current page.
    ///
    /// Long press during a job (cycle through running jobs) is not done yet.
    pub fn on_button(&mut self, action: ButtonAction, now: u64) {
        match (self.shown, action) {
            (Level::AlertFailed, ButtonAction::Short) => {
                self.ci.dismiss_alerts(AlertStatus::Failed)
            }
            (Level::AlertSuccess, ButtonAction::Short) => {
                self.ci.dismiss_alerts(AlertStatus::Success)
            }
            (Level::Notice, ButtonAction::Short) => self.ci.dismiss_notices(),
            (Level::Rotation, ButtonAction::Short) => self.next_page(now),
            (Level::Rotation, ButtonAction::Long) => self.rotation.toggle_pin(),
            _ => {}
        }
    }

    fn next_page(&mut self, now: u64) {
        // Collect emptiness first: `advance` borrows the rotation mutably.
        let empty: Vec<String> = ["prs", "pipelines"]
            .into_iter()
            .filter(|p| self.page_data(p).0.is_empty())
            .map(String::from)
            .collect();
        self.rotation
            .advance(|page| empty.iter().any(|e| e == page) || !known_page(page));
        self.rotated_at = now;
    }

    /// Data for a rotation page, and whether it is stale.
    fn page_data(&self, page: &str) -> (ScreenData, bool) {
        match page {
            "stats" => match &self.stats {
                Some(stats) => (ScreenData::Stats(stats.clone()), false),
                None => (ScreenData::Stats(StatsData::default()), true),
            },
            "prs" => (ScreenData::List(self.ci.pulls_page()), self.ci.pulls_stale),
            // Pipeline history arrives with a later change; until then the
            // page is empty and skipped.
            "pipelines" => (
                ScreenData::List(ListData::new("Pipelines", 0, vec![])),
                false,
            ),
            other => (ScreenData::List(ListData::new(other, 0, vec![])), false),
        }
    }
}

/// Pages the composer can fill. Unknown names in the config show as empty lists.
fn known_page(page: &str) -> bool {
    matches!(page, "stats" | "prs" | "pipelines")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{OpenPull, RunningJob};
    use crate::model::{AlertData, JobData, JobKind, Template};

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

    fn running_job(started: u64) -> RunningJob {
        RunningJob {
            data: JobData {
                source: "gitea".into(),
                project: "demo".into(),
                pipeline: "deploy.yml".into(),
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
        }
    }

    fn failed() -> AlertData {
        AlertData {
            status: AlertStatus::Failed,
            source: "gitea".into(),
            project: "demo".into(),
            pipeline: "ci.yml".into(),
            step: Some("cargo test".into()),
            started: 0,
            finished: 10,
            others: 0,
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
        let pull = OpenPull {
            number: 7,
            title: "fix login".into(),
            created: 1,
        };
        composer.ci.pull_opened("r", "demo", "gitea", pull);
        assert_eq!(composer.screen(0).position, Some([1, 2]));
        let screen = composer.screen(20);
        assert_eq!(screen.page, "prs");
        assert_eq!(screen.position, Some([2, 2]));
        assert_eq!(composer.badges().items[0].count, 1);
    }

    #[test]
    fn job_interrupts_then_rotation_restarts_at_stats() {
        let mut composer = Composer::new(rotation(), 0);
        composer.ci.pull_opened(
            "r",
            "demo",
            "gitea",
            OpenPull {
                number: 1,
                title: "t".into(),
                created: 1,
            },
        );
        assert_eq!(composer.screen(20).page, "prs");

        composer.ci.job_running("j", running_job(21));
        let screen = composer.screen(21);
        assert_eq!(screen.template, Template::Job);
        assert_eq!(screen.position, None);

        composer.ci.job_done("j");
        assert_eq!(composer.screen(22).page, "stats");
    }

    #[test]
    fn button_dismisses_failed_alert_and_clears_badge() {
        let mut composer = Composer::new(rotation(), 0);
        composer.ci.run_finished("p", failed(), 0);
        assert_eq!(composer.screen(1).template, Template::Alert);
        assert_eq!(composer.badges().items[0].id, "ci-failed");

        composer.on_button(ButtonAction::Short, 2);
        assert_eq!(composer.screen(2).template, Template::Stats);
        assert!(composer.badges().items.is_empty());
    }

    #[test]
    fn failed_alert_drops_to_badge_after_ten_minutes() {
        let mut composer = Composer::new(rotation(), 0);
        composer.ci.run_finished("p", failed(), 0);
        assert_eq!(composer.screen(599).template, Template::Alert);
        assert_eq!(composer.screen(600).template, Template::Stats);
        assert_eq!(composer.badges().items.len(), 1);
    }

    #[test]
    fn long_press_pins_rotation() {
        let mut composer = Composer::new(rotation(), 0);
        composer.screen(0);
        composer.on_button(ButtonAction::Long, 1);
        assert!(composer.screen(1).pinned);
    }
}
