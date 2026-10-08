//! Host stats and container health reported by sources such as Prometheus.
//!
//! The built-in `server` source only knows the machine the bridge runs on.
//! A source that sees more machines sends a `StatsReport` with one entry per
//! host and the containers or hosts that are down. Each report replaces the
//! previous one from the same source, so a host that disappears from a
//! report disappears from the panel too.
//!
//! The composer reads this store to fill the `stats:<host>` pages and the
//! `containers` page, and for the `server` badge.

use std::collections::BTreeMap;

use crate::model::{ListData, ListRow, MAX_TITLE_CHARS, RowStatus, StatsData, truncate};
use crate::source::SourceId;

/// One machine, as a source sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct HostFacts {
    /// Page name: the panel shows this host on `stats:<name>`.
    pub name: String,
    /// Is the machine answering its scrapes? When not, the page is greyed out.
    pub up: bool,
    pub stats: StatsData,
}

/// Something that should be running and is not: a container, a collector
/// such as cAdvisor, or a whole host.
#[derive(Debug, Clone, PartialEq)]
pub struct Down {
    /// First line of the row, such as the container name.
    pub text: String,
    /// Second line of the row, such as the host it runs on.
    pub sub: String,
}

/// Everything one source knows about its hosts right now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatsReport {
    pub hosts: Vec<HostFacts>,
    pub down: Vec<Down>,
}

struct Entry {
    report: StatsReport,
    /// False while the source cannot reach its backend; the numbers are
    /// then out of date.
    source_ok: bool,
}

/// The latest report of every source that sends one.
#[derive(Default)]
pub struct Fleet {
    sources: BTreeMap<SourceId, Entry>,
}

impl Fleet {
    /// Replace what `source` reported before.
    pub fn apply(&mut self, source: &SourceId, report: StatsReport) {
        self.sources.insert(
            source.clone(),
            Entry {
                report,
                source_ok: true,
            },
        );
    }

    /// A source's health changed. Its numbers count as stale until it works again.
    pub fn set_source_ok(&mut self, source: &SourceId, ok: bool) {
        if let Some(entry) = self.sources.get_mut(source) {
            entry.source_ok = ok;
        }
    }

    /// Stats for the `stats:<name>` page, and whether they are stale.
    /// If two sources report the same name, the first source (by name) wins.
    pub fn host(&self, name: &str) -> Option<(&StatsData, bool)> {
        self.sources.values().find_map(|entry| {
            let host = entry.report.hosts.iter().find(|h| h.name == name)?;
            Some((&host.stats, !entry.source_ok || !host.up))
        })
    }

    /// Rows for the `containers` page: what is down, newest report only.
    pub fn down_page(&self) -> ListData {
        let down: Vec<&Down> = self
            .sources
            .values()
            .flat_map(|entry| &entry.report.down)
            .collect();
        let rows = down
            .iter()
            .map(|d| ListRow {
                text: truncate(&d.text, MAX_TITLE_CHARS),
                sub: truncate(&d.sub, MAX_TITLE_CHARS),
                status: RowStatus::Failed,
                source: "prometheus".into(),
            })
            .collect();
        ListData::new("Down", down.len() as u32, rows)
    }

    /// Number of things that are down, for the `server` badge.
    pub fn down_count(&self) -> u32 {
        self.sources
            .values()
            .map(|entry| entry.report.down.len() as u32)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, up: bool) -> HostFacts {
        HostFacts {
            name: name.into(),
            up,
            stats: StatsData {
                host: name.into(),
                cpu_pct: Some(12.5),
                ..StatsData::default()
            },
        }
    }

    fn down(text: &str) -> Down {
        Down {
            text: text.into(),
            sub: "home".into(),
        }
    }

    #[test]
    fn report_replaces_the_previous_one() {
        let id = SourceId::new("prometheus", "home");
        let mut fleet = Fleet::default();
        fleet.apply(
            &id,
            StatsReport {
                hosts: vec![host("home", true), host("nas", true)],
                down: vec![down("db")],
            },
        );
        assert_eq!(fleet.host("nas").unwrap().0.cpu_pct, Some(12.5));
        assert_eq!(fleet.down_count(), 1);

        fleet.apply(
            &id,
            StatsReport {
                hosts: vec![host("home", true)],
                down: vec![],
            },
        );
        assert!(fleet.host("nas").is_none());
        assert_eq!(fleet.down_count(), 0);
        assert_eq!(fleet.down_page().count, 0);
    }

    #[test]
    fn stale_when_host_down_or_source_unhealthy() {
        let id = SourceId::new("prometheus", "home");
        let mut fleet = Fleet::default();
        fleet.apply(
            &id,
            StatsReport {
                hosts: vec![host("home", true), host("nas", false)],
                down: vec![],
            },
        );
        assert!(!fleet.host("home").unwrap().1);
        assert!(fleet.host("nas").unwrap().1, "host not answering");

        fleet.set_source_ok(&id, false);
        assert!(fleet.host("home").unwrap().1, "source cannot reach backend");
        fleet.set_source_ok(&id, true);
        assert!(!fleet.host("home").unwrap().1);

        // Unknown sources are ignored.
        fleet.set_source_ok(&SourceId::new("gitea", "x"), false);
    }

    #[test]
    fn down_page_lists_rows_as_failed() {
        let id = SourceId::new("prometheus", "home");
        let mut fleet = Fleet::default();
        fleet.apply(
            &id,
            StatsReport {
                hosts: vec![],
                down: vec![down("db"), down("web")],
            },
        );
        let page = fleet.down_page();
        assert_eq!(page.count, 2);
        assert_eq!(page.rows[0].text, "db");
        assert_eq!(page.rows[0].sub, "home");
        assert_eq!(page.rows[0].status, RowStatus::Failed);
    }
}
