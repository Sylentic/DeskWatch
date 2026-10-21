use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

use super::*;
use crate::alerts::AlertMessage;
use crate::ci::{
    OpenPull, RepoPulls, Run, RunJob, RunStatus, Runner, RunnerStatus, RunningJob, Update,
};
use crate::fleet::{Down, HostFacts, StatsReport};
use crate::model::{JobData, JobKind, RotationEntry, StatsData};
use crate::source::{Health, SourceId};

const NOW: u64 = 1_000_000;

fn composer() -> Composer {
    let rotation = vec![RotationEntry {
        page: "stats".into(),
        dwell_s: 10,
        skip_when_empty: false,
    }];
    Composer::new(rotation, NOW)
}

fn job(project: &str, started: u64, interrupt: bool) -> RunningJob {
    RunningJob {
        data: JobData {
            source: "gitea".into(),
            project: project.into(),
            pipeline: "deploy.yml".into(),
            kind: JobKind::Deploy,
            git_ref: "main".into(),
            commit: "3f9ac21".into(),
            step: Some("Migrate".into()),
            step_no: Some(2),
            step_count: Some(4),
            progress: Some(0.5),
            started,
            others: 0,
        },
        interrupt,
        updated: NOW,
        url: None,
        group: None,
    }
}

fn run(pipeline: &str, status: RunStatus) -> Run {
    Run {
        source: "gitea".into(),
        project: "webshop".into(),
        pipeline: pipeline.into(),
        git_ref: "main".into(),
        status,
        step: Some("tests".into()),
        started: NOW - 100,
        finished: Some(NOW - 50),
        interrupt: false,
        url: None,
    }
}

/// A composer that knows a bit of everything.
fn busy() -> Composer {
    let mut c = composer();
    c.set_stats(StatsData {
        host: "homeserver".into(),
        cpu_pct: Some(12.0),
        ..Default::default()
    });
    let facts = |c: &mut Composer, update| c.ci.apply(update, NOW);
    facts(
        &mut c,
        Update::JobRunning {
            key: "g:1".into(),
            job: job("webshop", NOW - 60, true),
        },
    );
    facts(
        &mut c,
        Update::JobRunning {
            key: "g:2".into(),
            job: job("docs", NOW - 10, false),
        },
    );
    facts(
        &mut c,
        Update::Run {
            key: "g:ok".into(),
            run: run("ci.yml", RunStatus::Success),
        },
    );
    facts(
        &mut c,
        Update::Run {
            key: "g:bad".into(),
            run: run("plan.yml", RunStatus::Failed),
        },
    );
    facts(
        &mut c,
        Update::Pulls {
            key: "g:repo".into(),
            repo: RepoPulls {
                project: "webshop".into(),
                source: "gitea".into(),
                pulls: vec![
                    OpenPull {
                        number: 1,
                        title: "Old".into(),
                        created: 10,
                        url: None,
                    },
                    OpenPull {
                        number: 2,
                        title: "New".into(),
                        created: 20,
                        url: None,
                    },
                ],
                total: 5,
            },
        },
    );
    c.fleet.apply(
        &SourceId::new("prometheus", "lab"),
        StatsReport {
            hosts: vec![
                HostFacts {
                    name: "nas".into(),
                    up: true,
                    stats: StatsData::default(),
                },
                // Same name as the local host: shown once.
                HostFacts {
                    name: "homeserver".into(),
                    up: true,
                    stats: StatsData::default(),
                },
            ],
            down: vec![Down {
                text: "grafana".into(),
                sub: "nas".into(),
            }],
        },
    );
    c.health.set(&SourceId::new("gitea", "home"), Health::Ok);
    c.health
        .set(&SourceId::new("prometheus", "lab"), Health::Unreachable);
    c.alerts.apply(
        AlertMessage::parse(
            br#"{"v":2,"id":"door","severity":"warning","title":"Door","message":"Open 5 min"}"#,
        )
        .unwrap(),
        NOW,
    );
    c
}

fn kiosk() -> Kiosk {
    Kiosk::new(&KioskConfig::default())
}

fn json_of(kiosk: &Kiosk) -> Value {
    serde_json::from_str(&kiosk.tx.borrow()).unwrap()
}

async fn get(router: &Router, uri: &str) -> (StatusCode, HeaderMap, String) {
    let response = router
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

/// Pull requests, runs, running jobs and runner lists carry the web address
/// the source gave them, so the page can link to it; `null` when there is none.
#[test]
fn snapshot_carries_the_links_of_prs_runs_jobs_and_runners() {
    let mut kiosk = kiosk();
    let mut c = composer();
    let mut job = job("webshop", NOW - 60, true);
    job.url = Some("https://git.example/me/webshop/actions/runs/3".into());
    let mut done = run("ci.yml", RunStatus::Success);
    done.url = Some("https://git.example/me/webshop/actions/runs/2".into());
    let runner = Runner {
        source: "gitea".into(),
        name: "r1".into(),
        status: RunnerStatus::Idle,
        disabled: false,
        labels: vec![],
        url: Some("https://git.example/-/admin/actions/runners".into()),
    };
    let pull = |number, url: Option<&str>| OpenPull {
        number,
        title: "t".into(),
        created: 1,
        url: url.map(String::from),
    };
    c.ci.apply(
        Update::JobRunning {
            key: "j".into(),
            job,
        },
        NOW,
    );
    c.ci.apply(
        Update::Run {
            key: "r".into(),
            run: done,
        },
        NOW,
    );
    c.ci.apply(
        Update::Runners {
            key: "x".into(),
            runners: vec![runner],
        },
        NOW,
    );
    c.ci.apply(
        Update::Pulls {
            key: "p".into(),
            repo: RepoPulls {
                project: "webshop".into(),
                source: "gitea".into(),
                pulls: vec![
                    pull(2, Some("https://git.example/me/webshop/pulls/2")),
                    pull(1, None),
                ],
                total: 2,
            },
        },
        NOW,
    );
    kiosk.update(&mut c, NOW);
    let v = json_of(&kiosk);
    assert_eq!(
        v["jobs"][0]["url"],
        "https://git.example/me/webshop/actions/runs/3"
    );
    assert_eq!(
        v["runs"][0]["url"],
        "https://git.example/me/webshop/actions/runs/2"
    );
    assert_eq!(
        v["runners"][0]["url"],
        "https://git.example/-/admin/actions/runners"
    );
    assert_eq!(
        v["pulls"][0]["items"][0]["url"],
        "https://git.example/me/webshop/pulls/2"
    );
    assert_eq!(v["pulls"][0]["items"][1]["url"], Value::Null);
}

/// The Pipelines panel shows the latest run first, across sources, even when
/// an older run failed. A running run counts from its start.
#[test]
fn snapshot_lists_the_parallel_jobs_of_a_run() {
    let mut kiosk = kiosk();
    let mut c = composer();
    let lane = |name: &str, status| RunJob {
        name: name.into(),
        status,
        step: None,
        progress: None,
        started: Some(NOW - 30),
        finished: None,
        url: None,
    };
    c.ci.apply(
        Update::RunJobs {
            key: "gitea:home:run:2".into(),
            jobs: vec![
                lane("stable", RunStatus::Success),
                lane("msrv", RunStatus::Running),
            ],
        },
        NOW,
    );
    // A list nobody's job points at stays out of the snapshot.
    c.ci.apply(
        Update::RunJobs {
            key: "gitea:home:run:3".into(),
            jobs: vec![lane("lint", RunStatus::Queued)],
        },
        NOW,
    );
    let mut running = job("webshop", NOW - 30, false);
    running.group = Some("gitea:home:run:2".into());
    c.ci.apply(
        Update::JobRunning {
            key: "gitea:home:me/webshop:8".into(),
            job: running,
        },
        NOW,
    );
    kiosk.update(&mut c, NOW);
    let snap = json_of(&kiosk);
    assert_eq!(snap["jobs"][0]["group"], "gitea:home:run:2");
    let lanes = snap["run_jobs"].as_object().unwrap();
    assert_eq!(lanes.len(), 1);
    let rows: Vec<(&str, &str)> = lanes["gitea:home:run:2"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["name"].as_str().unwrap(), l["status"].as_str().unwrap()))
        .collect();
    assert_eq!(rows, [("stable", "success"), ("msrv", "running")]);

    // The last job of the run ends: its list goes too.
    c.ci.apply(
        Update::JobDone {
            key: "gitea:home:me/webshop:8".into(),
        },
        NOW + 5,
    );
    assert!(c.ci.run_jobs("gitea:home:run:2").is_none());
}

#[test]
fn runs_are_listed_newest_first() {
    let mut kiosk = kiosk();
    let mut c = composer();
    let at = |source: &str, pipeline: &str, status: RunStatus, finished: u64| {
        let mut r = run(pipeline, status);
        r.source = source.into();
        r.started = finished - 30;
        r.finished = Some(finished);
        r
    };
    for (key, r) in [
        (
            "g:old",
            at("gitea", "old.yml", RunStatus::Failed, NOW - 900),
        ),
        (
            "h:new",
            at("github", "new.yml", RunStatus::Success, NOW - 10),
        ),
        (
            "a:mid",
            at("azure_devops", "mid.yml", RunStatus::Success, NOW - 300),
        ),
        (
            "g:run",
            Run {
                finished: None,
                ..at("gitea", "live.yml", RunStatus::Running, NOW - 60)
            },
        ),
    ] {
        c.ci.apply(
            Update::Run {
                key: key.into(),
                run: r,
            },
            NOW,
        );
    }
    kiosk.update(&mut c, NOW);
    let snap = json_of(&kiosk);
    let order: Vec<&str> = snap["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["pipeline"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["new.yml", "live.yml", "mid.yml", "old.yml"]);
}

#[test]
fn snapshot_carries_every_fact() {
    let mut kiosk = kiosk();
    let mut c = busy();
    kiosk.update(&mut c, NOW);
    let snap = json_of(&kiosk);

    assert_eq!(snap["v"], 1);
    assert_eq!(snap["now"], NOW);
    assert_eq!(snap["layout"]["columns"], 4);
    assert_eq!(snap["layout"]["panels"][0]["widget"], "jobs");
    assert_eq!(snap["layout"]["panels"][1]["widget"], "hosts");

    // Local host first, the duplicate name only once.
    let names: Vec<&str> = snap["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["homeserver", "nas"]);
    assert_eq!(snap["down"][0]["text"], "grafana");

    // All jobs, newest first, including the one that may not interrupt.
    let jobs = snap["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0]["project"], "docs");
    assert_eq!(jobs[1]["ref"], "main");
    assert_eq!(jobs[1]["step_no"], 2);
    assert!(jobs[1].get("others").is_none());

    // Failed runs come before successful ones.
    assert_eq!(snap["runs"][0]["status"], "failed");
    assert_eq!(snap["runs"][1]["status"], "success");

    // PRs are not capped to the five panel rows, newest first, with the total.
    assert_eq!(snap["pulls"][0]["total"], 5);
    assert_eq!(snap["pulls"][0]["items"][0]["number"], 2);

    assert_eq!(snap["alerts"][0]["title"], "Door");
    assert_eq!(snap["alerts"][0]["severity"], "warn");
    let sources: Vec<(&str, &str)> = snap["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), s["health"].as_str().unwrap()))
        .collect();
    assert_eq!(
        sources,
        [("gitea:home", "ok"), ("prometheus:lab", "unreachable")]
    );

    // The composer's pick is a critical alert or the running job here; the
    // warning alert is level 2, below the running job (level 1).
    assert_eq!(snap["screen"]["level"], 1);
    assert_eq!(snap["screen"]["template"], "job");
    assert!(
        snap["badges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == "prs" && b["count"] == 5)
    );
}

#[test]
fn snapshot_lists_runners_offline_first() {
    let mut kiosk = kiosk();
    let mut c = composer();
    let runner = |name: &str, status, disabled| Runner {
        source: "gitea".into(),
        name: name.into(),
        status,
        disabled,
        labels: vec!["ubuntu-latest".into()],
        url: None,
    };
    c.ci.apply(
        Update::Runners {
            key: "gitea:home:runners:user".into(),
            runners: vec![
                runner("idle-1", RunnerStatus::Idle, false),
                runner("parked", RunnerStatus::Offline, true),
                runner("down-1", RunnerStatus::Offline, false),
                runner("busy-1", RunnerStatus::Busy, false),
            ],
        },
        NOW,
    );
    kiosk.update(&mut c, NOW);
    let snap = json_of(&kiosk);
    let rows: Vec<(&str, &str, bool)> = snap["runners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap(),
                r["status"].as_str().unwrap(),
                r["disabled"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("down-1", "offline", false),
            ("busy-1", "busy", false),
            ("idle-1", "idle", false),
            ("parked", "offline", true),
        ]
    );
    assert_eq!(snap["runners"][0]["labels"][0], "ubuntu-latest");
    assert_eq!(snap["runners"][0]["updated"], NOW);
    assert_eq!(snap["runners"][0]["source"], "gitea");
}

#[test]
fn layout_carries_theme_pixel_shift_and_panel_filters() {
    // Defaults leave the look unchanged: day theme, no pixel shift, no filters.
    let mut kiosk = kiosk();
    kiosk.update(&mut busy(), NOW);
    let layout = &json_of(&kiosk)["layout"];
    assert_eq!(layout["theme"]["mode"], "day");
    assert_eq!(layout["pixel_shift_s"], 0);
    assert!(layout["panels"][2].get("source").is_none());

    let config = crate::config::Config::from_toml(
        r#"
[kiosk]
theme = "auto"
night_from = "23:00"
night_to = "06:30"
pixel_shift = true
pixel_shift_minutes = 4

[[kiosk.panel]]
widget = "pipelines"
source = "github"
repo = "acme/*"
"#,
    )
    .unwrap();
    let mut kiosk = Kiosk::new(&config.kiosk);
    kiosk.update(&mut busy(), NOW);
    let layout = &json_of(&kiosk)["layout"];
    assert_eq!(layout["theme"]["mode"], "auto");
    assert_eq!(layout["theme"]["night_from"], "23:00");
    assert_eq!(layout["theme"]["night_to"], "06:30");
    assert_eq!(layout["pixel_shift_s"], 240);
    assert_eq!(layout["panels"][0]["source"], "github");
    assert_eq!(layout["panels"][0]["repo"], "acme/*");
}

#[test]
fn page_has_a_night_theme_and_pixel_shift() {
    assert!(KIOSK_CSS.contains(":root[data-theme=\"night\"]"));
    assert!(KIOSK_JS.contains("data-theme"));
    assert!(KIOSK_JS.contains("pixel_shift_s"));
    // Running stays orange in the night theme too.
    assert!(KIOSK_CSS.contains("--orange: #c85200"));
}

#[test]
fn idle_snapshot_has_no_screen() {
    let mut kiosk = kiosk();
    let mut c = composer();
    kiosk.update(&mut c, NOW);
    let snap = json_of(&kiosk);
    assert!(snap["screen"].is_null());
    assert_eq!(snap["jobs"], json!([]));
    assert_eq!(snap["runners"], json!([]));
    assert_eq!(snap["hosts"], json!([]));
}

#[test]
fn snapshot_is_sent_on_change_and_on_the_heartbeat_only() {
    let mut kiosk = kiosk();
    let mut rx = kiosk.tx.subscribe();
    let mut c = composer();

    kiosk.update(&mut c, NOW);
    assert!(rx.has_changed().unwrap());
    rx.borrow_and_update();

    // Nothing changed one second later: nothing is sent.
    kiosk.update(&mut c, NOW + 1);
    assert!(!rx.has_changed().unwrap());

    // A change goes out at once.
    c.set_stats(StatsData {
        host: "homeserver".into(),
        ..Default::default()
    });
    kiosk.update(&mut c, NOW + 2);
    assert!(rx.has_changed().unwrap());
    rx.borrow_and_update();

    // Quiet again until the heartbeat is due, and then the timestamp moves.
    kiosk.update(&mut c, NOW + 2 + HEARTBEAT_S - 1);
    assert!(!rx.has_changed().unwrap());
    kiosk.update(&mut c, NOW + 2 + HEARTBEAT_S);
    assert!(rx.has_changed().unwrap());
    assert_eq!(json_of(&kiosk)["now"], NOW + 2 + HEARTBEAT_S);
}

#[tokio::test]
async fn serves_the_page_and_its_files() {
    let router = kiosk().router();
    let (status, headers, body) = get(&router, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>DeskWatch</title>"));
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );

    let (status, headers, body) = get(&router, "/kiosk.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    assert!(body.contains("DeskWatchKiosk"));
    let (status, _, body) = get(&router, "/kiosk.css").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("--bg"));
}

#[test]
fn page_files_load_nothing_from_other_hosts() {
    // The page must work on a LAN with no internet: no CDN, font or image URLs.
    for (name, text) in [
        ("index.html", INDEX_HTML),
        ("kiosk.css", KIOSK_CSS),
        ("kiosk.js", KIOSK_JS),
    ] {
        assert!(
            !text.contains("https://"),
            "{name} mentions an https:// URL"
        );
        assert!(!text.contains("//cdn"), "{name} mentions a CDN");
    }
}
#[test]
fn page_explains_why_it_has_no_data() {
    // Guard for the failure states: the notice box must exist in the page and
    // the script must have a message for each of them.
    assert!(INDEX_HTML.contains("id=\"notice\""));
    for pill in ["bridge unreachable", "polling, no live socket"] {
        assert!(KIOSK_JS.contains(pill), "page never says '{pill}'");
    }
    assert!(KIOSK_JS.contains("Waiting for the first data"));
    // The page fetches once at start instead of only after the socket fails.
    // (Compared without line breaks: a Windows checkout has CRLF.)
    let flat: String = KIOSK_JS.split_whitespace().collect();
    assert!(flat.contains("poll();connect();"));
}

#[test]
fn page_has_no_login() {
    // The dashboard is open on the trusted network: no token field, no
    // login request, no token in the address bar.
    for text in [KIOSK_JS, INDEX_HTML, KIOSK_CSS] {
        for word in [
            "api/login",
            "token-input",
            "tokenbox",
            "takeToken",
            "?token=",
        ] {
            assert!(!text.contains(word), "the page still mentions {word}");
        }
    }
}

#[tokio::test]
async fn snapshot_endpoint_returns_the_latest_json() {
    let mut kiosk = kiosk();
    let router = kiosk.router();
    let mut c = busy();
    kiosk.update(&mut c, NOW);
    let (status, headers, body) = get(&router, "/api/kiosk").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let snap: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(snap["jobs"].as_array().unwrap().len(), 2);
}
/// A request with free choice of method, headers and body.
async fn send(
    router: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, HeaderMap) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    (response.status(), response.headers().clone())
}
/// Read one unmasked server frame header and payload from a WebSocket.
async fn read_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await.unwrap();
    let len = match head[1] & 0x7f {
        126 => {
            let mut ext = [0u8; 2];
            stream.read_exact(&mut ext).await.unwrap();
            u16::from_be_bytes(ext) as usize
        }
        127 => {
            let mut ext = [0u8; 8];
            stream.read_exact(&mut ext).await.unwrap();
            u64::from_be_bytes(ext) as usize
        }
        n => n as usize,
    };
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.unwrap();
    (head[0] & 0x0f, payload)
}

#[tokio::test]
async fn websocket_sends_the_current_snapshot_and_then_changes() {
    let mut kiosk = kiosk();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, kiosk.router()).into_future());

    let mut c = composer();
    kiosk.update(&mut c, NOW);

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = "GET /api/kiosk/ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                   Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
                   Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
    stream.write_all(request.as_bytes()).await.unwrap();
    // Read the handshake response up to the blank line.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.unwrap());
    }
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 101"));

    let (opcode, payload) = read_frame(&mut stream).await;
    assert_eq!(opcode, 1, "text frame");
    let first: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(first["now"], NOW);

    // A change is pushed without being asked for.
    c.ci.apply(
        Update::Run {
            key: "g:ok".into(),
            run: run("ci.yml", RunStatus::Success),
        },
        NOW,
    );
    kiosk.update(&mut c, NOW + 1);
    let (_, payload) = read_frame(&mut stream).await;
    let next: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(next["runs"][0]["pipeline"], "ci.yml");
}

/// Open the WebSocket and return the stream (after the handshake) or the
/// status line when the bridge refused it.
async fn open_socket(addr: std::net::SocketAddr) -> Result<TcpStream, String> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = "GET /api/kiosk/ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                   Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
                   Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.unwrap());
    }
    let head = String::from_utf8_lossy(&head).to_string();
    if head.starts_with("HTTP/1.1 101") {
        Ok(stream)
    } else {
        Err(head.lines().next().unwrap_or("").to_string())
    }
}

#[tokio::test]
async fn websocket_count_is_capped_and_a_closed_client_frees_its_slot() {
    let config = KioskConfig {
        max_ws_clients: 2,
        ..KioskConfig::default()
    };
    let kiosk = Kiosk::new(&config);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, kiosk.router()).into_future());

    let first = open_socket(addr).await.unwrap();
    let _second = open_socket(addr).await.unwrap();
    let refused = open_socket(addr).await.unwrap_err();
    assert!(refused.contains("503"), "{refused}");

    // The page still works without the socket: polling is not limited.
    assert_eq!(get(&kiosk.router(), "/api/kiosk").await.0, StatusCode::OK);

    // Closing a client gives its slot back (the bridge notices on its next read).
    drop(first);
    let mut reopened = Err(String::new());
    for _ in 0..50 {
        reopened = open_socket(addr).await;
        if reopened.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(reopened.is_ok(), "slot was not freed: {reopened:?}");
}
#[test]
fn queued_jobs_are_in_the_snapshot_busiest_pool_first() {
    let mut kiosk = kiosk();
    let mut c = composer();
    for (pool, queued) in [("Small", 1), ("Big", 5), ("Empty", 0)] {
        c.ci.apply(
            Update::QueuedJobs {
                key: format!("azure_devops:work:queue:pool:{pool}"),
                source: "azure_devops".into(),
                pool: pool.into(),
                queued,
            },
            NOW,
        );
    }
    kiosk.update(&mut c, NOW);
    let snap = json_of(&kiosk);
    let pools: Vec<(&str, u64)> = snap["queued_jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| (q["pool"].as_str().unwrap(), q["queued"].as_u64().unwrap()))
        .collect();
    assert_eq!(pools, [("Big", 5), ("Small", 1)]);
}

#[test]
fn setup_link_is_in_the_layout_only_when_asked() {
    let plain = kiosk();
    assert!(json_of(&plain)["layout"].get("setup").is_none());
    let linked = kiosk().with_setup_link(true);
    assert_eq!(json_of(&linked)["layout"]["setup"], true);
    let off = kiosk().with_setup_link(false);
    assert!(json_of(&off)["layout"].get("setup").is_none());
}

#[test]
fn ticker_settings_travel_in_the_layout() {
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let alerts = crate::config::AlertsConfig {
        ticker_speed: crate::config::TickerSpeed::Slow,
        show_source: false,
        ..Default::default()
    };
    let kiosk = kiosk().with_alerts(&alerts, tx);
    let layout = &json_of(&kiosk)["layout"]["alerts"];
    assert_eq!(layout["ticker"], true);
    assert_eq!(layout["show_source"], false);
    assert_eq!(layout["dismiss"], true);
    assert!(layout["speed"].as_f64().unwrap() < 1.0);
}

#[tokio::test]
async fn dismissing_an_alert_only_clears_it() {
    use crate::mqtt::Inbound;
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let router = kiosk()
        .with_alerts(&crate::config::AlertsConfig::default(), tx)
        .router();
    let uri = "/api/kiosk/alert/dismiss";
    let (status, _) = send(&router, "POST", uri, &[], r#"{"id":"door"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let Some(Inbound::Alert(msg)) = rx.recv().await else {
        panic!("expected a clear")
    };
    assert!(msg.clear);
    assert_eq!(msg.id.as_deref(), Some("door"));

    // No id, or a cross-origin page: nothing happens.
    let (status, _) = send(&router, "POST", uri, &[], "{}").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let evil = [("origin", "https://evil.example"), ("host", "bridge.lan")];
    let (status, _) = send(&router, "POST", uri, &evil, r#"{"id":"door"}"#).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn dismissing_is_refused_when_switched_off() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let alerts = crate::config::AlertsConfig {
        dismiss: false,
        ..Default::default()
    };
    let router = kiosk().with_alerts(&alerts, tx).router();
    let (status, _) = send(
        &router,
        "POST",
        "/api/kiosk/alert/dismiss",
        &[],
        r#"{"id":"door"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(rx.try_recv().is_err());
    assert_eq!(
        json_of(&kiosk().with_alerts(&alerts, tokio::sync::mpsc::channel(1).0))["layout"]["alerts"]
            ["dismiss"],
        false
    );
}

#[tokio::test]
async fn data_needs_no_login() {
    let router = kiosk().router();
    for uri in ["/", "/api/kiosk"] {
        assert_eq!(get(&router, uri).await.0, StatusCode::OK, "{uri}");
    }
    // The old login route is gone, and a stray token or cookie changes nothing.
    assert_eq!(
        send(&router, "POST", "/api/login", &[], "{}").await.0,
        StatusCode::NOT_FOUND
    );
    let with_extras = [
        ("authorization", "Bearer whatever"),
        ("cookie", "deskwatch_session=1.2"),
    ];
    assert_eq!(
        send(&router, "GET", "/api/kiosk", &with_extras, "").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn cross_origin_data_requests_are_refused() {
    let router = kiosk().router();
    let foreign = [
        ("host", "dash.lan:8787"),
        ("origin", "https://evil.example"),
    ];
    for uri in ["/api/kiosk", "/api/kiosk/ws"] {
        assert_eq!(
            send(&router, "GET", uri, &foreign, "").await.0,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    let own = [
        ("host", "dash.lan:8787"),
        ("origin", "http://dash.lan:8787"),
    ];
    assert_eq!(
        send(&router, "GET", "/api/kiosk", &own, "").await.0,
        StatusCode::OK
    );
    // Passes the Origin check (then fails only for lack of an upgrade).
    assert_ne!(
        send(&router, "GET", "/api/kiosk/ws", &own, "").await.0,
        StatusCode::FORBIDDEN
    );
}

#[test]
fn snapshot_lists_the_failed_runs_behind_the_badge() {
    let mut kiosk = kiosk();
    let mut c = composer();
    let mut bad = run("deploy.yml", RunStatus::Failed);
    bad.url = Some("https://git.example/me/webshop/actions/runs/9".into());
    for (key, run) in [
        ("a", bad),
        ("b", run("ci.yml", RunStatus::Success)),
        ("c", run("nightly.yml", RunStatus::Failed)),
    ] {
        c.ci.apply(
            Update::Run {
                key: key.into(),
                run,
            },
            NOW,
        );
    }
    kiosk.update(&mut c, NOW);
    let value = json_of(&kiosk);
    let failed = value["failed"].as_array().unwrap();
    // Only failures, one per pipeline; the badge counts the same.
    assert_eq!(failed.len(), 2);
    assert_eq!(value["badges"][0]["count"], 2);
    assert_eq!(failed[0]["key"], "a");
    assert_eq!(failed[0]["pipeline"], "deploy.yml");
    assert_eq!(failed[0]["ref"], "main");
    assert_eq!(failed[0]["step"], "tests");
    assert_eq!(failed[0]["started"], NOW - 100);
    assert_eq!(
        failed[0]["url"],
        "https://git.example/me/webshop/actions/runs/9"
    );
    assert!(failed[1]["url"].is_null());

    // A green run of the same pipeline takes it off the list by itself.
    c.ci.apply(
        Update::Run {
            key: "a".into(),
            run: run("deploy.yml", RunStatus::Success),
        },
        NOW,
    );
    kiosk.update(&mut c, NOW);
    let value = json_of(&kiosk);
    assert_eq!(value["failed"].as_array().unwrap().len(), 1);
}

#[test]
fn page_lets_the_failed_badge_be_dismissed_on_the_bridge() {
    let js = include_str!("../../kiosk/kiosk.js");
    // The badge opens the list, each failure and "all" can be dismissed, and
    // the choice goes to the bridge, not into the browser.
    for needle in [
        "data-failed-toggle",
        "data-dismiss-all",
        "api/kiosk/failed/dismiss",
        "key + '@' + f.started",
    ] {
        assert!(js.contains(needle), "kiosk.js lacks {needle}");
    }
    assert!(!js.contains("deskwatch.kiosk.dismissed"));
    assert!(include_str!("../../kiosk/index.html").contains("id=\"failed-pop\""));
}

#[tokio::test]
async fn a_dismissed_failure_leaves_every_screen_until_it_fails_again() {
    let mut kiosk = kiosk();
    let router = kiosk.router();
    let mut c = composer();
    for (key, pipeline) in [("a", "deploy.yml"), ("b", "nightly.yml")] {
        c.ci.apply(
            Update::Run {
                key: key.into(),
                run: run(pipeline, RunStatus::Failed),
            },
            NOW,
        );
    }
    kiosk.update(&mut c, NOW);
    let first = json_of(&kiosk);
    let id = format!("a@{}", first["failed"][0]["started"]);

    // Dismiss one, from "a screen": the snapshot for everyone loses it and the badge follows.
    let body = format!(r#"{{"ids":["{id}"]}}"#);
    let (status, _) = send(&router, "POST", "/api/kiosk/failed/dismiss", &[], &body).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    kiosk.update(&mut c, NOW + 1);
    let value = json_of(&kiosk);
    assert_eq!(value["failed"].as_array().unwrap().len(), 1);
    assert_eq!(value["failed"][0]["key"], "b");
    assert_eq!(value["badges"][0]["count"], 1);

    // The same pipeline failing again (another start time) is not hidden.
    let mut again = run("deploy.yml", RunStatus::Failed);
    again.started += 500;
    c.ci.apply(
        Update::Run {
            key: "a".into(),
            run: again,
        },
        NOW + 2,
    );
    kiosk.update(&mut c, NOW + 2);
    assert_eq!(json_of(&kiosk)["failed"].as_array().unwrap().len(), 2);

    // Bad input and foreign pages change nothing.
    let uri = "/api/kiosk/failed/dismiss";
    for bad in ["{}", r#"{"ids":[1]}"#, r#"{"ids":[""]}"#, "nope"] {
        let (status, _) = send(&router, "POST", uri, &[], bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let evil = [("origin", "https://evil.example"), ("host", "bridge.lan")];
    let (status, _) = send(&router, "POST", uri, &evil, r#"{"ids":["b@1"]}"#).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_muted_source_is_flagged_until_it_works_again() {
    use crate::source::{Health, SourceBody, SourceMsg};
    let mut kiosk = kiosk();
    let router = kiosk.router();
    let mut c = composer();
    let health = |h| SourceMsg {
        source: crate::source::SourceId::new("gitea", "home"),
        body: SourceBody::Health(h),
    };
    c.apply(health(Health::Unreachable), NOW);
    kiosk.update(&mut c, NOW);
    assert!(json_of(&kiosk)["sources"][0].get("muted").is_none());

    let (status, _) = send(
        &router,
        "POST",
        "/api/kiosk/source/mute",
        &[],
        r#"{"id":"gitea:home"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    kiosk.update(&mut c, NOW + 1);
    let value = json_of(&kiosk);
    // Still listed as a problem (the chip counts it), only flagged as muted.
    assert_eq!(value["sources"][0]["health"], "unreachable");
    assert_eq!(value["sources"][0]["muted"], true);

    // Working again forgets the mute, so the next outage shows the pill.
    c.apply(health(Health::Ok), NOW + 2);
    kiosk.update(&mut c, NOW + 2);
    c.apply(health(Health::AuthFailed), NOW + 3);
    kiosk.update(&mut c, NOW + 3);
    assert!(json_of(&kiosk)["sources"][0].get("muted").is_none());

    let (status, _) = send(&router, "POST", "/api/kiosk/source/mute", &[], "{}").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn page_shows_a_pill_for_a_source_that_is_down() {
    let js = include_str!("../../kiosk/kiosk.js");
    for needle in [
        "Source down",
        "data-mute",
        "api/kiosk/source/mute",
        "x.muted",
    ] {
        assert!(js.contains(needle), "kiosk.js lacks {needle}");
    }
    assert!(include_str!("../../kiosk/index.html").contains("id=\"src-pill\""));
}

#[test]
fn a_newer_release_travels_in_the_snapshot_only_when_found() {
    let notice = crate::update::UpdateNotice::default();
    let mut kiosk = kiosk().with_update(notice.clone());
    kiosk.update(&mut busy(), NOW);
    assert!(json_of(&kiosk).get("update").is_none());
    notice.set(Some(crate::update::Update {
        version: "0.9.20".into(),
        url: "https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.20".into(),
    }));
    kiosk.update(&mut busy(), NOW + 1);
    assert_eq!(json_of(&kiosk)["update"]["version"], "0.9.20");
    assert!(KIOSK_JS.contains("renderUpdate"));
}
