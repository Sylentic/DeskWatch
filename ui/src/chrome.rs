//! Everything around the page body: header with title and badges, footer with
//! page dots, small icons, and the boot screen.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{
    Circle, Line, PrimitiveStyle, Rectangle, RoundedRectangle, Triangle,
};

use crate::payload::{Badge, Badges, Icon, MAX_BADGES};
use crate::text::{self, Align};
use crate::theme::*;

/// Size of a badge or footer icon.
const ICON: i32 = 16;

fn fill(color: Rgb565) -> PrimitiveStyle<Rgb565> {
    PrimitiveStyle::with_fill(color)
}

fn stroke(color: Rgb565, width: u32) -> PrimitiveStyle<Rgb565> {
    PrimitiveStyle::with_stroke(color, width)
}

/// Draw a 16x16 icon centred on `c`.
pub fn icon<D>(target: &mut D, icon: Icon, c: Point, color: Rgb565) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    match icon {
        Icon::Pr => {
            // Two branches joining: like the usual pull request glyph.
            Line::new(c + Point::new(-4, -5), c + Point::new(-4, 5))
                .into_styled(stroke(color, 2))
                .draw(target)?;
            Line::new(c + Point::new(5, 4), c + Point::new(5, -3))
                .into_styled(stroke(color, 2))
                .draw(target)?;
            Line::new(c + Point::new(5, -3), c + Point::new(1, -6))
                .into_styled(stroke(color, 2))
                .draw(target)?;
            for p in [Point::new(-4, -6), Point::new(-4, 6), Point::new(5, 6)] {
                Circle::with_center(c + p, 6)
                    .into_styled(fill(color))
                    .draw(target)?;
            }
        }
        Icon::Pipeline => {
            Line::new(c + Point::new(-6, 0), c + Point::new(6, 0))
                .into_styled(stroke(color, 2))
                .draw(target)?;
            for x in [-6, 0, 6] {
                Circle::with_center(c + Point::new(x, 0), 6)
                    .into_styled(fill(color))
                    .draw(target)?;
            }
        }
        Icon::Server => {
            for y in [-7, 1] {
                let top_left = c + Point::new(-7, y);
                Rectangle::new(top_left, Size::new(14, 6))
                    .into_styled(stroke(color, 2))
                    .draw(target)?;
                Rectangle::new(top_left + Point::new(9, 2), Size::new(2, 2))
                    .into_styled(fill(color))
                    .draw(target)?;
            }
        }
        Icon::Warn => {
            Triangle::new(
                c + Point::new(0, -7),
                c + Point::new(-8, 7),
                c + Point::new(8, 7),
            )
            .into_styled(fill(color))
            .draw(target)?;
            // The "!" is cut out in the header colour so it reads on any badge.
            Rectangle::new(c + Point::new(-1, -2), Size::new(2, 5))
                .into_styled(fill(HEADER_BG))
                .draw(target)?;
            Rectangle::new(c + Point::new(-1, 4), Size::new(2, 2))
                .into_styled(fill(HEADER_BG))
                .draw(target)?;
        }
        Icon::Home => {
            Triangle::new(
                c + Point::new(0, -8),
                c + Point::new(-8, 0),
                c + Point::new(8, 0),
            )
            .into_styled(fill(color))
            .draw(target)?;
            Rectangle::new(c + Point::new(-5, 0), Size::new(10, 7))
                .into_styled(fill(color))
                .draw(target)?;
        }
        Icon::Dot => {
            Circle::with_center(c, 9)
                .into_styled(fill(color))
                .draw(target)?;
        }
    }
    Ok(())
}

/// Small pin, shown when the user pinned the page.
fn pin<D>(target: &mut D, c: Point, color: Rgb565) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    Circle::with_center(c + Point::new(0, -3), 8)
        .into_styled(fill(color))
        .draw(target)?;
    Line::new(c + Point::new(0, 0), c + Point::new(0, 7))
        .into_styled(stroke(color, 2))
        .draw(target)?;
    Ok(())
}

/// Width of one badge pill.
fn badge_width(badge: &Badge) -> i32 {
    let mut n = text::Buf::new();
    let _ = core::fmt::Write::write_fmt(&mut n, format_args!("{}", badge.count));
    8 + ICON + 6 + text::width(&FONT_BODY_BOLD, &n) + 10
}

/// Header strip: title on the left, badges on the right.
pub fn header<D>(target: &mut D, title: &str, badges: &Badges) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    Rectangle::new(Point::zero(), Size::new(WIDTH, HEADER_H))
        .into_styled(fill(HEADER_BG))
        .draw(target)?;

    // Badges from the right edge inwards, in the order the bridge sent them.
    let shown = &badges.items[..badges.items.len().min(MAX_BADGES)];
    let mut x = WIDTH as i32 - MARGIN + 6;
    for badge in shown.iter().rev() {
        let w = badge_width(badge);
        x -= w + 6;
        let color = status_color(badge.status);
        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(x, 7), Size::new(w as u32, 26)),
            Size::new(13, 13),
        )
        .into_styled(fill(color))
        .draw(target)?;
        icon(
            target,
            badge.icon,
            Point::new(x + 8 + ICON / 2, 20),
            HEADER_BG,
        )?;
        let mut n = text::Buf::new();
        let _ = core::fmt::Write::write_fmt(&mut n, format_args!("{}", badge.count));
        text::draw(
            target,
            &FONT_BODY_BOLD,
            &n,
            Point::new(x + 8 + ICON + 6, 27),
            Align::Left,
            60,
            HEADER_BG,
        )?;
    }

    let title_w = x - MARGIN - 12;
    text::draw(
        target,
        &FONT_BODY_BOLD,
        title,
        Point::new(MARGIN, 27),
        Align::Left,
        title_w,
        TEXT,
    )
}

/// What the footer should say on the right.
pub struct FooterInfo<'a> {
    /// Page name, bottom left.
    pub page: &'a str,
    /// `(n, of)` rotation position for the dots.
    pub position: Option<(u8, u8)>,
    pub pinned: bool,
    pub stale: bool,
    /// A problem worth showing, such as an unsupported schema version.
    pub warning: Option<&'a str>,
    /// The bridge reported `offline` (its Last Will).
    pub bridge_offline: bool,
}

/// Footer strip: page name, rotation dots, pin and stale markers.
pub fn footer<D>(target: &mut D, info: &FooterInfo<'_>) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let top = BODY_BOTTOM;
    let baseline = HEIGHT as i32 - 6;

    if info.bridge_offline {
        // A loud bar: the data on screen is frozen until the bridge is back.
        Rectangle::new(Point::new(0, top), Size::new(WIDTH, FOOTER_H))
            .into_styled(fill(ALERT_RED_BG))
            .draw(target)?;
        return text::draw(
            target,
            &FONT_SMALL,
            "Bridge offline, showing last known data",
            Point::new(WIDTH as i32 / 2, baseline),
            Align::Center,
            WIDTH as i32 - 2 * MARGIN,
            TEXT,
        );
    }

    Rectangle::new(Point::new(0, top), Size::new(WIDTH, FOOTER_H))
        .into_styled(fill(BG))
        .draw(target)?;

    text::draw(
        target,
        &FONT_SMALL,
        info.page,
        Point::new(MARGIN, baseline),
        Align::Left,
        150,
        TEXT_DIM,
    )?;

    if let Some((n, of)) = info.position {
        let of = i32::from(of.min(12));
        let spacing = 14;
        let start = WIDTH as i32 / 2 - (of - 1) * spacing / 2;
        let cy = top + FOOTER_H as i32 / 2;
        for i in 0..of {
            let current = i + 1 == i32::from(n);
            let color = if current { TEXT } else { TRACK };
            Circle::with_center(
                Point::new(start + i * spacing, cy),
                if current { 9 } else { 7 },
            )
            .into_styled(fill(color))
            .draw(target)?;
        }
    }

    // Right side, from the edge inwards: warning, stale, pinned.
    let mut right = WIDTH as i32 - MARGIN;
    if let Some(warning) = info.warning {
        text::draw(
            target,
            &FONT_SMALL,
            warning,
            Point::new(right, baseline),
            Align::Right,
            260,
            AMBER,
        )?;
        right -= text::width(&FONT_SMALL, warning).min(260) + 12;
    }
    if info.stale {
        text::draw(
            target,
            &FONT_SMALL,
            "stale",
            Point::new(right, baseline),
            Align::Right,
            80,
            AMBER,
        )?;
        right -= text::width(&FONT_SMALL, "stale") + 12;
    }
    if info.pinned {
        text::draw(
            target,
            &FONT_SMALL,
            "pinned",
            Point::new(right, baseline),
            Align::Right,
            80,
            TEXT_DIM,
        )?;
        right -= text::width(&FONT_SMALL, "pinned") + 10;
        pin(
            target,
            Point::new(right, top + FOOTER_H as i32 / 2),
            TEXT_DIM,
        )?;
    }
    Ok(())
}

/// Shown until the first page arrives.
pub fn boot<D>(target: &mut D, line: &str) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let cx = WIDTH as i32 / 2;
    text::draw(
        target,
        &FONT_LARGE,
        "DeskWatch",
        Point::new(cx, 150),
        Align::Center,
        440,
        TEXT,
    )?;
    text::draw(
        target,
        &FONT_BODY,
        line,
        Point::new(cx, 190),
        Align::Center,
        440,
        TEXT_DIM,
    )
}
