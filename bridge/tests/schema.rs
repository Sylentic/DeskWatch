//! The bridge side of the alert contract in `docs/mqtt-schema.md`.
//!
//! Every sample in `docs/schema/` is checked here against what the bridge
//! accepts, so a change that breaks the documented messages fails a test.

use std::path::Path;

use deskwatch_bridge::alerts::{AlertMessage, Alerts};
use deskwatch_bridge::model::{Level, ScreenData};
use deskwatch_bridge::mqtt::Inbound;

/// Time used for the samples.
const NOW: u64 = 1_791_410_400;

fn sample(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/schema")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn alert_samples_parse() {
    let raise = AlertMessage::parse(sample("alert-raise.json").as_bytes()).unwrap();
    assert_eq!(raise.id.as_deref(), Some("washer"));
    assert_eq!(raise.ttl_s, Some(600));
    let clear = AlertMessage::parse(sample("alert-clear.json").as_bytes()).unwrap();
    assert!(clear.clear);
    assert_eq!(clear.id.as_deref(), Some("washer"));
}

#[test]
fn the_raise_sample_flashes_a_notice_and_the_clear_sample_ends_it() {
    let mut alerts = Alerts::default();
    let raise = AlertMessage::parse(sample("alert-raise.json").as_bytes()).unwrap();
    alerts.apply(raise, NOW);
    let candidates = alerts.candidates(NOW);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].level, Level::Notice);
    assert!(matches!(candidates[0].data, ScreenData::Notice(_)));

    let clear = AlertMessage::parse(sample("alert-clear.json").as_bytes()).unwrap();
    alerts.apply(clear, NOW + 1);
    assert!(alerts.views().is_empty());
}

#[test]
fn the_alert_topic_carries_inbound_alerts() {
    // `Inbound` has one variant: alerts are all the bridge reads from the broker.
    let raise = AlertMessage::parse(sample("alert-raise.json").as_bytes()).unwrap();
    let Inbound::Alert(alert) = Inbound::Alert(raise);
    assert_eq!(alert.id.as_deref(), Some("washer"));
}

#[test]
fn every_sample_is_checked() {
    // A new sample file must get a test here.
    let mut names: Vec<String> =
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/schema"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
    names.sort();
    assert_eq!(names, ["alert-clear.json", "alert-raise.json"]);
}
