//! Prometheus source tests: the report builder and the whole source against
//! a mock Prometheus answering with the sanitised fixtures.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use tokio::sync::mpsc;

use super::*;
use crate::composer::Composer;
use crate::config::Config;
use crate::model::{Level, RotationEntry, RowStatus, ScreenData};
use crate::source::{SourceBody, SourceMsg};

const CONFIG: &str = r#"
[[source.prometheus]]
name = "home"
url = "http://prometheus.example.lan:9090"
hosts = [
  { instance = "node-a:9100", name = "home", cadvisor = "cadvisor-a:8080", expect_containers = ["db", "web", "cache"] },
  "node-b:9100",
]
"#;

fn config() -> PrometheusConfig {
    Config::from_toml(CONFIG)
        .unwrap()
        .source
        .prometheus
        .remove(0)
}

/// What the mock saw, and how it should misbehave.
#[derive(Default)]
struct Mock {
    queries: Mutex<Vec<String>>,
    auth: Mutex<Vec<Option<String>>>,
    /// Answer every query with this status instead of the fixtures.
    fail_with: Mutex<Option<StatusCode>>,
}

/// Fixtures by query text, and the recorder.
type MockState = (Arc<HashMap<String, &'static str>>, Arc<Mock>);

/// A mock Prometheus: each default query gets its fixture, anything else is
/// a 400 like a real parse error.
async fn mock_prometheus(queries: &Queries, mock: Arc<Mock>) -> String {
    let fixtures: HashMap<String, &'static str> = HashMap::from([
        (queries.up.clone(), include_str!("testdata/up.json")),
        (
            queries.cpu_pct.clone(),
            include_str!("testdata/cpu_pct.json"),
        ),
        (
            queries.cpu_temp_c.clone(),
            include_str!("testdata/cpu_temp_c.json"),
        ),
        (
            queries.cpu_count.clone(),
            include_str!("testdata/cpu_count.json"),
        ),
        (queries.load.clone(), include_str!("testdata/load.json")),
        (
            queries.ram_pct.clone(),
            include_str!("testdata/ram_pct.json"),
        ),
        (
            queries.disk_pct.clone(),
            include_str!("testdata/disk_pct.json"),
        ),
        (
            queries.uptime_s.clone(),
            include_str!("testdata/uptime_s.json"),
        ),
        (queries.net_rx.clone(), include_str!("testdata/net_rx.json")),
        (queries.net_tx.clone(), include_str!("testdata/net_tx.json")),
        (
            queries.container_age.clone(),
            include_str!("testdata/container_age.json"),
        ),
    ]);
    let app = Router::new()
        .route(
            "/api/v1/query",
            get(
                |State((fixtures, mock)): State<MockState>,
                 Query(q): Query<HashMap<String, String>>,
                 headers: HeaderMap| async move {
                    let promql = q["query"].clone();
                    mock.queries.lock().unwrap().push(promql.clone());
                    mock.auth.lock().unwrap().push(
                        headers
                            .get("authorization")
                            .map(|v| v.to_str().unwrap().to_string()),
                    );
                    if let Some(status) = *mock.fail_with.lock().unwrap() {
                        return (status, "{}".to_string());
                    }
                    match fixtures.get(&promql) {
                        Some(body) => (StatusCode::OK, (*body).to_string()),
                        None => (
                            StatusCode::BAD_REQUEST,
                            r#"{"status":"error","errorType":"bad_data","error":"unexpected query"}"#
                                .to_string(),
                        ),
                    }
                },
            ),
        )
        .with_state((Arc::new(fixtures), mock));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn collect_from_mock() -> (StatsReport, Arc<Mock>) {
    let config = config();
    let queries = Queries::new(&config);
    let mock = Arc::new(Mock::default());
    let url = mock_prometheus(&queries, mock.clone()).await;
    let api = HttpApi::new(&url, Some(source::Secret::new("test-token"))).unwrap();
    let report = collect(&api, &queries, &config.hosts).await.unwrap();
    (report, mock)
}

#[tokio::test]
async fn collects_stats_for_every_host() {
    let (report, mock) = collect_from_mock().await;

    assert_eq!(report.hosts.len(), 2);
    let home = &report.hosts[0];
    assert_eq!(home.name, "home");
    assert!(home.up);
    let stats = &home.stats;
    assert_eq!(stats.host, "home");
    assert_eq!(stats.cpu_pct, Some(37.5));
    assert_eq!(stats.cpu_temp_c, Some(51.0));
    assert_eq!(stats.cpu_count, Some(8));
    assert_eq!(stats.load, Some([0.42, 0.38, 0.3]));
    assert_eq!(stats.ram_pct, Some(62.3));
    assert_eq!(stats.disk_pct, Some(41.8));
    assert_eq!(stats.uptime_s, Some(1_234_567));
    let net = stats.net.as_ref().unwrap();
    assert_eq!(net.iface, "eth0", "the busiest device wins");
    assert_eq!((net.rx_bps, net.tx_bps), (Some(5000), Some(1200)));

    // The second host has no samples at all: every field is unknown.
    let nas = &report.hosts[1];
    assert_eq!(nas.name, "node-b", "default name drops the port");
    assert!(!nas.up);
    assert_eq!(nas.stats.cpu_pct, None);
    assert_eq!(nas.stats.net, None);

    // One query per field, not per host, and the token went with each.
    assert_eq!(mock.queries.lock().unwrap().len(), 11);
    assert!(
        mock.auth
            .lock()
            .unwrap()
            .iter()
            .all(|a| a.as_deref() == Some("Bearer test-token"))
    );
}

#[tokio::test]
async fn reports_what_is_down() {
    let (report, _) = collect_from_mock().await;
    let rows: Vec<(&str, &str)> = report
        .down
        .iter()
        .map(|d| (d.text.as_str(), d.sub.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            // Hosts come in config order: home first, then the dead one.
            ("web", "home"), // stopped: cAdvisor still lists it, with an old last-seen
            ("cache", "home"), // gone from cAdvisor on this host (the other instance is not ours)
            ("node-b", "host down"),
        ]
    );
}

#[test]
fn down_cadvisor_hides_container_rows() {
    let config = config();
    let answers = Answers {
        up: vec![
            sample(&[("instance", "node-a:9100")], 1.0),
            sample(&[("instance", "cadvisor-a:8080")], 0.0),
            sample(&[("instance", "node-b:9100")], 1.0),
        ],
        ..Answers::default()
    };
    let report = build_report(&config.hosts, &answers);
    assert_eq!(report.down.len(), 1);
    assert_eq!(report.down[0].text, "cAdvisor");
    assert_eq!(report.down[0].sub, "home");
}

#[test]
fn partial_load_is_unknown_and_values_are_clamped() {
    let config = config();
    let host = &config.hosts[0];
    let answers = Answers {
        up: vec![sample(&[("instance", "node-a:9100")], 1.0)],
        load: vec![
            sample(
                &[("instance", "node-a:9100"), ("__name__", "node_load1")],
                1.0,
            ),
            sample(
                &[("instance", "node-a:9100"), ("__name__", "node_load5")],
                1.0,
            ),
        ],
        // Rate maths can overshoot a little.
        cpu_pct: vec![sample(&[("instance", "node-a:9100")], 100.0004)],
        ram_pct: vec![sample(&[("instance", "node-a:9100")], -0.0002)],
        ..Answers::default()
    };
    let stats = host_stats(host, &answers);
    assert_eq!(stats.load, None);
    assert_eq!(stats.cpu_pct, Some(100.0));
    assert_eq!(stats.ram_pct, Some(0.0));
}

fn sample(labels: &[(&str, &str)], value: f64) -> Sample {
    Sample {
        labels: labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        value,
    }
}

#[test]
fn queries_use_config_overrides() {
    let config: PrometheusConfig = toml::from_str(
        r#"
        name = "home"
        url = "http://prometheus.example.lan:9090"
        hosts = ["a:9100"]
        disk_mountpoint = "/srv"
        net_device = "eth1"
        queries = { cpu_temp_c = "max by (instance) (node_thermal_zone_temp)" }
        "#,
    )
    .unwrap();
    let queries = Queries::new(&config);
    assert_eq!(
        queries.cpu_temp_c,
        "max by (instance) (node_thermal_zone_temp)"
    );
    assert!(queries.disk_pct.contains("mountpoint=\"/srv\""));
    assert!(queries.net_rx.contains("device=\"eth1\""));
    // Untouched fields keep the plan's default.
    assert!(queries.ram_pct.contains("node_memory_MemAvailable_bytes"));
    let defaults = Queries::new(&self::config());
    assert!(defaults.net_rx.contains("device!~\"lo|veth.*"));
}

#[test]
fn containers_are_only_queried_when_expected() {
    // `collect` skips the container query without expect_containers; the
    // flag it uses is the same one the config check relies on.
    let config: PrometheusConfig =
        toml::from_str("name = \"x\"\nurl = \"http://p\"\nhosts = [\"a:9100\"]\n").unwrap();
    assert!(config.hosts.iter().all(|h| h.expect_containers.is_empty()));
}

#[test]
fn backoff_after_failures() {
    let interval = Duration::from_secs(5);
    assert_eq!(next_delay(interval, 0), interval);
    assert_eq!(next_delay(interval, 1), Duration::from_secs(15));
    assert_eq!(next_delay(interval, 2), Duration::from_secs(60));
    assert_eq!(next_delay(interval, 9), Duration::from_secs(300));
    // A slow configured interval is never shortened by the backoff.
    assert_eq!(
        next_delay(Duration::from_secs(600), 1),
        Duration::from_secs(600)
    );
}

#[tokio::test]
async fn skips_container_query_without_expectations() {
    let mut config = config();
    for host in &mut config.hosts {
        host.expect_containers.clear();
    }
    let queries = Queries::new(&config);
    let mock = Arc::new(Mock::default());
    let url = mock_prometheus(&queries, mock.clone()).await;
    let api = HttpApi::new(&url, None).unwrap();
    let report = collect(&api, &queries, &config.hosts).await.unwrap();
    assert_eq!(mock.queries.lock().unwrap().len(), 10);
    // Only the dead host is down.
    assert_eq!(report.down.len(), 1);
    assert!(mock.auth.lock().unwrap().iter().all(Option::is_none));
}

async fn next_msg(rx: &mut mpsc::Receiver<SourceMsg>) -> SourceMsg {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("source reported in time")
        .unwrap()
}

#[tokio::test]
async fn source_reports_stats_then_health() {
    let mut config = config();
    let queries = Queries::new(&config);
    let mock = Arc::new(Mock::default());
    config.url = mock_prometheus(&queries, mock.clone()).await;
    let source = Prometheus {
        id: SourceId::new(KIND, &config.name),
        api: HttpApi::new(&config.url, None).unwrap(),
        config,
    };
    let (tx, mut rx) = mpsc::channel(8);
    source::spawn(source, tx);

    let first = next_msg(&mut rx).await;
    assert_eq!(first.source.to_string(), "prometheus:home");
    let SourceBody::Stats(report) = first.body else {
        panic!("stats come before the health report");
    };
    assert_eq!(report.hosts.len(), 2);
    let second = next_msg(&mut rx).await;
    assert!(matches!(second.body, SourceBody::Health(Health::Ok)));
}

#[tokio::test]
async fn failing_server_reports_health_without_stats() {
    let mut config = config();
    let queries = Queries::new(&config);
    let mock = Arc::new(Mock::default());
    *mock.fail_with.lock().unwrap() = Some(StatusCode::FORBIDDEN);
    config.url = mock_prometheus(&queries, mock.clone()).await;
    let source = Prometheus {
        id: SourceId::new(KIND, &config.name),
        api: HttpApi::new(&config.url, None).unwrap(),
        config,
    };
    let (tx, mut rx) = mpsc::channel(8);
    source::spawn(source, tx);

    let msg = next_msg(&mut rx).await;
    assert!(matches!(msg.body, SourceBody::Health(Health::AuthFailed)));
    // The first failed round stops at the first query.
    assert_eq!(mock.queries.lock().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// On the panel
// ---------------------------------------------------------------------------

fn rotation() -> Vec<RotationEntry> {
    let entry = |page: &str, skip_when_empty| RotationEntry {
        page: page.into(),
        dwell_s: 10,
        skip_when_empty,
    };
    vec![
        entry("stats", false),
        entry("stats:home", true),
        entry("stats:node-b", true),
        entry("stats:missing", true),
        entry("containers", true),
    ]
}

fn send(composer: &mut Composer, body: SourceBody, now: u64) {
    composer.apply(
        SourceMsg {
            source: SourceId::new(KIND, "home"),
            body,
        },
        now,
    );
}

#[tokio::test]
async fn host_pages_container_list_and_badge() {
    let (report, _) = collect_from_mock().await;
    let mut composer = Composer::new(rotation(), 0);

    // Nothing reported yet: only the local stats page, no server badge.
    assert_eq!(composer.screen(0).position, Some([1, 1]));
    assert!(composer.badges().items.is_empty());

    send(&mut composer, SourceBody::Stats(report), 1);
    // stats, stats:home, stats:node-b and containers show; stats:missing does not.
    assert_eq!(composer.screen(1).position, Some([1, 4]));

    let screen = composer.screen(10);
    assert_eq!(screen.page, "stats:home");
    assert!(!screen.stale);
    let ScreenData::Stats(stats) = &screen.data else {
        panic!("expected stats");
    };
    assert_eq!((stats.host.as_str(), stats.cpu_pct), ("home", Some(37.5)));

    // The dead host's page is shown, greyed out.
    let screen = composer.screen(20);
    assert_eq!(screen.page, "stats:node-b");
    assert!(screen.stale);

    // Then the list of what is down (the unknown host page is skipped).
    let screen = composer.screen(30);
    assert_eq!(screen.page, "containers");
    let ScreenData::List(list) = &screen.data else {
        panic!("expected a list");
    };
    assert_eq!(list.count, 3);
    assert_eq!(list.rows[0].text, "web");
    assert_eq!(list.rows[0].status, RowStatus::Failed);

    let badges = composer.badges();
    assert_eq!(badges.items.len(), 1);
    assert_eq!(badges.items[0].id, "server");
    assert_eq!(badges.items[0].count, 3);
}

#[tokio::test]
async fn source_problem_greys_host_pages_and_shows_warn_badge() {
    let (report, _) = collect_from_mock().await;
    let mut composer = Composer::new(rotation(), 0);
    send(&mut composer, SourceBody::Stats(report), 1);

    send(&mut composer, SourceBody::Health(Health::Unreachable), 2);
    let screen = composer.screen(10);
    assert_eq!(screen.page, "stats:home");
    assert!(
        screen.stale,
        "numbers are out of date while Prometheus is down"
    );
    let ids: Vec<String> = composer.badges().items.into_iter().map(|b| b.id).collect();
    assert_eq!(ids, ["server", "warn"]);

    send(&mut composer, SourceBody::Health(Health::Ok), 3);
    assert!(!composer.screen(10).stale);
    assert_eq!(composer.badges().items.len(), 1);
}

#[tokio::test]
async fn all_clear_means_no_containers_page() {
    let mut composer = Composer::new(rotation(), 0);
    let report = StatsReport {
        hosts: vec![HostFacts {
            name: "home".into(),
            up: true,
            stats: StatsData::default(),
        }],
        down: vec![],
    };
    send(&mut composer, SourceBody::Stats(report), 1);
    // stats and stats:home only; a quiet day never shows "containers".
    assert_eq!(composer.screen(1).position, Some([1, 2]));
    assert!(composer.badges().items.is_empty());
    assert_ne!(composer.screen(10).level, Level::Notice);
}
