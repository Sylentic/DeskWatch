//! The panel's state: the last screen, badges and bridge status, plus the
//! rules for when to redraw and when to start an LED effect.
//!
//! Firmware and simulator both feed MQTT messages into [`Panel::handle`] and
//! call [`Panel::draw`] when it says something changed.

use core::fmt::Write;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;

use crate::chrome::{self, FooterInfo};
use crate::dimmed::Dimmed;
use crate::pages;
use crate::payload::{AlertStatus, Badges, Body, ParseError, Screen};
use crate::text::Buf;
use crate::theme::BG;

/// The topics the panel subscribes to, without the `deskpanel/` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Screen,
    Badges,
    BridgeStatus,
}

impl Topic {
    /// Topic suffixes, to append to the configured prefix when subscribing.
    pub const ALL: [(Topic, &'static str); 3] = [
        (Topic::Screen, "screen"),
        (Topic::Badges, "badges"),
        (Topic::BridgeStatus, "bridge/status"),
    ];

    /// Match a full topic such as `deskpanel/screen` against `prefix`.
    pub fn from_topic(prefix: &str, topic: &str) -> Option<Topic> {
        let rest = topic.strip_prefix(prefix)?.strip_prefix('/')?;
        Topic::ALL
            .iter()
            .find(|(_, suffix)| *suffix == rest)
            .map(|(t, _)| *t)
    }

    /// Topic suffix, for example `"screen"`.
    pub fn suffix(self) -> &'static str {
        Topic::ALL
            .iter()
            .find(|(t, _)| *t == self)
            .map(|(_, s)| *s)
            .unwrap_or("")
    }
}

/// The onboard RGB LED effect for the current screen (schema section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Led {
    Off,
    /// Slow blue pulse while a job runs.
    JobPulse,
    /// Solid red for a failed alert.
    Red,
    /// Fast red blink for a critical alert.
    RedBlink,
    /// Solid amber for a warning.
    Amber,
    /// Green that fades out after a success.
    GreenFade,
}

impl Led {
    fn for_screen(screen: &Screen) -> Led {
        match &screen.body {
            Body::Job(_) => Led::JobPulse,
            Body::Alert(a) => match (a.status, screen.level) {
                (AlertStatus::Success, _) => Led::GreenFade,
                (_, 0) => Led::RedBlink,
                (AlertStatus::Failed, _) => Led::Red,
                (AlertStatus::Warn | AlertStatus::Unknown, _) => Led::Amber,
            },
            _ => Led::Off,
        }
    }
}

/// What changed after a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Update {
    /// Something on screen changed; call [`Panel::draw`].
    pub redraw: bool,
    /// Start this LED effect. Only set when the effect actually changes, so a
    /// retained message replayed after a reconnect does not restart it.
    pub led: Option<Led>,
}

/// Panel state.
#[derive(Debug, Clone)]
pub struct Panel {
    screen: Option<Screen>,
    /// Hash of the last accepted screen payload, to skip exact repeats.
    screen_hash: u32,
    badges: Badges,
    /// `None` until the bridge status topic has been seen.
    bridge_online: Option<bool>,
    led: Led,
    /// The last payload that could not be parsed, shown in the footer.
    error: Option<ParseError>,
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

/// FNV-1a, enough to spot an identical payload.
fn hash(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
    })
}

impl Panel {
    pub const fn new() -> Self {
        Self {
            screen: None,
            screen_hash: 0,
            badges: Badges {
                v: 2,
                items: heapless::Vec::new(),
            },
            bridge_online: None,
            led: Led::Off,
            error: None,
        }
    }

    pub fn screen(&self) -> Option<&Screen> {
        self.screen.as_ref()
    }

    pub fn badges(&self) -> &Badges {
        &self.badges
    }

    pub fn last_error(&self) -> Option<ParseError> {
        self.error
    }

    /// Feed one MQTT message. Bad payloads keep the previous state and are
    /// remembered so the footer can show them.
    pub fn handle(&mut self, topic: Topic, payload: &[u8]) -> Update {
        match topic {
            Topic::Screen => {
                let h = hash(payload);
                if self.screen.is_some() && h == self.screen_hash {
                    // Same payload again, e.g. a retained replay after reconnect.
                    return Update::default();
                }
                match Screen::parse(payload) {
                    Ok(screen) => {
                        let led = Led::for_screen(&screen);
                        let led_change = (led != self.led).then_some(led);
                        self.led = led;
                        self.screen = Some(screen);
                        self.screen_hash = h;
                        self.error = None;
                        Update {
                            redraw: true,
                            led: led_change,
                        }
                    }
                    Err(e) => self.reject(e),
                }
            }
            Topic::Badges => match Badges::parse(payload) {
                Ok(badges) => {
                    let changed = badges != self.badges;
                    self.badges = badges;
                    Update {
                        redraw: changed,
                        led: None,
                    }
                }
                Err(e) => self.reject(e),
            },
            Topic::BridgeStatus => {
                let online = match payload {
                    b"online" => true,
                    b"offline" => false,
                    _ => return Update::default(),
                };
                let changed = self.bridge_online != Some(online);
                self.bridge_online = Some(online);
                Update {
                    redraw: changed,
                    led: None,
                }
            }
        }
    }

    fn reject(&mut self, e: ParseError) -> Update {
        let changed = self.error != Some(e);
        self.error = Some(e);
        Update {
            redraw: changed,
            led: None,
        }
    }

    /// Draw the whole frame. `now` is Unix time in seconds if the device has a
    /// clock (SNTP on the board); without it, elapsed times are left out.
    pub fn draw<D>(&self, target: &mut D, now: Option<u64>) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        target.clear(BG)?;

        let mut warning = Buf::new();
        if let Some(e) = self.error {
            let _ = write!(warning, "{e}");
        }
        let warning = (!warning.is_empty()).then_some(warning.as_str());
        let bridge_offline = self.bridge_online == Some(false);

        let Some(screen) = &self.screen else {
            chrome::header(target, "", &self.badges)?;
            let line = if bridge_offline {
                "Bridge is offline"
            } else {
                "Waiting for the bridge"
            };
            chrome::boot(target, warning.unwrap_or(line))?;
            return Ok(());
        };

        let title = title(screen);
        chrome::header(target, &title, &self.badges)?;

        // Stale or bridge offline: same drawing, greyed out.
        if screen.stale || bridge_offline {
            draw_body(&mut Dimmed(target), screen, now)?;
        } else {
            draw_body(target, screen, now)?;
        }

        chrome::footer(
            target,
            &FooterInfo {
                page: screen.page.as_str(),
                position: screen.position,
                pinned: screen.pinned,
                stale: screen.stale,
                warning,
                bridge_offline,
            },
        )
    }
}

fn draw_body<D>(target: &mut D, screen: &Screen, now: Option<u64>) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    match &screen.body {
        Body::Stats(d) => pages::stats(target, d),
        Body::Job(d) => pages::job(target, d, now),
        Body::Alert(d) => pages::alert(target, d, screen.level, now),
        Body::List(d) => pages::list(target, d),
        Body::Number(d) => pages::number(target, d),
        Body::Notice(d) => pages::notice(target, d),
        Body::Unknown => pages::unknown(target),
    }
}

/// Header title for a screen.
fn title(screen: &Screen) -> Buf {
    let mut t = Buf::new();
    let _ = match &screen.body {
        Body::Stats(d) if !d.host.is_empty() => write!(t, "{}", d.host.as_str()),
        Body::Stats(_) => write!(t, "Server"),
        Body::Job(d) => write!(t, "Running on {}", d.source.as_str()),
        Body::Alert(d) => write!(t, "Alert from {}", d.source.as_str()),
        Body::List(d) => write!(t, "{} ({})", d.title.as_str(), d.count),
        Body::Number(_) | Body::Notice(_) | Body::Unknown => write!(t, "{}", screen.page.as_str()),
    };
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: &[u8] =
        br#"{"v":2,"seq":1,"template":"job","level":1,"page":"j","data":{"project":"p"}}"#;
    const STATS: &[u8] =
        br#"{"v":2,"seq":2,"template":"stats","level":5,"page":"stats","data":{"host":"h"}}"#;

    #[test]
    fn topics_match_prefix() {
        assert_eq!(
            Topic::from_topic("deskpanel", "deskpanel/screen"),
            Some(Topic::Screen)
        );
        assert_eq!(
            Topic::from_topic("deskpanel-test", "deskpanel-test/bridge/status"),
            Some(Topic::BridgeStatus)
        );
        assert_eq!(
            Topic::from_topic("deskpanel", "deskpanel/panel/event"),
            None
        );
        assert_eq!(Topic::from_topic("deskpanel", "deskpanelx/screen"), None);
    }

    #[test]
    fn repeat_payload_is_ignored_and_led_fires_once() {
        let mut p = Panel::new();
        assert_eq!(
            p.handle(Topic::Screen, JOB),
            Update {
                redraw: true,
                led: Some(Led::JobPulse)
            }
        );
        assert_eq!(p.handle(Topic::Screen, JOB), Update::default());
        assert_eq!(
            p.handle(Topic::Screen, STATS),
            Update {
                redraw: true,
                led: Some(Led::Off)
            }
        );
    }

    #[test]
    fn bad_payload_keeps_last_screen() {
        let mut p = Panel::new();
        p.handle(Topic::Screen, STATS);
        let u = p.handle(Topic::Screen, b"{broken");
        assert!(u.redraw);
        assert_eq!(p.last_error(), Some(ParseError::Json));
        assert_eq!(p.screen().map(|s| s.seq), Some(2));
    }

    #[test]
    fn bridge_status_redraws_only_on_change() {
        let mut p = Panel::new();
        assert!(p.handle(Topic::BridgeStatus, b"offline").redraw);
        assert!(!p.handle(Topic::BridgeStatus, b"offline").redraw);
        assert!(p.handle(Topic::BridgeStatus, b"online").redraw);
    }
}
