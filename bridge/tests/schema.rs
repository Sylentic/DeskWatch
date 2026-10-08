//! The bridge side of the MQTT contract in `docs/mqtt-schema.md`.
//!
//! Every sample in `docs/schema/` is checked here against what the bridge
//! really produces or accepts. The panel side (`ui/tests/schema.rs`) parses
//! the same files, so a change to either side that breaks the contract fails
//! a test.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use deskwatch_bridge::alerts::{AlertMessage, Alerts};
use deskwatch_bridge::ci::{CiFacts, Run, RunStatus, Update};
use deskwatch_bridge::model::{
    Badge, BadgeIcon, Badges, ButtonAction, Candidate, JobData, JobKind, Level, ListData, ListRow,
    NetData, NumberData, PanelEvent, RowStatus, Screen, ScreenData, StatsData, pick,
};

/// Time used for every sample: the `started` value in the files.
const STARTED: u64 = 1_791_410_400;

fn sample(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/schema")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Compare as JSON values. Goes through a string so `f32` fields compare as
/// the panel would read them (0.42, not 0.41999998688697815).
fn assert_matches(name: &str, payload: &impl Serialize) {
    let produced: Value = serde_json::from_str(&serde_json::to_string(payload).unwrap()).unwrap();
    let expected: Value = serde_json::from_str(&sample(name)).unwrap();
    assert_eq!(
        produced, expected,
        "{name} differs from what the bridge sends"
    );
}

/// A screen with the envelope fields of the sample files.
fn screen(
    level: Level,
    page: &str,
    data: ScreenData,
    seq: u64,
    position: Option<[u8; 2]>,
) -> Screen {
    let mut screen = Screen::new(level, page, data);
    screen.seq = seq;
    screen.position = position;
    screen
}

fn top(candidates: &[Candidate]) -> &Candidate {
    pick(candidates, STARTED).expect("a candidate")
}

fn alert_screen(json: &str, seq: u64) -> Screen {
    let mut alerts = Alerts::default();
    alerts.apply(AlertMessage::parse(json.as_bytes()).unwrap(), STARTED);
    let candidates = alerts.candidates(STARTED);
    let c = top(&candidates);
    screen(c.level, &c.page, c.data.clone(), seq, None)
}

fn ci_alert_screen(
    status: RunStatus,
    pipeline: &str,
    step: Option<&str>,
    finished: u64,
    seq: u64,
) -> Screen {
    let mut ci = CiFacts::default();
    let run = Run {
        source: "gitea".into(),
        project: "webshop".into(),
        pipeline: pipeline.into(),
        git_ref: "main".into(),
        status,
        step: step.map(String::from),
        started: STARTED,
        finished: Some(finished),
        interrupt: true,
    };
    ci.apply(
        Update::Run {
            key: "k".into(),
            run,
        },
        finished,
    );
    let candidates = ci.candidates();
    let c = top(&candidates);
    screen(c.level, &c.page, c.data.clone(), seq, None)
}

#[test]
fn stats_screen() {
    let data = ScreenData::Stats(StatsData {
        host: "homeserver".into(),
        cpu_pct: Some(12.5),
        cpu_temp_c: Some(48.5),
        load: Some([0.42, 0.38, 0.3]),
        cpu_count: Some(4),
        ram_pct: Some(41.0),
        disk_pct: Some(63.0),
        uptime_s: Some(1_234_567),
        net: Some(NetData {
            iface: "eth0".into(),
            rx_bps: Some(125_000),
            tx_bps: Some(23_000),
        }),
    });
    let s = screen(Level::Rotation, "stats", data, 1001, Some([1, 3]));
    assert_matches("screen-stats.json", &s);
}

#[test]
fn job_screen() {
    let data = ScreenData::Job(JobData {
        source: "gitea".into(),
        project: "webshop".into(),
        pipeline: "deploy.yml".into(),
        kind: JobKind::Deploy,
        git_ref: "main".into(),
        commit: "a1b2c3d".into(),
        step: Some("Run migrations".into()),
        step_no: Some(3),
        step_count: Some(5),
        progress: Some(0.6),
        started: STARTED,
        others: 1,
    });
    let s = screen(Level::Job, "gitea-job", data, 1010, None);
    assert_matches("screen-job.json", &s);
}

#[test]
fn ci_alert_screens() {
    let failed = ci_alert_screen(
        RunStatus::Failed,
        "ci.yml",
        Some("cargo test"),
        STARTED + 300,
        1020,
    );
    assert_matches("screen-alert-failed.json", &failed);
    let success = ci_alert_screen(RunStatus::Success, "deploy.yml", None, STARTED + 282, 1021);
    assert_matches("screen-alert-success.json", &success);
}

#[test]
fn home_alert_screens() {
    let warn = alert_screen(
        r#"{"v":2,"id":"freezer","severity":"warning","title":"Freezer door",
            "message":"Open for 5 min","source":"homeassistant"}"#,
        1022,
    );
    assert_matches("screen-alert-warn.json", &warn);
    let critical = alert_screen(
        r#"{"v":2,"id":"leak","severity":"critical","title":"Water leak",
            "message":"Utility room sensor","source":"homeassistant"}"#,
        1023,
    );
    assert_matches("screen-alert-critical.json", &critical);
}

#[test]
fn info_alert_sample_becomes_the_notice_screen() {
    let notice = alert_screen(&sample("alert-raise.json"), 1050);
    assert_matches("screen-notice.json", &notice);
}

#[test]
fn list_number_screens() {
    let row = |text: &str, sub: &str, status, source: &str| ListRow {
        text: text.into(),
        sub: sub.into(),
        status,
        source: source.into(),
    };
    let list = ListData::new(
        "Open PRs",
        3,
        vec![
            row(
                "fix login redirect",
                "webshop #42",
                RowStatus::Open,
                "gitea",
            ),
            row("bump deps", "bridge #7", RowStatus::Review, "github"),
        ],
    );
    let s = screen(
        Level::Rotation,
        "prs",
        ScreenData::List(list),
        1030,
        Some([2, 3]),
    );
    assert_matches("screen-list.json", &s);

    let number = ScreenData::Number(NumberData {
        title: "Backups".into(),
        value: "OK".into(),
        sub: "last run 03:00".into(),
    });
    let s = screen(Level::Rotation, "backups", number, 1040, Some([3, 3]));
    assert_matches("screen-number.json", &s);
}

#[test]
fn badges() {
    let badge = |id: &str, icon, count, status| Badge {
        id: id.into(),
        icon,
        count,
        status,
    };
    let badges = Badges::new([
        badge("alerts", BadgeIcon::Home, 2, RowStatus::Failed),
        badge("failed", BadgeIcon::Pipeline, 1, RowStatus::Failed),
        badge("prs", BadgeIcon::Pr, 3, RowStatus::Open),
        badge("warn", BadgeIcon::Warn, 1, RowStatus::Failed),
    ]);
    assert_matches("badges.json", &badges);
}

#[test]
fn inbound_samples_parse() {
    let event: PanelEvent = serde_json::from_str(&sample("panel-event.json")).unwrap();
    assert_eq!(event.action, ButtonAction::Short);

    let raise = AlertMessage::parse(sample("alert-raise.json").as_bytes()).unwrap();
    assert_eq!(raise.id.as_deref(), Some("washer"));
    assert_eq!(raise.ttl_s, Some(600));
    let clear = AlertMessage::parse(sample("alert-clear.json").as_bytes()).unwrap();
    assert!(clear.clear);
}

#[test]
fn every_sample_is_checked() {
    // A new sample file must get a test here (and is parsed by the ui tests).
    let mut names: Vec<String> =
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/schema"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "alert-clear.json",
            "alert-raise.json",
            "badges.json",
            "panel-event.json",
            "screen-alert-critical.json",
            "screen-alert-failed.json",
            "screen-alert-success.json",
            "screen-alert-warn.json",
            "screen-job.json",
            "screen-list.json",
            "screen-notice.json",
            "screen-number.json",
            "screen-stats.json",
        ]
    );
}
