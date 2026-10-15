use serde_json::json;

use super::*;
use crate::config::Config;

fn fields(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn secrets(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn github(current: &str, create: bool, f: Value, s: &[(&str, &str)]) -> Result<Plan> {
    upsert(current, "github", "main", create, &fields(f), &secrets(s))
}

#[test]
fn adding_a_source_writes_a_file_the_config_accepts_and_a_secret_by_name() {
    let plan = github(
        "",
        true,
        json!({ "base_url": "https://api.github.com", "repos": ["o/a", " o/b ", ""] }),
        &[("token", "  ghp_secret  ")],
    )
    .unwrap();
    assert!(plan.text.contains("[[source.github]]"), "{}", plan.text);
    assert!(plan.text.contains("token_file = \"github-main-token\""));
    assert!(
        !plan.text.contains("ghp_secret"),
        "the token must not be in the file"
    );
    assert_eq!(plan.secrets.len(), 1);
    assert_eq!(plan.secrets[0].0, "github-main-token");
    assert_eq!(plan.secrets[0].1.expose(), "ghp_secret");

    let config = Config::from_toml("")
        .unwrap()
        .with_overlay(&plan.text)
        .unwrap();
    assert_eq!(config.source.github[0].repos, ["o/a", "o/b"]);
}

#[test]
fn editing_keeps_settings_the_page_does_not_know_and_a_blank_secret() {
    let current = "[[source.github]]\nname = \"main\"\ntoken_file = \"github-main-token\"\n\
                   repos = [\"o/a\"]\npoll_s = 120\ninterrupt = [\"o/a\"]\n\
                   [source.github.alias]\n\"o/a\" = \"A\"\n";
    let plan = github(
        current,
        false,
        json!({ "repos": ["o/a", "o/c"] }),
        &[("token", "  ")],
    )
    .unwrap();
    assert!(plan.secrets.is_empty(), "a blank token keeps the old one");
    let config = Config::from_toml("")
        .unwrap()
        .with_overlay(&plan.text)
        .unwrap();
    let source = &config.source.github[0];
    assert_eq!(source.repos, ["o/a", "o/c"]);
    assert_eq!(source.poll_s, 120);
    assert_eq!(source.alias.get("o/a").map(String::as_str), Some("A"));
    assert_eq!(source.token_file, "github-main-token");
}

#[test]
fn replacing_a_token_points_the_source_at_the_pages_own_credential() {
    let current = "[[source.github]]\nname = \"main\"\ntoken_file = \"old-mounted-name\"\nrepos = [\"o/a\"]\n";
    let plan = github(current, false, json!({}), &[("token", "new")]).unwrap();
    assert!(plan.text.contains("token_file = \"github-main-token\""));
    assert_eq!(plan.secrets[0].0, "github-main-token");
}

#[test]
fn bad_requests_are_refused_with_a_reason() {
    let one = "[[source.github]]\nname = \"main\"\ntoken_file = \"t\"\nrepos = [\"o/a\"]\n";
    let err = |r: Result<Plan>| format!("{:#}", r.unwrap_err());
    assert!(
        err(github(one, true, json!({}), &[("token", "x")])).contains("already in sources.toml")
    );
    assert!(err(github("", false, json!({}), &[])).contains("no github source named"));
    assert!(err(github("", true, json!({ "repos": ["o/a"] }), &[])).contains("token is required"));
    assert!(
        err(github(
            "",
            true,
            json!({ "token_file": "x" }),
            &[("token", "t")]
        ))
        .contains("cannot be set")
    );
    assert!(
        err(github(
            "",
            true,
            json!({ "base_url": "" }),
            &[("token", "t")]
        ))
        .contains("must not be empty")
    );
    assert!(
        err(github(
            "",
            true,
            json!({ "repos": "o/a" }),
            &[("token", "t")]
        ))
        .contains("must be a list")
    );
    assert!(err(github("", true, json!({}), &[("password", "t")])).contains("no secret called"));
    assert!(
        err(upsert(
            "",
            "github",
            "a b",
            true,
            &Map::new(),
            &secrets(&[("token", "t")])
        ))
        .contains("letters, digits")
    );
    assert!(
        err(upsert("", "svn", "x", true, &Map::new(), &secrets(&[])))
            .contains("unknown source type")
    );
    assert!(
        err(github("not = [valid", true, json!({}), &[("token", "t")])).contains("not valid TOML")
    );
}

#[test]
fn gitea_needs_its_webhook_secret_and_may_skip_the_token() {
    let f = fields(json!({ "base_url": "https://git.example", "repos": ["o/r"] }));
    let err = upsert("", "gitea", "home", true, &f, &secrets(&[])).unwrap_err();
    assert!(format!("{err:#}").contains("webhook_secret is required"));
    let plan = upsert(
        "",
        "gitea",
        "home",
        true,
        &f,
        &secrets(&[("webhook_secret", "h")]),
    )
    .unwrap();
    assert!(
        plan.text
            .contains("webhook_secret_file = \"gitea-home-webhook_secret\"")
    );
    assert!(!plan.text.contains("token_file"));
    Config::from_toml("")
        .unwrap()
        .with_overlay(&plan.text)
        .unwrap();
}

#[test]
fn azure_devops_booleans_and_prometheus_host_tables() {
    let f = fields(
        json!({ "organization": "acme", "base_url": "https://dev.azure.com", "projects": ["P"], "pull_requests": true }),
    );
    let plan = upsert(
        "",
        "azure_devops",
        "work",
        true,
        &f,
        &secrets(&[("token", "t")]),
    )
    .unwrap();
    let config = Config::from_toml("")
        .unwrap()
        .with_overlay(&plan.text)
        .unwrap();
    assert!(config.source.azure_devops[0].pull_requests);

    // A host with its own settings is not flattened by a list edit.
    let current = "[[source.prometheus]]\nname = \"p\"\nurl = \"http://p\"\n\
                   hosts = [{ instance = \"a:9100\", cadvisor = \"c:8080\" }]\n";
    let err = upsert(
        current,
        "prometheus",
        "p",
        false,
        &fields(json!({ "hosts": ["a:9100"] })),
        &secrets(&[]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("by hand"));
    // Other fields of the same source can still change.
    let plan = upsert(
        current,
        "prometheus",
        "p",
        false,
        &fields(json!({ "url": "http://q" })),
        &secrets(&[]),
    )
    .unwrap();
    let config = Config::from_toml("")
        .unwrap()
        .with_overlay(&plan.text)
        .unwrap();
    assert_eq!(config.source.prometheus[0].url, "http://q");
    assert_eq!(
        config.source.prometheus[0].hosts[0].cadvisor.as_deref(),
        Some("c:8080")
    );
}

#[test]
fn removing_returns_the_credentials_and_leaves_other_sources() {
    let current = "[[source.github]]\nname = \"a\"\ntoken_file = \"github-a-token\"\nrepos = [\"o/a\"]\n\
                   [[source.github]]\nname = \"b\"\ntoken_file = \"github-b-token\"\nrepos = [\"o/b\"]\n";
    let (text, used) = remove(current, "github", "a").unwrap();
    assert_eq!(used, ["github-a-token"]);
    let config = Config::from_toml("").unwrap().with_overlay(&text).unwrap();
    assert_eq!(config.source.github.len(), 1);
    assert_eq!(config.source.github[0].name, "b");
    // The last one leaves an empty file.
    let (text, _) = remove(&text, "github", "b").unwrap();
    assert_eq!(text.trim(), "");
    assert!(remove(&text, "github", "b").is_err());
}

#[test]
fn mqtt_settings_are_written_as_a_patch_the_config_applies() {
    let plan = set_mqtt(
        "",
        &fields(json!({
            "host": " broker.lan ", "port": 8883, "tls": true, "enabled": true,
            "username": "deskwatch-bridge",
        })),
        &secrets(&[("password", "  s3cret  ")]),
    )
    .unwrap();
    assert!(plan.text.contains("[mqtt]"), "{}", plan.text);
    assert!(plan.text.contains("password_file = \"mqtt-password\""));
    assert!(
        !plan.text.contains("s3cret"),
        "the password must not be in the file"
    );
    assert_eq!(plan.secrets.len(), 1);
    assert_eq!(plan.secrets[0].0, "mqtt-password");
    assert_eq!(plan.secrets[0].1.expose(), "s3cret");

    // Keys the base config set and the patch leaves alone stay.
    let base = Config::from_toml(
        "[mqtt]\nclient_id = \"mine\"\ntopic_prefix = \"desk\"\nkeep_alive_s = 60\n",
    )
    .unwrap();
    let config = base.with_overlay(&plan.text).unwrap();
    assert_eq!(config.mqtt.host, "broker.lan");
    assert_eq!(config.mqtt.port, 8883);
    assert!(config.mqtt.tls);
    assert_eq!(config.mqtt.username.as_deref(), Some("deskwatch-bridge"));
    assert_eq!(config.mqtt.password_file.as_deref(), Some("mqtt-password"));
    assert_eq!(config.mqtt.client_id, "mine");
    assert_eq!(config.mqtt.topic_prefix, "desk");
    assert_eq!(config.mqtt.keep_alive_s, 60);

    // Without the overlay it is bridge.toml again.
    let back = config.without_overlay();
    assert_eq!(back.mqtt.host, "localhost");
    assert!(back.mqtt_before_overlay.is_none());
}

#[test]
fn mqtt_edits_keep_sources_and_a_blank_password_and_can_clear_the_login() {
    let first = set_mqtt(
        "",
        &fields(json!({ "username": "u" })),
        &secrets(&[("password", "p")]),
    )
    .unwrap();
    let with_source = github(
        &first.text,
        true,
        json!({ "repos": ["o/a"] }),
        &[("token", "t")],
    )
    .unwrap()
    .text;
    let second = set_mqtt(
        &with_source,
        &fields(json!({ "username": "" })),
        &secrets(&[("password", "  ")]),
    )
    .unwrap();
    assert!(
        second.secrets.is_empty(),
        "a blank password keeps the saved one"
    );
    assert!(second.text.contains("password_file = \"mqtt-password\""));
    assert!(second.text.contains("[[source.github]]"));
    let config = Config::from_toml("[mqtt]\nusername = \"old\"\n")
        .unwrap()
        .with_overlay(&second.text)
        .unwrap();
    assert_eq!(
        config.mqtt.username, None,
        "an empty name switches the login off"
    );
}

#[test]
fn bad_mqtt_requests_are_refused() {
    let no_secrets = secrets(&[]);
    for bad in [
        json!({ "port": 0 }),
        json!({ "port": 70000 }),
        json!({ "port": "1883" }),
        json!({ "host": "  " }),
        json!({ "tls": "yes" }),
        json!({ "client_id": "x" }),
        json!({ "password_file": "/etc/passwd" }),
    ] {
        assert!(
            set_mqtt("", &fields(bad.clone()), &no_secrets).is_err(),
            "{bad}"
        );
    }
    assert!(set_mqtt("", &fields(json!({})), &secrets(&[("token", "x")])).is_err());
}

#[test]
fn a_bridge_toml_source_can_be_switched_off_and_on_without_losing_other_settings() {
    let kept = "[kiosk]\nhost_view = \"compact\"\n";
    let off = set_hidden(kept, "github", "main", true).unwrap();
    assert!(off.contains("hidden = [\"github:main\"]"), "{off}");
    assert!(off.contains("host_view"), "{off}");
    // Twice is still once.
    let twice = set_hidden(&off, "github", "main", true).unwrap();
    assert_eq!(twice.matches("github:main").count(), 1, "{twice}");
    let other = set_hidden(&off, "gitea", "home", true).unwrap();
    assert!(other.contains("gitea:home") && other.contains("github:main"));
    let on = set_hidden(&off, "github", "main", false).unwrap();
    assert!(!on.contains("hidden"), "{on}");
    assert!(on.contains("host_view"));
    assert!(set_hidden("", "nope", "x", true).is_err());
}

#[test]
fn removing_the_broker_login_or_all_saved_broker_settings() {
    let saved = "[mqtt]\nhost = \"b.lan\"\nusername = \"me\"\npassword_file = \"mqtt-password\"\n\
                 [kiosk]\nhost_view = \"compact\"\n";
    let (text, creds) = remove_mqtt(saved, MqttRemoval::Login).unwrap();
    assert_eq!(creds, ["mqtt-password"]);
    assert!(
        text.contains("username = \"\"") && text.contains("host = \"b.lan\""),
        "{text}"
    );
    assert!(!text.contains("password_file"), "{text}");
    let overlay = Config::from_toml("[mqtt]\nusername = \"x\"\n")
        .unwrap()
        .with_overlay(&text)
        .unwrap();
    assert_eq!(overlay.mqtt.username, None);

    let (text, creds) = remove_mqtt(saved, MqttRemoval::Settings).unwrap();
    assert_eq!(creds, ["mqtt-password"]);
    assert!(
        !text.contains("mqtt") && text.contains("host_view"),
        "{text}"
    );

    // A password file that is not the page's own is never named for deletion.
    let mounted = "[mqtt]\npassword_file = \"mounted-name\"\n";
    assert!(
        remove_mqtt(mounted, MqttRemoval::Settings)
            .unwrap()
            .1
            .is_empty()
    );
}
