//! Priority model: what can pre-empt the idle kiosk page (a running job, a
//! failed or successful run, an alert, a notice) and the header badges.
//!
//! These are plain types with no I/O. The kiosk snapshot serialises the
//! screen and the badges as they are; the composer types decide which one is up.

use std::time::Duration;

use serde::{Serialize, Serializer};

/// Value of the `"v"` field in alert messages (`deskpanel/alert`, `POST /api/alert`).
pub const SCHEMA_VERSION: u8 = 2;

/// Most badges the header shows.
pub const MAX_BADGES: usize = 4;

/// Longest string the bridge sends for titles and other short text.
pub const MAX_TITLE_CHARS: usize = 40;

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

/// Screen priority level. A lower number wins.
///
/// The derived `Ord` follows declaration order, so `Level::Job < Level::Notice`
/// and the "best" screen is simply the minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// 0: a critical alert (Home Assistant and other alert sources), red full
    /// screen above running jobs.
    Critical,
    /// 1: a running job, shown with live progress.
    Job,
    /// 2: a failed job (red) or a warning alert (amber), full screen.
    AlertFailed,
    /// 3: a successful job, short green flash.
    AlertSuccess,
    /// 4: a short notice, such as a new PR.
    Notice,
}

impl Level {
    /// The number sent in the `level` field.
    pub fn number(self) -> u8 {
        match self {
            Level::Critical => 0,
            Level::Job => 1,
            Level::AlertFailed => 2,
            Level::AlertSuccess => 3,
            Level::Notice => 4,
        }
    }

    /// How long a screen at this level stays up on its own, or `None` if it
    /// stays until something else ends it (a job finishes).
    ///
    /// Alerts are also cleared by Dismiss. A failed or warning alert
    /// drops to a badge after this time instead of holding the screen; a
    /// critical one stays until it is acknowledged or cleared.
    pub fn max_duration(self) -> Option<Duration> {
        match self {
            Level::Critical | Level::Job => None,
            Level::AlertFailed => Some(Duration::from_secs(10 * 60)),
            Level::AlertSuccess => Some(Duration::from_secs(10)),
            Level::Notice => Some(Duration::from_secs(5)),
        }
    }
}

impl Serialize for Level {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.number())
    }
}

// ---------------------------------------------------------------------------
// The interrupt screen
// ---------------------------------------------------------------------------

/// Which kind of interrupt a screen is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Template {
    Job,
    Alert,
    Notice,
}

/// The data part of a screen. The variant decides the template, so a page can
/// never be sent with data that does not match its layout.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ScreenData {
    Job(JobData),
    Alert(AlertData),
    Notice(NoticeData),
}

impl ScreenData {
    pub fn template(&self) -> Template {
        match self {
            ScreenData::Job(_) => Template::Job,
            ScreenData::Alert(_) => Template::Alert,
            ScreenData::Notice(_) => Template::Notice,
        }
    }
}

/// The screen that is up while something pre-empts the idle page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Screen {
    pub template: Template,
    pub level: Level,
    pub page: String,
    pub data: ScreenData,
}

impl Screen {
    /// Build a screen with the template taken from `data`.
    pub fn new(level: Level, page: impl Into<String>, data: ScreenData) -> Self {
        Self {
            template: data.template(),
            level,
            page: page.into(),
            data,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One GPU of a host with several. `None` values are left out.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GpuData {
    /// Text for the bar: "GPU 0" or the short name chosen on the setup page.
    pub label: String,
    /// Model name when the exporter gives one, for example "GeForce RTX 3060".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<f32>,
}

/// Stats of one machine. `None` values become `null` and the kiosk draws `--`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StatsData {
    pub host: String,
    pub cpu_pct: Option<f32>,
    pub cpu_temp_c: Option<f32>,
    pub ram_pct: Option<f32>,
    pub disk_pct: Option<f32>,
    /// GPU figures are left out of the snapshot when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_mem_pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_temp_c: Option<f32>,
    /// Every GPU of the host, only when it has more than one. The three
    /// `gpu_*` figures above then hold the busiest/hottest of them, so a
    /// reader that knows nothing about this list keeps working.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<GpuData>,
    /// `true` when the host's setup says no line under each GPU bar.
    #[serde(skip_serializing_if = "is_false")]
    pub gpu_plain: bool,
    pub uptime_s: Option<u64>,
    pub net: Option<NetData>,
    /// Pending OS updates and a pending restart, only for hosts that report
    /// them (a Prometheus host with the update metrics).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates: Option<u32>,
    /// How many of `updates` are security updates, when the host says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates_security: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reboot_needed: Option<bool>,
    /// Parts of the host card the setup page switched off for this host
    /// (`config::HOST_SHOW` names). Their values are left empty too.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hide: Vec<String>,
}

/// Network part of the stats, rates in bytes per second.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetData {
    pub iface: String,
    pub rx_bps: Option<u64>,
    pub tx_bps: Option<u64>,
}

/// Build or deploy, as in schema v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Build,
    Deploy,
}

/// A running job.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobData {
    pub source: String,
    pub project: String,
    pub pipeline: String,
    pub kind: JobKind,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub commit: String,
    pub step: Option<String>,
    pub step_no: Option<u32>,
    pub step_count: Option<u32>,
    /// 0.0 to 1.0, or `None` for a spinner.
    pub progress: Option<f32>,
    pub started: u64,
    /// Other jobs running at the same time.
    pub others: u32,
}

/// Outcome shown by an alert screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    /// Red: a failed job or a critical alert.
    Failed,
    /// Amber: a warning alert.
    Warn,
    /// Green: a successful job.
    Success,
}

/// An alert screen. A finished job fills `project`, `pipeline` and
/// `step`; any other alert (Home Assistant, scripts) fills `title` and
/// `message` instead and leaves the CI fields `null`. The kiosk draws
/// whichever pair is present.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlertData {
    pub status: AlertStatus,
    pub source: String,
    pub project: Option<String>,
    pub pipeline: Option<String>,
    pub step: Option<String>,
    /// Left out of the JSON for CI alerts, which have no title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub started: u64,
    /// `None` while the alert is still active, such as an open door.
    pub finished: Option<u64>,
    pub others: u32,
}

/// Colour key for badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RowStatus {
    Ok,
    Running,
    Failed,
    Open,
    Review,
    Neutral,
}

/// A short flash, such as a new PR.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoticeData {
    pub text: String,
    pub sub: String,
    pub source: String,
}

/// Cut `text` to at most `max` characters, never splitting a character.
pub fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ---------------------------------------------------------------------------
// Badges
// ---------------------------------------------------------------------------

/// Icon of a header badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BadgeIcon {
    Pr,
    Pipeline,
    Server,
    Warn,
    /// Active alerts from Home Assistant and other alert sources.
    Home,
}

/// One counter in the header strip.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Badge {
    pub id: String,
    pub icon: BadgeIcon,
    pub count: u32,
    pub status: RowStatus,
}

/// The header badges.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Badges {
    pub items: Vec<Badge>,
}

impl Badges {
    /// Drop zero counts and keep at most `MAX_BADGES`.
    pub fn new(items: impl IntoIterator<Item = Badge>) -> Self {
        let items = items
            .into_iter()
            .filter(|badge| badge.count > 0)
            .take(MAX_BADGES)
            .collect();
        Self { items }
    }
}

// ---------------------------------------------------------------------------
// Composer: which screen is up
// ---------------------------------------------------------------------------

/// A screen that wants to be shown, plus when it was raised. The composer
/// shows the candidate with the best level; among equals the newest wins
/// (`others` counts the rest).
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub level: Level,
    pub page: String,
    pub data: ScreenData,
    /// Unix seconds when this candidate was raised.
    pub raised_at: u64,
}

impl Candidate {
    /// True once a timed level (alerts, notices) has run its course.
    pub fn is_expired(&self, now: u64) -> bool {
        match self.level.max_duration() {
            Some(max) => now.saturating_sub(self.raised_at) >= max.as_secs(),
            None => false,
        }
    }
}

/// Pick the screen to show: best level first, then newest. Expired candidates
/// are ignored. Returns `None` when nothing pre-empts the idle page.
pub fn pick(candidates: &[Candidate], now: u64) -> Option<&Candidate> {
    candidates
        .iter()
        .filter(|c| !c.is_expired(now))
        .min_by(|a, b| {
            a.level
                .cmp(&b.level)
                .then_with(|| b.raised_at.cmp(&a.raised_at))
        })
}

#[cfg(test)]
mod tests {

    use super::*;

    fn notice(text: &str) -> ScreenData {
        ScreenData::Notice(NoticeData {
            text: text.into(),
            sub: String::new(),
            source: "gitea".into(),
        })
    }

    fn candidate(level: Level, page: &str, raised_at: u64) -> Candidate {
        Candidate {
            level,
            page: page.into(),
            data: notice(page),
            raised_at,
        }
    }

    #[test]
    fn levels_order_and_numbers() {
        assert!(Level::Critical < Level::Job);
        assert!(Level::Job < Level::AlertFailed);
        assert!(Level::AlertFailed < Level::AlertSuccess);
        assert!(Level::AlertSuccess < Level::Notice);
        let numbers: Vec<u8> = [
            Level::Critical,
            Level::Job,
            Level::AlertFailed,
            Level::AlertSuccess,
            Level::Notice,
        ]
        .iter()
        .map(|l| l.number())
        .collect();
        assert_eq!(numbers, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn job_screen_uses_ref_key_and_template() {
        let screen = Screen::new(
            Level::Job,
            "gitea-job",
            ScreenData::Job(JobData {
                source: "gitea".into(),
                project: "demo".into(),
                pipeline: "deploy.yml".into(),
                kind: JobKind::Deploy,
                git_ref: "main".into(),
                commit: "a1b2c3d".into(),
                step: Some("Run migrations".into()),
                step_no: Some(3),
                step_count: Some(5),
                progress: None,
                started: 1791410400,
                others: 1,
            }),
        );
        let value = serde_json::to_value(&screen).unwrap();
        assert_eq!(value["template"], "job");
        assert_eq!(value["level"], 1);
        assert_eq!(value["data"]["ref"], "main");
        assert_eq!(value["data"]["kind"], "deploy");
        assert_eq!(value["data"]["progress"], serde_json::Value::Null);
    }

    #[test]
    fn truncate_respects_multibyte_characters() {
        assert_eq!(truncate("ééé", 2), "éé");
    }

    #[test]
    fn badges_drop_zero_counts_and_cap_at_four() {
        let badge = |id: &str, count| Badge {
            id: id.into(),
            icon: BadgeIcon::Pr,
            count,
            status: RowStatus::Open,
        };
        let badges = Badges::new([
            badge("a", 1),
            badge("zero", 0),
            badge("b", 2),
            badge("c", 3),
            badge("d", 4),
            badge("e", 5),
        ]);
        let ids: Vec<&str> = badges.items.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d"]);
        let value = serde_json::to_value(&badges).unwrap();
        assert_eq!(value["items"][0]["icon"], "pr");
    }

    #[test]
    fn pick_prefers_level_then_newest() {
        let candidates = [
            candidate(Level::Notice, "notice", 100),
            candidate(Level::Job, "old-job", 50),
            candidate(Level::Job, "new-job", 90),
            candidate(Level::AlertFailed, "failed", 99),
        ];
        assert_eq!(pick(&candidates, 100).unwrap().page, "new-job");
        assert!(pick(&[], 100).is_none());
    }

    #[test]
    fn timed_levels_expire() {
        let success = candidate(Level::AlertSuccess, "ok", 1000);
        assert!(!success.is_expired(1009));
        assert!(success.is_expired(1010));

        let failed = candidate(Level::AlertFailed, "failed", 1000);
        assert!(!failed.is_expired(1000 + 599));
        assert!(failed.is_expired(1000 + 600));

        // Jobs never expire on their own.
        assert!(!candidate(Level::Job, "job", 0).is_expired(u64::MAX));

        // An expired success flash falls back to a lower-priority notice.
        let candidates = [success, candidate(Level::Notice, "notice", 1008)];
        assert_eq!(pick(&candidates, 1009).unwrap().page, "ok");
        assert_eq!(pick(&candidates, 1010).unwrap().page, "notice");
        assert!(pick(&candidates, 1013).is_none());
    }
}
