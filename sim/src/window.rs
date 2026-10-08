//! The SDL2 window: run mode (live from MQTT) and preview mode (example files).

use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use anyhow::Result;
use deskwatch_ui::{Panel, Topic};
use embedded_graphics_simulator::sdl2::{Keycode, MouseButton};
use embedded_graphics_simulator::{OutputSettings, OutputSettingsBuilder, SimulatorEvent, Window};

use crate::args::Args;
use crate::mqtt::{Incoming, Link, Press};
use crate::{load_screen, new_display, now};

/// Holding the mouse button at least this long is a long press. The firmware
/// will pick its own threshold for the real button.
const LONG_PRESS: Duration = Duration::from_millis(600);

/// Redraw at least this often, so elapsed times and the progress spinner move.
const TICK: Duration = Duration::from_secs(1);

fn settings(args: &Args) -> OutputSettings {
    OutputSettingsBuilder::new()
        .scale(args.scale)
        .pixel_spacing(args.pixel_spacing)
        .build()
}

/// Live mode: draw whatever arrives over MQTT, send button presses back.
pub fn run(args: &Args, link: Link) -> Result<()> {
    let mut display = new_display();
    let mut window = Window::new("DeskWatch panel", &settings(args));
    window.set_max_fps(30);

    let mut panel = Panel::new();
    panel.draw(&mut display, now()).ok();
    let mut last_draw = Instant::now();
    let mut mouse_down: Option<Instant> = None;

    loop {
        let mut redraw = false;
        loop {
            match link.rx.try_recv() {
                Ok(Incoming::Message(topic, payload)) => {
                    let update = panel.handle(topic, &payload);
                    if let Some(e) = panel
                        .last_error()
                        .filter(|_| update.redraw && topic != Topic::BridgeStatus)
                    {
                        eprintln!("{}: {e}", topic.suffix());
                    }
                    if let Some(led) = update.led {
                        println!("led: {led:?}");
                    }
                    redraw |= update.redraw;
                }
                Ok(Incoming::Connected(up)) => {
                    if !up {
                        println!("broker connection lost");
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => anyhow::bail!("MQTT thread stopped"),
            }
        }

        if redraw || last_draw.elapsed() >= TICK {
            panel.draw(&mut display, now()).ok();
            last_draw = Instant::now();
        }
        window.update(&display);

        for event in window.events() {
            match event {
                SimulatorEvent::Quit => return Ok(()),
                SimulatorEvent::MouseButtonDown {
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    mouse_down = Some(Instant::now());
                }
                SimulatorEvent::MouseButtonUp {
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    if let Some(down) = mouse_down.take() {
                        let press = if down.elapsed() >= LONG_PRESS {
                            Press::Long
                        } else {
                            Press::Short
                        };
                        link.press(press);
                    }
                }
                SimulatorEvent::KeyDown {
                    keycode,
                    repeat: false,
                    ..
                } => match keycode {
                    Keycode::Space | Keycode::Return => link.press(Press::Short),
                    Keycode::L => link.press(Press::Long),
                    Keycode::Escape | Keycode::Q => return Ok(()),
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// Preview mode: show each example payload in turn.
pub fn preview(args: &Args, files: &[PathBuf], base: &Panel) -> Result<()> {
    let mut display = new_display();
    let mut window = Window::new("DeskWatch preview", &settings(args));
    window.set_max_fps(30);

    let mut index = 0usize;
    let mut shown = None;
    let mut last_draw = Instant::now();

    loop {
        if shown != Some(index) || last_draw.elapsed() >= TICK {
            if shown != Some(index) {
                println!("[{}/{}] {}", index + 1, files.len(), files[index].display());
            }
            let mut panel = base.clone();
            if let Err(e) = load_screen(&mut panel, &files[index]) {
                eprintln!("{e:#}");
            }
            panel.draw(&mut display, now()).ok();
            shown = Some(index);
            last_draw = Instant::now();
        }
        window.update(&display);

        for event in window.events() {
            match event {
                SimulatorEvent::Quit => return Ok(()),
                SimulatorEvent::MouseButtonUp {
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    index = (index + 1) % files.len();
                }
                SimulatorEvent::KeyDown { keycode, .. } => match keycode {
                    Keycode::Space | Keycode::Right | Keycode::Return => {
                        index = (index + 1) % files.len()
                    }
                    Keycode::Left => index = (index + files.len() - 1) % files.len(),
                    Keycode::Escape | Keycode::Q => return Ok(()),
                    _ => {}
                },
                _ => {}
            }
        }
    }
}
