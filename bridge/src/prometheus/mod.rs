//! Prometheus source: host stats and container health from a Prometheus
//! server that scrapes node_exporter and cAdvisor (integrations plan
//! section 3).
//!
//! Every `interval_s` the source runs a handful of instant queries (one per
//! stats field, covering all hosts at once) and sends the answer to the main
//! loop as a `StatsReport`:
//!
//! - one `stats:<name>` page per configured host;
//! - a list of what is down: expected containers that cAdvisor no longer
//!   sees, and hosts that Prometheus cannot scrape;
//! - the source health, so a refused token or an unreachable Prometheus
//!   shows as the `warn` badge and greys out the host pages.
//!
//! The built-in `/proc` source keeps working without any of this: its `stats`
//! page is always the bridge host itself.
//!
//! Failures back off (15 s, 1 min, 5 min) instead of hammering a server that
//! is down.

pub mod api;
#[cfg(test)]
mod tests;

use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::config::{HostConfig, PrometheusConfig};
use crate::fleet::{Down, HostFacts, StatsReport};
use crate::model::{NetData, StatsData};
use crate::source::{self, FactSink, Health, Source, SourceId};
use api::{HttpApi, Sample};

/// Adapter type: `[[source.prometheus]]` in the config.
pub const KIND: &str = "prometheus";

/// Stats fields whose PromQL can be replaced in the config (`queries = { ... }`).
pub const QUERY_KEYS: [&str; 6] = [
    "cpu_pct",
    "cpu_temp_c",
    "cpu_count",
    "ram_pct",
    "disk_pct",
    "uptime_s",
];

/// A container counts as down when cAdvisor has not seen it for this long.
/// A stopped container keeps showing up in cAdvisor for a while, with an
/// old `container_last_seen`.
const CONTAINER_MAX_AGE_S: f64 = 60.0;

/// Poll delays after 1, 2, and 3 or more failed rounds in a row.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(300),
];

/// Network devices that are never "the" link: loopback, bridges and the
/// virtual interfaces containers and VMs create.
const VIRTUAL_NET_DEVICES: &str = "lo|veth.*|docker.*|br-.*|virbr.*|cni.*|flannel.*";

// ---------------------------------------------------------------------------
// The source task
// ---------------------------------------------------------------------------

/// One `[[source.prometheus]]` instance, ready to run.
pub struct Prometheus {
    id: SourceId,
    config: PrometheusConfig,
    api: HttpApi,
}

impl Prometheus {
    /// Load the token, if one is configured. Fails when its file is missing.
    pub fn new(config: &PrometheusConfig) -> Result<Self> {
        let id = SourceId::new(KIND, &config.name);
        let token = config
            .token_file
            .as_deref()
            .map(source::load_secret)
            .transpose()
            .with_context(|| format!("{id}: token_file"))?;
        let api = HttpApi::new(&config.url, token).context("Prometheus API")?;
        Ok(Self {
            id,
            config: config.clone(),
            api,
        })
    }
}

impl Source for Prometheus {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    async fn run(self, sink: FactSink) -> Result<()> {
        let Prometheus { id, config, api } = self;
        info!(source = %id, hosts = config.hosts.len(), "querying Prometheus");
        let queries = Queries::new(&config);
        let interval = Duration::from_secs(config.interval_s);

        let mut failures = 0;
        let mut reported = None;
        loop {
            let health = match collect(&api, &queries, &config.hosts).await {
                Ok(report) => {
                    failures = 0;
                    if !sink.stats(report).await {
                        break; // main loop has stopped
                    }
                    Health::Ok
                }
                Err(err) => {
                    failures += 1;
                    warn!(source = %id, "cannot query Prometheus: {err:#}");
                    Health::from_error(&err)
                }
            };
            if reported != Some(health) {
                reported = Some(health);
                if !sink.health(health).await {
                    break;
                }
            }
            tokio::time::sleep(next_delay(interval, failures)).await;
        }
        Ok(())
    }
}

/// How long to wait before the next round.
fn next_delay(interval: Duration, failures: usize) -> Duration {
    match failures {
        0 => interval,
        n => interval.max(BACKOFF[(n - 1).min(BACKOFF.len() - 1)]),
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// The PromQL for every stats field: the plan's defaults, with the config's
/// replacements, mount point and network device applied.
#[derive(Debug, Clone)]
pub struct Queries {
    pub up: String,
    pub cpu_pct: String,
    pub cpu_temp_c: String,
    pub cpu_count: String,
    pub load: String,
    pub ram_pct: String,
    pub disk_pct: String,
    pub uptime_s: String,
    pub net_rx: String,
    pub net_tx: String,
    /// Seconds since cAdvisor last saw each named container.
    pub container_age: String,
}

impl Queries {
    pub fn new(config: &PrometheusConfig) -> Self {
        let pick = |key: &str, default: String| config.queries.get(key).cloned().unwrap_or(default);
        let mount = &config.disk_mountpoint;
        // Without `net_device`, take every real device and let the report
        // pick the busiest one.
        let device = match &config.net_device {
            Some(device) => format!("device=\"{device}\""),
            None => format!("device!~\"{VIRTUAL_NET_DEVICES}\""),
        };
        Self {
            up: "up".into(),
            cpu_pct: pick(
                "cpu_pct",
                "100 * (1 - avg by (instance) (rate(node_cpu_seconds_total{mode=\"idle\"}[1m])))"
                    .into(),
            ),
            cpu_temp_c: pick(
                "cpu_temp_c",
                "max by (instance) (node_hwmon_temp_celsius)".into(),
            ),
            cpu_count: pick(
                "cpu_count",
                "count by (instance) (node_cpu_seconds_total{mode=\"idle\"})".into(),
            ),
            load: "{__name__=~\"node_load1|node_load5|node_load15\"}".into(),
            ram_pct: pick(
                "ram_pct",
                "100 * (1 - node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes)".into(),
            ),
            disk_pct: pick(
                "disk_pct",
                format!(
                    "100 * (1 - max by (instance) (node_filesystem_avail_bytes{{mountpoint=\"{mount}\"}} \
                     / node_filesystem_size_bytes{{mountpoint=\"{mount}\"}}))"
                ),
            ),
            uptime_s: pick("uptime_s", "time() - node_boot_time_seconds".into()),
            net_rx: format!(
                "sum by (instance, device) (rate(node_network_receive_bytes_total{{{device}}}[1m]))"
            ),
            net_tx: format!(
                "sum by (instance, device) (rate(node_network_transmit_bytes_total{{{device}}}[1m]))"
            ),
            container_age: "time() - container_last_seen{name!=\"\"}".into(),
        }
    }
}

/// The raw answers of one round.
#[derive(Debug, Default)]
pub struct Answers {
    pub up: Vec<Sample>,
    pub cpu_pct: Vec<Sample>,
    pub cpu_temp_c: Vec<Sample>,
    pub cpu_count: Vec<Sample>,
    pub load: Vec<Sample>,
    pub ram_pct: Vec<Sample>,
    pub disk_pct: Vec<Sample>,
    pub uptime_s: Vec<Sample>,
    pub net_rx: Vec<Sample>,
    pub net_tx: Vec<Sample>,
    /// Empty unless a host lists `expect_containers`.
    pub container_age: Vec<Sample>,
}

/// Run one round of queries and turn the answers into a report. The first
/// failing query ends the round: a half-filled page would look like a healthy
/// host with missing sensors.
pub async fn collect(
    api: &HttpApi,
    queries: &Queries,
    hosts: &[HostConfig],
) -> Result<StatsReport> {
    let ask = |name: &'static str, promql: &str| {
        let promql = promql.to_string();
        async move {
            api.query(&promql)
                .await
                .with_context(|| format!("query {name}"))
        }
    };
    let mut answers = Answers {
        up: ask("up", &queries.up).await?,
        cpu_pct: ask("cpu_pct", &queries.cpu_pct).await?,
        cpu_temp_c: ask("cpu_temp_c", &queries.cpu_temp_c).await?,
        cpu_count: ask("cpu_count", &queries.cpu_count).await?,
        load: ask("load", &queries.load).await?,
        ram_pct: ask("ram_pct", &queries.ram_pct).await?,
        disk_pct: ask("disk_pct", &queries.disk_pct).await?,
        uptime_s: ask("uptime_s", &queries.uptime_s).await?,
        net_rx: ask("net_rx", &queries.net_rx).await?,
        net_tx: ask("net_tx", &queries.net_tx).await?,
        container_age: Vec::new(),
    };
    if hosts.iter().any(|h| !h.expect_containers.is_empty()) {
        answers.container_age = ask("container_age", &queries.container_age).await?;
    }
    Ok(build_report(hosts, &answers))
}

// ---------------------------------------------------------------------------
// Answers to facts
// ---------------------------------------------------------------------------

/// Turn one round of answers into the report. Pure, so the tests can feed it
/// fixtures without a server.
pub fn build_report(hosts: &[HostConfig], answers: &Answers) -> StatsReport {
    let mut report = StatsReport::default();
    for host in hosts {
        let up = is_up(&answers.up, &host.instance);
        report.hosts.push(HostFacts {
            name: host.name.clone(),
            up,
            stats: host_stats(host, answers),
        });
        if !up {
            // Nothing else to say about a machine that does not answer; its
            // containers would only add rows that repeat the same problem.
            report.down.push(Down {
                text: host.name.clone(),
                sub: "host down".into(),
            });
            continue;
        }
        report.down.extend(down_containers(host, answers));
    }
    report
}

fn host_stats(host: &HostConfig, answers: &Answers) -> StatsData {
    let instance = host.instance.as_str();
    let value = |samples: &[Sample]| first_value(samples, instance);
    StatsData {
        host: host.name.clone(),
        cpu_pct: value(&answers.cpu_pct).map(percent),
        cpu_temp_c: value(&answers.cpu_temp_c).map(round1),
        load: load(&answers.load, instance),
        cpu_count: value(&answers.cpu_count).map(|c| c.max(0.0).round() as u32),
        ram_pct: value(&answers.ram_pct).map(percent),
        disk_pct: value(&answers.disk_pct).map(percent),
        uptime_s: value(&answers.uptime_s).map(|s| s.max(0.0) as u64),
        net: net(&answers.net_rx, &answers.net_tx, instance),
    }
}

/// Is the target with this `instance` label scraped successfully? A target
/// Prometheus does not know at all counts as down.
fn is_up(up: &[Sample], instance: &str) -> bool {
    up.iter()
        .any(|s| s.label("instance") == Some(instance) && s.value >= 1.0)
}

/// The value of the first series of `instance`.
fn first_value(samples: &[Sample], instance: &str) -> Option<f64> {
    samples
        .iter()
        .find(|s| s.label("instance") == Some(instance))
        .map(|s| s.value)
}

/// Load averages, only when all three series are there.
fn load(samples: &[Sample], instance: &str) -> Option<[f32; 3]> {
    let get = |name: &str| {
        samples
            .iter()
            .find(|s| s.label("instance") == Some(instance) && s.label("__name__") == Some(name))
            .map(|s| round2(s.value))
    };
    Some([get("node_load1")?, get("node_load5")?, get("node_load15")?])
}

/// Receive and transmit rates of the busiest device (the one with the most
/// received bytes per second). With `net_device` set there is only one.
fn net(rx: &[Sample], tx: &[Sample], instance: &str) -> Option<NetData> {
    let device_of = |s: &Sample| s.label("instance") == Some(instance);
    let busiest = rx
        .iter()
        .filter(|s| device_of(s))
        .max_by(|a, b| a.value.total_cmp(&b.value))?;
    let iface = busiest.label("device")?.to_string();
    let tx_rate = tx
        .iter()
        .find(|s| device_of(s) && s.label("device") == Some(iface.as_str()))
        .map(|s| s.value.max(0.0).round() as u64);
    Some(NetData {
        iface,
        rx_bps: Some(busiest.value.max(0.0).round() as u64),
        tx_bps: tx_rate,
    })
}

/// Containers the host must run that cAdvisor has not seen lately. When the
/// cAdvisor target itself is down nothing can be said about the containers,
/// so that is the one row.
fn down_containers(host: &HostConfig, answers: &Answers) -> Vec<Down> {
    let Some(cadvisor) = host.cadvisor.as_deref() else {
        return Vec::new();
    };
    if host.expect_containers.is_empty() {
        return Vec::new();
    }
    if !is_up(&answers.up, cadvisor) {
        return vec![Down {
            text: "cAdvisor".into(),
            sub: host.name.clone(),
        }];
    }
    host.expect_containers
        .iter()
        .filter(|name| {
            !answers.container_age.iter().any(|s| {
                s.label("instance") == Some(cadvisor)
                    && s.label("name") == Some(name.as_str())
                    && s.value < CONTAINER_MAX_AGE_S
            })
        })
        .map(|name| Down {
            text: name.clone(),
            sub: host.name.clone(),
        })
        .collect()
}

/// A percentage with one decimal, kept between 0 and 100 (rate maths can
/// land a hair outside).
fn percent(value: f64) -> f32 {
    round1(value.clamp(0.0, 100.0))
}

fn round1(value: f64) -> f32 {
    crate::stats::round1(value)
}

fn round2(value: f64) -> f32 {
    ((value * 100.0).round() / 100.0) as f32
}
