//! Page and priority model, following MQTT schema v2.
//!
//! These are plain types with no I/O. The payload structs serialise to exactly
//! the JSON the panel expects on `deskpanel/screen` and `deskpanel/badges`, and
//! the composer types decide which page is up.

use std::time::Duration;

use serde::{Deserialize, Serialize, Serializer};

/// Value of the `"v"` field in every payload.
pub const SCHEMA_VERSION: u8 = 2;

/// Most badges the panel header shows.
pub const MAX_BADGES: usize = 4;

/// Longest string the bridge sends for titles and other short text.
pub const MAX_TITLE_CHARS: usize = 40;

/// Most rows the bridge sends in a `list` page.
pub const MAX_LIST_ROWS: usize = 5;

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

/// Screen priority level from schema section 3. A lower number wins.
///
/// The derived `Ord` follows declaration order, so `Level::Job < Level::Rotation`
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
    /// 5: idle rotation.
    Rotation,
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
            Level::Rotation => 5,
        }
    }

    /// How long a screen at this level stays up on its own, or `None` if it
    /// stays until something else ends it (job finishes, rotation loops).
    ///
    /// Alerts are also cleared by a button press. A failed or warning alert
    /// drops to a badge after this time instead of holding the screen; a
    /// critical one stays until it is acknowledged or cleared.
    pub fn max_duration(self) -> Option<Duration> {
        match self {
            Level::Critical | Level::Job | Level::Rotation => None,
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
// deskpanel/screen
// ---------------------------------------------------------------------------

/// Layout the panel draws. The only field the firmware switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Template {
    Stats,
    Job,
    Alert,
    List,
    Number,
    Notice,
}

/// The data part of a screen. The variant decides the template, so a page can
/// never be sent with data that does not match its layout.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ScreenData {
    Stats(StatsData),
    Job(JobData),
    Alert(AlertData),
    List(ListData),
    Number(NumberData),
    Notice(NoticeData),
}

impl ScreenData {
    pub fn template(&self) -> Template {
        match self {
            ScreenData::Stats(_) => Template::Stats,
            ScreenData::Job(_) => Template::Job,
            ScreenData::Alert(_) => Template::Alert,
            ScreenData::List(_) => Template::List,
            ScreenData::Number(_) => Template::Number,
            ScreenData::Notice(_) => Template::Notice,
        }
    }

    /// True when the page has nothing worth showing, used by `skip_when_empty`.
    pub fn is_empty(&self) -> bool {
        match self {
            ScreenData::List(list) => list.count == 0,
            _ => false,
        }
    }
}

/// Full `deskpanel/screen` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Screen {
    pub v: u8,
    /// Increments on every publish so the panel can spot duplicates.
    pub seq: u64,
    pub template: Template,
    pub level: Level,
    pub page: String,
    pub pinned: bool,
    /// `[n, of]` while rotating, `None` during interrupts.
    pub position: Option<[u8; 2]>,
    pub stale: bool,
    pub data: ScreenData,
}

impl Screen {
    /// Build a screen with the template taken from `data`. `seq` is filled in
    /// by the publisher.
    pub fn new(level: Level, page: impl Into<String>, data: ScreenData) -> Self {
        Self {
            v: SCHEMA_VERSION,
            seq: 0,
            template: data.template(),
            level,
            page: page.into(),
            pinned: false,
            position: None,
            stale: false,
            data,
        }
    }
}

/// `stats` template data. `None` values become `null` and the panel draws `--`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StatsData {
    pub host: String,
    pub cpu_pct: Option<f32>,
    pub cpu_temp_c: Option<f32>,
    pub load: Option<[f32; 3]>,
    pub cpu_count: Option<u32>,
    pub ram_pct: Option<f32>,
    pub disk_pct: Option<f32>,
    pub uptime_s: Option<u64>,
    pub net: Option<NetData>,
}

/// Network part of the stats page, rates in bytes per second.
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

/// `job` template data: a running job.
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

/// Outcome shown by the `alert` template.
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

/// `alert` template data. A finished job fills `project`, `pipeline` and
/// `step`; any other alert (Home Assistant, scripts) fills `title` and
/// `message` instead and leaves the CI fields `null`. The panel draws
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

/// Colour key for list rows and badges.
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

/// `list` template data.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListData {
    pub title: String,
    /// Total number of items, which can be more than the rows sent.
    pub count: u32,
    pub rows: Vec<ListRow>,
}

impl ListData {
    /// Build a list, truncating the title and keeping at most `MAX_LIST_ROWS` rows.
    pub fn new(title: &str, count: u32, mut rows: Vec<ListRow>) -> Self {
        rows.truncate(MAX_LIST_ROWS);
        Self {
            title: truncate(title, MAX_TITLE_CHARS),
            count,
            rows,
        }
    }
}

/// One row of a `list` page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListRow {
    pub text: String,
    pub sub: String,
    pub status: RowStatus,
    pub source: String,
}

/// `number` template data: one big figure.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NumberData {
    pub title: String,
    pub value: String,
    pub sub: String,
}

/// `notice` template data: a short flash.
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
// deskpanel/badges
// ---------------------------------------------------------------------------

/// Icon set built into the firmware. Unknown icons fall back to a dot there.
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

/// Full `deskpanel/badges` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Badges {
    pub v: u8,
    pub items: Vec<Badge>,
}

impl Badges {
    /// Apply the schema rules: drop zero counts and keep at most `MAX_BADGES`.
    pub fn new(items: impl IntoIterator<Item = Badge>) -> Self {
        let items = items
            .into_iter()
            .filter(|badge| badge.count > 0)
            .take(MAX_BADGES)
            .collect();
        Self {
            v: SCHEMA_VERSION,
            items,
        }
    }
}

// ---------------------------------------------------------------------------
// deskpanel/panel/event
// ---------------------------------------------------------------------------

/// Button press length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ButtonAction {
    Short,
    Long,
}

/// A `deskpanel/panel/event` payload. Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PanelEvent {
    pub v: u8,
    pub event: String,
    pub action: ButtonAction,
}

// ---------------------------------------------------------------------------
// Composer: which screen is up
// ---------------------------------------------------------------------------

/// A screen that wants to be shown, plus when it was raised. The composer
/// shows the candidate with the best level; among equals the newest wins
/// (schema: "the newest one is shown, `others` counts the rest").
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
/// are ignored. Returns `None` when nothing pre-empts rotation.
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

/// One `[[rotation]]` block from the config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationEntry {
    pub page: String,
    pub dwell_s: u64,
    #[serde(default)]
    pub skip_when_empty: bool,
}

/// Idle rotation state: which page is up and whether it is pinned.
#[derive(Debug, Clone)]
pub struct Rotation {
    entries: Vec<RotationEntry>,
    current: usize,
    pinned: bool,
}

impl Rotation {
    /// Start at the first page (the stats page in a normal config).
    pub fn new(entries: Vec<RotationEntry>) -> Self {
        assert!(!entries.is_empty(), "rotation needs at least one page");
        Self {
            entries,
            current: 0,
            pinned: false,
        }
    }

    pub fn current(&self) -> &RotationEntry {
        &self.entries[self.current]
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// Every page name in the rotation, in order.
    pub fn pages(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.page.as_str())
    }

    /// Long press: pin or unpin the current page.
    pub fn toggle_pin(&mut self) {
        self.pinned = !self.pinned;
    }

    /// Back to the first page, used when an interrupt ends.
    pub fn reset(&mut self) {
        self.current = 0;
    }

    /// Move to the next page that has something to show, and return it.
    /// `is_empty(page)` says whether a page has nothing to show right now; it
    /// only matters for entries with `skip_when_empty`. When pinned, or when
    /// every other page is skipped, the current page stays.
    pub fn advance(&mut self, is_empty: impl Fn(&str) -> bool) -> &RotationEntry {
        if !self.pinned {
            let len = self.entries.len();
            for step in 1..=len {
                let index = (self.current + step) % len;
                let entry = &self.entries[index];
                if !(entry.skip_when_empty && is_empty(&entry.page)) {
                    self.current = index;
                    break;
                }
            }
        }
        self.current()
    }

    /// `[n, of]` for the page dots, counting only pages that are not skipped.
    /// `None` if the current page itself would be skipped.
    pub fn position(&self, is_empty: impl Fn(&str) -> bool) -> Option<[u8; 2]> {
        let shown = |e: &RotationEntry| !(e.skip_when_empty && is_empty(&e.page));
        if !shown(self.current()) {
            return None;
        }
        let before = self.entries[..self.current]
            .iter()
            .filter(|e| shown(e))
            .count();
        let total = self.entries.iter().filter(|e| shown(e)).count();
        Some([
            u8::try_from(before + 1).unwrap_or(u8::MAX),
            u8::try_from(total).unwrap_or(u8::MAX),
        ])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

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

    fn entry(page: &str, skip_when_empty: bool) -> RotationEntry {
        RotationEntry {
            page: page.into(),
            dwell_s: 10,
            skip_when_empty,
        }
    }

    #[test]
    fn levels_order_and_numbers() {
        assert!(Level::Critical < Level::Job);
        assert!(Level::Job < Level::AlertFailed);
        assert!(Level::AlertFailed < Level::AlertSuccess);
        assert!(Level::AlertSuccess < Level::Notice);
        assert!(Level::Notice < Level::Rotation);
        let numbers: Vec<u8> = [
            Level::Critical,
            Level::Job,
            Level::AlertFailed,
            Level::AlertSuccess,
            Level::Notice,
            Level::Rotation,
        ]
        .iter()
        .map(|l| l.number())
        .collect();
        assert_eq!(numbers, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn stats_screen_matches_schema() {
        let mut screen = Screen::new(
            Level::Rotation,
            "stats",
            ScreenData::Stats(StatsData {
                host: "homeserver".into(),
                cpu_pct: Some(12.5),
                cpu_temp_c: None,
                load: Some([0.5, 0.25, 0.125]),
                cpu_count: Some(4),
                ram_pct: Some(41.0),
                disk_pct: Some(63.0),
                uptime_s: Some(1234567),
                net: Some(NetData {
                    iface: "eth0".into(),
                    rx_bps: Some(125000),
                    tx_bps: None,
                }),
            }),
        );
        screen.seq = 7;
        screen.position = Some([1, 3]);

        let expected = json!({
            "v": 2, "seq": 7, "template": "stats", "level": 5, "page": "stats",
            "pinned": false, "position": [1, 3], "stale": false,
            "data": {
                "host": "homeserver", "cpu_pct": 12.5, "cpu_temp_c": null,
                "load": [0.5, 0.25, 0.125], "cpu_count": 4, "ram_pct": 41.0,
                "disk_pct": 63.0, "uptime_s": 1234567,
                "net": { "iface": "eth0", "rx_bps": 125000, "tx_bps": null }
            }
        });
        assert_eq!(serde_json::to_value(&screen).unwrap(), expected);
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
        assert_eq!(value["position"], serde_json::Value::Null);
        assert_eq!(value["data"]["ref"], "main");
        assert_eq!(value["data"]["kind"], "deploy");
        assert_eq!(value["data"]["progress"], serde_json::Value::Null);
    }

    #[test]
    fn list_is_truncated_and_empty_when_count_is_zero() {
        let row = ListRow {
            text: "fix login redirect".into(),
            sub: "demo #42".into(),
            status: RowStatus::Open,
            source: "gitea".into(),
        };
        let list = ListData::new(&"x".repeat(60), 9, vec![row; 8]);
        assert_eq!(list.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(list.rows.len(), MAX_LIST_ROWS);
        assert_eq!(list.count, 9);
        assert!(!ScreenData::List(list).is_empty());
        assert!(ScreenData::List(ListData::new("Open PRs", 0, vec![])).is_empty());
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
        assert_eq!(value["v"], 2);
        assert_eq!(value["items"][0]["icon"], "pr");
    }

    #[test]
    fn panel_event_parses_and_ignores_unknown_fields() {
        let event: PanelEvent =
            serde_json::from_str(r#"{"v":2,"event":"button","action":"long","extra":1}"#).unwrap();
        assert_eq!(event.action, ButtonAction::Long);
        assert!(
            serde_json::from_str::<PanelEvent>(r#"{"v":2,"event":"button","action":"double"}"#)
                .is_err()
        );
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

    #[test]
    fn rotation_skips_empty_pages() {
        let mut rotation = Rotation::new(vec![
            entry("stats", false),
            entry("prs", true),
            entry("pipelines", true),
        ]);
        let prs_empty = |page: &str| page == "prs";
        assert_eq!(rotation.advance(prs_empty).page, "pipelines");
        assert_eq!(rotation.position(prs_empty), Some([2, 2]));
        assert_eq!(rotation.advance(prs_empty).page, "stats");
        assert_eq!(rotation.position(prs_empty), Some([1, 2]));

        // A quiet day: only the stats page is left.
        let all_empty = |_: &str| true;
        assert_eq!(rotation.advance(all_empty).page, "stats");
        assert_eq!(rotation.position(all_empty), Some([1, 1]));
    }

    #[test]
    fn rotation_pin_and_reset() {
        let mut rotation = Rotation::new(vec![entry("stats", false), entry("prs", false)]);
        rotation.advance(|_| false);
        rotation.toggle_pin();
        assert!(rotation.is_pinned());
        assert_eq!(rotation.advance(|_| false).page, "prs");
        rotation.toggle_pin();
        assert_eq!(rotation.advance(|_| false).page, "stats");
        rotation.advance(|_| false);
        rotation.reset();
        assert_eq!(rotation.current().page, "stats");
    }
}
