//! "A newer version is out": a quiet daily look at the latest GitHub release.
//!
//! The bridge asks GitHub once a day for the latest release of DeskWatch and
//! compares its version number with its own. Nothing about the install is
//! sent (a plain GET with a user agent, no id, no host name). A failed check,
//! a missing internet connection or an odd answer is silent: there is just no
//! notice. `[updates] check = false` turns the whole thing off.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

/// The latest release, as the GitHub API gives it.
const LATEST_URL: &str = "https://api.github.com/repos/Sylentic/DeskWatch/releases/latest";
/// Release pages must be on this repository; anything else is ignored.
const RELEASE_PREFIX: &str = "https://github.com/Sylentic/DeskWatch/";
/// Wait this long after start before the first look, then repeat daily.
const FIRST_DELAY: Duration = Duration::from_secs(30);
const EVERY: Duration = Duration::from_secs(24 * 3600);
const TIMEOUT: Duration = Duration::from_secs(15);

/// A release newer than the running bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Update {
    /// Version number without the leading `v`, e.g. `0.9.20`.
    pub version: String,
    /// The release notes page on github.com.
    pub url: String,
}

/// Where the checker leaves its finding. Clones share it.
#[derive(Debug, Clone, Default)]
pub struct UpdateNotice(Arc<Mutex<Option<Update>>>);

impl UpdateNotice {
    /// The newer release, if one was found.
    pub fn get(&self) -> Option<Update> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set(&self, update: Option<Update>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = update;
    }

    /// Start the daily check on the running tokio runtime.
    pub fn spawn(&self, current: &'static str) {
        let notice = self.clone();
        tokio::spawn(async move {
            let client = match reqwest::Client::builder()
                .user_agent(concat!("deskwatch-bridge/", env!("CARGO_PKG_VERSION")))
                .timeout(TIMEOUT)
                .build()
            {
                Ok(client) => client,
                Err(_) => return,
            };
            tokio::time::sleep(FIRST_DELAY).await;
            loop {
                match fetch(&client).await {
                    Some(body) => notice.set(newer_than(current, &body)),
                    // Offline or refused: keep what we knew, say nothing.
                    None => tracing::debug!("update check: no answer"),
                }
                tokio::time::sleep(EVERY).await;
            }
        });
    }
}

async fn fetch(client: &reqwest::Client) -> Option<Value> {
    let response = client
        .get(LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    response.json().await.ok()
}

/// `1.2.3` or `v1.2.3` as numbers. Anything else (a pre-release suffix, extra
/// parts, letters) is not a version we compare.
fn parse(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut out = [0u64; 3];
    for slot in &mut out {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// The release in a GitHub "latest release" answer, when it is newer than
/// `current` and has a release page on this repository.
pub fn newer_than(current: &str, release: &Value) -> Option<Update> {
    let tag = release.get("tag_name")?.as_str()?;
    let url = release.get("html_url")?.as_str()?;
    let latest = parse(tag)?;
    if latest <= parse(current)? || !url.starts_with(RELEASE_PREFIX) {
        return None;
    }
    Some(Update {
        version: tag.strip_prefix('v').unwrap_or(tag).to_string(),
        url: url.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> Value {
        json!({ "tag_name": tag, "html_url": format!("{RELEASE_PREFIX}releases/tag/{tag}") })
    }

    #[test]
    fn a_higher_version_is_an_update_an_equal_or_lower_one_is_not() {
        let u = newer_than("0.9.19", &release("v0.9.20")).unwrap();
        assert_eq!(u.version, "0.9.20");
        assert!(u.url.ends_with("/releases/tag/v0.9.20"));
        assert!(newer_than("0.9.19", &release("v0.9.19")).is_none());
        assert!(newer_than("0.9.19", &release("v0.9.9")).is_none());
        assert!(newer_than("0.9.9", &release("v0.9.10")).is_some());
        assert!(newer_than("0.9.19", &release("v1.0.0")).is_some());
    }

    #[test]
    fn odd_answers_give_no_notice() {
        for tag in ["latest", "v1.0.0-rc1", "v1.0", "v1.0.0.1", "v1.x.0", ""] {
            assert!(newer_than("0.9.19", &release(tag)).is_none(), "{tag}");
        }
        assert!(newer_than("0.9.19", &json!({})).is_none());
        assert!(newer_than("0.9.19", &json!({ "tag_name": "v9.0.0" })).is_none());
        // A release page on another site is never shown.
        let evil = json!({ "tag_name": "v9.0.0", "html_url": "https://evil.example/x" });
        assert!(newer_than("0.9.19", &evil).is_none());
    }
}
