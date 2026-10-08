//! The panel side of the MQTT contract in `docs/mqtt-schema.md`.
//!
//! Parses every outbound sample in `docs/schema/` (the payloads the bridge
//! publishes) with the same code the firmware uses. The bridge checks the
//! same files from its side in `bridge/tests/schema.rs`.

use std::path::{Path, PathBuf};

use deskwatch_ui::payload::{AlertStatus, Body, Icon, Template};
use deskwatch_ui::{Badges, Screen};

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/schema")
}

fn screen(name: &str) -> Screen {
    let bytes = std::fs::read(schema_dir().join(name)).unwrap();
    Screen::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn every_screen_sample_parses_with_its_template() {
    let mut count = 0;
    for entry in std::fs::read_dir(schema_dir()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let Some(rest) = name.strip_prefix("screen-") else {
            continue;
        };
        let parsed = screen(&name);
        let template = rest.trim_end_matches(".json").split('-').next().unwrap();
        let expected = match template {
            "stats" => Template::Stats,
            "job" => Template::Job,
            "alert" => Template::Alert,
            "list" => Template::List,
            "number" => Template::Number,
            "notice" => Template::Notice,
            other => panic!("{name}: unknown template {other}"),
        };
        assert_eq!(parsed.template, expected, "{name}");
        assert!(!matches!(parsed.body, Body::Unknown), "{name}");
        count += 1;
    }
    assert_eq!(count, 9);
}

#[test]
fn home_alerts_carry_title_and_level() {
    let critical = screen("screen-alert-critical.json");
    assert_eq!(critical.level, 0);
    let Body::Alert(alert) = critical.body else {
        panic!("expected an alert")
    };
    assert_eq!(alert.status, AlertStatus::Failed);
    assert_eq!(alert.title.unwrap().as_str(), "Water leak");
    assert!(alert.project.is_none());

    let Body::Alert(warn) = screen("screen-alert-warn.json").body else {
        panic!("expected an alert")
    };
    assert_eq!(warn.status, AlertStatus::Warn);
    assert_eq!(warn.message.unwrap().as_str(), "Open for 5 min");
}

#[test]
fn badges_sample_parses() {
    let bytes = std::fs::read(schema_dir().join("badges.json")).unwrap();
    let badges = Badges::parse(&bytes).unwrap();
    let icons: Vec<Icon> = badges.items.iter().map(|b| b.icon).collect();
    assert_eq!(icons, [Icon::Home, Icon::Pipeline, Icon::Pr, Icon::Warn]);
}
