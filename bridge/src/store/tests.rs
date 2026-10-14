use super::*;
use crate::config::Config;

/// A fresh empty folder under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("deskwatch-store-{tag}-{}", std::process::id()));
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

#[test]
fn open_creates_the_folders_and_secrets_round_trip() {
    let temp = Temp::new("secrets");
    let store = Store::open(&temp.0.join("data")).unwrap();
    assert!(!store.has_secret("github-main"));
    store.write_secret("github-main", "  ghp_abc\n").unwrap();
    assert!(store.has_secret("github-main"));
    // Stored trimmed, and readable by the same rule load_secret uses.
    let path = temp.0.join("data/credentials/github-main");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ghp_abc");
    // Replacing keeps one file and leaves no temporary behind.
    store.write_secret("github-main", "ghp_new").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ghp_new");
    let names: Vec<_> = std::fs::read_dir(temp.0.join("data/credentials"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["github-main"]);
    store.remove_secret("github-main").unwrap();
    store.remove_secret("github-main").unwrap(); // already gone is fine
    assert!(!store.has_secret("github-main"));
}

#[cfg(unix)]
#[test]
fn secrets_are_private_to_the_bridge_user() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new("modes");
    let store = Store::open(&temp.0.join("data")).unwrap();
    store.write_secret("t", "x").unwrap();
    let mode = |p: &str| {
        std::fs::metadata(temp.0.join(p))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("data/credentials/t"), 0o600);
    assert_eq!(mode("data/credentials"), 0o700);
}

#[test]
fn bad_secret_names_and_values_are_refused() {
    let temp = Temp::new("names");
    let store = Store::open(&temp.0.join("data")).unwrap();
    for name in [
        "",
        "../x",
        "a/b",
        "a\\b",
        "with space",
        ".hidden",
        "/etc/passwd",
    ] {
        assert!(store.write_secret(name, "v").is_err(), "{name:?}");
        assert!(!store.has_secret(name));
    }
    assert!(store.write_secret("ok", "  \n").is_err());
    assert!(!store.has_secret("ok"));
}

#[test]
fn sources_file_round_trips_and_missing_means_empty() {
    let temp = Temp::new("sources");
    let store = Store::open(&temp.0.join("data")).unwrap();
    assert_eq!(store.read_sources().unwrap(), "");
    let text =
        "[[source.github]]\nname = \"extra\"\ntoken_file = \"github-extra\"\nrepos = [\"o/r\"]\n";
    store.write_sources(text).unwrap();
    assert_eq!(store.read_sources().unwrap(), text);
}

#[test]
fn a_read_only_folder_stops_startup_with_the_reason() {
    // A file where the folder should be: cannot be created, on every platform.
    let temp = Temp::new("blocked");
    let blocker = temp.0.join("file");
    std::fs::write(&blocker, "x").unwrap();
    let err = format!("{:#}", Store::open(&blocker.join("data")).unwrap_err());
    assert!(err.contains("data folder"), "{err}");
}

const BASE: &str =
    "[[source.github]]\nname = \"main\"\ntoken_file = \"github-main\"\nrepos = [\"o/a\"]\n";
const EXTRA: &str = "[[source.github]]\nname = \"extra\"\ntoken_file = \"github-extra\"\nrepos = [\"o/b\"]\n\
                     [[source.gitea]]\nname = \"home\"\nbase_url = \"https://g.example\"\nwebhook_secret_file = \"gitea-hook\"\n";

#[test]
fn overlay_sources_are_added_to_the_main_config() {
    let config = Config::from_toml(BASE)
        .unwrap()
        .with_overlay(EXTRA)
        .unwrap();
    let names: Vec<&str> = config
        .source
        .github
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["main", "extra"]);
    assert_eq!(config.source.gitea.len(), 1);
    // An empty overlay changes nothing.
    assert_eq!(
        Config::from_toml(BASE)
            .unwrap()
            .with_overlay("")
            .unwrap()
            .source
            .github
            .len(),
        1
    );
}

#[test]
fn overlay_cannot_replace_a_source_or_change_other_settings() {
    let dup = "[[source.github]]\nname = \"main\"\ntoken_file = \"x\"\nrepos = [\"o/z\"]\n";
    let err = format!(
        "{:#}",
        Config::from_toml(BASE)
            .unwrap()
            .with_overlay(dup)
            .unwrap_err()
    );
    assert!(err.contains("both bridge.toml"), "{err}");
    // Only [[source.*]] and the few [mqtt] keys the page sets belong in the overlay.
    for bad in ["[kiosk]\nenabled = true\n", "[mqtt]\nclient_id = \"x\"\n"] {
        assert!(
            Config::from_toml("").unwrap().with_overlay(bad).is_err(),
            "{bad}"
        );
    }
    // A broken source in the overlay is caught by the normal checks.
    let broken = "[[source.github]]\nname = \"b\"\ntoken_file = \"t\"\nrepos = [\"not a repo\"]\n";
    assert!(Config::from_toml("").unwrap().with_overlay(broken).is_err());
}

#[test]
fn data_dir_comes_from_the_config() {
    let config = Config::from_toml("[data]\ndir = \"/var/lib/deskwatch\"\n").unwrap();
    assert_eq!(
        config.data_dir().as_deref(),
        Some(Path::new("/var/lib/deskwatch"))
    );
}
