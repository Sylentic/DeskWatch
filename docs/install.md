# Installing DeskWatch

DeskWatch supports **Linux and Windows** (x86_64 release binaries for both). Pick your system:

| System | Guide |
|---|---|
| Linux with systemd (written for Debian 12 or newer; other systemd distributions work the same, with their own package commands) | This page |
| Windows 10 and 11, Windows Server 2019 or newer | [windows.md](windows.md) covers the programs, Mosquitto, a config, running the bridge as a service and what differs from Linux |
| Any Linux or Windows machine with Docker | [docker.md](docker.md) covers building the image, a Compose file, secrets as files and reaching the broker; sources, Mosquitto and Home Assistant stay as on this page |
| A full-size status page in a browser (no broker) | [docker.md](docker.md#3-the-dashboard-on-your-homelab) for a container on a homelab, [kiosk.md](kiosk.md) for the page itself and a Linux machine with a screen; skip the Mosquitto steps below and set `[mqtt] enabled = false` |
| macOS | Not covered yet; contributions are welcome |

The source setup in [section 5](#5-sources) (Gitea, GitHub, GitLab, Bitbucket Cloud, Azure DevOps, Prometheus), the Mosquitto logins and
Home Assistant steps are the same on both systems apart from how secrets are stored; [windows.md](windows.md)
points back here for them.

This guide takes a Linux server from nothing to a bridge that serves the dashboard and listens for alerts on your Mosquitto broker. Every
host name, address, port and name below is a placeholder: replace `gitea.example.com`, `broker.example.lan` and
friends with your own.

You can watch everything on the dashboard without any sources, see [Try it with the demo](#8-try-it-with-the-demo).

What you need:

- A Linux server that stays on, with `systemd` (Debian 12 or newer is what the steps are written for).
- A Mosquitto broker the bridge can reach (often the same server), only for alerts over MQTT. The dashboard alone needs
  none: set `[mqtt] enabled = false` and skip step 4.
- At least one source: Gitea, GitHub or GitHub Enterprise, GitLab, Bitbucket Cloud, Azure DevOps, Prometheus, or only Home Assistant alerts. With no
  source at all the Hosts panel is empty: hosts come from a Prometheus source.

Contents: [1. Get the binary](#1-get-the-binary) · [2. Install it](#2-install-the-bridge) ·
[3. Configure](#3-configure-the-bridge) · [4. Mosquitto](#4-mosquitto-logins) · [5. Sources](#5-sources) ·
[6. Home Assistant](#6-home-assistant) · [7. Start and check](#7-start-and-check) ·
[8. Try it with the demo](#8-try-it-with-the-demo) · [9. Update, roll back, remove](#9-update-roll-back-remove) ·
[10. Troubleshooting](#10-troubleshooting)

Prefer a browser to a text editor for the sources? The optional [setup page](setup-page.md) can add, test and remove
sources and tokens (and change the MQTT broker) once you give the bridge a writable [data folder](data-dir.md); on
systemd that means `StateDirectory=deskwatch` in the unit and `[data] dir = "/var/lib/deskwatch"` plus
`[setup] enabled = true` in `bridge.toml`. It has no login until you set an administrator password ([auth.md](auth.md)): trusted network only until then.

## 1. Get the binary

### Option A: download a release

Each release at <https://github.com/Sylentic/DeskWatch/releases> has a tarball for Linux x86_64 (and one for
aarch64 for ARM servers; on a small ARM machine Docker is the easier way) and a checksum
file (the Windows zip is covered in [windows.md](windows.md)). It is built on Ubuntu 22.04, so it runs on Debian 12 and newer. It needs nothing but the system C library.

```sh
VERSION=v1.0.12       # the release you want
cd "$(mktemp -d)"
curl -fLO "https://github.com/Sylentic/DeskWatch/releases/download/$VERSION/deskwatch-bridge-$VERSION-x86_64-linux.tar.gz"
curl -fLO "https://github.com/Sylentic/DeskWatch/releases/download/$VERSION/deskwatch-bridge-$VERSION-x86_64-linux.tar.gz.sha256"
sha256sum -c "deskwatch-bridge-$VERSION-x86_64-linux.tar.gz.sha256"
tar xzf "deskwatch-bridge-$VERSION-x86_64-linux.tar.gz"
cd "deskwatch-bridge-$VERSION-x86_64-linux"
```

The folder holds the `deskwatch-bridge` binary, `config.example.toml`, `deploy/` (systemd unit and Mosquitto
examples), `homeassistant/` (blueprints), `docs/` and the licenses. The steps below run from inside it.

### Option B: build from source

Needed on other CPUs (aarch64 has its own tarball), or if you want to read the code first. The bridge needs Rust
1.88 or newer, which is newer than Debian's packaged compiler, so use [rustup](https://rustup.rs):

```sh
sudo apt install build-essential cmake git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"

git clone https://github.com/Sylentic/DeskWatch.git
cd DeskWatch
git checkout v1.0.12             # or dev for the newest code that is not released yet
cargo build --release --locked -p deskwatch-bridge
```

The binary is `target/release/deskwatch-bridge`. The steps below run from the repository root; where Option A says
`./deskwatch-bridge`, use `target/release/deskwatch-bridge`.

## 2. Install the bridge

```sh
sudo install -m 755 deskwatch-bridge /usr/local/bin/deskwatch-bridge       # Option B: target/release/deskwatch-bridge
sudo install -D -m 644 config.example.toml /etc/deskwatch/bridge.toml      # Option B: bridge/config.example.toml
sudo install -m 644 deploy/deskwatch-bridge.service /etc/systemd/system/
sudo install -d -m 700 /etc/deskwatch/credentials
```

The service runs as a throwaway user (`DynamicUser=yes`) with a read-only view of the system. It cannot read
`/etc/deskwatch/credentials` itself; systemd reads each secret as root and hands it to the bridge through
`LoadCredential=` lines in the unit. So the pattern for every secret is the same:

1. Put the secret in its own file in `/etc/deskwatch/credentials/`, owned by root, mode 600.
2. Add a `LoadCredential=<name>:/etc/deskwatch/credentials/<name>` line to the unit.
3. Name it in `bridge.toml` with a key ending in `_file`, using the plain `<name>`.

Secrets never go into `bridge.toml`. Write them with a command that keeps them out of your shell history, for
example `sudo sh -c 'umask 077; cat > /etc/deskwatch/credentials/NAME'` and paste, then press Ctrl-D.

Changing the unit (`LoadCredential=` lines) needs `sudo systemctl daemon-reload`; changing `bridge.toml` or a
credential needs `sudo systemctl restart deskwatch-bridge`.

## 3. Configure the bridge

Edit `/etc/deskwatch/bridge.toml`. [`bridge/config.example.toml`](../bridge/config.example.toml) explains every
key; every key is optional. Unknown keys are an error, so a typo stops the bridge at start instead of being
ignored. The parts you will touch:

- `[mqtt]`: `host` and `port` of your broker. `username` and `password_file` once you turn on broker logins (step 4).
- `[server]`: no longer used. The bridge does not read its own machine; add it as a host under Prometheus ([hosts.md](hosts.md)).
- `[http]`: the address webhooks arrive on (default `0.0.0.0:8787`). It also serves the dashboard, the setup page and the health check, so it is open whenever MQTT is on, and
  that is the case by default. If the server has a firewall, allow this port from the Gitea host only. Behind a reverse proxy, set
  `listen = "127.0.0.1:8787"`.
- `[[source.gitea]]`, `[[source.github]]`, `[[source.gitlab]]`, `[[source.bitbucket]]`, `[[source.azure_devops]]`, `[[source.prometheus]]`: one block per instance (step 5).
- `[alerts]`: alerts raised over MQTT or HTTP (Home Assistant, scripts). `[[rotation]]` blocks from older versions are ignored with a warning; delete them.

**The example config ships with a Gitea block switched on.** Without a Gitea server, delete that
`[[source.gitea]]` block (or the bridge stops at start, asking for the `gitea-webhook-secret` credential), and
also delete the `LoadCredential=gitea-webhook-secret` line from the unit. A config with no `[[source.*]]` block
at all is valid.

## 4. Mosquitto logins

A broker that accepts anyone lets anything on your network raise alerts on your dashboard. Give the bridge and
Home Assistant each their own login with topic limits. The full guide, including how to find the other clients
using your broker before you turn anonymous access off (do not skip that), is [mosquitto.md](mosquitto.md), with
examples in [`deploy/mosquitto/`](../deploy/mosquitto). In short:

```sh
sudo mosquitto_passwd -c /etc/mosquitto/passwd deskwatch-bridge     # -c only for the first user
sudo mosquitto_passwd /etc/mosquitto/passwd homeassistant
# install deploy/mosquitto/acl.example as the acl_file, then reload Mosquitto
```

Then give the bridge its password, and uncomment `LoadCredential=mqtt-password:...` in the unit:

```sh
sudo sh -c 'umask 077; printf %s "THE-BRIDGE-PASSWORD" > /etc/deskwatch/credentials/mqtt-password'
```

```toml
[mqtt]
host = "broker.example.lan"    # "localhost" if Mosquitto runs on this server
username = "deskwatch-bridge"
password_file = "mqtt-password"
```

If your broker still allows anonymous clients, you can leave `username` out and start there. Do move to logins
before you put the broker on a shared network.

## 5. Sources

Each source is optional. Add the ones you use.

### Gitea (Actions and pull requests)

Gitea tells the bridge about runs the moment they start and end, by webhook. Tested on a real Gitea server.

1. Pick a random webhook secret and store it as the credential the config names:

   ```sh
   openssl rand -hex 32 | sudo sh -c 'umask 077; cat > /etc/deskwatch/credentials/gitea-webhook-secret'
   ```

2. For private repositories also create a read-only token in Gitea (Settings, Applications, scope
   `read:repository`) and store it as `/etc/deskwatch/credentials/gitea-token`. Public repositories need no token.
3. Uncomment or add the `LoadCredential=` lines for both in the unit, then `sudo systemctl daemon-reload`.
4. Fill in the block in `bridge.toml` (in the example, `token_file` is commented out; uncomment it for private repositories):

   ```toml
   [[source.gitea]]
   name = "home"                              # used in the webhook URL
   base_url = "https://gitea.example.com"
   webhook_secret_file = "gitea-webhook-secret"
   token_file = "gitea-token"                 # leave out for public repositories (commented out in the example)
   repos = ["owner/repo"]                     # repositories whose open PRs are counted
   ```

5. In Gitea, add a webhook (per repository, per organization, or system-wide in the site admin):
   - Target URL: `http://<bridge-host>:8787/webhook/gitea/home` (the last part is the source's `name`), method POST,
     content type `application/json`
   - Secret: the same value as the webhook secret credential
   - Trigger on custom events: **Workflow Run**, **Workflow Job** and **Pull Request**, plus **Issues** to get a new
     issue in the alerts list (`alert_new_issues = false` turns that off)
6. Gitea refuses webhooks to private addresses by default. Allow the bridge host in Gitea's `app.ini`, then
   restart Gitea:

   ```ini
   [webhook]
   ALLOWED_HOST_LIST = private
   ```

The bridge rejects any request without a valid `X-Gitea-Signature`. Step progress comes from polling the Actions
API every 5 seconds while a job runs, and open PRs are re-polled every 60 seconds as a safety net. Cancelled runs
show no alert. `interrupt` limits which repositories may take over the screen (`true`, `false`, or a list of
`owner/repo` names) and `alias` gives repositories short labels on the dashboard. For several Gitea instances add one block
per instance, each with its own `name` and webhook URL.

### GitHub and GitHub Enterprise

GitHub cannot reach a server at home, so the bridge polls: open PRs and the latest run of every workflow each 60
seconds, and the jobs of a run every 5 seconds while it is in progress. Requests send the last `ETag`, so an
unchanged answer does not count against the rate limit on github.com. One block type covers github.com, GitHub
Enterprise Server and GHE.com; only `base_url` differs:

| Host | `base_url` |
|---|---|
| github.com, including Enterprise Cloud | leave out (`https://api.github.com`) |
| GitHub Enterprise Server | `https://ghe.example.com/api/v3` |
| GHE.com (data residency) | `https://api.your-subdomain.ghe.com` |

1. Create a fine-grained personal access token limited to the repositories you want on the dashboard, with read-only
   permissions **Metadata**, **Pull requests** and **Actions**. Nothing else is needed. Set an expiry you will
   remember; when it lapses the dashboard shows a **Source down** pill.
2. Store it: `/etc/deskwatch/credentials/github-personal` (mode 600), and add
   `LoadCredential=github-personal:/etc/deskwatch/credentials/github-personal` to the unit.
3. Add the block, one per account or Enterprise instance:

   ```toml
   [[source.github]]
   name = "personal"
   token_file = "github-personal"
   repos = ["your-user/repo-a"]
   ```

A refused token or an unreachable server shows as a **Source down** pill; polling backs off (1, 2, 5 minutes) and slows
down when less than 10 % of the rate limit is left. Runs that finished before the bridge started show in the
Pipelines card and count in the red Failed badge but do not take over the screen. `interrupt` and `alias` work as for
Gitea; a new non-draft PR flashes "New PR" unless `notify_new_prs = false`. Only the first 100 open PRs per
repository are counted. A new issue goes to the alerts list until dismissed, unless `alert_new_issues = false` (one
extra request per repository per poll; the token needs **Issues: Read**).

To get an info alert when a build and publish finishes (for example a release or the `:dev` image), list the
workflow file in `alert_finished_runs`, such as `alert_finished_runs = ["pipeline.yml"]`. It is off unless listed, and
only push runs that end successfully count; pull request runs, failed runs (they already show red) and runs that
finished before the bridge started do not. It costs no extra requests.

### GitLab

For gitlab.com and self-managed GitLab the bridge polls open merge requests (counted as the open PRs) and pipelines
with a read-only token (scope `read_api`, Reporter role), and shows a running pipeline with its current stage.
Creating the token, the options and what is and is not tested are in [gitlab.md](gitlab.md).

```toml
[[source.gitlab]]
name = "personal"
token_file = "gitlab-personal"
projects = ["your-group/project-a"]
# base_url = "https://gitlab.example.com"   # self-managed
```

### Bitbucket Cloud

For `bitbucket.org` the bridge polls open pull requests and Pipelines with a read-only access token (Pull requests:
Read, Pipelines: Read) and shows a running pipeline with its current step. Creating the token, the options and what
is and is not tested are in [bitbucket.md](bitbucket.md).

```toml
[[source.bitbucket]]
name = "personal"
token_file = "bitbucket-personal"
repos = ["your-workspace/repo-a"]
```

### Azure DevOps

For Azure DevOps Services (`dev.azure.com`) the bridge polls builds and open PRs with a read-only personal access
token and shows YAML pipeline runs (such as Terraform deploys) with their stage and task, plus a notice when a
deploy waits for an approval. A work pipeline stays out of the way unless you set `interrupt`. Creating the token
with the minimal scopes (**Build: Read**, **Code: Read**), the credential file and the options are in
[azure-devops.md](azure-devops.md).

```toml
[[source.azure_devops]]
name = "work"
organization = "your-org"
projects = ["project-a"]
token_file = "azdo-work"
```

### Prometheus (host stats and containers)

Host stats come from Prometheus. If you run it with
node_exporter or windows_exporter (and cAdvisor for containers), a `[[source.prometheus]]` block adds a host card per
machine you choose and a list of expected containers that stopped. On the setup page, add the source under **Sources**
and tick the machines under **Hosts**; the file below is the same thing by hand. The bridge only reads, over the HTTP
API. A bearer token (`token_file`) is only needed behind a reverse proxy.

```toml
[[source.prometheus]]
name = "home"
url = "http://prometheus.example.lan:9090"
hosts = [
  { instance = "node-exporter:9100", name = "Home server", cadvisor = "cadvisor:8080", expect_containers = ["db", "web"] },
  # show = ["cpu", "memory", "updates"] limits what the card shows, see hosts.md
]

```

`instance` is the label Prometheus gives the node_exporter (or, for Windows, windows_exporter) target; [hosts.md](hosts.md) covers Windows and pending updates. A stopped container disappears from cAdvisor,
so only names in `expect_containers` can be reported, and they must match the container name exactly. Each host
gets a card on the dashboard and what is down is listed there. Hosts and containers that are down appear in the Containers list. GPU usage, VRAM and GPU temperature show on
the card of any host that reports them: by default they are read from the NVIDIA DCGM exporter
(usage is the busiest of `DCGM_FI_DEV_GPU_UTIL`, `DCGM_FI_DEV_ENC_UTIL` and `DCGM_FI_DEV_DEC_UTIL`, so video
transcoding counts; framebuffer used and free; `DCGM_FI_DEV_GPU_TEMP`), so add that exporter as a Prometheus
target; otherwise replace `gpu_pct`, `gpu_mem_pct` and `gpu_temp_c` under `queries`. Tested on a GeForce card, where
these basic DCGM fields work. The temperature is the CPU's own sensor (`coretemp`, `k10temp`, `zenpower` or
`cpu_thermal`), or the hottest sensor on a host without one.
The PromQL for each field is in `bridge/src/prometheus/mod.rs` and can be replaced per field
with `queries`.

## 6. Home Assistant

Home Assistant raises and clears alerts on `deskpanel/alert` through two script blueprints. Setup (broker
login limited to that one topic, MQTT integration, importing the blueprints, creating the scripts) is in
[home-assistant.md](home-assistant.md). Alerts are on by default in the bridge and show on the dashboard. Without Home Assistant, any script can do the same with
`mosquitto_pub`, see [home-assistant.md](home-assistant.md).

## 7. Start and check

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now deskwatch-bridge
sudo journalctl -u deskwatch-bridge -f
```

Look for `loaded config from /etc/deskwatch/bridge.toml` and `connected to MQTT broker`. The bridge builds all
sources before it connects, so a missing credential or a port already in use stops it right away with a message
naming the problem.

Then watch the broker, with a login that may read `deskpanel/#`:

```sh
mosquitto_sub -h broker.example.lan -u deskwatch-bridge -P '...' -t 'deskpanel/#' -v
```

You should see `deskpanel/bridge/status online`, and an alert you raise on `deskpanel/alert` (see
[home-assistant.md](home-assistant.md#check-it-without-an-automation)). To test a Gitea webhook, use "Test delivery" on the webhook in
Gitea and look for a 2xx answer there; run a workflow to see a job page appear.

## 8. Try it with the demo

`deskwatch-bridge --demo --kiosk` plays a loop of fake data (a running job, a failed run, PRs, an alert) on the
dashboard page, with no sources and no broker needed. Open `http://localhost:8787/`. The
[development.md](development.md#demo-mode) has the details.

## 9. Update, roll back, remove

Update: repeat step 1 for the new version, then

```sh
sudo install -m 755 deskwatch-bridge /usr/local/bin/deskwatch-bridge
sudo systemctl restart deskwatch-bridge
```

Read [CHANGELOG.md](../CHANGELOG.md) first; a new release may add config keys. Compare your `bridge.toml` with
the new `config.example.toml`. Roll back by installing the older binary the same way. Remove everything with:

```sh
sudo systemctl disable --now deskwatch-bridge
sudo rm /etc/systemd/system/deskwatch-bridge.service /usr/local/bin/deskwatch-bridge
sudo rm -r /etc/deskwatch
```

and delete the broker users you created.

## 10. Troubleshooting

| Symptom | Likely cause |
|---|---|
| Bridge exits at start naming a credential | The `LoadCredential=` line is missing, or the file does not exist or is not readable by root. Remember `daemon-reload` |
| Bridge exits with a TOML error | A mistyped or unknown key (unknown keys are rejected); the message names it |
| `MQTT connection error`, retries every 5 s | Wrong host, port or login, or the broker's ACL/listener settings; see [mosquitto.md](mosquitto.md) |
| Gitea says delivery failed | `ALLOWED_HOST_LIST` not set, firewall blocks the port, or the URL's last part does not match the source `name` |
| Webhook answers 401 | The secret in Gitea differs from the credential file |
| **Source down** pill on the dashboard | A source cannot log in or connect: check the journal for the source's name; a GitHub token may have expired |
| The dashboard shows nothing new after a Gitea run | Check the repository is not excluded by `interrupt`, and that the webhook has the **Workflow Run** and **Workflow Job** events |
| A host card shows `--` for CPU or network right after a start | Normal: both need two samples from Prometheus |

Logs are verbose on request: set `Environment=RUST_LOG=debug` in the unit (or `deskwatch_bridge=debug` to leave
other crates quiet).
