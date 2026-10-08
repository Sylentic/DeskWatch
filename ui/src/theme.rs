//! Colours, fonts and fixed layout numbers for the 480x320 landscape panel.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use u8g2_fonts::{FontRenderer, fonts};

use crate::payload::RowStatus;

/// Panel width in landscape.
pub const WIDTH: u32 = 480;
/// Panel height in landscape.
pub const HEIGHT: u32 = 320;
/// Header strip with the page title and badges.
pub const HEADER_H: u32 = 40;
/// Footer strip with page dots.
pub const FOOTER_H: u32 = 22;
/// Left and right margin of the body.
pub const MARGIN: i32 = 16;

/// Top of the body area.
pub const BODY_TOP: i32 = HEADER_H as i32;
/// Bottom of the body area (exclusive).
pub const BODY_BOTTOM: i32 = (HEIGHT - FOOTER_H) as i32;

/// Build an Rgb565 colour from 8-bit channels, so the palette reads like CSS.
pub const fn rgb(r: u8, g: u8, b: u8) -> Rgb565 {
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}

// Base palette: dark background, light text.
pub const BG: Rgb565 = rgb(0x10, 0x13, 0x1a);
pub const HEADER_BG: Rgb565 = rgb(0x1c, 0x21, 0x2b);
pub const PANEL_BG: Rgb565 = rgb(0x22, 0x28, 0x34);
pub const TEXT: Rgb565 = rgb(0xf2, 0xf4, 0xf8);
pub const TEXT_DIM: Rgb565 = rgb(0x9a, 0xa3, 0xb2);
pub const TRACK: Rgb565 = rgb(0x33, 0x3a, 0x48);

// Status colours.
pub const GREEN: Rgb565 = rgb(0x3d, 0xd6, 0x8c);
pub const AMBER: Rgb565 = rgb(0xff, 0xb0, 0x20);
pub const RED: Rgb565 = rgb(0xff, 0x4d, 0x4d);
pub const BLUE: Rgb565 = rgb(0x4d, 0xa3, 0xff);
pub const PURPLE: Rgb565 = rgb(0xb0, 0x7c, 0xff);
pub const GREY: Rgb565 = rgb(0x8a, 0x93, 0xa2);

// Full-screen alert backgrounds: darker than the status colours so white text
// stays readable.
pub const ALERT_RED_BG: Rgb565 = rgb(0x9e, 0x16, 0x16);
pub const ALERT_CRITICAL_BG: Rgb565 = rgb(0xc8, 0x00, 0x00);
pub const ALERT_AMBER_BG: Rgb565 = rgb(0x9a, 0x5c, 0x00);
pub const ALERT_GREEN_BG: Rgb565 = rgb(0x12, 0x7a, 0x45);

/// Colour for a list row or badge status.
pub fn status_color(status: RowStatus) -> Rgb565 {
    match status {
        RowStatus::Ok => GREEN,
        RowStatus::Running => BLUE,
        RowStatus::Failed => RED,
        RowStatus::Open => PURPLE,
        RowStatus::Review => AMBER,
        RowStatus::Neutral => GREY,
    }
}

/// Green below `warn`, amber up to `crit`, red above. Used for gauges.
pub fn threshold_color(value: f32, warn: f32, crit: f32) -> Rgb565 {
    if value >= crit {
        RED
    } else if value >= warn {
        AMBER
    } else {
        GREEN
    }
}

/// Grey version of a colour, used to dim stale content. Keeps brightness so
/// the layout stays readable, and halves it so it clearly looks inactive.
pub fn dimmed(color: Rgb565) -> Rgb565 {
    // Scale every channel to 0..=255 first; Rgb565 has 6 bits of green.
    let r = u32::from(color.r()) * 255 / 31;
    let g = u32::from(color.g()) * 255 / 63;
    let b = u32::from(color.b()) * 255 / 31;
    let luma = (r * 3 + g * 6 + b) / 10;
    let v = (luma / 2 + 0x18) as u8;
    rgb(v, v, v)
}

// Fonts. FreeUniversal (SIL Open Font License) from the u8g2 project; the
// `_tf` variants include Latin-1, so names like "Zürich" and the degree sign
// render. Unknown characters are skipped instead of failing the draw.
pub const FONT_SMALL: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_fur11_tf>().with_ignore_unknown_chars(true);
pub const FONT_BODY: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_fur14_tf>().with_ignore_unknown_chars(true);
pub const FONT_BODY_BOLD: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_fub14_tf>().with_ignore_unknown_chars(true);
pub const FONT_TITLE: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_fub20_tf>().with_ignore_unknown_chars(true);
pub const FONT_LARGE: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_fub30_tf>().with_ignore_unknown_chars(true);
pub const FONT_HUGE: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_fub42_tf>().with_ignore_unknown_chars(true);
