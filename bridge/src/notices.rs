//! Notices: short messages for colleagues, typed on the setup page and shown
//! as pills at the top of the Alerting Kiosk ("Patching all servers, until
//! 23:00"). Each has a kind (planned work or a problem), an optional start and
//! an optional end. The page shows a notice from its start until its end; one
//! without an end stays until someone ends it.
//!
//! They are kept in `notices.json` in the data folder, so a restart keeps
//! them. Ended notices stay listed on the setup page for a week, then go.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// File in the data folder with the notices.
const NOTICES_FILE: &str = "notices.json";

/// Longest message, in characters: it has to fit in a pill.
pub const MAX_TEXT: usize = 80;

/// Most notices kept at once, ended ones included.
const MAX_NOTICES: usize = 50;

/// An ended notice is listed for this long, then removed.
const ENDED_KEEP_S: u64 = 7 * 24 * 3600;

/// What a notice is about: the colour of its pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Planned work: a blue pill.
    Planned,
    /// Something is wrong: a yellow pill.
    Problem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub kind: Kind,
    pub text: String,
    /// Unix seconds. `None`: shown from the moment it was saved.
    pub start: Option<u64>,
    /// Unix seconds. `None`: shown until someone ends it.
    pub end: Option<u64>,
    /// Unix seconds when it was made, to keep the list in a stable order.
    pub created: u64,
}

impl Notice {
    /// Over, at `now`.
    pub fn ended(&self, now: u64) -> bool {
        self.end.is_some_and(|end| end <= now)
    }
}

/// What the setup page sends to add or change a notice.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// Set when changing an existing notice.
    #[serde(default)]
    pub id: Option<String>,
    pub kind: Kind,
    pub text: String,
    #[serde(default)]
    pub start: Option<u64>,
    #[serde(default)]
    pub end: Option<u64>,
}

#[derive(Default)]
struct Inner {
    list: Vec<Notice>,
    /// Where the list is kept; `None` keeps it in memory only (tests).
    path: Option<PathBuf>,
}

/// The notices, shared by the setup page (which changes them) and the kiosk
/// page (which shows them). Cheap to clone.
#[derive(Clone, Default)]
pub struct Notices {
    inner: Arc<Mutex<Inner>>,
}

impl Notices {
    /// Read the notices from the data folder. A missing or broken file means none.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(NOTICES_FILE);
        let list = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<Notice>>(&text).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(Mutex::new(Inner {
                list,
                path: Some(path),
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every notice, ended ones of the last week included, oldest first.
    pub fn all(&self, now: u64) -> Vec<Notice> {
        let mut inner = self.lock();
        inner.prune(now);
        inner.list.clone()
    }

    /// The notices that are showing or still to come, for the kiosk page. The
    /// page itself decides which have started, by the bridge's clock.
    pub fn current(&self, now: u64) -> Vec<Notice> {
        self.lock()
            .list
            .iter()
            .filter(|n| !n.ended(now))
            .cloned()
            .collect()
    }

    /// Add a notice, or change the one with `input.id`. Gives back the saved notice.
    pub fn save(&self, input: Input, now: u64) -> Result<Notice> {
        let text = input.text.trim().to_string();
        ensure!(!text.is_empty(), "the message is empty");
        ensure!(
            text.chars().count() <= MAX_TEXT,
            "the message is longer than {MAX_TEXT} characters"
        );
        ensure!(
            !text.chars().any(char::is_control),
            "the message must be one line"
        );
        if let (Some(start), Some(end)) = (input.start, input.end) {
            ensure!(end > start, "the end is before the start");
        }
        if let Some(end) = input.end {
            ensure!(end > now, "the end is in the past");
        }
        let mut inner = self.lock();
        inner.prune(now);
        let notice = match input.id {
            Some(id) => {
                let Some(found) = inner.list.iter_mut().find(|n| n.id == id) else {
                    anyhow::bail!("that notice no longer exists");
                };
                found.kind = input.kind;
                found.text = text;
                found.start = input.start;
                found.end = input.end;
                found.clone()
            }
            None => {
                ensure!(
                    inner.list.len() < MAX_NOTICES,
                    "there are already {MAX_NOTICES} notices; delete some first"
                );
                let notice = Notice {
                    id: new_id(),
                    kind: input.kind,
                    text,
                    start: input.start,
                    end: input.end,
                    created: now,
                };
                inner.list.push(notice.clone());
                notice
            }
        };
        inner.save()?;
        Ok(notice)
    }

    /// End a notice now: it leaves the screen and moves to the ended ones.
    pub fn end_now(&self, id: &str, now: u64) -> Result<()> {
        let mut inner = self.lock();
        let Some(found) = inner.list.iter_mut().find(|n| n.id == id) else {
            anyhow::bail!("that notice no longer exists");
        };
        found.end = Some(now);
        if found.start.is_some_and(|start| start > now) {
            // It never started: keep the period sensible.
            found.start = Some(now);
        }
        inner.save()
    }

    /// Remove a notice for good.
    pub fn delete(&self, id: &str) -> Result<()> {
        let mut inner = self.lock();
        let before = inner.list.len();
        inner.list.retain(|n| n.id != id);
        ensure!(inner.list.len() != before, "that notice no longer exists");
        inner.save()
    }
}

impl Inner {
    /// Drop notices that ended more than a week ago.
    fn prune(&mut self, now: u64) {
        let before = self.list.len();
        self.list.retain(|n| {
            n.end
                .is_none_or(|end| now.saturating_sub(end) < ENDED_KEEP_S)
        });
        if self.list.len() != before
            && let Err(e) = self.save()
        {
            tracing::warn!("cannot save the notices: {e:#}");
        }
    }

    /// Write the list (0600).
    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = serde_json::to_string_pretty(&self.list)?;
        crate::store::write_atomic(path, text.as_bytes(), 0o600)
    }
}

/// A short random id, only used to tell notices apart.
fn new_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the system has no random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str, start: Option<u64>, end: Option<u64>) -> Input {
        Input {
            id: None,
            kind: Kind::Planned,
            text: text.into(),
            start,
            end,
        }
    }

    #[test]
    fn a_notice_is_current_until_its_end_and_listed_a_week_after() {
        let notices = Notices::default();
        let n = notices
            .save(input(" Patching all servers ", Some(100), Some(200)), 50)
            .unwrap();
        assert_eq!(n.text, "Patching all servers");
        assert_eq!(notices.current(150), vec![n.clone()]);
        // Still to come counts as current: the page waits for the start.
        assert_eq!(notices.current(60).len(), 1);
        assert!(notices.current(200).is_empty());
        assert_eq!(notices.all(200).len(), 1);
        assert!(notices.all(200 + ENDED_KEEP_S).is_empty());
    }

    #[test]
    fn bad_notices_are_refused() {
        let notices = Notices::default();
        assert!(notices.save(input("   ", None, None), 0).is_err());
        assert!(notices.save(input(&"x".repeat(81), None, None), 0).is_err());
        assert!(notices.save(input("two\nlines", None, None), 0).is_err());
        assert!(
            notices
                .save(input("backwards", Some(20), Some(10)), 0)
                .is_err()
        );
        assert!(notices.save(input("over", None, Some(5)), 10).is_err());
        assert!(notices.save(input(&"x".repeat(80), None, None), 0).is_ok());
    }

    #[test]
    fn edit_end_now_and_delete() {
        let notices = Notices::default();
        let n = notices.save(input("NAS is slow", None, None), 10).unwrap();
        let changed = notices
            .save(
                Input {
                    id: Some(n.id.clone()),
                    kind: Kind::Problem,
                    text: "NAS is slow, looking into it".into(),
                    start: None,
                    end: None,
                },
                20,
            )
            .unwrap();
        assert_eq!((changed.kind, changed.created), (Kind::Problem, 10));
        notices.end_now(&n.id, 30).unwrap();
        assert!(notices.current(30).is_empty());
        notices.delete(&n.id).unwrap();
        assert!(notices.all(30).is_empty());
        assert!(notices.delete(&n.id).is_err());
    }

    #[test]
    fn notices_survive_a_restart() {
        let dir = crate::probe::tests::Temp::new("notices");
        let n = Notices::load(&dir.0)
            .save(input("Firewall upgrade", Some(100), None), 50)
            .unwrap();
        assert_eq!(Notices::load(&dir.0).all(60), vec![n]);
        let _ = std::fs::remove_dir_all(&dir.0);
    }
}
