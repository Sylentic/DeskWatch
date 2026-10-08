//! `--demo`: a scripted two-minute loop of fake data, so the whole panel can be
//! watched in the desktop simulator without Gitea, Home Assistant or a real
//! server behind it.
//!
//! The script feeds the real composer, exactly as sources and the panel
//! would, so what the simulator shows is what the bridge really publishes.
//! One loop shows:
//!
//! - the stats page (fake, gently moving numbers), open PRs and pipelines;
//! - an info alert (notice flash) that then joins the alerts page;
//! - a "New PR" notice;
//! - a running deploy job with step progress, a second job (`others`), long
//!   presses cycling between them, and a step with unknown progress (spinner);
//! - the deploy failing (red alert), then a short press clearing it;
//! - a retry that succeeds (green flash);
//! - an amber warning alert and a red critical alert, each cleared with the
//!   button, after which they stay as a `home` badge until cleared over MQTT.
//!
//! Pressing the real (simulated) button during the demo works as usual.

use crate::alerts::{AlertMessage, Severity};
use crate::ci::{OpenPull, RepoPulls, Run, RunStatus, RunningJob, Update};
use crate::composer::Composer;
use crate::model::{
    ButtonAction, JobData, JobKind, NetData, RotationEntry, SCHEMA_VERSION, StatsData,
};

/// Length of one loop in seconds. The script starts over after this.
pub const LOOP_S: u64 = 130;

/// Source label on all demo facts.
const SOURCE: &str = "gitea";
const PROJECT: &str = "webshop";

/// One scripted event.
#[derive(Debug, Clone)]
enum Step {
    /// Forget everything and start the loop again.
    Reset,
    Facts(Vec<Update>),
    Alert(AlertMessage),
    Button(ButtonAction),
}

/// Plays the script against a composer.
pub struct Demo {
    script: Vec<(u64, Step)>,
    /// Unix seconds when the current loop started.
    started: u64,
    /// Index of the next step to play.
    next: usize,
}

/// Short dwell times, so every page comes up within the loop.
pub fn rotation() -> Vec<RotationEntry> {
    let entry = |page: &str, dwell_s, skip_when_empty| RotationEntry {
        page: page.into(),
        dwell_s,
        skip_when_empty,
    };
    vec![
        entry("stats", 8, false),
        entry("prs", 5, true),
        entry("pipelines", 5, true),
        entry("alerts", 5, true),
    ]
}

impl Demo {
    pub fn new(now: u64) -> Self {
        Self {
            script: script(),
            started: now,
            next: 0,
        }
    }

    /// Play every step that is due at `now` and refresh the fake stats. Call
    /// this before asking the composer for the screen.
    pub fn tick(&mut self, composer: &mut Composer, now: u64) {
        if now.saturating_sub(self.started) >= LOOP_S {
            self.started = now;
            self.next = 0;
        }
        let elapsed = now.saturating_sub(self.started);
        while let Some((at, step)) = self.script.get(self.next) {
            if *at > elapsed {
                break;
            }
            // The script uses times relative to the loop; facts need real ones.
            let step = step.clone();
            self.next += 1;
            match step {
                Step::Reset => {
                    *composer = Composer::new(rotation(), now);
                }
                Step::Facts(updates) => {
                    for update in updates {
                        composer.ci.apply(shift(update, self.started), now);
                    }
                }
                Step::Alert(alert) => composer.alert(alert, now),
                Step::Button(action) => {
                    // The button acts on what is shown, so bring that up to date.
                    composer.screen(now);
                    composer.on_button(action, now);
                }
            }
        }
        composer.set_stats(stats(now));
    }
}

/// Fake server stats that drift slowly, so the gauges move.
pub fn stats(now: u64) -> StatsData {
    // New numbers every 5 s, like the real stats interval.
    let now = now - now % 5;
    let wave = |period: f32, phase: f32| ((now as f32 / period + phase).sin() + 1.0) / 2.0;
    let round = |x: f32| (x * 10.0).round() / 10.0;
    let cpu = round(8.0 + 60.0 * wave(9.0, 0.0));
    StatsData {
        host: "demo-server".into(),
        cpu_pct: Some(cpu),
        cpu_temp_c: Some(round(42.0 + cpu / 4.0)),
        load: Some([round(cpu / 25.0), round(cpu / 30.0), 0.6]),
        cpu_count: Some(4),
        ram_pct: Some(round(38.0 + 20.0 * wave(23.0, 1.0))),
        disk_pct: Some(63.0),
        uptime_s: Some(1_234_567 + now % 100_000),
        net: Some(NetData {
            iface: "eth0".into(),
            rx_bps: Some((40_000.0 + 900_000.0 * wave(7.0, 2.0)) as u64),
            tx_bps: Some((8_000.0 + 120_000.0 * wave(11.0, 0.5)) as u64),
        }),
    }
}

/// Script timestamps (`started`, `finished`, `created`) are seconds into the
/// loop; turn them into Unix seconds so the panel shows sensible times.
fn shift(update: Update, base: u64) -> Update {
    match update {
        Update::JobRunning { key, mut job } => {
            job.data.started += base;
            job.updated += base;
            Update::JobRunning { key, job }
        }
        Update::Run { key, mut run } => {
            run.started += base;
            run.finished = run.finished.map(|f| f + base);
            Update::Run { key, run }
        }
        other => other,
    }
}

fn job(
    pipeline: &str,
    kind: JobKind,
    step: (&str, u32, u32),
    progress: Option<f32>,
    started: u64,
) -> Update {
    Update::JobRunning {
        key: format!("demo:{pipeline}"),
        job: RunningJob {
            data: JobData {
                source: SOURCE.into(),
                project: PROJECT.into(),
                pipeline: pipeline.into(),
                kind,
                git_ref: "main".into(),
                commit: "a1b2c3d".into(),
                step: Some(step.0.into()),
                step_no: Some(step.1),
                step_count: Some(step.2),
                progress,
                started,
                others: 0,
            },
            interrupt: true,
            updated: started,
        },
    }
}

fn job_done(pipeline: &str) -> Update {
    Update::JobDone {
        key: format!("demo:{pipeline}"),
    }
}

fn run(
    pipeline: &str,
    status: RunStatus,
    step: Option<&str>,
    started: u64,
    interrupt: bool,
) -> Update {
    let finished = matches!(status, RunStatus::Failed | RunStatus::Success).then_some(started + 30);
    Update::Run {
        key: format!("demo:{pipeline}"),
        run: Run {
            source: SOURCE.into(),
            project: PROJECT.into(),
            pipeline: pipeline.into(),
            git_ref: "main".into(),
            status,
            step: step.map(String::from),
            started,
            finished,
            interrupt,
        },
    }
}

fn alert(id: &str, severity: Severity, title: &str, message: &str) -> Step {
    Step::Alert(AlertMessage {
        v: SCHEMA_VERSION,
        id: Some(id.into()),
        severity: Some(severity),
        title: Some(title.into()),
        message: Some(message.into()),
        ttl_s: None,
        source: Some("homeassistant".into()),
        clear: false,
    })
}

fn clear(id: &str) -> Step {
    Step::Alert(AlertMessage {
        v: SCHEMA_VERSION,
        id: Some(id.into()),
        severity: None,
        title: None,
        message: None,
        ttl_s: None,
        source: None,
        clear: true,
    })
}

fn pull(number: u64, title: &str) -> OpenPull {
    OpenPull {
        number,
        title: title.into(),
        created: number,
    }
}

/// The loop, as (seconds into the loop, step). Must be sorted by time.
fn script() -> Vec<(u64, Step)> {
    use JobKind::{Build, Deploy};
    use RunStatus::{Failed, Running, Success};
    use Step::{Button, Facts};
    let short = ButtonAction::Short;
    let long = ButtonAction::Long;

    vec![
        // Idle: stats, an info alert flash, then PRs, pipelines and the
        // alerts page rotate in.
        (0, Step::Reset),
        (
            0,
            Facts(vec![
                Update::Pulls {
                    key: "demo:pulls".into(),
                    repo: RepoPulls {
                        project: PROJECT.into(),
                        source: SOURCE.into(),
                        pulls: vec![
                            pull(41, "fix login redirect"),
                            pull(42, "bump dependencies"),
                            pull(44, "dark mode for the checkout"),
                        ],
                        total: 3,
                    },
                },
                // Older runs: rows on the pipelines page and a failed badge,
                // no flash.
                run("ci.yml", Success, None, 0, false),
                run("nightly.yml", Failed, Some("e2e tests"), 0, false),
                run("docs.yml", Running, None, 0, false),
            ]),
        ),
        (
            2,
            alert("washer", Severity::Info, "Washing machine", "Done, 58 min"),
        ),
        (
            31,
            Facts(vec![Update::PullOpened {
                key: "demo:pulls".into(),
                project: PROJECT.into(),
                source: SOURCE.into(),
                pull: pull(45, "add invoice export"),
                notify: true,
            }]),
        ),
        // A deploy runs, with a second job alongside it.
        (
            38,
            Facts(vec![job(
                "deploy.yml",
                Deploy,
                ("Checkout", 1, 5),
                Some(0.1),
                38,
            )]),
        ),
        (
            41,
            Facts(vec![job(
                "deploy.yml",
                Deploy,
                ("Build image", 2, 5),
                Some(0.3),
                38,
            )]),
        ),
        (
            43,
            Facts(vec![job(
                "ci.yml",
                Build,
                ("cargo test", 2, 3),
                Some(0.5),
                43,
            )]),
        ),
        (45, Button(long)),
        (48, Button(long)),
        (
            49,
            Facts(vec![job(
                "deploy.yml",
                Deploy,
                ("Run migrations", 3, 5),
                None,
                38,
            )]),
        ),
        (
            53,
            Facts(vec![
                job_done("ci.yml"),
                run("ci.yml", Success, None, 43, false),
            ]),
        ),
        (
            54,
            Facts(vec![job(
                "deploy.yml",
                Deploy,
                ("Push image", 4, 5),
                Some(0.75),
                38,
            )]),
        ),
        // The deploy fails: red alert until the button is pressed.
        (
            59,
            Facts(vec![
                job_done("deploy.yml"),
                run("deploy.yml", Failed, Some("Smoke test"), 38, true),
            ]),
        ),
        (67, Button(short)),
        // A retry succeeds: green flash.
        (
            71,
            Facts(vec![job(
                "deploy.yml",
                Deploy,
                ("Build image", 1, 2),
                Some(0.4),
                71,
            )]),
        ),
        (
            75,
            Facts(vec![job(
                "deploy.yml",
                Deploy,
                ("Smoke test", 2, 2),
                Some(0.85),
                71,
            )]),
        ),
        (
            79,
            Facts(vec![
                job_done("deploy.yml"),
                run("deploy.yml", Success, None, 71, true),
            ]),
        ),
        // Home alerts: amber warning, then red critical, each acknowledged
        // with the button and later cleared over MQTT.
        (
            93,
            alert(
                "freezer",
                Severity::Warning,
                "Freezer door",
                "Open for 5 min",
            ),
        ),
        (100, Button(short)),
        (
            103,
            alert(
                "leak",
                Severity::Critical,
                "Water leak",
                "Utility room sensor",
            ),
        ),
        (111, Button(short)),
        (118, clear("leak")),
        (121, clear("freezer")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AlertStatus, Level, ScreenData, Template};

    /// Run one loop second by second and describe every screen shown.
    fn play(start: u64) -> Vec<String> {
        let mut composer = Composer::new(rotation(), start);
        let mut demo = Demo::new(start);
        let mut seen = Vec::new();
        for now in start..start + LOOP_S {
            demo.tick(&mut composer, now);
            let screen = composer.screen(now);
            let label = match &screen.data {
                ScreenData::Alert(a) => format!("alert:{:?}:{}", a.status, screen.level.number()),
                ScreenData::Job(j) => format!("job:{}:{}", j.pipeline, j.others),
                _ => format!("{:?}:{}", screen.template, screen.page),
            };
            if seen.last() != Some(&label) {
                seen.push(label);
            }
        }
        seen
    }

    #[test]
    fn script_is_sorted_and_fits_the_loop() {
        let times: Vec<u64> = script().iter().map(|(at, _)| *at).collect();
        assert!(times.is_sorted());
        assert!(*times.last().unwrap() < LOOP_S);
    }

    /// A composer after the demo has run from `start` up to `start + t`.
    fn run_until(start: u64, t: u64) -> Composer {
        let mut composer = Composer::new(rotation(), start);
        let mut demo = Demo::new(start);
        for now in start..=start + t {
            demo.tick(&mut composer, now);
            composer.screen(now);
        }
        composer
    }

    fn has_badge(composer: &Composer, id: &str) -> bool {
        composer.badges().items.iter().any(|b| b.id == id)
    }

    #[test]
    fn one_loop_shows_every_page_and_interrupt() {
        let seen = play(1_791_410_000);
        for expected in [
            "Stats:stats",
            "List:prs",
            "List:pipelines",
            "List:alerts",
            "Notice:homeassistant-notice",
            "Notice:gitea-notice",
            "job:deploy.yml:1",
            "job:ci.yml:1",
            "job:deploy.yml:0",
            "alert:Failed:2",
            "alert:Success:3",
            "alert:Warn:2",
            "alert:Failed:0",
        ] {
            assert!(
                seen.iter().any(|s| s == expected),
                "{expected} not in {seen:?}"
            );
        }
    }

    #[test]
    fn button_clears_the_failure_and_badges_settle() {
        let start = 1_000;
        let mut composer = run_until(start, 66);
        let screen = composer.screen(start + 66);
        assert!(matches!(&screen.data, ScreenData::Alert(a) if a.status == AlertStatus::Failed));
        assert!(has_badge(&composer, "failed"));

        let mut composer = run_until(start, 67);
        assert_ne!(composer.screen(start + 67).level, Level::AlertFailed);
        assert!(!has_badge(&composer, "failed"));

        // Both home alerts acknowledged: off screen, still in the badge.
        let mut composer = run_until(start, 112);
        assert_eq!(composer.screen(start + 112).level, Level::Rotation);
        let home = composer
            .badges()
            .items
            .into_iter()
            .find(|b| b.id == "alerts");
        assert_eq!(home.map(|b| b.count), Some(2));

        // Cleared over MQTT at the end of the loop.
        assert!(!has_badge(&run_until(start, 125), "alerts"));
    }

    #[test]
    fn loop_starts_over() {
        let start = 5_000;
        let mut composer = run_until(start, LOOP_S + 1);
        // One second into the second loop: a fresh start on the stats page.
        assert_eq!(
            composer.screen(start + LOOP_S + 1).template,
            Template::Stats
        );
        assert_eq!(composer.ci.open_pull_count(), 3);
    }
}
