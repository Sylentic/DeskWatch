use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

use super::*;
use crate::alerts::AlertMessage;
use crate::ci::{OpenPull, RepoPulls, Run, RunStatus, RunningJob, Update};
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
                    },
                    OpenPull {
                        number: 2,
                        title: "New".into(),
                        created: 20,
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

fn kiosk_with(token: Option<&str>) -> Kiosk {
    Kiosk::new(&KioskConfig::default(), token.map(Secret::new))
}

fn json_of(kiosk: &Kiosk) -> Value {
    serde_json::from_str(&kiosk.tx.borrow()).unwrap()
}

async fn get(router: &Router, uri: &str, bearer: Option<&str>) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::get(uri);
    if let Some(token) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

#[test]
fn snapshot_carries_every_fact() {
    let mut kiosk = kiosk_with(None);
    let mut c = busy();
    kiosk.update(&mut c, NOW);
    let snap = json_of(&kiosk);

    assert_eq!(snap["v"], 1);
    assert_eq!(snap["now"], NOW);
    assert_eq!(snap["layout"]["columns"], 4);
    assert_eq!(snap["layout"]["panels"][0]["widget"], "stats");

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
fn idle_snapshot_has_no_screen() {
    let mut kiosk = kiosk_with(None);
    let mut c = composer();
    kiosk.update(&mut c, NOW);
    let snap = json_of(&kiosk);
    assert!(snap["screen"].is_null());
    assert_eq!(snap["jobs"], json!([]));
    assert_eq!(snap["hosts"], json!([]));
}

#[test]
fn snapshot_is_sent_on_change_and_on_the_heartbeat_only() {
    let mut kiosk = kiosk_with(None);
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
    let router = kiosk_with(None).router();
    let (status, headers, body) = get(&router, "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>DeskWatch</title>"));
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );

    let (status, headers, body) = get(&router, "/kiosk.js", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    assert!(body.contains("DeskWatchKiosk"));
    let (status, _, body) = get(&router, "/kiosk.css", None).await;
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
fn page_script_takes_the_token_out_of_the_address_bar() {
    // Guard for the ?token= hardening: the script must rewrite the address
    // (replaceState) and must not put the token back into a page link.
    assert!(KIOSK_JS.contains("history.replaceState"));
    assert!(KIOSK_JS.contains("params.delete('token')"));
    assert!(!INDEX_HTML.contains("token="));
}

#[test]
fn page_explains_why_it_has_no_data() {
    // Guard for the failure states: the page must ask for a token on a 401
    // (the HTTP fetch is what sees the status, a WebSocket error has none),
    // and the notice box and its field must exist in the page.
    assert!(INDEX_HTML.contains("id=\"notice\""));
    assert!(KIOSK_JS.contains("r.status === 401"));
    for pill in [
        "token required",
        "token rejected",
        "bridge unreachable",
        "polling, no live socket",
    ] {
        assert!(KIOSK_JS.contains(pill), "page never says '{pill}'");
    }
    assert!(KIOSK_JS.contains("Waiting for the first data"));
    // The page fetches once at start instead of only after the socket fails.
    assert!(KIOSK_JS.contains("{ poll(); connect(); }"));
    // The token field must not need a form, the CSP has form-action 'none'.
    assert!(!INDEX_HTML.contains("<form"));
}

#[tokio::test]
async fn snapshot_endpoint_returns_the_latest_json() {
    let mut kiosk = kiosk_with(None);
    let router = kiosk.router();
    let mut c = busy();
    kiosk.update(&mut c, NOW);
    let (status, headers, body) = get(&router, "/api/kiosk", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let snap: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(snap["jobs"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn token_protects_the_data_but_not_the_page() {
    let router = kiosk_with(Some("s3cret/+")).router();
    assert_eq!(get(&router, "/", None).await.0, StatusCode::OK);
    assert_eq!(
        get(&router, "/api/kiosk", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&router, "/api/kiosk?token=nope", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&router, "/api/kiosk/ws", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&router, "/api/kiosk", Some("nope")).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&router, "/api/kiosk", Some("s3cret/+")).await.0,
        StatusCode::OK
    );
    // The page sends the token the way encodeURIComponent writes it.
    assert_eq!(
        get(&router, "/api/kiosk?x=1&token=s3cret%2F%2B", None)
            .await
            .0,
        StatusCode::OK
    );
}

#[test]
fn percent_decoding_handles_odd_input() {
    assert_eq!(percent_decode("a%2Fb%2b"), "a/b+");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
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
    let mut kiosk = kiosk_with(Some("tok"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, kiosk.router()).into_future());

    let mut c = composer();
    kiosk.update(&mut c, NOW);

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = "GET /api/kiosk/ws?token=tok HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
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
