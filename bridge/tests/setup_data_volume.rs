//! Saving a source on the setup page, end to end, against the real binary.
//!
//! This is the Docker named volume case (issue 71): an empty, writable data
//! folder that the bridge fills through the setup page, and that has to be
//! there again after the bridge is restarted. The unit tests in
//! `setup/tests.rs` call the routes in one process; this test starts the
//! `deskwatch-bridge` binary twice, so the startup wiring in `main.rs`
//! (opening the folder, reading `sources.toml`, starting the sources) is
//! covered as well.
//!
//! Only mock servers are used: a small Gitea on a local port, and tokens that
//! are made up here. No broker is needed (`DESKWATCH_NO_MQTT`).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use serde_json::{Value, json};

const TOKEN: &str = "volume-test-token";
const HOOK_SECRET: &str = "volume-test-hook-secret";

/// A folder that is removed when the test ends, also when it fails.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("deskwatch-{tag}-{}", std::process::id()));
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

/// A running bridge, stopped when dropped so a failed test leaves nothing behind.
struct Bridge {
    child: Child,
    base: String,
}

impl Bridge {
    /// Start the binary with a config that serves the dashboard and the setup
    /// page on a free port and keeps its data in `data`.
    async fn start(dir: &Path, data: &Path) -> Self {
        let port = free_port();
        let config = dir.join("bridge.toml");
        std::fs::write(
            &config,
            format!(
                "[http]\nlisten = \"127.0.0.1:{port}\"\n\
                 [mqtt]\nenabled = false\n\
                 [setup]\nenabled = true\n\
                 [kiosk]\nenabled = true\n\
                 [data]\ndir = {:?}\n",
                data.to_str().unwrap()
            ),
        )
        .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_deskwatch-bridge"))
            .arg(&config)
            .env("RUST_LOG", "warn")
            .env("DESKWATCH_NO_MQTT", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the bridge binary starts");
        let bridge = Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
        };
        bridge.wait_until_ready().await;
        bridge
    }

    async fn wait_until_ready(&self) {
        for _ in 0..200 {
            if http()
                .get(self.url("/api/setup/status"))
                .send()
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the bridge did not start listening on {}", self.base);
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn json(&self, path: &str) -> Value {
        let response = http().get(self.url(path)).send().await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
        response.json().await.unwrap()
    }

    /// The setup status, once the source `id` is running (it starts a moment
    /// after the bridge listens).
    async fn status_with_running(&self, id: &str) -> Value {
        let mut last = Value::Null;
        for _ in 0..100 {
            last = self.json("/api/setup/status").await;
            if find(&last["sources"], id).is_some_and(|s| s["running"] == true) {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("{id} never ran: {last}");
    }

    /// The dashboard snapshot, once it lists the source `id` under `sources`.
    async fn kiosk_with_source(&self, id: &str) -> Value {
        let mut last = Value::Null;
        for _ in 0..100 {
            last = self.json("/api/kiosk").await;
            if last["sources"]
                .as_array()
                .is_some_and(|all| all.iter().any(|s| s["id"] == id))
            {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the dashboard never listed {id}: {last}");
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn find<'a>(sources: &'a Value, id: &str) -> Option<&'a Value> {
    sources.as_array()?.iter().find(|s| s["id"] == id)
}

/// Just enough of Gitea for the connection test: the version, the token
/// check and one repository. Everything else answers 404.
async fn mock_gitea() -> String {
    let app = Router::new()
        .route(
            "/api/v1/version",
            get(|| async { "{\"version\":\"1.22\"}" }),
        )
        .route(
            "/api/v1/user",
            get(|headers: HeaderMap| async move {
                let given = headers.get("authorization").and_then(|v| v.to_str().ok());
                if given == Some(&format!("token {TOKEN}")) {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            }),
        )
        .route("/api/v1/repos/o/r", get(|| async { "{}" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

#[tokio::test]
async fn a_source_saved_on_the_setup_page_is_live_and_survives_a_restart() {
    let temp = Temp::new("volume");
    // Like the empty, writable /var/lib/deskwatch of the Docker image.
    let data = temp.0.join("volume");
    std::fs::create_dir_all(&data).unwrap();
    let gitea = mock_gitea().await;

    // First run: nothing configured, but the page can save.
    let bridge = Bridge::start(&temp.0, &data).await;
    let status = bridge.json("/api/setup/status").await;
    assert_eq!(status["can_edit"], true, "{status}");
    assert!(status["sources"].as_array().unwrap().is_empty());

    // A wrong token is tested first and nothing is written.
    let request = |token: &str| {
        json!({
            "kind": "gitea", "name": "home", "create": true,
            "fields": { "base_url": gitea, "repos": ["o/r"] },
            "secrets": { "token": token, "webhook_secret": HOOK_SECRET },
        })
    };
    let save = |token: &str| {
        let body = request(token);
        let url = bridge.url("/api/setup/source");
        async move {
            let response = http().post(url).json(&body).send().await.unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        }
    };
    let refused = save("wrong-token").await;
    assert_eq!(refused["saved"], false, "{refused}");
    assert!(
        !data.join("sources.toml").exists() || {
            !std::fs::read_to_string(data.join("sources.toml"))
                .unwrap()
                .contains("home")
        }
    );

    // The right token is saved and the source starts without a restart.
    let saved = save(TOKEN).await;
    assert_eq!(saved["saved"], true, "{saved}");
    assert_eq!(saved["applied"], true, "{saved}");
    assert_eq!(saved["restart_needed"], false, "{saved}");
    let status = bridge.status_with_running("gitea:home").await;
    let source = find(&status["sources"], "gitea:home").unwrap();
    assert_eq!(source["editable"], true);
    assert_eq!(source["origin"], "data_folder");
    bridge.kiosk_with_source("gitea:home").await;

    // It is on disk in the volume, the secrets with the owner-only mode, and
    // the page never sends a secret back.
    let sources = std::fs::read_to_string(data.join("sources.toml")).unwrap();
    assert!(
        sources.contains("gitea") && sources.contains("home"),
        "{sources}"
    );
    assert!(!sources.contains(TOKEN) && !sources.contains(HOOK_SECRET));
    assert_eq!(
        std::fs::read_to_string(data.join("credentials/gitea-home-token")).unwrap(),
        TOKEN
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(data.join("credentials/gitea-home-token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let text = status.to_string();
    assert!(!text.contains(TOKEN) && !text.contains(HOOK_SECRET));
    bridge.stop();

    // Second run, a fresh process on the same volume: the source comes back
    // by itself, from the folder, and runs.
    let bridge = Bridge::start(&temp.0, &data).await;
    let status = bridge.status_with_running("gitea:home").await;
    let source = find(&status["sources"], "gitea:home").unwrap();
    assert_eq!(source["origin"], "data_folder");
    assert_eq!(source["editable"], true);
    for credential in source["credentials"].as_array().unwrap() {
        assert_eq!(credential["status"], "data_folder", "{source}");
    }
    assert!(!status.to_string().contains(TOKEN));
    bridge.kiosk_with_source("gitea:home").await;
    bridge.stop();
}
