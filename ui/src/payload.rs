//! Panel-side payload types for MQTT schema v2 (`docs/mqtt-schema.md`).
//!
//! Everything here is `no_std` and heap free: strings are fixed-capacity
//! [`Text`] values that truncate instead of failing, so a long title from the
//! bridge never makes the panel drop a whole page. Unknown fields are ignored
//! and unknown enum values map to an `Unknown` variant, so the bridge can add
//! fields without a firmware update.

use core::fmt;

use heapless::{String, Vec};
use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

/// The schema version this crate understands.
pub const SCHEMA_VERSION: u8 = 2;

/// Most badges the header draws. The bridge sends at most this many.
pub const MAX_BADGES: usize = 4;

/// Most list rows drawn. The bridge sends at most this many.
pub const MAX_LIST_ROWS: usize = 5;

/// Scratch space for unescaping JSON strings. Must be at least as long as the
/// longest single string in a payload.
const UNESCAPE_BUF: usize = 256;

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// A string of at most `N` bytes. Longer input is cut at a character boundary.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Text<const N: usize>(String<N>);

impl<const N: usize> Text<N> {
    /// Build from a `&str`, truncating to fit.
    pub fn new(s: &str) -> Self {
        let mut out = String::new();
        for ch in s.chars() {
            if out.push(ch).is_err() {
                break;
            }
        }
        Self(out)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<const N: usize> fmt::Debug for Text<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<'de, const N: usize> Deserialize<'de> for Text<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor<const N: usize>;

        impl<const N: usize> Visitor<'_> for TextVisitor<N> {
            type Value = Text<N>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Text::new(v))
            }
        }

        deserializer.deserialize_str(TextVisitor::<N>)
    }
}

/// Short labels: page names, sources, refs.
pub type Short = Text<48>;
/// Titles and row text. The bridge truncates to 40 characters; 128 bytes
/// leaves room for non-ASCII characters.
pub type Long = Text<128>;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Which layout to draw. The only field the panel switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Template {
    Stats,
    Job,
    Alert,
    List,
    Number,
    Notice,
    /// A template added after this firmware was built.
    #[serde(other)]
    Unknown,
}

/// `job.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Build,
    Deploy,
    #[serde(other)]
    Unknown,
}

/// `alert.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    Failed,
    Warn,
    Success,
    #[serde(other)]
    Unknown,
}

/// Row and badge status, mapped to a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowStatus {
    Ok,
    Running,
    Failed,
    Open,
    Review,
    #[default]
    #[serde(other)]
    Neutral,
}

/// Badge icon. Unknown icons fall back to a dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Icon {
    Pr,
    Pipeline,
    Server,
    Warn,
    Home,
    #[default]
    #[serde(other)]
    Dot,
}

// ---------------------------------------------------------------------------
// Template data
// ---------------------------------------------------------------------------

/// `stats` data. `None` is drawn as `--`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct StatsData {
    #[serde(default)]
    pub host: Short,
    #[serde(default)]
    pub cpu_pct: Option<f32>,
    #[serde(default)]
    pub cpu_temp_c: Option<f32>,
    #[serde(default)]
    pub load: Option<[f32; 3]>,
    #[serde(default)]
    pub cpu_count: Option<u32>,
    #[serde(default)]
    pub ram_pct: Option<f32>,
    #[serde(default)]
    pub disk_pct: Option<f32>,
    #[serde(default)]
    pub uptime_s: Option<u64>,
    #[serde(default)]
    pub net: Option<NetData>,
}

/// Network rates in bytes per second.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct NetData {
    #[serde(default)]
    pub iface: Short,
    #[serde(default)]
    pub rx_bps: Option<u64>,
    #[serde(default)]
    pub tx_bps: Option<u64>,
}

/// `job` data: a running job.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JobData {
    #[serde(default)]
    pub source: Short,
    #[serde(default)]
    pub project: Long,
    #[serde(default)]
    pub pipeline: Long,
    #[serde(default = "unknown_kind")]
    pub kind: JobKind,
    #[serde(default, rename = "ref")]
    pub git_ref: Short,
    #[serde(default)]
    pub commit: Short,
    #[serde(default)]
    pub step: Option<Long>,
    #[serde(default)]
    pub step_no: Option<u32>,
    #[serde(default)]
    pub step_count: Option<u32>,
    /// 0.0 to 1.0, or `None` for a spinner.
    #[serde(default)]
    pub progress: Option<f32>,
    #[serde(default)]
    pub started: Option<u64>,
    #[serde(default)]
    pub others: u32,
}

fn unknown_kind() -> JobKind {
    JobKind::Unknown
}

/// `alert` data: CI result (`project`/`pipeline`/`step`) or a general alert
/// (`title`/`message`). The panel draws whichever pair is present.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AlertData {
    #[serde(default = "unknown_status")]
    pub status: AlertStatus,
    #[serde(default)]
    pub source: Short,
    #[serde(default)]
    pub project: Option<Long>,
    #[serde(default)]
    pub pipeline: Option<Long>,
    #[serde(default)]
    pub step: Option<Long>,
    #[serde(default)]
    pub title: Option<Long>,
    #[serde(default)]
    pub message: Option<Long>,
    #[serde(default)]
    pub started: Option<u64>,
    #[serde(default)]
    pub finished: Option<u64>,
    #[serde(default)]
    pub others: u32,
}

fn unknown_status() -> AlertStatus {
    AlertStatus::Unknown
}

/// `list` data.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ListData {
    #[serde(default)]
    pub title: Long,
    /// Total number of items, which can be more than the rows sent.
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub rows: Vec<ListRow, MAX_LIST_ROWS>,
}

/// One row in a `list` page.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ListRow {
    #[serde(default)]
    pub text: Long,
    #[serde(default)]
    pub sub: Long,
    #[serde(default)]
    pub status: RowStatus,
    #[serde(default)]
    pub source: Short,
}

/// `number` data: one big figure.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct NumberData {
    #[serde(default)]
    pub title: Long,
    #[serde(default)]
    pub value: Short,
    #[serde(default)]
    pub sub: Long,
}

/// `notice` data: a short flash.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct NoticeData {
    #[serde(default)]
    pub text: Long,
    #[serde(default)]
    pub sub: Long,
    #[serde(default)]
    pub source: Short,
}

/// The data part of a screen, already matched to its template.
///
/// The variants differ in size (a list is the biggest), but there is no heap
/// on the panel to box them into, and only one screen is held at a time.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Stats(StatsData),
    Job(JobData),
    Alert(AlertData),
    List(ListData),
    Number(NumberData),
    Notice(NoticeData),
    /// A template this firmware does not know. Drawn as a hint to update.
    Unknown,
}

// ---------------------------------------------------------------------------
// deskpanel/screen
// ---------------------------------------------------------------------------

/// A parsed `deskpanel/screen` payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub v: u8,
    pub seq: u64,
    pub template: Template,
    /// 0 to 5, see schema section 3. Only used for styling.
    pub level: u8,
    pub page: Short,
    pub pinned: bool,
    /// `(n, of)` while rotating, `None` during interrupts.
    pub position: Option<(u8, u8)>,
    pub stale: bool,
    pub body: Body,
}

/// Envelope without the data, read in a first pass to learn the template.
#[derive(Deserialize)]
struct Header {
    v: u8,
    #[serde(default)]
    seq: u64,
    #[serde(default = "unknown_template")]
    template: Template,
    #[serde(default = "rotation_level")]
    level: u8,
    #[serde(default)]
    page: Short,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    position: Option<[u8; 2]>,
    #[serde(default)]
    stale: bool,
}

fn unknown_template() -> Template {
    Template::Unknown
}

fn rotation_level() -> u8 {
    5
}

/// Second pass: just the `data` field, typed by the template.
#[derive(Deserialize)]
struct WithData<D> {
    data: D,
}

/// Why a payload was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Not valid JSON, or a field had the wrong type.
    Json,
    /// `"v"` is not [`SCHEMA_VERSION`].
    Version(u8),
    /// Payload is not UTF-8.
    Utf8,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Json => f.write_str("invalid JSON"),
            ParseError::Version(v) => write!(f, "schema v{v} not supported"),
            ParseError::Utf8 => f.write_str("payload is not UTF-8"),
        }
    }
}

fn parse<'a, T: Deserialize<'a>>(json: &'a str) -> Result<T, ParseError> {
    let mut buf = [0u8; UNESCAPE_BUF];
    serde_json_core::from_str_escaped(json, &mut buf)
        .map(|(value, _)| value)
        .map_err(|_| ParseError::Json)
}

fn as_str(payload: &[u8]) -> Result<&str, ParseError> {
    core::str::from_utf8(payload).map_err(|_| ParseError::Utf8)
}

impl Screen {
    /// Parse a `deskpanel/screen` payload.
    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let json = as_str(payload)?;
        let header: Header = parse(json)?;
        if header.v != SCHEMA_VERSION {
            return Err(ParseError::Version(header.v));
        }

        let body = match header.template {
            Template::Stats => Body::Stats(parse::<WithData<_>>(json)?.data),
            Template::Job => Body::Job(parse::<WithData<_>>(json)?.data),
            Template::Alert => Body::Alert(parse::<WithData<_>>(json)?.data),
            Template::List => Body::List(parse::<WithData<_>>(json)?.data),
            Template::Number => Body::Number(parse::<WithData<_>>(json)?.data),
            Template::Notice => Body::Notice(parse::<WithData<_>>(json)?.data),
            Template::Unknown => Body::Unknown,
        };

        Ok(Screen {
            v: header.v,
            seq: header.seq,
            template: header.template,
            level: header.level,
            page: header.page,
            pinned: header.pinned,
            position: header.position.map(|[n, of]| (n, of)),
            stale: header.stale,
            body,
        })
    }

    /// True for the levels that interrupt rotation (critical alert to success).
    pub fn is_interrupt(&self) -> bool {
        self.level <= 3
    }
}

// ---------------------------------------------------------------------------
// deskpanel/badges
// ---------------------------------------------------------------------------

/// One header badge.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Badge {
    #[serde(default)]
    pub id: Short,
    #[serde(default)]
    pub icon: Icon,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub status: RowStatus,
}

/// A parsed `deskpanel/badges` payload.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Badges {
    pub v: u8,
    /// Room for a few more than [`MAX_BADGES`] so an over-full message still
    /// parses; only the first [`MAX_BADGES`] are drawn.
    #[serde(default)]
    pub items: Vec<Badge, 8>,
}

impl Badges {
    /// Parse a `deskpanel/badges` payload.
    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let badges: Badges = parse(as_str(payload)?)?;
        if badges.v != SCHEMA_VERSION {
            return Err(ParseError::Version(badges.v));
        }
        Ok(badges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_job_with_unknown_fields_and_escapes() {
        let json = br#"{"v":2,"seq":7,"template":"job","level":1,"page":"gitea-job","pinned":false,
            "position":null,"stale":false,"extra":{"nested":[1,2]},
            "data":{"source":"gitea","project":"repo \"a\"","pipeline":"deploy.yml","kind":"deploy",
            "ref":"main","commit":"a1b2c3d","step":"Run migrations","step_no":3,"step_count":5,
            "progress":0.6,"started":1791410400,"others":1,"future_field":true}}"#;
        let screen = Screen::parse(json).unwrap();
        assert_eq!(screen.seq, 7);
        assert_eq!(screen.position, None);
        let Body::Job(job) = screen.body else {
            panic!("not a job")
        };
        assert_eq!(job.project.as_str(), "repo \"a\"");
        assert_eq!(job.kind, JobKind::Deploy);
        assert_eq!(job.step_no, Some(3));
        assert_eq!(job.progress, Some(0.6));
    }

    #[test]
    fn unknown_template_and_status_do_not_fail() {
        let screen = Screen::parse(br#"{"v":2,"template":"chart","data":{}}"#).unwrap();
        assert_eq!(screen.body, Body::Unknown);

        let json = br#"{"v":2,"template":"list","data":{"title":"x","count":1,
            "rows":[{"text":"a","sub":"b","status":"exploded","source":"s"}]}}"#;
        let Body::List(list) = Screen::parse(json).unwrap().body else {
            panic!()
        };
        assert_eq!(list.rows[0].status, RowStatus::Neutral);
    }

    #[test]
    fn long_strings_are_truncated_not_rejected() {
        let long = "x".repeat(300);
        let json =
            format!(r#"{{"v":2,"template":"number","data":{{"title":"{long}","value":"1"}}}}"#);
        let Body::Number(n) = Screen::parse(json.as_bytes()).unwrap().body else {
            panic!()
        };
        assert_eq!(n.title.as_str().len(), 128);
    }

    #[test]
    fn wrong_version_is_rejected() {
        assert_eq!(
            Screen::parse(br#"{"v":3,"template":"stats","data":{}}"#),
            Err(ParseError::Version(3))
        );
        assert_eq!(Screen::parse(b"not json"), Err(ParseError::Json));
    }

    #[test]
    fn badges_parse() {
        let b = Badges::parse(
            br#"{"v":2,"items":[{"id":"prs","icon":"pr","count":3,"status":"open"},
            {"id":"x","icon":"rocket","count":1,"status":"failed"}]}"#,
        )
        .unwrap();
        assert_eq!(b.items.len(), 2);
        assert_eq!(b.items[1].icon, Icon::Dot);
    }
}
