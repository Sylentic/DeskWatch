//! Alerts raised over MQTT on `deskpanel/alert` (schema section 5) by Home
//! Assistant or any script on the LAN.
//!
//! The severity decides how loud an alert is:
//!
//! | Severity   | Level | Screen                         | Leaves the screen          |
//! |------------|-------|--------------------------------|----------------------------|
//! | `critical` | 0     | red `alert`, above running jobs | button press               |
//! | `warning`  | 2     | amber `alert`                  | button press or 10 minutes |
//! | `info`     | 4     | `notice` flash                 | 5 seconds                  |
//!
//! Leaving the screen does not end an alert: it stays on the `alerts` rotation
//! page, and warnings and criticals stay in the `home` header badge, until it
//! is cleared with `{"id": ..., "clear": true}` or its `ttl_s` runs out.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::config::AlertsConfig;
use crate::model::{
    AlertData, AlertStatus, Badge, BadgeIcon, Candidate, Level, ListData, ListRow, MAX_TITLE_CHARS,
    NoticeData, RowStatus, SCHEMA_VERSION, ScreenData, truncate,
};

/// Largest inbound payload accepted. Bigger ones are dropped unread.
pub const MAX_PAYLOAD: usize = 1024;

/// Default lifetime of an `info` alert when it has no `ttl_s`. Warnings and
/// criticals stay until cleared.
pub const INFO_TTL_S: u64 = 30 * 60;

/// Longest alert id kept. Longer ids are cut, not rejected.
const MAX_ID_CHARS: usize = 64;

/// How urgent an alert is. Declared most urgent first, so sorting puts
/// critical alerts at the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl Severity {
    /// Screen level while the alert is on screen.
    fn level(self) -> Level {
        match self {
            Severity::Critical => Level::Critical,
            Severity::Warning => Level::AlertFailed,
            Severity::Info => Level::Notice,
        }
    }

    fn row_status(self) -> RowStatus {
        match self {
            Severity::Critical => RowStatus::Failed,
            Severity::Warning => RowStatus::Review, // amber on the panel
            Severity::Info => RowStatus::Neutral,
        }
    }
}

/// A `deskpanel/alert` payload. Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AlertMessage {
    pub v: u8,
    /// Same id replaces the earlier alert; needed to clear. Missing means a
    /// new alert every time.
    #[serde(default)]
    pub id: Option<String>,
    /// Defaults to `info`, the quietest.
    #[serde(default)]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Lifetime in seconds.
    #[serde(default)]
    pub ttl_s: Option<u64>,
    /// Free text label, default `mqtt`.
    #[serde(default)]
    pub source: Option<String>,
    /// True to remove the alert with this `id`.
    #[serde(default)]
    pub clear: bool,
}

impl AlertMessage {
    /// Parse and check a payload: at most `MAX_PAYLOAD` bytes, schema v2.
    pub fn parse(payload: &[u8]) -> Result<Self> {
        if payload.len() > MAX_PAYLOAD {
            bail!(
                "alert is {} bytes, over the {MAX_PAYLOAD} limit",
                payload.len()
            );
        }
        let msg: AlertMessage = serde_json::from_slice(payload).context("invalid alert")?;
        if msg.v != SCHEMA_VERSION {
            bail!("alert has schema v{}, expected v{SCHEMA_VERSION}", msg.v);
        }
        if msg.clear && msg.id.is_none() {
            bail!("clear needs an id");
        }
        Ok(msg)
    }
}

/// One active alert.
#[derive(Debug, Clone, PartialEq)]
struct Active {
    severity: Severity,
    title: String,
    message: String,
    source: String,
    /// Unix seconds of the last raise. Screen timers count from here.
    raised_at: u64,
    /// Unix seconds when it ends on its own, or `None` to wait for `clear`.
    expires: Option<u64>,
    /// False once the button acknowledged it: it keeps its badge and its row
    /// on the alerts page but no longer takes the screen.
    on_screen: bool,
}

/// Every active alert, by id.
#[derive(Debug, Default)]
pub struct Alerts {
    config: AlertsConfig,
    active: BTreeMap<String, Active>,
    /// Counter for alerts sent without an id.
    anonymous: u64,
}

impl Alerts {
    pub fn new(config: AlertsConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    /// Raise, replace or clear an alert.
    pub fn apply(&mut self, msg: AlertMessage, now: u64) {
        if msg.clear {
            let id = msg.id.as_deref().map(alert_id).unwrap_or_default();
            if self.active.remove(&id).is_some() {
                info!(id, "alert cleared");
            }
            return;
        }

        let id = match &msg.id {
            Some(id) => alert_id(id),
            None => {
                self.anonymous += 1;
                format!("~{}", self.anonymous)
            }
        };
        // One update per id per second, so a looping automation cannot
        // flicker the panel.
        if self.active.get(&id).is_some_and(|a| a.raised_at == now) {
            debug!(id, "alert repeated within a second, ignored");
            return;
        }

        let mut severity = msg.severity.unwrap_or(Severity::Info);
        if severity == Severity::Critical && !self.config.allows_critical(&id) {
            warn!(
                id,
                "alert id is not in alerts.critical_ids, shown as a warning"
            );
            severity = Severity::Warning;
        }
        let ttl = msg.ttl_s.or(match severity {
            Severity::Info => Some(INFO_TTL_S),
            Severity::Warning | Severity::Critical => None,
        });
        let title = msg.title.as_deref().unwrap_or("Alert");
        let alert = Active {
            severity,
            title: truncate(title, MAX_TITLE_CHARS),
            message: truncate(msg.message.as_deref().unwrap_or(""), MAX_TITLE_CHARS),
            source: truncate(msg.source.as_deref().unwrap_or("mqtt"), MAX_TITLE_CHARS),
            raised_at: now,
            expires: ttl.map(|ttl| now.saturating_add(ttl)),
            on_screen: true,
        };
        info!(id, ?severity, title = alert.title, "alert raised");
        self.active.insert(id, alert);

        // Keep at most `max_active`: drop the least urgent, oldest one.
        while self.active.len() > self.config.max_active.max(1) {
            let drop = self
                .active
                .iter()
                .max_by_key(|(_, a)| (a.severity, std::cmp::Reverse(a.raised_at)))
                .map(|(id, _)| id.clone());
            let Some(drop) = drop else { break };
            warn!(id = drop, "too many active alerts, dropping one");
            self.active.remove(&drop);
        }
    }

    /// Drop alerts whose `ttl_s` has run out.
    pub fn expire(&mut self, now: u64) {
        self.active
            .retain(|_, a| a.expires.is_none_or(|expires| now < expires));
    }

    /// Button press on a screen of `level`: every alert shown at that level
    /// leaves the screen. They stay in the badge and on the alerts page.
    pub fn acknowledge(&mut self, level: Level) {
        for alert in self.active.values_mut() {
            if alert.severity.level() == level {
                alert.on_screen = false;
            }
        }
    }

    /// Forget every alert (used by the demo when it starts over).
    pub fn clear_all(&mut self) {
        self.active.clear();
    }

    /// Alerts that want the screen. `model::pick` chooses among these and the
    /// CI candidates.
    pub fn candidates(&self, now: u64) -> Vec<Candidate> {
        let showing = |a: &Active| a.on_screen && !candidate(a).is_expired(now);
        self.active
            .values()
            .filter(|a| showing(a))
            .map(|a| {
                let mut c = candidate(a);
                if let ScreenData::Alert(data) = &mut c.data {
                    let same = |b: &&Active| b.severity == a.severity && showing(b);
                    data.others =
                        self.active.values().filter(same).count().saturating_sub(1) as u32;
                }
                c
            })
            .collect()
    }

    /// The `home` header badge: active warning and critical alerts, red when
    /// any of them is critical.
    pub fn badge(&self) -> Badge {
        let loud = || {
            self.active
                .values()
                .filter(|a| a.severity != Severity::Info)
        };
        let critical = loud().any(|a| a.severity == Severity::Critical);
        Badge {
            id: "alerts".into(),
            icon: BadgeIcon::Home,
            count: loud().count() as u32,
            status: if critical {
                RowStatus::Failed
            } else {
                RowStatus::Review
            },
        }
    }

    /// The `alerts` rotation page: most urgent first, then newest.
    pub fn page(&self) -> ListData {
        let mut alerts: Vec<&Active> = self.active.values().collect();
        alerts.sort_by_key(|a| (a.severity, std::cmp::Reverse(a.raised_at)));
        let rows = alerts
            .into_iter()
            .map(|a| ListRow {
                text: a.title.clone(),
                sub: a.message.clone(),
                status: a.severity.row_status(),
                source: a.source.clone(),
            })
            .collect();
        ListData::new("Alerts", self.active.len() as u32, rows)
    }
}

/// Screen candidate for one alert: a red or amber `alert` page, or a
/// `notice` for info alerts.
fn candidate(a: &Active) -> Candidate {
    let data = match a.severity {
        Severity::Info => ScreenData::Notice(NoticeData {
            text: a.title.clone(),
            sub: a.message.clone(),
            source: a.source.clone(),
        }),
        Severity::Warning | Severity::Critical => ScreenData::Alert(AlertData {
            status: if a.severity == Severity::Critical {
                AlertStatus::Failed
            } else {
                AlertStatus::Warn
            },
            source: a.source.clone(),
            project: None,
            pipeline: None,
            step: None,
            title: Some(a.title.clone()),
            message: Some(a.message.clone()).filter(|m| !m.is_empty()),
            started: a.raised_at,
            finished: None,
            others: 0,
        }),
    };
    let kind = if a.severity == Severity::Info {
        "notice"
    } else {
        "alert"
    };
    Candidate {
        level: a.severity.level(),
        page: format!("{}-{kind}", a.source),
        data,
        raised_at: a.raised_at,
    }
}

/// Ids are trimmed and cut to `MAX_ID_CHARS`.
fn alert_id(id: &str) -> String {
    truncate(id.trim(), MAX_ID_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pick;

    fn raise(id: &str, severity: &str) -> AlertMessage {
        let json = format!(
            r#"{{"v":2,"id":"{id}","severity":"{severity}","title":"{id} title","message":"msg","source":"homeassistant"}}"#
        );
        AlertMessage::parse(json.as_bytes()).unwrap()
    }

    fn clear(id: &str) -> AlertMessage {
        AlertMessage::parse(format!(r#"{{"v":2,"id":"{id}","clear":true}}"#).as_bytes()).unwrap()
    }

    fn top(alerts: &Alerts, now: u64) -> Option<(Level, ScreenData)> {
        let candidates = alerts.candidates(now);
        pick(&candidates, now).map(|c| (c.level, c.data.clone()))
    }

    #[test]
    fn parse_checks_size_version_and_clear_id() {
        assert!(AlertMessage::parse(br#"{"v":2,"title":"x"}"#).is_ok());
        assert!(AlertMessage::parse(br#"{"v":1,"title":"x"}"#).is_err());
        assert!(AlertMessage::parse(br#"{"v":2,"clear":true}"#).is_err());
        assert!(AlertMessage::parse(br#"{"v":2,"severity":"panic"}"#).is_err());
        let big = format!(r#"{{"v":2,"title":"{}"}}"#, "x".repeat(MAX_PAYLOAD));
        assert!(AlertMessage::parse(big.as_bytes()).is_err());
    }

    #[test]
    fn severities_map_to_levels() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("washer", "info"), 0);
        let (level, data) = top(&alerts, 0).unwrap();
        assert_eq!(level, Level::Notice);
        assert!(matches!(data, ScreenData::Notice(n) if n.text == "washer title"));

        alerts.apply(raise("freezer", "warning"), 1);
        let (level, data) = top(&alerts, 1).unwrap();
        assert_eq!(level, Level::AlertFailed);
        let ScreenData::Alert(alert) = data else {
            panic!("expected an alert")
        };
        assert_eq!(alert.status, AlertStatus::Warn);
        assert_eq!(alert.title.as_deref(), Some("freezer title"));
        assert_eq!(alert.project, None);

        alerts.apply(raise("leak", "critical"), 2);
        let (level, data) = top(&alerts, 2).unwrap();
        assert_eq!(level, Level::Critical);
        assert!(matches!(data, ScreenData::Alert(a) if a.status == AlertStatus::Failed));
    }

    #[test]
    fn info_flashes_then_stays_on_the_page_until_its_ttl() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("washer", "info"), 100);
        assert!(top(&alerts, 104).is_some());
        assert!(top(&alerts, 105).is_none(), "notice flash is 5 s");
        assert_eq!(alerts.page().count, 1);
        assert_eq!(alerts.badge().count, 0, "info is not in the home badge");
        alerts.expire(100 + INFO_TTL_S);
        assert_eq!(alerts.page().count, 0);
    }

    #[test]
    fn warning_drops_to_badge_after_ten_minutes() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("freezer", "warning"), 0);
        assert!(top(&alerts, 599).is_some());
        assert!(top(&alerts, 600).is_none());
        alerts.expire(10_000);
        assert_eq!(alerts.badge().count, 1, "stays until cleared");
        assert_eq!(alerts.badge().status, RowStatus::Review);
    }

    #[test]
    fn button_acknowledges_only_the_shown_level() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("freezer", "warning"), 0);
        alerts.apply(raise("leak", "critical"), 0);
        assert_eq!(alerts.badge().status, RowStatus::Failed);

        alerts.acknowledge(Level::Critical);
        assert_eq!(top(&alerts, 1).unwrap().0, Level::AlertFailed);
        alerts.acknowledge(Level::AlertFailed);
        assert!(top(&alerts, 1).is_none());
        assert_eq!(
            alerts.badge().count,
            2,
            "acknowledged alerts keep the badge"
        );

        // Raising the same id again puts it back on screen.
        alerts.apply(raise("leak", "critical"), 5);
        assert_eq!(top(&alerts, 5).unwrap().0, Level::Critical);
    }

    #[test]
    fn clear_and_ttl_remove_alerts() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("leak", "critical"), 0);
        alerts.apply(clear("leak"), 1);
        assert!(top(&alerts, 1).is_none());
        assert_eq!(alerts.badge().count, 0);

        let mut short = raise("door", "critical");
        short.ttl_s = Some(30);
        alerts.apply(short, 0);
        alerts.expire(29);
        assert_eq!(alerts.badge().count, 1);
        alerts.expire(30);
        assert_eq!(alerts.badge().count, 0);
    }

    #[test]
    fn same_id_replaces_and_others_counts_the_rest() {
        let mut alerts = Alerts::default();
        alerts.apply(raise("a", "warning"), 0);
        alerts.apply(raise("a", "warning"), 1);
        alerts.apply(raise("b", "warning"), 2);
        assert_eq!(alerts.page().count, 2);
        let Some((_, ScreenData::Alert(alert))) = top(&alerts, 2) else {
            panic!("expected an alert")
        };
        assert_eq!(alert.title.as_deref(), Some("b title"), "newest first");
        assert_eq!(alert.others, 1);
    }

    #[test]
    fn rate_limit_cap_and_critical_ids() {
        let mut alerts = Alerts::new(AlertsConfig {
            max_active: 2,
            critical_ids: Some(vec!["leak".into()]),
            ..Default::default()
        });
        alerts.apply(raise("door", "critical"), 0);
        assert_eq!(top(&alerts, 0).unwrap().0, Level::AlertFailed, "downgraded");
        alerts.apply(raise("leak", "critical"), 0);
        assert_eq!(top(&alerts, 0).unwrap().0, Level::Critical);

        // A second update of the same id in the same second is ignored.
        let mut again = raise("leak", "critical");
        again.title = Some("changed".into());
        alerts.apply(again, 0);
        assert_eq!(alerts.page().rows[0].text, "leak title");

        // Over the cap, the least urgent, oldest alert goes first.
        alerts.apply(raise("washer", "info"), 1);
        let ids: Vec<String> = alerts.page().rows.into_iter().map(|r| r.text).collect();
        assert_eq!(ids, ["leak title", "door title"]);
    }

    #[test]
    fn long_text_is_truncated_and_defaults_apply() {
        let mut alerts = Alerts::default();
        let long = "x".repeat(100);
        let json = format!(r#"{{"v":2,"title":"{long}"}}"#);
        alerts.apply(AlertMessage::parse(json.as_bytes()).unwrap(), 0);
        let row = &alerts.page().rows[0];
        assert_eq!(row.text.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(row.source, "mqtt");
        assert_eq!(row.status, RowStatus::Neutral, "default severity is info");
    }
}
