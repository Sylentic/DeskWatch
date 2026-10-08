//! One draw function per template. Each draws only the body area between the
//! header and the footer.

use core::fmt::Write;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle, RoundedRectangle};

use crate::payload::{
    AlertData, AlertStatus, JobData, JobKind, ListData, NoticeData, NumberData, StatsData,
};
use crate::text::{self, Align, Buf, UNKNOWN};
use crate::theme::*;

const CENTER_X: i32 = WIDTH as i32 / 2;
/// Usable body width inside the margins.
const INNER_W: i32 = WIDTH as i32 - 2 * MARGIN;

fn fill(color: Rgb565) -> PrimitiveStyle<Rgb565> {
    PrimitiveStyle::with_fill(color)
}

/// Clear the body area to `color`.
pub fn clear_body<D>(target: &mut D, color: Rgb565) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    Rectangle::new(
        Point::new(0, BODY_TOP),
        Size::new(WIDTH, (BODY_BOTTOM - BODY_TOP) as u32),
    )
    .into_styled(fill(color))
    .draw(target)
}

/// Horizontal bar: grey track with `fraction` (0..=1) filled in `color`.
fn bar<D>(
    target: &mut D,
    top_left: Point,
    size: Size,
    fraction: f32,
    color: Rgb565,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let radius = Size::new(size.height / 2, size.height / 2);
    RoundedRectangle::with_equal_corners(Rectangle::new(top_left, size), radius)
        .into_styled(fill(TRACK))
        .draw(target)?;
    let fraction = fraction.clamp(0.0, 1.0);
    let filled = (size.width as f32 * fraction) as u32;
    if filled > 0 {
        // Keep the rounded end visible even for tiny values.
        let filled = filled.max(size.height);
        RoundedRectangle::with_equal_corners(
            Rectangle::new(top_left, Size::new(filled, size.height)),
            radius,
        )
        .into_styled(fill(color))
        .draw(target)?;
    }
    Ok(())
}

/// Bar for unknown progress: diagonal stripes. `phase` shifts the stripes so
/// the panel can animate them by redrawing with a new phase.
fn stripes<D>(target: &mut D, top_left: Point, size: Size, phase: u32) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let area = Rectangle::new(top_left, size);
    let period = 24i32;
    let shift = (phase % period as u32) as i32;
    let pixels = area.points().map(|p| {
        let d = (p.x - top_left.x + (p.y - top_left.y) + shift).rem_euclid(period);
        Pixel(p, if d < period / 2 { BLUE } else { TRACK })
    });
    target.draw_iter(pixels)
}

// ---------------------------------------------------------------------------
// stats
// ---------------------------------------------------------------------------

/// One of the three big gauges: label, big value, bar.
fn gauge<D>(
    target: &mut D,
    x: i32,
    label: &str,
    value: Option<f32>,
    warn: f32,
    crit: f32,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let w = 140;
    text::draw(
        target,
        &FONT_SMALL,
        label,
        Point::new(x, 66),
        Align::Left,
        w,
        TEXT_DIM,
    )?;
    let color = value.map_or(TEXT_DIM, |v| threshold_color(v, warn, crit));
    text::draw(
        target,
        &FONT_LARGE,
        &text::pct(value),
        Point::new(x, 106),
        Align::Left,
        w,
        TEXT,
    )?;
    bar(
        target,
        Point::new(x, 118),
        Size::new(w as u32, 10),
        value.unwrap_or(0.0) / 100.0,
        color,
    )
}

/// Small label above a value, used in the lower half of the stats page.
fn cell<D>(
    target: &mut D,
    x: i32,
    y: i32,
    label: &str,
    value: &str,
    color: Rgb565,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let w = 212;
    text::draw(
        target,
        &FONT_SMALL,
        label,
        Point::new(x, y),
        Align::Left,
        w,
        TEXT_DIM,
    )?;
    text::draw(
        target,
        &FONT_TITLE,
        value,
        Point::new(x, y + 30),
        Align::Left,
        w,
        color,
    )
}

pub fn stats<D>(target: &mut D, d: &StatsData) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let mut cpu_label = Buf::new();
    match d.cpu_count {
        Some(n) => write!(cpu_label, "CPU \u{b7} {n} cores").ok(),
        None => write!(cpu_label, "CPU").ok(),
    };
    gauge(target, MARGIN, &cpu_label, d.cpu_pct, 70.0, 90.0)?;
    gauge(target, MARGIN + 154, "MEMORY", d.ram_pct, 80.0, 92.0)?;
    gauge(target, MARGIN + 308, "DISK", d.disk_pct, 80.0, 92.0)?;

    Line::new(
        Point::new(MARGIN, 146),
        Point::new(WIDTH as i32 - MARGIN, 146),
    )
    .into_styled(PrimitiveStyle::with_stroke(TRACK, 1))
    .draw(target)?;

    let left = MARGIN;
    let right = MARGIN + 232;

    // Temperature: amber from 70 C, red from 85 C (typical x86 limits).
    let mut temp = Buf::new();
    let temp_color = match d.cpu_temp_c {
        Some(t) => {
            let _ = write!(temp, "{t:.0}\u{b0}C");
            threshold_color(t, 70.0, 85.0)
        }
        None => {
            let _ = temp.push_str(UNKNOWN);
            TEXT
        }
    };
    cell(target, left, 172, "CPU TEMP", &temp, temp_color)?;

    // Load average, coloured against the number of cores.
    let mut load = Buf::new();
    let load_color = match d.load {
        Some([a, b, c]) => {
            let _ = write!(load, "{a:.2}  {b:.2}  {c:.2}");
            let cores = d.cpu_count.unwrap_or(1).max(1) as f32;
            threshold_color(a / cores, 0.7, 1.0)
        }
        None => {
            let _ = load.push_str(UNKNOWN);
            TEXT
        }
    };
    cell(target, right, 172, "LOAD 1 / 5 / 15 MIN", &load, load_color)?;

    // Network: one line per direction, so gigabit rates still fit the cell.
    let mut net_label = Buf::new();
    let (rx, tx) = match &d.net {
        Some(n) => {
            let _ = write!(net_label, "NETWORK \u{b7} {}", n.iface.as_str());
            (text::rate(n.rx_bps), text::rate(n.tx_bps))
        }
        None => {
            let _ = net_label.push_str("NETWORK");
            (text::rate(None), text::rate(None))
        }
    };
    text::draw(
        target,
        &FONT_SMALL,
        &net_label,
        Point::new(left, 232),
        Align::Left,
        212,
        TEXT_DIM,
    )?;
    for (y, dir, value) in [(256, "in", &rx), (280, "out", &tx)] {
        text::draw(
            target,
            &FONT_SMALL,
            dir,
            Point::new(left, y),
            Align::Left,
            40,
            TEXT_DIM,
        )?;
        text::draw(
            target,
            &FONT_BODY_BOLD,
            value,
            Point::new(left + 40, y),
            Align::Left,
            172,
            TEXT,
        )?;
    }

    let uptime = d.uptime_s.map(text::duration);
    cell(
        target,
        right,
        232,
        "UPTIME",
        uptime.as_ref().map_or(UNKNOWN, |u| u.as_str()),
        TEXT,
    )
}

// ---------------------------------------------------------------------------
// job
// ---------------------------------------------------------------------------

pub fn job<D>(target: &mut D, d: &JobData, now: Option<u64>) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    // Accent line under the header: this screen interrupts rotation.
    Rectangle::new(Point::new(0, BODY_TOP), Size::new(WIDTH, 3))
        .into_styled(fill(BLUE))
        .draw(target)?;

    // Kind chip, then the project name.
    let (kind, chip_color) = match d.kind {
        JobKind::Deploy => ("DEPLOY", AMBER),
        JobKind::Build => ("BUILD", BLUE),
        JobKind::Unknown => ("JOB", GREY),
    };
    let chip_w = text::width(&FONT_BODY_BOLD, kind) + 16;
    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(MARGIN, 56), Size::new(chip_w as u32, 26)),
        Size::new(6, 6),
    )
    .into_styled(fill(chip_color))
    .draw(target)?;
    text::draw(
        target,
        &FONT_BODY_BOLD,
        kind,
        Point::new(MARGIN + 8, 75),
        Align::Left,
        chip_w,
        BG,
    )?;
    let px = MARGIN + chip_w + 12;
    text::draw(
        target,
        &FONT_TITLE,
        d.project.as_str(),
        Point::new(px, 78),
        Align::Left,
        WIDTH as i32 - MARGIN - px,
        TEXT,
    )?;

    // Pipeline, branch and commit on one line.
    let mut line = Buf::new();
    let _ = write!(
        line,
        "{}  \u{b7}  {}",
        d.pipeline.as_str(),
        d.git_ref.as_str()
    );
    if !d.commit.is_empty() {
        let _ = write!(line, " @ {}", d.commit.as_str());
    }
    text::draw(
        target,
        &FONT_BODY,
        &line,
        Point::new(MARGIN, 110),
        Align::Left,
        INNER_W,
        TEXT_DIM,
    )?;

    // Step counter and name.
    let mut counter = Buf::new();
    match (d.step_no, d.step_count) {
        (Some(n), Some(of)) => write!(counter, "STEP {n} OF {of}").ok(),
        (Some(n), None) => write!(counter, "STEP {n}").ok(),
        _ => write!(counter, "STEP").ok(),
    };
    text::draw(
        target,
        &FONT_SMALL,
        &counter,
        Point::new(MARGIN, 152),
        Align::Left,
        200,
        TEXT_DIM,
    )?;

    let mut pct = Buf::new();
    match d.progress {
        Some(p) => write!(pct, "{:.0}%", p.clamp(0.0, 1.0) * 100.0).ok(),
        None => write!(pct, "working").ok(),
    };
    let pct_w = text::width(&FONT_TITLE, &pct);
    let step = d.step.as_ref().map_or(UNKNOWN, |s| s.as_str());
    text::draw(
        target,
        &FONT_TITLE,
        step,
        Point::new(MARGIN, 184),
        Align::Left,
        INNER_W - pct_w - 16,
        TEXT,
    )?;
    text::draw(
        target,
        &FONT_TITLE,
        &pct,
        Point::new(WIDTH as i32 - MARGIN, 184),
        Align::Right,
        pct_w + 1,
        TEXT,
    )?;

    let bar_pos = Point::new(MARGIN, 200);
    let bar_size = Size::new(INNER_W as u32, 24);
    match d.progress {
        Some(p) => bar(target, bar_pos, bar_size, p, BLUE)?,
        None => stripes(
            target,
            bar_pos,
            bar_size,
            (now.unwrap_or(0) % 24) as u32 * 6,
        )?,
    }

    // Elapsed time needs a clock; without one the line is left out.
    if let (Some(now), Some(started)) = (now, d.started) {
        let mut elapsed = Buf::new();
        let _ = write!(
            elapsed,
            "running {}",
            text::duration(now.saturating_sub(started)).as_str()
        );
        text::draw(
            target,
            &FONT_BODY,
            &elapsed,
            Point::new(MARGIN, 262),
            Align::Left,
            240,
            TEXT_DIM,
        )?;
    }
    if d.others > 0 {
        let mut others = Buf::new();
        let plural = if d.others == 1 { "" } else { "s" };
        let _ = write!(others, "+{} more job{plural}", d.others);
        text::draw(
            target,
            &FONT_BODY,
            &others,
            Point::new(WIDTH as i32 - MARGIN, 262),
            Align::Right,
            200,
            TEXT,
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// alert
// ---------------------------------------------------------------------------

/// Background colour and headline for an alert. `level` 0 is critical.
pub fn alert_style(d: &AlertData, level: u8) -> (Rgb565, &'static str) {
    match d.status {
        AlertStatus::Failed if level == 0 => (ALERT_CRITICAL_BG, "CRITICAL"),
        AlertStatus::Warn if level == 0 => (ALERT_CRITICAL_BG, "CRITICAL"),
        AlertStatus::Failed => (ALERT_RED_BG, "FAILED"),
        AlertStatus::Warn => (ALERT_AMBER_BG, "WARNING"),
        AlertStatus::Success => (ALERT_GREEN_BG, "SUCCESS"),
        AlertStatus::Unknown => (ALERT_AMBER_BG, "ALERT"),
    }
}

pub fn alert<D>(target: &mut D, d: &AlertData, level: u8, now: Option<u64>) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let (bg, headline) = alert_style(d, level);
    clear_body(target, bg)?;
    // Light text on the coloured background; "dim" is a soft white.
    let soft = rgb(0xe6, 0xe6, 0xe6);

    if level == 0 {
        // Critical: a white frame so it cannot be mistaken for a normal failure.
        Rectangle::new(
            Point::new(4, BODY_TOP + 4),
            Size::new(WIDTH - 8, (BODY_BOTTOM - BODY_TOP - 8) as u32),
        )
        .into_styled(PrimitiveStyle::with_stroke(TEXT, 4))
        .draw(target)?;
    }

    text::draw(
        target,
        &FONT_HUGE,
        headline,
        Point::new(CENTER_X, 104),
        Align::Center,
        INNER_W,
        TEXT,
    )?;

    // CI alerts fill project/pipeline/step; other alerts fill title/message.
    let main = d.project.as_ref().or(d.title.as_ref()).map(|s| s.as_str());
    let mut detail = Buf::new();
    match (&d.pipeline, &d.step, &d.message) {
        (Some(p), Some(s), _) => write!(detail, "{}  \u{b7}  {}", p.as_str(), s.as_str()).ok(),
        (Some(p), None, _) => write!(detail, "{}", p.as_str()).ok(),
        (None, Some(s), _) => write!(detail, "{}", s.as_str()).ok(),
        (None, None, Some(m)) => write!(detail, "{}", m.as_str()).ok(),
        (None, None, None) => None,
    };
    if let Some(main) = main {
        text::draw(
            target,
            &FONT_TITLE,
            main,
            Point::new(CENTER_X, 152),
            Align::Center,
            INNER_W,
            TEXT,
        )?;
    }
    text::draw(
        target,
        &FONT_BODY,
        &detail,
        Point::new(CENTER_X, 186),
        Align::Center,
        INNER_W,
        soft,
    )?;

    // How long the run took, or how long the alert has been active.
    let mut when = Buf::new();
    match (d.started, d.finished, now) {
        (Some(s), Some(f), _) => write!(
            when,
            "took {}",
            text::duration(f.saturating_sub(s)).as_str()
        )
        .ok(),
        (Some(s), None, Some(now)) => write!(
            when,
            "active for {}",
            text::duration(now.saturating_sub(s)).as_str()
        )
        .ok(),
        _ => None,
    };
    if d.others > 0 {
        let sep = if when.is_empty() { "" } else { "  \u{b7}  " };
        let _ = write!(when, "{sep}+{} more", d.others);
    }
    text::draw(
        target,
        &FONT_BODY,
        &when,
        Point::new(CENTER_X, 220),
        Align::Center,
        INNER_W,
        soft,
    )?;

    if d.status != AlertStatus::Success {
        text::draw(
            target,
            &FONT_SMALL,
            "Press the button to dismiss",
            Point::new(CENTER_X, 276),
            Align::Center,
            INNER_W,
            soft,
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

pub fn list<D>(target: &mut D, d: &ListData) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    if d.rows.is_empty() {
        return text::draw(
            target,
            &FONT_BODY,
            "Nothing here",
            Point::new(CENTER_X, 170),
            Align::Center,
            INNER_W,
            TEXT_DIM,
        );
    }

    let row_h = 51;
    for (i, row) in d.rows.iter().enumerate() {
        let top = BODY_TOP + 4 + i as i32 * row_h;
        let color = status_color(row.status);

        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(MARGIN, top + 6), Size::new(5, 38)),
            Size::new(2, 2),
        )
        .into_styled(fill(color))
        .draw(target)?;

        // Right column first, so the text column knows how much room it has.
        let status = status_label(row.status);
        let right_w = text::width(&FONT_SMALL, status)
            .max(text::width(&FONT_SMALL, row.source.as_str()).min(110));
        let rx = WIDTH as i32 - MARGIN;
        text::draw(
            target,
            &FONT_SMALL,
            status,
            Point::new(rx, top + 22),
            Align::Right,
            110,
            color,
        )?;
        text::draw(
            target,
            &FONT_SMALL,
            row.source.as_str(),
            Point::new(rx, top + 41),
            Align::Right,
            110,
            TEXT_DIM,
        )?;

        let tx = MARGIN + 16;
        let tw = rx - right_w - 16 - tx;
        text::draw(
            target,
            &FONT_BODY_BOLD,
            row.text.as_str(),
            Point::new(tx, top + 23),
            Align::Left,
            tw,
            TEXT,
        )?;
        text::draw(
            target,
            &FONT_SMALL,
            row.sub.as_str(),
            Point::new(tx, top + 42),
            Align::Left,
            tw,
            TEXT_DIM,
        )?;

        if i + 1 < d.rows.len() {
            let y = top + row_h - 1;
            Line::new(Point::new(tx, y), Point::new(rx, y))
                .into_styled(PrimitiveStyle::with_stroke(TRACK, 1))
                .draw(target)?;
        }
    }
    Ok(())
}

fn status_label(status: crate::payload::RowStatus) -> &'static str {
    use crate::payload::RowStatus::*;
    match status {
        Ok => "ok",
        Running => "running",
        Failed => "failed",
        Open => "open",
        Review => "review",
        Neutral => "",
    }
}

// ---------------------------------------------------------------------------
// number and notice
// ---------------------------------------------------------------------------

pub fn number<D>(target: &mut D, d: &NumberData) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    text::draw(
        target,
        &FONT_BODY,
        d.title.as_str(),
        Point::new(CENTER_X, 104),
        Align::Center,
        INNER_W,
        TEXT_DIM,
    )?;
    let value = if d.value.is_empty() {
        UNKNOWN
    } else {
        d.value.as_str()
    };
    text::draw(
        target,
        &FONT_HUGE,
        value,
        Point::new(CENTER_X, 186),
        Align::Center,
        INNER_W,
        TEXT,
    )?;
    text::draw(
        target,
        &FONT_BODY,
        d.sub.as_str(),
        Point::new(CENTER_X, 232),
        Align::Center,
        INNER_W,
        TEXT_DIM,
    )
}

pub fn notice<D>(target: &mut D, d: &NoticeData) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let card = Rectangle::new(Point::new(MARGIN, 66), Size::new(INNER_W as u32, 190));
    RoundedRectangle::with_equal_corners(card, Size::new(10, 10))
        .into_styled(fill(PANEL_BG))
        .draw(target)?;
    Rectangle::new(Point::new(MARGIN, 76), Size::new(5, 170))
        .into_styled(fill(BLUE))
        .draw(target)?;

    let w = INNER_W - 40;
    text::draw(
        target,
        &FONT_LARGE,
        d.text.as_str(),
        Point::new(CENTER_X, 136),
        Align::Center,
        w,
        BLUE,
    )?;
    text::draw(
        target,
        &FONT_BODY_BOLD,
        d.sub.as_str(),
        Point::new(CENTER_X, 184),
        Align::Center,
        w,
        TEXT,
    )?;
    text::draw(
        target,
        &FONT_SMALL,
        d.source.as_str(),
        Point::new(CENTER_X, 224),
        Align::Center,
        w,
        TEXT_DIM,
    )
}

/// For a template this firmware does not know.
pub fn unknown<D>(target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    text::draw(
        target,
        &FONT_TITLE,
        "Unknown page type",
        Point::new(CENTER_X, 160),
        Align::Center,
        INNER_W,
        TEXT,
    )?;
    text::draw(
        target,
        &FONT_BODY,
        "Update the panel firmware",
        Point::new(CENTER_X, 194),
        Align::Center,
        INNER_W,
        TEXT_DIM,
    )
}
