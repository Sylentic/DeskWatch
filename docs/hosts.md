# Linux and Windows hosts

The Hosts panel gets its numbers from Prometheus, not from the machine DeskWatch runs on. The bridge does not measure
its own machine (inside Docker it could only see the container), so every machine you want to see, the DeskWatch
server included, is a host in Prometheus.

A host is a Linux machine running [node_exporter](https://github.com/prometheus/node_exporter) or a Windows machine
running [windows_exporter](https://github.com/prometheus-community/windows_exporter), collected by one Prometheus
server. DeskWatch only talks to that Prometheus.

## Add a new machine

Do this when a machine is missing from the **Hosts** page. Prometheus is a small monitoring service: it asks every
machine you list for its numbers (CPU, memory, disk and so on) every few seconds. DeskWatch only reads from Prometheus,
so a machine has to be known to Prometheus first.

**1. Install the helper program on the machine.**

Linux, Debian or Ubuntu (listens on port 9100):

```sh
sudo apt update && sudo apt install prometheus-node-exporter
sudo systemctl enable --now prometheus-node-exporter
```

Other Linux: download node_exporter from its [releases page](https://github.com/prometheus/node_exporter/releases)
and run it as a service.

Windows (listens on port 9182): download the `.msi` from the
[windows_exporter releases page](https://github.com/prometheus-community/windows_exporter/releases), run it and accept
the defaults. In PowerShell as administrator, you can check that it runs with `Get-Service windows_exporter`.

Open the port in the machine's firewall for the Prometheus server if it is blocked.

**2. Tell Prometheus about the machine.**

Open `prometheus.yml` on the Prometheus server and add the machine under `scrape_configs`. Use the machine's name or IP
address; put Linux machines on port 9100 and Windows machines on port 9182:

```yaml
scrape_configs:
  - job_name: my-machines
    static_configs:
      - targets: ["my-linux-box:9100", "my-windows-pc:9182"]
```

Then reload Prometheus (`sudo systemctl reload prometheus`, or `docker restart prometheus` when it runs in Docker).

**3. Check that Prometheus sees it.**

Open Prometheus in a browser (`http://your-prometheus:9090`), go to **Status > Targets** and look for the machine. Its
state must be **UP**. If it says **DOWN**, the address or port is wrong, or a firewall blocks it; the error next to it
says which.

**4. Refresh DeskWatch.**

Wait a minute, open **Setup > Hosts**, press **Refresh**, tick the new machine and press **Save**.

Want GPU numbers too? See [GPU](#gpu) below.

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

## GPU

GPU figures (GPU %, VRAM, GPU temperature) only show up when Prometheus has GPU metrics for the host. A normal
node_exporter or windows_exporter does not report an NVIDIA GPU, so you need one extra exporter on the machine with the
GPU. Without it the GPU lines stay hidden. This is the usual reason for "no GPU stats".

**NVIDIA (default).** DeskWatch expects the [DCGM exporter](https://github.com/NVIDIA/dcgm-exporter). It needs the
NVIDIA driver and the NVIDIA Container Toolkit on the host. GPU % is the busiest of the shader, encoder and decoder
engines, so a video encode on NVENC shows up.

```yaml
# docker-compose.yml on the GPU machine
services:
  dcgm-exporter:
    image: nvidia/dcgm-exporter:latest
    restart: unless-stopped
    cap_add: [SYS_ADMIN]
    ports: ["9400:9400"]
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all
              capabilities: [gpu]
```

```yaml
# prometheus.yml, under scrape_configs. Use the same host:port style as for node_exporter, but keep the
# instance label equal to the node_exporter one so the GPU lands on the same host card.
  - job_name: dcgm
    static_configs:
      - targets: ["gpu-host:9400"]
    relabel_configs:
      - source_labels: [__address__]
        regex: "([^:]+):.*"
        target_label: instance
        replacement: "${1}:9100"
```

Check in Prometheus (`http://your-prometheus:9090`, Graph tab) that `DCGM_FI_DEV_GPU_UTIL` returns a line. If it does
not, DeskWatch cannot show it either.

**Other exporters.** Point the queries at your metrics with `queries` on the Prometheus source. The result must have an
`instance` label that matches the host. Examples:

```toml
[[source.prometheus]]
name = "home"
url = "http://prometheus.example.lan:9090"
# utkuozdemir/nvidia_gpu_exporter (nvidia-smi based)
queries = { gpu_pct = "100 * max by (instance) (nvidia_smi_utilization_gpu_ratio)", gpu_mem_pct = "100 * max by (instance) (nvidia_smi_memory_used_bytes / nvidia_smi_memory_total_bytes)", gpu_temp_c = "max by (instance) (nvidia_smi_temperature_gpu)" }
# AMD with node_exporter's drm collector (start node_exporter with --collector.drm):
# queries = { gpu_pct = "max by (instance) (node_drm_gpu_busy_percent)", gpu_mem_pct = "100 * max by (instance) (node_drm_memory_vram_used_bytes / node_drm_memory_vram_size_bytes)" }
```

## Windows

Install windows_exporter on the PC or server (the MSI from its releases page; the default collectors are enough) and
add Prometheus a scrape job for `win-host:9182`. Use that `win-host:9182` as the host. The exporter's port is 9182.

## Pending updates and restart needed

The host card shows **Pending updates** (how many are waiting, and when the host says, how many of them are security
updates, like `21 (13 security)`; the number turns amber when there is at least one) and, when the machine asks for it, **Restart needed**. A host
that needs a restart sorts to the top of the list with an amber pip. Nothing shows for a host that does not report
them.

Both come from a small text file that the host's exporter reads (the "textfile collector"). They are asked from
Prometheus every 15 minutes, not every few seconds, because they change a few times a day.

- **Debian and Ubuntu:** use `apt_info.py` (older versions: `apt.sh`) from
  [node-exporter-textfile-collector-scripts](https://github.com/prometheus-community/node-exporter-textfile-collector-scripts).
  Run it from cron or a systemd timer, write its output to the textfile directory, and the metrics `node_apt_upgrades_pending` and
  `node_reboot_required` are picked up as they are. The script labels each count with its origin, so the origins
  with "security" in their name (for example `Debian-Security`) are added up as the security count.
- **Other Linux and Windows:** write a file with the metrics below. DeskWatch reads them under these names.
  `deskwatch_updates_security` is optional: leave it out and the card shows just the total.

```
# HELP deskwatch_updates_pending Updates waiting to be installed.
# TYPE deskwatch_updates_pending gauge
deskwatch_updates_pending 12
# HELP deskwatch_updates_security Of those, the security updates (optional).
# TYPE deskwatch_updates_security gauge
deskwatch_updates_security 5
# HELP deskwatch_reboot_required 1 when a restart is needed.
# TYPE deskwatch_reboot_required gauge
deskwatch_reboot_required 0
```

Windows, as a scheduled task that runs every hour (save the file in the folder windows_exporter's `textfile`
collector reads, with the `.prom` extension, for example `C:\Program Files\windows_exporter\textfile_inputs\updates.prom`):

```powershell
$session = New-Object -ComObject Microsoft.Update.Session
$updates = $session.CreateUpdateSearcher().Search("IsInstalled=0 and IsHidden=0").Updates
$found = $updates.Count
# Updates in the Security Updates or Critical Updates category count as security.
$security = @($updates | Where-Object { $_.Categories | Where-Object { $_.Name -in "Security Updates", "Critical Updates" } }).Count
$reboot = [int](Test-Path "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired")
@("deskwatch_updates_pending $found", "deskwatch_updates_security $security", "deskwatch_reboot_required $reboot") |
  Set-Content -Encoding ascii "C:\Program Files\windows_exporter\textfile_inputs\updates.prom"
```

Fedora, RHEL and similar: count with `dnf check-update -q | grep -c .`, count the security ones with
`dnf updateinfo list security -q | grep -c .` and use `needs-restarting -r` (exit code 1 means a restart is needed).
As a script that writes the file:

```sh
#!/bin/sh
out=/var/lib/node_exporter/textfile_collector/deskwatch_updates.prom
total=$(dnf check-update -q | grep -c .)
security=$(dnf updateinfo list security -q | grep -c .)
needs-restarting -r >/dev/null 2>&1; reboot=$?
printf 'deskwatch_updates_pending %s\ndeskwatch_updates_security %s\ndeskwatch_reboot_required %s\n' \
  "$total" "$security" "$reboot" > "$out.tmp" && mv "$out.tmp" "$out"
```

### Turn on the textfile collector

The file only counts when the exporter reads it. That is the step most often missed.

- **node_exporter** reads a folder only when it is started with `--collector.textfile.directory=<folder>`. Packages
  often set this already (Debian's `prometheus-node-exporter` reads `/var/lib/prometheus/node-exporter`); a plain
  binary or the Docker image does not.
- **windows_exporter** reads `textfile_inputs` next to the exe by default. The file needs the `.prom` extension and
  must be plain ASCII or UTF-8 without a BOM, or the exporter skips it.

Nothing changes in `prometheus.yml`: the metrics come in with the normal node_exporter or windows_exporter scrape, so
they already carry that host's `instance` label.

### node_exporter in Docker

When node_exporter runs as a container, give it a volume for the text file and let a small second container write
into it. The one below looks at the host's own package lists through read-only mounts, so it needs no root on the
host and installs nothing. The host keeps its package lists fresh itself (`apt-daily.timer` does this on Debian and
Ubuntu). Restart needed is `/run/reboot-required`, or a newer kernel in `/boot` than the one running.

```yaml
# docker-compose.yml on a Debian or Ubuntu host
services:
  node-exporter:
    image: prom/node-exporter:latest
    restart: unless-stopped
    network_mode: host        # so the network figures are the real network card, not the container's
    pid: host
    command:
      - --path.procfs=/host/proc
      - --path.sysfs=/host/sys
      - --path.rootfs=/rootfs
      - --collector.textfile.directory=/textfile
    volumes:
      - /proc:/host/proc:ro
      - /sys:/host/sys:ro
      - /:/rootfs:ro,rslave
      - textfile:/textfile:ro

  host-updates:
    image: debian:stable-slim
    restart: unless-stopped
    entrypoint: ["/bin/sh", "-c"]
    command:
      - |
        while true; do
          n=$$(apt-get -s -o Debug::NoLocking=1 dist-upgrade 2>/dev/null | grep -c '^Inst ')
          newest=$$(ls /host/boot/vmlinuz-* 2>/dev/null | sed 's,.*/vmlinuz-,,' | sort -V | tail -n1)
          r=0
          { [ -e /host/run/reboot-required ] || { [ -n "$$newest" ] && [ "$$newest" != "$$(uname -r)" ]; }; } && r=1
          printf 'node_apt_upgrades_pending %s\nnode_reboot_required %s\n' "$$n" "$$r" > /textfile/updates.prom.tmp
          mv /textfile/updates.prom.tmp /textfile/updates.prom
          sleep 900
        done
    volumes:
      - /etc/apt:/etc/apt:ro
      - /var/lib/apt/lists:/var/lib/apt/lists:ro
      - /var/lib/dpkg:/var/lib/dpkg:ro
      - /boot:/host/boot:ro
      - /run:/host/run:ro
      - textfile:/textfile

volumes:
  textfile:
```

The `$$` is how Compose writes a single `$`. Writing to a `.tmp` file and then moving it keeps the exporter from
reading a half-written file. This sidecar gives only the total, no security count.

### Check it

1. On the host: `curl -s http://localhost:9100/metrics | grep -E 'apt_upgrades|reboot_required|deskwatch_'`
   (Windows: port 9182). No lines means the exporter does not read the file; see above.
2. In Prometheus (Graph tab): `node_apt_upgrades_pending` or `deskwatch_updates_pending`. The `instance` label must be
   the one you chose as the host in DeskWatch. If it is different, the scrape job for the exporter has another
   address or label than the host.
3. In DeskWatch: **Hosts**, then **Refresh**. Tick **Pending updates** and **Restart needed** under **Edit** if they
   are not ticked. The card can take up to 15 minutes to show the first number.

The queries can be changed per field under `queries` (`updates_security` next to `updates`), like the others.

## What is not shown

The old **Load** and **Cores** lines are gone: CPU % says the same thing more plainly. A host that does not answer
Prometheus shows as down.
