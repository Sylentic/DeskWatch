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
            queries.updates.clone(),
            include_str!("testdata/updates.json"),
        ),
        (
            queries.updates_security.clone(),
            include_str!("testdata/updates_security.json"),
        ),
        (
            queries.reboot_needed.clone(),
            include_str!("testdata/reboot_needed.json"),
        ),
        (
            queries.ram_pct.clone(),
            include_str!("testdata/ram_pct.json"),
        ),
        (
            queries.disk_pct.clone(),
            include_str!("testdata/disk_pct.json"),
        ),
        (
            queries.gpu_pct.clone(),
            include_str!("testdata/gpu_pct.json"),
        ),
        (
            queries.gpu_mem_pct.clone(),
            include_str!("testdata/gpu_mem_pct.json"),
        ),
        (
            queries.gpu_temp_c.clone(),
            include_str!("testdata/gpu_temp_c.json"),
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
    let report = collect(&api, &queries, &config.hosts, &mut Slow::default())
        .await
        .unwrap();
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
    assert_eq!(stats.ram_pct, Some(62.3));
    assert_eq!(stats.disk_pct, Some(41.8));
    assert_eq!(stats.gpu_pct, Some(73.0));
    assert_eq!(stats.gpu_mem_pct, Some(41.3));
    assert_eq!(stats.gpu_temp_c, Some(66.4));
    assert_eq!(stats.uptime_s, Some(1_234_567));
    assert_eq!(stats.updates, Some(12));
    assert_eq!(stats.updates_security, Some(5));
    assert_eq!(stats.reboot_needed, Some(true));
    let net = stats.net.as_ref().unwrap();
    assert_eq!(net.iface, "eth0", "the busiest device wins");
    assert_eq!((net.rx_bps, net.tx_bps), (Some(5000), Some(1200)));

    // The second host has no samples at all: every field is unknown.
    let nas = &report.hosts[1];
    assert_eq!(nas.name, "node-b", "default name drops the port");
    assert!(!nas.up);
    assert_eq!(nas.stats.cpu_pct, None);
    assert_eq!(nas.stats.net, None);
    assert_eq!(
        (
            nas.stats.updates,
            nas.stats.updates_security,
            nas.stats.reboot_needed
        ),
        (None, None, None)
    );

    // One query per field, not per host, and the token went with each.
    assert_eq!(mock.queries.lock().unwrap().len(), 15);
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
fn values_are_clamped() {
    let config = config();
    let host = &config.hosts[0];
    let answers = Answers {
        up: vec![sample(&[("instance", "node-a:9100")], 1.0)],
        // Rate maths can overshoot a little.
        cpu_pct: vec![sample(&[("instance", "node-a:9100")], 100.0004)],
        ram_pct: vec![sample(&[("instance", "node-a:9100")], -0.0002)],
        ..Answers::default()
    };
    let stats = host_stats(host, &answers);
    assert_eq!(stats.cpu_pct, Some(100.0));
    assert_eq!(stats.ram_pct, Some(0.0));
}

#[test]
fn several_gpus_are_listed_and_the_plain_figures_stay_the_busiest() {
    let config = config();
    let host = &config.hosts[0];
    let gpu = |i: &str, name: &str, v: f64| {
        sample(
            &[("instance", "node-a:9100"), ("gpu", i), ("modelName", name)],
            v,
        )
    };
    let answers = Answers {
        up: vec![sample(&[("instance", "node-a:9100")], 1.0)],
        // Listed out of order on purpose: index 10 must sort after 2.
        gpu_pct: vec![
            gpu("10", "Third", 5.0),
            gpu("1", "Second", 80.4),
            gpu("0", "First", 12.0),
        ],
        gpu_mem_pct: vec![
            sample(&[("instance", "node-a:9100"), ("gpu", "0")], 30.0),
            sample(&[("instance", "node-a:9100"), ("gpu", "1")], 55.0),
        ],
        gpu_temp_c: vec![sample(&[("instance", "node-a:9100"), ("gpu", "1")], 71.0)],
        ..Answers::default()
    };
    let stats = host_stats(host, &answers);
    assert_eq!(stats.gpu_pct, Some(80.4));
    assert_eq!(stats.gpu_mem_pct, Some(55.0));
    assert_eq!(stats.gpu_temp_c, Some(71.0));
    let names: Vec<_> = stats.gpus.iter().map(|g| g.name.as_deref()).collect();
    assert_eq!(names, [Some("First"), Some("Second"), Some("Third")]);
    let labels: Vec<_> = stats.gpus.iter().map(|g| g.label.as_str()).collect();
    assert_eq!(labels, ["GPU 0", "GPU 1", "GPU 10"]);
    assert!(!stats.gpu_plain);
    assert_eq!(stats.gpus[1].pct, Some(80.4));
    assert_eq!(stats.gpus[1].mem_pct, Some(55.0));
    assert_eq!(stats.gpus[1].temp_c, Some(71.0));
    assert_eq!(stats.gpus[0].temp_c, None);
}

#[test]
fn custom_gpu_names_replace_the_labels_and_detail_can_be_switched_off() {
    let config = Config::from_toml(&CONFIG.replace(
        "\"node-b:9100\",",
        "{ instance = \"node-b:9100\", gpu_names = [\"3060\", \"\"], gpu_detail = false },",
    ))
    .unwrap()
    .source
    .prometheus
    .remove(0);
    let gpu = |i: &str| sample(&[("instance", "node-b:9100"), ("gpu", i)], 50.0);
    let answers = Answers {
        gpu_pct: vec![gpu("0"), gpu("1")],
        ..Answers::default()
    };
    let stats = host_stats(&config.hosts[1], &answers);
    let labels: Vec<_> = stats.gpus.iter().map(|g| g.label.as_str()).collect();
    assert_eq!(labels, ["3060", "GPU 1"]);
    assert!(stats.gpu_plain);
}

#[test]
fn a_custom_name_also_applies_to_a_single_gpu() {
    let config = Config::from_toml(&CONFIG.replace(
        "\"node-b:9100\",",
        "{ instance = \"node-b:9100\", gpu_names = [\"3070\"] },",
    ))
    .unwrap()
    .source
    .prometheus
    .remove(0);
    let answers = Answers {
        gpu_pct: vec![sample(&[("instance", "node-b:9100")], 50.0)],
        ..Answers::default()
    };
    let stats = host_stats(&config.hosts[1], &answers);
    assert!(stats.gpus.is_empty());
    assert_eq!(stats.gpu_label.as_deref(), Some("3070"));
}

#[test]
fn one_gpu_adds_no_list() {
    let config = config();
    let answers = Answers {
        up: vec![sample(&[("instance", "node-a:9100")], 1.0)],
        gpu_pct: vec![sample(
            &[
                ("instance", "node-a:9100"),
                ("gpu", "0"),
                ("modelName", "Solo"),
            ],
            40.0,
        )],
        ..Answers::default()
    };
    let stats = host_stats(&config.hosts[0], &answers);
    assert_eq!(stats.gpu_pct, Some(40.0));
    assert!(stats.gpus.is_empty());
}

#[test]
fn gpu_figures_hidden_by_show_hide_the_list_too() {
    let config = Config::from_toml(&CONFIG.replace(
        "\"node-b:9100\",",
        "{ instance = \"node-b:9100\", show = [\"cpu\"] },",
    ))
    .unwrap()
    .source
    .prometheus
    .remove(0);
    let host = &config.hosts[1];
    let gpu = |i: &str| sample(&[("instance", "node-b:9100"), ("gpu", i)], 50.0);
    let answers = Answers {
        gpu_pct: vec![gpu("0"), gpu("1")],
        ..Answers::default()
    };
    assert!(host_stats(host, &answers).gpus.is_empty());
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
    let report = collect(&api, &queries, &config.hosts, &mut Slow::default())
        .await
        .unwrap();
    assert_eq!(mock.queries.lock().unwrap().len(), 14);
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
// What the composer makes of it
// ---------------------------------------------------------------------------

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
async fn hosts_down_list_and_badge() {
    let (report, _) = collect_from_mock().await;
    let mut composer = Composer::default();

    // Nothing reported yet: no hosts, no server badge.
    assert!(composer.fleet.hosts().is_empty());
    assert!(composer.badges().items.is_empty());

    send(&mut composer, SourceBody::Stats(report), 1);
    let (stats, stale) = composer.fleet.host("home").unwrap();
    assert!(!stale);
    assert_eq!((stats.host.as_str(), stats.cpu_pct), ("home", Some(37.5)));

    // The dead host is listed, greyed out.
    assert!(composer.fleet.host("node-b").unwrap().1);
    assert!(composer.fleet.host("missing").is_none());

    // What is down, for the kiosk list.
    let down = composer.fleet.down();
    assert_eq!(down.len(), 3);
    assert_eq!(down[0].text, "web");

    let badges = composer.badges();
    assert_eq!(badges.items.len(), 1);
    assert_eq!(badges.items[0].id, "server");
    assert_eq!(badges.items[0].count, 3);
}

#[tokio::test]
async fn source_problem_greys_hosts_and_shows_warn_badge() {
    let (report, _) = collect_from_mock().await;
    let mut composer = Composer::default();
    send(&mut composer, SourceBody::Stats(report), 1);

    send(&mut composer, SourceBody::Health(Health::Unreachable), 2);
    assert!(
        composer.fleet.host("home").unwrap().1,
        "numbers are out of date while Prometheus is down"
    );
    let ids: Vec<String> = composer.badges().items.into_iter().map(|b| b.id).collect();
    assert_eq!(ids, ["server", "warn"]);

    send(&mut composer, SourceBody::Health(Health::Ok), 3);
    assert!(!composer.fleet.host("home").unwrap().1);
    assert_eq!(composer.badges().items.len(), 1);
}

#[tokio::test]
async fn all_clear_means_nothing_is_down() {
    let mut composer = Composer::default();
    let report = StatsReport {
        hosts: vec![HostFacts {
            name: "home".into(),
            up: true,
            stats: StatsData::default(),
        }],
        down: vec![],
    };
    send(&mut composer, SourceBody::Stats(report), 1);
    assert!(composer.fleet.down().is_empty());
    assert!(composer.badges().items.is_empty());
    assert!(composer.screen(10).is_none());
}

#[test]
fn every_default_query_covers_linux_and_windows() {
    let q = Queries::new(&config());
    for (name, promql) in [
        ("cpu_pct", &q.cpu_pct),
        ("ram_pct", &q.ram_pct),
        ("disk_pct", &q.disk_pct),
        ("uptime_s", &q.uptime_s),
        ("net_rx", &q.net_rx),
        ("net_tx", &q.net_tx),
    ] {
        assert!(promql.contains("node_"), "{name} has no Linux half");
        assert!(promql.contains("windows_"), "{name} has no Windows half");
    }
    assert!(q.disk_pct.contains("volume=\"C:\""));
}

#[test]
fn default_temperature_and_gpu_read_the_right_parts() {
    let q = Queries::new(&config());
    // The CPU sensor first, the hottest sensor only when there is none.
    let (cpu, fallback) = q.cpu_temp_c.split_once(" or ").unwrap();
    assert!(cpu.contains("chip_name=~\"coretemp|k10temp|zenpower|cpu_thermal\""));
    assert!(cpu.contains("sensor=\"temp1\""));
    assert_eq!(
        fallback.trim(),
        "max by (instance) (node_hwmon_temp_celsius)"
    );
    // Video encode/decode counts as GPU use, not only the shader cores.
    assert!(q.gpu_pct.contains("DCGM_FI_DEV_(GPU|ENC|DEC)_UTIL"));
}

#[test]
fn default_security_updates_read_apt_origins_and_the_textfile() {
    let q = Queries::new(&config());
    assert!(
        q.updates_security
            .contains("node_apt_upgrades_pending{origin=~")
    );
    assert!(q.updates_security.contains("deskwatch_updates_security"));
}

#[test]
fn default_updates_add_up_the_apt_origins() {
    let q = Queries::new(&config());
    assert!(
        q.updates
            .starts_with("sum by (instance) (node_apt_upgrades_pending)")
    );
    assert!(q.updates.contains("deskwatch_updates_pending"));
}

#[test]
fn windows_host_reads_the_same_fields() {
    let host: HostConfig = toml::from_str("instance = \"win-a:9182\"").unwrap();
    let win = |v: f64| vec![sample(&[("instance", "win-a:9182")], v)];
    let answers = Answers {
        up: win(1.0),
        cpu_pct: win(20.0),
        ram_pct: win(55.5),
        updates: win(3.0),
        updates_security: win(1.0),
        reboot_needed: win(0.0),
        ..Answers::default()
    };
    let stats = host_stats(&host, &answers);
    assert_eq!(stats.cpu_pct, Some(20.0));
    assert_eq!(stats.ram_pct, Some(55.5));
    assert_eq!(stats.updates, Some(3));
    assert_eq!(stats.updates_security, Some(1));
    assert_eq!(stats.reboot_needed, Some(false));
    assert_eq!(
        stats.cpu_temp_c, None,
        "windows_exporter has no temperature"
    );
}

#[tokio::test]
async fn updates_are_asked_rarely() {
    let config = config();
    let queries = Queries::new(&config);
    let mock = Arc::new(Mock::default());
    let url = mock_prometheus(&queries, mock.clone()).await;
    let api = HttpApi::new(&url, None).unwrap();
    let mut slow = Slow::default();
    collect(&api, &queries, &config.hosts, &mut slow)
        .await
        .unwrap();
    assert_eq!(mock.queries.lock().unwrap().len(), 15);
    // The next round skips both update queries but still reports the numbers.
    let report = collect(&api, &queries, &config.hosts, &mut slow)
        .await
        .unwrap();
    assert_eq!(mock.queries.lock().unwrap().len(), 15 + 12);
    assert_eq!(report.hosts[0].stats.updates, Some(12));
    assert_eq!(report.hosts[0].stats.updates_security, Some(5));
}

#[test]
fn discovery_lists_linux_and_windows_machines_only() {
    let at = |instance: &str| sample(&[("instance", instance)], 1.0);
    let up = vec![
        at("nas:9100"),
        at("office-pc:9182"),
        at("cadvisor:8080"),
        sample(&[("instance", "old-box:9100")], 0.0),
    ];
    let parts = vec![
        ("cpu", vec![at("nas:9100"), at("office-pc:9182")]),
        (
            "updates",
            vec![sample(&[("instance", "office-pc:9182")], 3.0)],
        ),
        (
            "updates_security",
            vec![sample(&[("instance", "office-pc:9182")], 2.0)],
        ),
        (
            "reboot",
            vec![sample(&[("instance", "office-pc:9182")], 1.0)],
        ),
        ("temperature", vec![at("nas:9100")]),
    ];
    let found = build_found(&up, &[at("nas:9100")], &[at("office-pc:9182")], &parts);
    // cAdvisor is not a machine, and a machine that is down has no metrics to
    // recognise it by.
    assert_eq!(
        found
            .iter()
            .map(|f| f.instance.as_str())
            .collect::<Vec<_>>(),
        ["nas:9100", "office-pc:9182"]
    );
    assert_eq!(found[0].kind, "linux");
    assert_eq!(found[0].reports, ["cpu", "temperature"]);
    assert_eq!(found[0].updates, None);
    assert_eq!(found[1].kind, "windows");
    assert_eq!(found[1].updates, Some(3));
    assert_eq!(found[1].updates_security, Some(2));
    assert_eq!(found[0].updates_security, None);
    assert_eq!(found[1].reboot_needed, Some(true));
}

#[test]
fn a_host_card_leaves_out_what_the_host_does_not_show() {
    let mut host: HostConfig = toml::from_str("instance = \"node-a:9100\"").unwrap();
    host.show = Some(vec!["cpu".into(), "updates".into()]);
    let at = |v: f64| vec![sample(&[("instance", "node-a:9100")], v)];
    let answers = Answers {
        up: at(1.0),
        cpu_pct: at(20.0),
        ram_pct: at(50.0),
        disk_pct: at(60.0),
        updates: at(4.0),
        updates_security: at(2.0),
        reboot_needed: at(1.0),
        ..Answers::default()
    };
    let stats = host_stats(&host, &answers);
    assert_eq!(stats.cpu_pct, Some(20.0));
    assert_eq!(stats.updates, Some(4));
    assert_eq!(stats.updates_security, Some(2));
    assert_eq!((stats.ram_pct, stats.disk_pct), (None, None));
    assert_eq!(stats.reboot_needed, None);
    assert!(stats.hide.contains(&"memory".to_string()));
    assert!(!stats.hide.contains(&"cpu".to_string()));
    // Without a list everything is shown.
    host.show = None;
    assert!(host_stats(&host, &answers).hide.is_empty());
}
