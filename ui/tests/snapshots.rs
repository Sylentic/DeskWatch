//! PNG snapshot tests.
//!
//! Every payload in `testdata/screens/` is drawn with the badges from
//! `testdata/badges/default.json` and compared pixel by pixel with the
//! approved image in `tests/snapshots/`. A few panel states that need more
//! than one message (boot, bridge offline, bad payload) are built in code.
//!
//! When a layout changes on purpose, re-approve with:
//!
//! ```sh
//! UPDATE_SNAPSHOTS=1 cargo test -p deskwatch-ui
//! ```
//!
//! On a mismatch the new render is written as `<name>.new.png` next to the
//! approved one, so the two can be compared side by side.

use std::fs;
use std::path::{Path, PathBuf};

use deskwatch_ui::{HEIGHT, Panel, Topic, WIDTH};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics_simulator::{OutputSettings, SimulatorDisplay};

/// Fixed clock for elapsed times: 5 min 12 s after the example jobs started.
const NOW: u64 = 1_791_410_712;

fn dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(sub)
}

fn render(panel: &Panel) -> SimulatorDisplay<Rgb565> {
    let mut display = SimulatorDisplay::new(Size::new(WIDTH, HEIGHT));
    panel.draw(&mut display, Some(NOW)).unwrap();
    display
}

/// Compare with the approved snapshot, or write it. Returns an error message
/// instead of panicking so one run reports every changed snapshot.
fn check(name: &str, display: &SimulatorDisplay<Rgb565>) -> Result<(), String> {
    let snapshots = dir("tests/snapshots");
    fs::create_dir_all(&snapshots).unwrap();
    let approved = snapshots.join(format!("{name}.png"));
    let new = snapshots.join(format!("{name}.new.png"));
    let image = display.to_rgb_output_image(&OutputSettings::default());

    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() || !approved.exists() {
        image.save_png(&approved).unwrap();
        let _ = fs::remove_file(&new);
        return Ok(());
    }

    let expected = SimulatorDisplay::<Rgb565>::load_png(&approved).unwrap();
    match expected.diff(display) {
        None => {
            let _ = fs::remove_file(&new);
            Ok(())
        }
        Some(_) => {
            image.save_png(&new).unwrap();
            Err(format!(
                "{name}: differs from snapshot, see {}",
                new.display()
            ))
        }
    }
}

fn panel_with_badges() -> Panel {
    let mut panel = Panel::new();
    let badges = fs::read(dir("testdata/badges/default.json")).unwrap();
    panel.handle(Topic::Badges, &badges);
    panel.handle(Topic::BridgeStatus, b"online");
    panel
}

#[test]
fn snapshots() {
    let mut failures = Vec::new();

    // One snapshot per example payload.
    let mut files: Vec<_> = fs::read_dir(dir("testdata/screens"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no example payloads found");

    for path in &files {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let mut panel = panel_with_badges();
        let update = panel.handle(Topic::Screen, &fs::read(path).unwrap());
        assert!(
            update.redraw,
            "{name}: payload did not parse: {:?}",
            panel.last_error()
        );
        if let Err(e) = check(name, &render(&panel)) {
            failures.push(e);
        }
    }

    // States that need more than one message.
    let stats = fs::read(dir("testdata/screens/stats.json")).unwrap();

    let boot = Panel::new();
    let mut cases = vec![("state_boot", boot)];

    let mut offline = panel_with_badges();
    offline.handle(Topic::Screen, &stats);
    offline.handle(Topic::BridgeStatus, b"offline");
    cases.push(("state_bridge_offline", offline));

    let mut bad = panel_with_badges();
    bad.handle(Topic::Screen, &stats);
    bad.handle(Topic::Screen, br#"{"v":3,"template":"stats","data":{}}"#);
    cases.push(("state_unsupported_version", bad));

    let mut many = Panel::new();
    many.handle(
        Topic::Badges,
        &fs::read(dir("testdata/badges/all_icons.json")).unwrap(),
    );
    many.handle(
        Topic::Screen,
        &fs::read(dir("testdata/screens/number.json")).unwrap(),
    );
    cases.push(("state_all_badge_icons", many));

    for (name, panel) in &cases {
        if let Err(e) = check(name, &render(panel)) {
            failures.push(e);
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
