//! `--demo`: a scripted two-minute loop of fake data, so the whole kiosk page
//! can be watched without Gitea, Home Assistant or a real server behind it.
//!
//! The script feeds the real composer, exactly as sources and alert
//! publishers would, so what the kiosk shows is what the bridge really has.
//! One loop shows:
//!
//! - fake, gently moving stats, open PRs and pipelines;
//! - an info alert (notice flash) that then stays in the alerts list;
//! - a "New PR" notice;
//! - a running deploy job with step progress, a second job (`others`) whose
//!   run has three parallel jobs, and a step with unknown progress (spinner);
//! - the deploy failing (red alert);
//! - a retry that succeeds (green flash);
//! - an amber warning alert and a red critical alert, cleared one after the
//!   other as Dismiss would.

use crate::alerts::{AlertMessage, Severity};
use crate::ci::{
    OpenPull, RepoPulls, Run, RunJob, RunStatus, Runner, RunnerStatus, RunningJob, Update,
};
use crate::composer::Composer;
use crate::fleet::{HostFacts, StatsReport};
use crate::model::{JobData, JobKind, NetData, SCHEMA_VERSION, StatsData};
use crate::source::{Health, SourceId};

/// Length of one loop in seconds. The script starts over after this.
pub const LOOP_S: u64 = 130;

/// Source label on all demo facts.
const SOURCE: &str = "gitea";

/// Where the demo's links point. `.example` never resolves, so a click in
/// demo mode goes nowhere, which is the point.
const WEB: &str = "https://git.example/demo/deskwatch";
const PROJECT: &str = "webshop";

/// One scripted event.
#[derive(Debug, Clone)]
enum Step {
    /// Forget everything and start the loop again.
    Reset,
    Facts(Vec<Update>),
    Alert(AlertMessage),
}

/// Plays the script against a composer.
pub struct Demo {
    script: Vec<(u64, Step)>,
    /// Unix seconds when the current loop started.
    started: u64,
    /// Index of the next step to play.
    next: usize,
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
                    *composer = Composer::default();
                }
                Step::Facts(updates) => {
                    for update in updates {
                        composer.ci.apply(shift(update, self.started), now);
                    }
                }
                Step::Alert(alert) => composer.alert(alert, now),
            }
        }
        composer.set_stats(stats(now));
        fake_fleet(composer, now);
    }
}

/// Two more hosts and healthy sources, so the kiosk page has something to
/// show in its hosts and sources widgets. Nothing is down, so the badges
/// stay as the script left them.
fn fake_fleet(composer: &mut Composer, now: u64) {
    let wave = |period: f32, phase: f32| ((now as f32 / period + phase).sin() + 1.0) / 2.0;
    let host = |name: &str, cpu: f32, ram: f32, disk: f32, updates: u32| HostFacts {
        name: name.into(),
        up: true,
        stats: StatsData {
            host: name.into(),
            cpu_pct: Some(cpu.round()),
            ram_pct: Some(ram.round()),
            disk_pct: Some(disk),
            updates: Some(updates),
            reboot_needed: Some(updates > 10),
            ..Default::default()
        },
    };
    let id = SourceId::new("prometheus", "demo");
    composer.fleet.apply(
        &id,
        StatsReport {
            hosts: vec![
                host("demo-nas", 4.0 + 12.0 * wave(17.0, 1.0), 41.0, 82.0, 14),
                host("demo-agent", 10.0 + 70.0 * wave(31.0, 2.0), 55.0, 47.0, 0),
            ],
            down: Vec::new(),
        },
    );
    composer.health.set(&id, Health::Ok);
    composer
        .health
        .set(&SourceId::new(SOURCE, "demo"), Health::Ok);

    // Four runners for the kiosk runners widget: one flips between busy and
    // idle, one is down and one is switched off. Refreshed every tick, like a poll.
    let runner = |name: &str, status: RunnerStatus, disabled: bool, labels: &[&str]| Runner {
        source: SOURCE.into(),
        name: name.into(),
        status,
        disabled,
        labels: labels.iter().map(|l| l.to_string()).collect(),
        url: Some(format!("{WEB}/settings/actions/runners")),
    };
    let busy = if wave(13.0, 0.0) > 0.5 {
        RunnerStatus::Busy
    } else {
        RunnerStatus::Idle
    };
    composer.ci.apply(
        Update::Runners {
            key: format!("{SOURCE}:demo:runners:user"),
            runners: vec![
                runner("demo-runner-1", busy, false, &["ubuntu-latest"]),
                runner(
                    "demo-runner-2",
                    RunnerStatus::Idle,
                    false,
                    &["ubuntu-latest"],
                ),
                runner("demo-runner-3", RunnerStatus::Offline, false, &["arm64"]),
                runner("demo-runner-4", RunnerStatus::Offline, true, &["windows"]),
            ],
        },
        now,
    );
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
        ram_pct: Some(round(38.0 + 20.0 * wave(23.0, 1.0))),
        disk_pct: Some(63.0),
        gpu_pct: Some(round(5.0 + 70.0 * wave(13.0, 0.7))),
        gpu_mem_pct: Some(round(30.0 + 25.0 * wave(29.0, 0.2))),
        gpu_temp_c: Some(round(38.0 + 30.0 * wave(13.0, 0.7))),
        uptime_s: Some(1_234_567 + now % 100_000),
        net: Some(NetData {
            iface: "eth0".into(),
            rx_bps: Some((40_000.0 + 900_000.0 * wave(7.0, 2.0)) as u64),
            tx_bps: Some((8_000.0 + 120_000.0 * wave(11.0, 0.5)) as u64),
        }),
        updates: Some(7),
        reboot_needed: Some(false),
    }
}

/// Script timestamps (`started`, `finished`, `created`) are seconds into the
/// loop; turn them into Unix seconds so the kiosk shows sensible times.
fn shift(update: Update, base: u64) -> Update {
    match update {
        Update::JobRunning { key, mut job } => {
            job.data.started += base;
            job.updated += base;
            Update::JobRunning { key, job }
        }
        Update::RunJobs { key, mut jobs } => {
            for job in &mut jobs {
                job.started = job.started.map(|s| s + base);
                job.finished = job.finished.map(|f| f + base);
            }
            Update::RunJobs { key, jobs }
        }
        Update::Run { key, mut run } => {
            run.started += base;
            run.finished = run.finished.map(|f| f + base);
            Update::Run { key, run }
        }
        Update::Pulls { key, mut repo } => {
            for pull in &mut repo.pulls {
                pull.created += base;
            }
            Update::Pulls { key, repo }
        }
        Update::PullOpened {
            key,
            project,
            source,
            mut pull,
            notify,
        } => {
            pull.created += base;
            Update::PullOpened {
                key,
                project,
                source,
                pull,
                notify,
            }
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
            url: Some(format!("{WEB}/actions/{pipeline}")),
            group: Some(format!("demo:run:{pipeline}")),
        },
    }
}

/// The parallel jobs of a running run, as (name, status, step, progress,
/// started, finished).
#[allow(clippy::type_complexity)]
fn run_jobs(
    pipeline: &str,
    jobs: &[(&str, RunStatus, Option<&str>, Option<f32>, u64, Option<u64>)],
) -> Update {
    Update::RunJobs {
        key: format!("demo:run:{pipeline}"),
        jobs: jobs
            .iter()
            .map(
                |&(name, status, step, progress, started, finished)| RunJob {
                    name: name.into(),
                    status,
                    step: step.map(String::from),
                    progress,
                    started: (status != RunStatus::Queued).then_some(started),
                    finished,
                    url: Some(format!("{WEB}/actions/{pipeline}")),
                },
            )
            .collect(),
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
            url: Some(format!("{WEB}/actions/{pipeline}")),
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
        url: None,
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
        url: None,
    })
}

fn pull(number: u64, title: &str) -> OpenPull {
    OpenPull {
        number,
        title: title.into(),
        created: number,
        url: Some(format!("{WEB}/pulls/{number}")),
    }
}

/// The loop, as (seconds into the loop, step). Must be sorted by time.
fn script() -> Vec<(u64, Step)> {
    use JobKind::{Build, Deploy};
    use RunStatus::{Failed, Queued, Running, Success};
    use Step::Facts;

    vec![
        // Idle: PRs, pipelines and an info alert flash.
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
                // Older runs: rows in the pipelines list and a failed badge,
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
        // A new issue and a PR comment, as the git sources raise them.
        (
            3,
            Step::Alert(AlertMessage::activity(
                "demo:issue:46".into(),
                "Checkout button hidden on phones".into(),
                format!("New issue #46 in {PROJECT} by dana"),
                SOURCE,
                Some(format!("{WEB}/issues/46")),
            )),
        ),
        (
            4,
            Step::Alert(AlertMessage::activity(
                "demo:comment:44".into(),
                "Sam on !44 dark mode for the checkout".into(),
                "Looks good, can you add a screenshot?".into(),
                "azure_devops",
                None,
            )),
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
            Facts(vec![
                run_jobs(
                    "ci.yml",
                    &[
                        ("stable", Running, Some("cargo test"), Some(0.5), 43, None),
                        ("msrv", Running, Some("cargo build"), Some(0.3), 43, None),
                        ("windows", Queued, None, None, 43, None),
                    ],
                ),
                job("ci.yml", Build, ("cargo test", 2, 3), Some(0.5), 43),
            ]),
        ),
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
            49,
            Facts(vec![run_jobs(
                "ci.yml",
                &[
                    ("stable", Success, None, None, 43, Some(48)),
                    ("msrv", Running, Some("cargo test"), Some(0.7), 43, None),
                    ("windows", Running, Some("cargo test"), Some(0.4), 46, None),
                ],
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
        // The deploy fails: red alert until the retry takes over.
        (
            59,
            Facts(vec![
                job_done("deploy.yml"),
                run("deploy.yml", Failed, Some("Smoke test"), 38, true),
            ]),
        ),
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
        // Home alerts: amber warning, then red critical, cleared one after
        // the other.
        (
            93,
            alert(
                "freezer",
                Severity::Warning,
                "Freezer door",
                "Open for 5 min",
            ),
        ),
        (
            103,
            alert(
                "leak",
                Severity::Critical,
                "Water leak",
                "Utility room sensor",
            ),
        ),
        (111, clear("leak")),
        (118, clear("freezer")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AlertStatus, Level, ScreenData};

    /// Run one loop second by second and describe every interrupt shown.
    fn play(start: u64) -> Vec<String> {
        let mut composer = Composer::default();
        let mut demo = Demo::new(start);
        let mut seen = Vec::new();
        for now in start..start + LOOP_S {
            demo.tick(&mut composer, now);
            let label = match composer.screen(now) {
                None => "idle".to_string(),
                Some(screen) => match &screen.data {
                    ScreenData::Alert(a) => {
                        format!("alert:{:?}:{}", a.status, screen.level.number())
                    }
                    ScreenData::Job(j) => format!("job:{}:{}", j.pipeline, j.others),
                    _ => format!("{:?}:{}", screen.template, screen.page),
                },
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
        let mut composer = Composer::default();
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
    fn one_loop_shows_every_interrupt() {
        let seen = play(1_791_410_000);
        for expected in [
            "idle",
            "Notice:homeassistant-notice",
            "Notice:gitea-notice",
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
    fn the_failure_shows_until_the_retry_and_the_home_alerts_clear() {
        let start = 1_000;
        let mut composer = run_until(start, 66);
        let screen = composer.screen(start + 66).unwrap();
        assert!(matches!(&screen.data, ScreenData::Alert(a) if a.status == AlertStatus::Failed));
        assert!(has_badge(&composer, "failed"));

        // The retry succeeded: only the older nightly failure is left in the badge.
        let composer = run_until(start, 80);
        let failed = composer
            .badges()
            .items
            .into_iter()
            .find(|b| b.id == "failed");
        assert_eq!(failed.map(|b| b.count), Some(1));

        // Both home alerts on screen and in the badge, then cleared.
        let mut composer = run_until(start, 105);
        assert_eq!(composer.screen(start + 105).unwrap().level, Level::Critical);
        let home = composer
            .badges()
            .items
            .into_iter()
            .find(|b| b.id == "alerts");
        assert_eq!(home.map(|b| b.count), Some(2));
        assert!(!has_badge(&run_until(start, 125), "alerts"));
    }

    #[test]
    fn loop_starts_over() {
        let start = 5_000;
        let mut composer = run_until(start, LOOP_S + 1);
        // One second into the second loop: a fresh start with the same PRs.
        assert_eq!(composer.ci.open_pull_count(), 3);
        assert!(composer.screen(start + LOOP_S + 1).is_none());
    }
}
