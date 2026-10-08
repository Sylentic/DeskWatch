//! Text drawing and number formatting helpers.

use core::fmt::Write;

use embedded_graphics::prelude::*;
use heapless::String;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

pub use u8g2_fonts::types::HorizontalAlignment as Align;

/// Scratch string for formatted values.
pub type Buf = String<64>;

/// Width in pixels of `s` in `font`.
pub fn width(font: &FontRenderer, s: &str) -> i32 {
    font.get_rendered_dimensions(s, Point::zero(), VerticalPosition::Baseline)
        .map(|d| d.advance.x)
        .unwrap_or(0)
}

/// `s` if it fits in `max_w` pixels, otherwise as many characters as fit
/// followed by `..`. ASCII dots, so every font has them.
pub fn fit(font: &FontRenderer, s: &str, max_w: i32) -> String<136> {
    let mut out: String<136> = String::new();
    if width(font, s) <= max_w {
        let _ = out.push_str(s);
        return out;
    }
    let budget = max_w - width(font, "..");
    for ch in s.chars() {
        let mut probe = out.clone();
        if probe.push(ch).is_err() || width(font, &probe) > budget {
            break;
        }
        out = probe;
    }
    // Do not end on a space before the dots.
    while out.ends_with(' ') {
        out.pop();
    }
    let _ = out.push_str("..");
    out
}

/// Draw `s` with its baseline at `pos.y`, aligned around `pos.x`, cut to
/// `max_w` pixels.
pub fn draw<D>(
    target: &mut D,
    font: &FontRenderer,
    s: &str,
    pos: Point,
    align: HorizontalAlignment,
    max_w: i32,
    color: D::Color,
) -> Result<(), D::Error>
where
    D: DrawTarget,
{
    let s = fit(font, s, max_w);
    match font.render_aligned(
        s.as_str(),
        pos,
        VerticalPosition::Baseline,
        align,
        FontColor::Transparent(color),
        target,
    ) {
        Ok(_) => Ok(()),
        Err(u8g2_fonts::Error::DisplayError(e)) => Err(e),
        // Glyph errors are switched off on our fonts; nothing else to report.
        Err(_) => Ok(()),
    }
}

/// `"--"` for unknown values, as the schema asks.
pub const UNKNOWN: &str = "--";

/// `12.6` -> `"13%"`.
pub fn pct(value: Option<f32>) -> Buf {
    let mut s = Buf::new();
    match value {
        Some(v) => {
            let _ = write!(s, "{:.0}%", v);
        }
        None => {
            let _ = s.push_str(UNKNOWN);
        }
    }
    s
}

/// Bytes per second as bits per second with a unit, e.g. `"1.0 Mb/s"`.
pub fn rate(bytes_per_s: Option<u64>) -> Buf {
    let mut s = Buf::new();
    let Some(bps) = bytes_per_s else {
        let _ = s.push_str(UNKNOWN);
        return s;
    };
    let bits = bps as f32 * 8.0;
    // One decimal below 100 of a unit, none above, so values stay short.
    let _ = if bits >= 1e9 {
        write!(s, "{:.1} Gb/s", bits / 1e9)
    } else if bits >= 1e8 {
        write!(s, "{:.0} Mb/s", bits / 1e6)
    } else if bits >= 1e6 {
        write!(s, "{:.1} Mb/s", bits / 1e6)
    } else if bits >= 1e3 {
        write!(s, "{:.0} kb/s", bits / 1e3)
    } else {
        write!(s, "{:.0} b/s", bits)
    };
    s
}

/// Seconds as the two largest units: `"14d 6h"`, `"3h 25m"`, `"5m 12s"`.
pub fn duration(secs: u64) -> Buf {
    let mut s = Buf::new();
    let (d, h, m, sec) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    let _ = if d > 0 {
        write!(s, "{d}d {h}h")
    } else if h > 0 {
        write!(s, "{h}h {m}m")
    } else if m > 0 {
        write!(s, "{m}m {sec}s")
    } else {
        write!(s, "{sec}s")
    };
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::FONT_BODY;

    #[test]
    fn formats() {
        assert_eq!(pct(Some(12.6)).as_str(), "13%");
        assert_eq!(pct(None).as_str(), "--");
        assert_eq!(rate(Some(125_000)).as_str(), "1.0 Mb/s");
        assert_eq!(rate(Some(23_000)).as_str(), "184 kb/s");
        assert_eq!(rate(Some(118_000_000)).as_str(), "944 Mb/s");
        assert_eq!(duration(1_234_567).as_str(), "14d 6h");
        assert_eq!(duration(312).as_str(), "5m 12s");
    }

    #[test]
    fn fit_truncates_with_dots() {
        let long = "a very long pull request title that will not fit";
        let cut = fit(&FONT_BODY, long, 120);
        assert!(cut.ends_with(".."));
        assert!(width(&FONT_BODY, &cut) <= 120);
        assert_eq!(fit(&FONT_BODY, "short", 120).as_str(), "short");
    }
}
