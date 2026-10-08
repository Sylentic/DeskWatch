//! The `server` source: stats about the machine the bridge runs on.
//!
//! Everything comes from `/proc` and `/sys`, so this only works on Linux. CPU
//! usage and network rates are deltas between two samples, which means the
//! very first sample reports them as unknown (`null` on the panel).
//!
//! The parsing functions take plain text so they can be unit tested without a
//! real `/proc`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tracing::debug;

use crate::config::ServerConfig;
use crate::model::{NetData, StatsData};

/// Cumulative CPU time counters from the `cpu` line of `/proc/stat`, in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    /// Ticks spent idle or waiting for I/O.
    pub idle: u64,
    /// All ticks.
    pub total: u64,
}

/// Cumulative byte counters for one interface from `/proc/net/dev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// Collects one `StatsData` per call, keeping the previous sample for deltas.
pub struct StatsCollector {
    host: String,
    net_iface: Option<String>,
    disk_path: PathBuf,
    prev_cpu: Option<CpuTimes>,
    prev_net: Option<(String, NetCounters, Instant)>,
}

impl StatsCollector {
    pub fn new(config: &ServerConfig) -> Self {
        let host = config.display_name.clone().unwrap_or_else(read_hostname);
        Self {
            host,
            net_iface: config.net_iface.clone(),
            disk_path: config.disk_path.clone(),
            prev_cpu: None,
            prev_net: None,
        }
    }

    /// Take one sample. Any value that cannot be read is left as `None`
    /// rather than failing the whole page.
    pub fn collect(&mut self) -> StatsData {
        let proc_stat = read("/proc/stat");

        // CPU usage since the previous sample.
        let cpu_now = proc_stat.as_deref().and_then(parse_cpu_times);
        let cpu_pct = match (self.prev_cpu, cpu_now) {
            (Some(prev), Some(now)) => cpu_usage_pct(prev, now),
            _ => None,
        };
        self.prev_cpu = cpu_now;

        StatsData {
            host: self.host.clone(),
            cpu_pct,
            cpu_temp_c: read_cpu_temp_c(),
            load: read("/proc/loadavg").as_deref().and_then(parse_loadavg),
            cpu_count: proc_stat.as_deref().and_then(parse_cpu_count),
            ram_pct: read("/proc/meminfo").as_deref().and_then(parse_ram_pct),
            disk_pct: disk_used_pct(&self.disk_path),
            uptime_s: read("/proc/uptime").as_deref().and_then(parse_uptime),
            net: self.sample_net(),
        }
    }

    /// Network rates since the previous sample for the configured interface,
    /// or the busiest non-loopback one.
    fn sample_net(&mut self) -> Option<NetData> {
        let text = read("/proc/net/dev")?;
        let iface = match &self.net_iface {
            Some(name) => name.clone(),
            None => busiest_iface(&text)?,
        };
        let now_counters = parse_net_dev(&text, &iface)?;
        let now = Instant::now();

        let (rx_bps, tx_bps) = match &self.prev_net {
            // Only compare samples from the same interface.
            Some((prev_iface, prev, at)) if *prev_iface == iface => {
                let secs = now.duration_since(*at).as_secs_f64();
                (
                    rate(prev.rx_bytes, now_counters.rx_bytes, secs),
                    rate(prev.tx_bytes, now_counters.tx_bytes, secs),
                )
            }
            _ => (None, None),
        };
        self.prev_net = Some((iface.clone(), now_counters, now));
        Some(NetData {
            iface,
            rx_bps,
            tx_bps,
        })
    }
}

/// Read a text file, logging (not failing) when it is missing.
fn read(path: impl AsRef<Path>) -> Option<String> {
    let path = path.as_ref();
    match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) => {
            debug!("cannot read {}: {err}", path.display());
            None
        }
    }
}

fn read_hostname() -> String {
    read("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "server".into())
}

/// Bytes per second between two counter readings. Counter resets give `None`.
fn rate(prev: u64, now: u64, secs: f64) -> Option<u64> {
    if secs <= 0.0 || now < prev {
        return None;
    }
    Some(((now - prev) as f64 / secs).round() as u64)
}

/// Parse the aggregate `cpu` line of `/proc/stat`.
pub fn parse_cpu_times(proc_stat: &str) -> Option<CpuTimes> {
    let line = proc_stat.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    // user nice system idle iowait irq softirq steal [guest guest_nice]
    // guest time is already counted in user/nice, so only the first 8 count.
    let idle = *fields.get(3)? + fields.get(4).copied().unwrap_or(0);
    let total = fields.iter().take(8).sum();
    Some(CpuTimes { idle, total })
}

/// Number of `cpuN` lines in `/proc/stat`.
pub fn parse_cpu_count(proc_stat: &str) -> Option<u32> {
    let count = proc_stat
        .lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
        .count();
    (count > 0).then(|| u32::try_from(count).unwrap_or(u32::MAX))
}

/// Busy percentage between two `CpuTimes` samples.
pub fn cpu_usage_pct(prev: CpuTimes, now: CpuTimes) -> Option<f32> {
    let total = now.total.checked_sub(prev.total)?;
    let idle = now.idle.checked_sub(prev.idle)?;
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle) as f64 / total as f64 * 100.0;
    Some(round1(busy))
}

/// The three load averages from `/proc/loadavg`.
pub fn parse_loadavg(text: &str) -> Option<[f32; 3]> {
    let mut parts = text.split_whitespace().map(|p| p.parse::<f32>().ok());
    Some([parts.next()??, parts.next()??, parts.next()??])
}

/// Used memory percentage from `/proc/meminfo`, based on `MemAvailable`.
pub fn parse_ram_pct(meminfo: &str) -> Option<f32> {
    let value = |key: &str| -> Option<u64> {
        meminfo
            .lines()
            .find_map(|l| l.strip_prefix(key))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    let total = value("MemTotal:")?;
    let available = value("MemAvailable:")?;
    if total == 0 {
        return None;
    }
    Some(round1(
        total.saturating_sub(available) as f64 / total as f64 * 100.0,
    ))
}

/// Whole seconds of uptime from `/proc/uptime`.
pub fn parse_uptime(text: &str) -> Option<u64> {
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    Some(secs as u64)
}

/// Byte counters for one interface from `/proc/net/dev`.
pub fn parse_net_dev(text: &str, iface: &str) -> Option<NetCounters> {
    net_dev_rows(text).find_map(|(name, counters)| (name == iface).then_some(counters))
}

/// The non-loopback interface with the most received bytes, a decent guess
/// for "the" network link on a home server.
pub fn busiest_iface(text: &str) -> Option<String> {
    net_dev_rows(text)
        .filter(|(name, _)| *name != "lo")
        .max_by_key(|(_, c)| c.rx_bytes)
        .map(|(name, _)| name.to_string())
}

/// Iterate `(iface, counters)` rows of `/proc/net/dev`, skipping the two header lines.
fn net_dev_rows(text: &str) -> impl Iterator<Item = (&str, NetCounters)> {
    text.lines().skip(2).filter_map(|line| {
        let (name, rest) = line.split_once(':')?;
        let fields: Vec<u64> = rest
            .split_whitespace()
            .map(|f| f.parse().ok())
            .collect::<Option<_>>()?;
        // Receive block is 8 fields, transmit bytes is the first after it.
        Some((
            name.trim(),
            NetCounters {
                rx_bytes: *fields.first()?,
                tx_bytes: *fields.get(8)?,
            },
        ))
    })
}

/// CPU temperature in degrees Celsius.
///
/// Tries the hwmon drivers for common CPUs first (Intel `coretemp`, AMD
/// `k10temp`/`zenpower`, Raspberry Pi `cpu_thermal`), then falls back to the
/// first thermal zone. Sensor layouts vary a lot, so this is best effort.
fn read_cpu_temp_c() -> Option<f32> {
    const HWMON_NAMES: [&str; 4] = ["coretemp", "k10temp", "zenpower", "cpu_thermal"];

    if let Ok(dirs) = fs::read_dir("/sys/class/hwmon") {
        let mut dirs: Vec<PathBuf> = dirs.flatten().map(|d| d.path()).collect();
        dirs.sort();
        for dir in dirs {
            let name = read(dir.join("name")).unwrap_or_default();
            if HWMON_NAMES.contains(&name.trim())
                && let Some(temp) = read(dir.join("temp1_input"))
                    .as_deref()
                    .and_then(parse_millidegrees)
            {
                return Some(temp);
            }
        }
    }

    read("/sys/class/thermal/thermal_zone0/temp")
        .as_deref()
        .and_then(parse_millidegrees)
}

/// Sysfs temperatures are in millidegrees Celsius.
pub fn parse_millidegrees(text: &str) -> Option<f32> {
    let milli: i64 = text.trim().parse().ok()?;
    Some(round1(milli as f64 / 1000.0))
}

/// Used percentage of the filesystem at `path`, as `df` shows it.
fn disk_used_pct(path: &Path) -> Option<f32> {
    let stat = match nix::sys::statvfs::statvfs(path) {
        Ok(stat) => stat,
        Err(err) => {
            debug!("statvfs {} failed: {err}", path.display());
            return None;
        }
    };
    let frag = stat.fragment_size() as f64;
    let total = stat.blocks() as f64 * frag;
    let free = stat.blocks_free() as f64 * frag;
    let available = stat.blocks_available() as f64 * frag;
    let used = total - free;
    // Like df: used / (used + available), so root-reserved space counts as full.
    let usable = used + available;
    if usable <= 0.0 {
        return None;
    }
    Some(round1(used / usable * 100.0))
}

/// Round to one decimal, plenty for a small screen and keeps payloads short.
fn round1(value: f64) -> f32 {
    ((value * 10.0).round() / 10.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_STAT: &str = "\
cpu  100 0 50 800 50 0 0 0 0 0
cpu0 50 0 25 400 25 0 0 0 0 0
cpu1 50 0 25 400 25 0 0 0 0 0
intr 12345
ctxt 6789
";

    const NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 9000000     100    0    0    0     0          0         0  9000000     100    0    0    0     0       0          0
  eth0: 5000000    4000    0    0    0     0          0         0  1000000    3000    0    0    0     0       0          0
 wlan0:   20000      40    0    0    0     0          0         0    10000      30    0    0    0     0       0          0
";

    #[test]
    fn cpu_times_and_usage() {
        let prev = parse_cpu_times(PROC_STAT).unwrap();
        assert_eq!(
            prev,
            CpuTimes {
                idle: 850,
                total: 1000
            }
        );

        // 100 more ticks, 25 of them idle: 75 % busy.
        let now = CpuTimes {
            idle: 875,
            total: 1100,
        };
        assert_eq!(cpu_usage_pct(prev, now), Some(75.0));
        // No time passed, or counters went backwards.
        assert_eq!(cpu_usage_pct(prev, prev), None);
        assert_eq!(cpu_usage_pct(now, prev), None);
    }

    #[test]
    fn cpu_count_counts_numbered_lines() {
        assert_eq!(parse_cpu_count(PROC_STAT), Some(2));
        assert_eq!(parse_cpu_count("cpu  1 2 3\n"), None);
    }

    #[test]
    fn loadavg() {
        assert_eq!(
            parse_loadavg("0.42 0.38 0.30 1/234 5678\n"),
            Some([0.42, 0.38, 0.30])
        );
        assert_eq!(parse_loadavg("0.42 junk"), None);
    }

    #[test]
    fn ram_uses_mem_available() {
        let meminfo =
            "MemTotal:       16000000 kB\nMemFree:  1000000 kB\nMemAvailable:    4000000 kB\n";
        assert_eq!(parse_ram_pct(meminfo), Some(75.0));
        assert_eq!(parse_ram_pct("MemTotal: 100 kB\n"), None);
    }

    #[test]
    fn uptime_is_whole_seconds() {
        assert_eq!(parse_uptime("1234567.89 4567890.12\n"), Some(1234567));
    }

    #[test]
    fn net_dev_parsing() {
        assert_eq!(
            parse_net_dev(NET_DEV, "eth0"),
            Some(NetCounters {
                rx_bytes: 5000000,
                tx_bytes: 1000000
            })
        );
        assert_eq!(parse_net_dev(NET_DEV, "missing"), None);
        // Loopback has the most traffic but is never picked.
        assert_eq!(busiest_iface(NET_DEV).as_deref(), Some("eth0"));
    }

    #[test]
    fn rates() {
        assert_eq!(rate(1000, 3000, 2.0), Some(1000));
        assert_eq!(rate(3000, 1000, 2.0), None);
        assert_eq!(rate(1000, 3000, 0.0), None);
    }

    #[test]
    fn temperatures() {
        assert_eq!(parse_millidegrees("48500\n"), Some(48.5));
        assert_eq!(parse_millidegrees("-1234"), Some(-1.2));
        assert_eq!(parse_millidegrees("n/a"), None);
    }

    #[test]
    fn collector_runs_on_this_machine() {
        // Smoke test against the real /proc: two samples, the second has deltas.
        let mut collector = StatsCollector::new(&ServerConfig {
            display_name: Some("test".into()),
            ..ServerConfig::default()
        });
        let first = collector.collect();
        assert_eq!(first.host, "test");
        assert_eq!(first.cpu_pct, None);
        let second = collector.collect();
        if cfg!(target_os = "linux") {
            assert!(second.uptime_s.is_some());
            assert!(second.load.is_some());
        }
    }
}
