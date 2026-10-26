//! Each signed-in user's own dashboard layout (hidden, moved and resized
//! panels), kept in `layouts.json` in the data folder under the user's
//! identity. Only the difference from the administrator's default is stored.
//! A screen opened with a key has no identity, so it keeps its choice in the
//! browser instead (the page falls back on a 409 here).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use super::*;

const FILE: &str = "layouts.json";
/// Most panels one layout may mention, and the longest panel id.
const MAX_PANELS: usize = 64;
const MAX_ID: usize = 80;
/// Most users whose layout is kept.
const MAX_USERS: usize = 1000;

/// What a user changed. Ids are the page's panel ids (`hosts#1`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserLayout {
    pub order: Vec<String>,
    pub hidden: Vec<String>,
    pub span: BTreeMap<String, [u8; 2]>,
    /// Where a panel sits on the grid: column and row, counted from 1.
    pub pos: BTreeMap<String, [u8; 2]>,
}

impl UserLayout {
    fn check(&self) -> Result<()> {
        let ids = self
            .order
            .iter()
            .chain(self.hidden.iter())
            .chain(self.span.keys())
            .chain(self.pos.keys());
        for id in ids {
            if id.is_empty() || id.len() > MAX_ID || id.chars().any(char::is_control) {
                bail!("a panel id is not valid");
            }
        }
        if self.order.len() > MAX_PANELS
            || self.hidden.len() > MAX_PANELS
            || self.span.len() > MAX_PANELS
            || self.pos.len() > MAX_PANELS
        {
            bail!("too many panels");
        }
        if self
            .span
            .values()
            .chain(self.pos.values())
            .any(|s| s.iter().any(|n| !(1..=12).contains(n)))
        {
            bail!("a panel size or place is out of range");
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct Layouts {
    path: Option<PathBuf>,
    map: Mutex<BTreeMap<String, UserLayout>>,
}

impl Layouts {
    pub fn open(dir: Option<&Path>) -> Self {
        let path = dir.map(|d| d.join(FILE));
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            path,
            map: Mutex::new(map),
        }
    }

    fn get(&self, who: &str) -> Option<UserLayout> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(who)
            .cloned()
    }

    /// Keep (`Some`) or forget (`None`) one user's layout.
    fn put(&self, who: &str, layout: Option<UserLayout>) -> Result<()> {
        let Some(path) = &self.path else {
            bail!("there is no data folder to save in");
        };
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = map.clone();
        match layout {
            Some(l) => {
                if !next.contains_key(who) && next.len() >= MAX_USERS {
                    bail!("too many saved layouts");
                }
                next.insert(who.to_string(), l);
            }
            None => {
                next.remove(who);
            }
        }
        let text = serde_json::to_string_pretty(&next)?;
        crate::store::write_atomic(path, text.as_bytes(), 0o600)?;
        *map = next;
        Ok(())
    }
}

impl Auth {
    /// Who a session is, for keeping things per person: a local account or
    /// a provider sign-in. `None` for a screen key.
    fn identity(&self, headers: &HeaderMap) -> Option<String> {
        let token = cookie_token(headers)?;
        if let Some((name, _)) = self.session_user(&token) {
            return Some(format!("local:{name}"));
        }
        self.session_label(&token).map(|l| format!("oauth:{l}"))
    }
}

fn no_identity() -> Response {
    text(
        StatusCode::CONFLICT,
        "this sign-in is not a person, so the layout stays in the browser",
    )
}

pub(super) async fn get_layout(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    let Some(who) = auth.identity(&headers) else {
        return no_identity();
    };
    let mut r = Json(json!({ "layout": auth.0.layouts.get(&who) })).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

pub(super) async fn put_layout(
    State(auth): State<Auth>,
    headers: HeaderMap,
    Json(layout): Json<UserLayout>,
) -> Response {
    let Some(who) = auth.identity(&headers) else {
        return no_identity();
    };
    if let Err(e) = layout.check() {
        return text(StatusCode::BAD_REQUEST, format!("{e:#}"));
    }
    match auth.0.layouts.put(&who, Some(layout)) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => text(StatusCode::INSUFFICIENT_STORAGE, format!("{e:#}")),
    }
}

pub(super) async fn reset_layout(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    let Some(who) = auth.identity(&headers) else {
        return no_identity();
    };
    match auth.0.layouts.put(&who, None) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => text(StatusCode::INSUFFICIENT_STORAGE, format!("{e:#}")),
    }
}

/// Check the text of a `layouts.json` (a backup being restored). Gives the
/// number of users whose layout it holds.
pub fn check_text(text: &str) -> Result<usize> {
    let map: BTreeMap<String, UserLayout> =
        serde_json::from_str(text).context("layouts.json in the backup is not valid")?;
    if map.len() > MAX_USERS {
        bail!("too many saved layouts");
    }
    for layout in map.values() {
        layout.check()?;
    }
    Ok(map.len())
}
