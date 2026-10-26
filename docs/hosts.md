# Linux and Windows hosts

The Hosts panel gets its numbers from Prometheus, not from the machine DeskWatch runs on. The bridge does not measure
its own machine (inside Docker it could only see the container), so every machine you want to see, the DeskWatch
server included, is a host in Prometheus.

A host is a Linux machine running [node_exporter](https://github.com/prometheus/node_exporter) or a Windows machine
running [windows_exporter](https://github.com/prometheus-community/windows_exporter), collected by one Prometheus
server. DeskWatch only talks to that Prometheus.

## Choosing hosts on the setup page

1. Under **Sources**, add your Prometheus (its address, for example `http://your-server:9090`).
2. Open **Hosts**. DeskWatch lists the machines Prometheus collects from, with Linux or Windows, up or not answering,
   and pending updates. Tick the ones to show and press **Save**.
3. **Edit** changes the name shown on the dashboard and which parts to show. Only what Prometheus reports for that
   machine can be ticked. **Refresh** asks Prometheus again.

Linux and Windows are told apart by the metrics a machine has, so there is no setting for it. A machine that is down
cannot be recognised until it answers again; one you have already chosen stays in the list as "not answering".

By hand, in `bridge.toml` or `sources.toml`:

```toml
[[source.prometheus]]
name = "home"
url = "http://prometheus.example.lan:9090"
hosts = [
  "nas:9100",
  { instance = "office-pc:9182", name = "Office PC", show = ["cpu", "memory", "updates", "reboot"] },
]
```

`show` takes `cpu`, `memory`, `disk`, `uptime`, `network`, `updates`, `reboot`, `temperature` and `gpu`; without it a
host shows everything it reports.

## What is read

| Shown | Linux (node_exporter) | Windows (windows_exporter) |
|---|---|---|
| CPU, memory | yes | yes |
| Disk | the `disk_mountpoint` (`/` by default) | the `C:` drive |
| Uptime, network in and out | yes | yes |
| Temperature | the CPU sensor (`coretemp`, `k10temp`, `zenpower`, `cpu_thermal`), else the hottest sensor | no, windows_exporter has none |
| Updates, restart needed | yes, see below | yes, see below |

The PromQL for each field can be changed per source with `queries = { ... }`; the names are `cpu_pct`, `cpu_temp_c`, `ram_pct`,
`disk_pct`, `gpu_pct`, `gpu_mem_pct`, `gpu_temp_c`, `uptime_s`, `updates` and `reboot_needed`.

## Windows

Install windows_exporter on the PC or server (the MSI from its releases page; the default collectors are enough) and
add Prometheus a scrape job for `win-host:9182`. Use that `win-host:9182` as the host. The exporter's port is 9182.

## Pending updates and restart needed

The host card shows **Pending updates** (how many are waiting) and, when the machine asks for it, **Restart needed**. A host
that needs a restart sorts to the top of the list with an amber pip. Nothing shows for a host that does not report
them.

Both come from a small text file that the host's exporter reads (the "textfile collector"). They are asked from
Prometheus every 15 minutes, not every few seconds, because they change a few times a day.

- **Debian and Ubuntu:** node_exporter ships `apt.sh` in its `text_collector_examples` folder. Run it from cron or a
  systemd timer, write its output to the textfile directory, and the metrics `node_apt_upgrades_pending` and
  `node_reboot_required` are picked up as they are.
- **Other Linux and Windows:** write a file with the two metrics below. DeskWatch reads them under these names.

```
# HELP deskwatch_updates_pending Updates waiting to be installed.
# TYPE deskwatch_updates_pending gauge
deskwatch_updates_pending 12
# HELP deskwatch_reboot_required 1 when a restart is needed.
# TYPE deskwatch_reboot_required gauge
deskwatch_reboot_required 0
```

Windows, as a scheduled task that runs every hour (save the file in the folder windows_exporter's `textfile`
collector reads, with the `.prom` extension, for example `C:\Program Files\windows_exporter\textfile_inputs\updates.prom`):

```powershell
$session = New-Object -ComObject Microsoft.Update.Session
$found = $session.CreateUpdateSearcher().Search("IsInstalled=0 and IsHidden=0").Updates.Count
$reboot = [int](Test-Path "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired")
@("deskwatch_updates_pending $found", "deskwatch_reboot_required $reboot") |
  Set-Content -Encoding ascii "C:\Program Files\windows_exporter\textfile_inputs\updates.prom"
```

Fedora, RHEL and similar: count with `dnf check-update -q | grep -c .` and `needs-restarting -r` (exit code 1 means a
restart is needed), and write the same two lines.

## What is not shown

The old **Load** and **Cores** lines are gone: CPU % says the same thing more plainly. A host that does not answer
Prometheus shows as down.
