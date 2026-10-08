//! DeskWatch panel UI: parses the MQTT schema v2 payloads and draws the six
//! page templates on any 480x320 `DrawTarget<Color = Rgb565>`.
//!
//! This crate is `no_std` and has no hardware code. The firmware draws through
//! it onto the ST7796 display, the desktop simulator (`deskwatch-sim`) onto a
//! window, and the snapshot tests onto an in-memory image, so all three show
//! exactly the same pixels.
//!
//! ```ignore
//! let mut panel = Panel::new();
//! if let Some(topic) = Topic::from_topic("deskpanel", topic) {
//!     let update = panel.handle(topic, payload);
//!     if update.redraw {
//!         panel.draw(&mut display, now)?;
//!     }
//! }
//! ```

#![cfg_attr(not(test), no_std)]

pub mod chrome;
pub mod dimmed;
pub mod pages;
pub mod panel;
pub mod payload;
pub mod text;
pub mod theme;

pub use panel::{Led, Panel, Topic, Update};
pub use payload::{Badges, ParseError, Screen};
pub use theme::{HEIGHT, WIDTH};
