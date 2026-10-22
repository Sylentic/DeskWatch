use super::*;
use crate::config::Config;

struct Temp(std::path::PathBuf);
impl Temp {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("deskwatch-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SOURCES: &str = "[[source.gitea]]\nname = \"home\"\nbase_url = \"https://git.example\"\n\
    webhook_secret_file = \"gitea-hook\"\ntoken_file = \"gitea-token\"\n";

fn filled(dir: &std::path::Path) -> Store {
    let store = Store::open_isolated(dir).unwrap();
    store.write_sources(SOURCES).unwrap();
    store
        .write_secret("gitea-token", "SUPER-SECRET-TOKEN")
        .unwrap();
    store.write_secret("gitea-hook", "HOOK-SECRET").unwrap();
    store.write_data_file("layouts.json", "{}").unwrap();
    store
}

#[test]
fn plain_export_has_no_secret() {
    let t = Temp::new("plain");
    let store = filled(&t.0);
    let bytes = export(&store, None).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains("SUPER-SECRET-TOKEN") && !text.contains("HOOK-SECRET"));
    assert!(
        text.contains("gitea-token"),
        "the name of the token file stays in sources"
    );
    let opened = open(&bytes, None).unwrap();
    assert!(!opened.locked && opened.secrets.is_empty());
    assert_eq!(opened.sources, SOURCES);
}

#[test]
fn locked_export_round_trips_and_hides_secrets() {
    let t = Temp::new("locked");
    let store = filled(&t.0);
    let bytes = export(&store, Some("correct horse battery")).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains("SUPER-SECRET-TOKEN") && !text.contains("HOOK-SECRET"));
    assert!(matches!(open(&bytes, None), Err(OpenError::NeedsPassword)));
    assert!(matches!(
        open(&bytes, Some("wrong wrong wrong")),
        Err(OpenError::WrongPassword)
    ));
    let opened = open(&bytes, Some("correct horse battery")).unwrap();
    assert!(opened.locked);
    assert_eq!(opened.secrets["gitea-token"], "SUPER-SECRET-TOKEN");
    assert_eq!(opened.secrets.len(), 2);
}

#[test]
fn short_password_is_refused() {
    let t = Temp::new("short");
    let store = filled(&t.0);
    assert!(export(&store, Some("short")).is_err());
}

#[test]
fn changed_ciphertext_is_refused() {
    let t = Temp::new("tamper");
    let store = filled(&t.0);
    let bytes = export(&store, Some("correct horse battery")).unwrap();
    let mut file: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let data = file["sealed"]["data"].as_str().unwrap().to_string();
    let mut raw = B64.decode(data).unwrap();
    raw[0] ^= 1;
    file["sealed"]["data"] = B64.encode(raw).into();
    let bad = serde_json::to_vec(&file).unwrap();
    assert!(matches!(
        open(&bad, Some("correct horse battery")),
        Err(OpenError::WrongPassword)
    ));
}

#[test]
fn excessive_key_cost_is_refused() {
    let t = Temp::new("cost");
    let store = filled(&t.0);
    let bytes = export(&store, Some("correct horse battery")).unwrap();
    let mut file: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    file["sealed"]["memory_kib"] = 4_000_000u32.into();
    let bad = serde_json::to_vec(&file).unwrap();
    assert!(matches!(
        open(&bad, Some("correct horse battery")),
        Err(OpenError::Invalid(_))
    ));
}

#[test]
fn not_a_backup_is_refused() {
    assert!(matches!(open(b"hello", None), Err(OpenError::Invalid(_))));
    assert!(matches!(
        open(b"{\"format\":\"other\",\"version\":1}", None),
        Err(OpenError::Invalid(_))
    ));
}

#[test]
fn restore_into_a_fresh_folder() {
    let from = Temp::new("from");
    let bytes = export(&filled(&from.0), Some("correct horse battery")).unwrap();
    let to = Temp::new("to");
    let store = Store::open_isolated(&to.0).unwrap();
    let opened = open(&bytes, Some("correct horse battery")).unwrap();
    let summary = check(&opened, &Config::from_toml("").unwrap()).unwrap();
    assert_eq!(summary.sources, vec![("gitea".to_string(), 1)]);
    assert_eq!(summary.secrets, 2);
    restore(&store, &opened).unwrap();
    assert_eq!(store.read_sources().unwrap(), SOURCES);
    assert_eq!(
        store.read_secret("gitea-token").unwrap(),
        "SUPER-SECRET-TOKEN"
    );
    assert_eq!(
        store.read_data_file("layouts.json").unwrap().as_deref(),
        Some("{}")
    );
}

#[test]
fn invalid_sources_are_caught_before_anything_is_written() {
    let backup = Backup {
        sources: "[[source.gitea]]\nname = 1\n".into(),
        ..Backup::default()
    };
    assert!(check(&backup, &Config::from_toml("").unwrap()).is_err());
}

#[test]
fn bad_secret_name_is_refused() {
    let mut backup = Backup {
        sources: SOURCES.into(),
        ..Backup::default()
    };
    backup.secrets.insert("../evil".into(), "x".into());
    assert!(check(&backup, &Config::from_toml("").unwrap()).is_err());
}
