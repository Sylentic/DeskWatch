//! DeskWatch desktop simulator.
//!
//! Draws the real panel UI (`deskwatch-ui`) in a 480x320 window and does what
//! the firmware will do: subscribe to the bridge's MQTT topics, redraw on
//! every message, and publish button presses.
//!
//! ```text
//! deskwatch-sim [options]                  connect to MQTT and show the panel
//! deskwatch-sim preview <file|dir>...      flip through example payloads
//! deskwatch-sim render <screen.json> -o out.png [--badges b.json]
//!                                          draw one payload to a PNG, no window
//! ```

mod args;
#[cfg(feature = "sdl")]
mod mqtt;
#[cfg(feature = "sdl")]
mod window;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use deskwatch_ui::{HEIGHT, Panel, Topic, WIDTH};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics_simulator::{OutputSettingsBuilder, SimulatorDisplay};

use crate::args::{Args, Command};

fn main() -> Result<()> {
    let args = args::parse(std::env::args().skip(1))?;
    match &args.command {
        Command::Help => {
            print!("{}", args::HELP);
            Ok(())
        }
        Command::Render { screen, out } => render(&args, screen, out),
        Command::Preview { paths } => preview(&args, paths),
        Command::Run => run(&args),
    }
}

/// Current Unix time, used for elapsed times on the job and alert pages.
pub fn now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

pub fn new_display() -> SimulatorDisplay<Rgb565> {
    SimulatorDisplay::new(Size::new(WIDTH, HEIGHT))
}

/// A panel with the badges from `--badges`, if given, and the bridge online.
fn panel_with_badges(args: &Args) -> Result<Panel> {
    let mut panel = Panel::new();
    if let Some(path) = &args.badges {
        let json = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        panel.handle(Topic::Badges, &json);
        if let Some(e) = panel.last_error() {
            bail!("{}: {e}", path.display());
        }
    }
    panel.handle(Topic::BridgeStatus, b"online");
    Ok(panel)
}

/// Load one screen payload into `panel`, failing on a parse error.
fn load_screen(panel: &mut Panel, path: &Path) -> Result<()> {
    let json = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    panel.handle(Topic::Screen, &json);
    if let Some(e) = panel.last_error() {
        bail!("{}: {e}", path.display());
    }
    Ok(())
}

/// `render`: draw one payload to a PNG at the chosen scale. Needs no display.
fn render(args: &Args, screen: &Path, out: &Path) -> Result<()> {
    let mut panel = panel_with_badges(args)?;
    load_screen(&mut panel, screen)?;
    let mut display = new_display();
    panel
        .draw(&mut display, now())
        .expect("simulator display cannot fail");
    let settings = OutputSettingsBuilder::new()
        .scale(args.scale)
        .pixel_spacing(args.pixel_spacing)
        .build();
    display
        .to_rgb_output_image(&settings)
        .save_png(out)
        .with_context(|| format!("writing {}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Expand directories into their `.json` files, sorted by name.
fn collect_payloads(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut in_dir: Vec<_> = fs::read_dir(path)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect();
            in_dir.sort();
            files.extend(in_dir);
        } else {
            files.push(path.clone());
        }
    }
    if files.is_empty() {
        bail!("no .json payloads found");
    }
    Ok(files)
}

#[cfg(feature = "sdl")]
fn preview(args: &Args, paths: &[PathBuf]) -> Result<()> {
    let files = collect_payloads(paths)?;
    let base = panel_with_badges(args)?;
    window::preview(args, &files, &base)
}

#[cfg(feature = "sdl")]
fn run(args: &Args) -> Result<()> {
    let link = mqtt::connect(args)?;
    window::run(args, link)
}

#[cfg(not(feature = "sdl"))]
fn preview(_: &Args, paths: &[PathBuf]) -> Result<()> {
    collect_payloads(paths)?;
    bail!("built without the `sdl` feature; use `render` instead")
}

#[cfg(not(feature = "sdl"))]
fn run(_: &Args) -> Result<()> {
    bail!("built without the `sdl` feature; use `render` instead")
}
