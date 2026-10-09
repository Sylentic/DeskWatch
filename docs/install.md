# Installing DeskWatch

DeskWatch supports **Linux and Windows** (x86_64 release binaries for both). Pick your system:

| System | Guide |
|---|---|
| Linux with systemd (written for Debian 12 or newer; other systemd distributions work the same, with their own package commands) | This page |
| Windows 10 and 11, Windows Server 2019 or newer | [windows.md](windows.md) covers the programs, Mosquitto, a config, running the bridge as a service and what differs from Linux |
| Raspberry Pi (64-bit Raspberry Pi OS) | [raspberry-pi.md](raspberry-pi.md) covers the aarch64 release binary with systemd, and the Docker route |
| Any Linux or Windows machine with Docker | [docker.md](docker.md) covers building the image, a Compose file, secrets as files and reaching the broker; sources, Mosquitto and Home Assistant stay as on this page |
| A full-size status page in a browser (no ESP panel, no broker) | [docker.md](docker.md#3-the-dashboard-on-your-homelab) for a container on a homelab, [kiosk.md](kiosk.md) for the page itself and a Raspberry Pi with a screen; skip the Mosquitto steps below and set `[mqtt] enabled = false` |
| macOS | Not covered yet; contributions are welcome |

The source setup in [section 5](#5-sources) (Gitea, GitHub, Azure DevOps, Prometheus), the Mosquitto logins and
Home Assistant steps are the same on both systems apart from how secrets are stored; [windows.md](windows.md)
points back here for them.

This guide takes a Linux server from nothing to a bridge that publishes pages to your Mosquitto broker. Every
host name, address, port and name below is a placeholder: replace `gitea.example.com`, `broker.example.lan` and
friends with your own.

The panel itself (the ESP32-S3 firmware) is not part of 0.9, see [What is not in 0.9](../README.md#what-is-not-in-09).
Until it exists you can watch everything in the desktop simulator, see [Try it without hardware](#8-try-it-without-hardware).

What you need:

- A Linux server that stays on, with `systemd` (Debian 12 or newer is what the steps are written for).
- A Mosquitto broker the bridge can reach (often the same server).
- At least one source: Gitea, GitHub or GitHub Enterprise, Azure DevOps, Prometheus, or only Home Assistant alerts. With no
  source at all the bridge still shows the server's own stats.

Contents: [1. Get the binary](#1-get-the-binary) · [2. Install it](#2-install-the-bridge) ·
[3. Configure](#3-configure-the-bridge) · [4. Mosquitto](#4-mosquitto-logins) · [5. Sources](#5-sources) ·
[6. Home Assistant](#6-home-assistant) · [7. Start and check](#7-start-and-check) ·
[8. Try it without hardware](#8-try-it-without-hardware) · [9. Update, roll back, remove](#9-update-roll-back-remove) ·
[10. Troubleshooting](#10-troubleshooting)

## 1. Get the binary

### Option A: download a release

Each release at <https://github.com/Sylentic/DeskWatch/releases> has a tarball for Linux x86_64 (and one for
aarch64, the Raspberry Pi, see [raspberry-pi.md](raspberry-pi.md)) and a checksum
file (the Windows zip is covered in [windows.md](windows.md)). It is built on Ubuntu 22.04, so it runs on Debian 12 and newer. It needs nothing but the system C library.

```sh
VERSION=v0.9.5      # the release you want
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

Needed on other CPUs (a Raspberry Pi has its own tarball, see [raspberry-pi.md](raspberry-pi.md)), or if you want to read the code first. The bridge needs Rust
1.88 or newer, which is newer than Debian's packaged compiler, so use [rustup](https://rustup.rs):

```sh
sudo apt install build-essential cmake git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"

git clone https://github.com/Sylentic/DeskWatch.git
cd DeskWatch
git checkout v0.9.5            # or stay on main for the newest, untagged code
cargo build --release --locked -p deskwatch-bridge
```

The binary is `target/release/deskwatch-bridge`. Building only the bridge (`-p deskwatch-bridge`) does not need
SDL2; that is only for the simulator. The steps below run from the repository root; where Option A says
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
- `[server]`: the name shown on the stats page and which network interface and disk to report.
- `[http]`: the address webhooks arrive on (default `0.0.0.0:8787`). It only opens when a Gitea source exists.
  If the server has a firewall, allow this port from the Gitea host only. Behind a reverse proxy, set
  `listen = "127.0.0.1:8787"`.
- `[[source.gitea]]`, `[[source.github]]`, `[[source.azure_devops]]`, `[[source.prometheus]]`: one block per instance (step 5).
- `[alerts]` and `[[rotation]]`: which pages rotate and for how long.

**The example config ships with a Gitea block switched on.** Without a Gitea server, delete that
`[[source.gitea]]` block (or the bridge stops at start, asking for the `gitea-webhook-secret` credential), and
also delete the `LoadCredential=gitea-webhook-secret` line from the unit. A config with no `[[source.*]]` block
at all is valid.

## 4. Mosquitto logins

A broker that accepts anyone lets anything on your network draw on the panel. Give the bridge, the panel and
Home Assistant each their own login with topic limits. The full guide, including how to find the other clients
using your broker before you turn anonymous access off (do not skip that), is [mosquitto.md](mosquitto.md), with
examples in [`deploy/mosquitto/`](../deploy/mosquitto). In short:

```sh
sudo mosquitto_passwd -c /etc/mosquitto/passwd deskwatch-bridge     # -c only for the first user
sudo mosquitto_passwd /etc/mosquitto/passwd deskwatch-panel
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
before you put the panel on a shared network.

## 5. Sources

Each source is optional. Add the ones you use.

### Gitea (Actions and pull requests)

Gitea tells the bridge about runs the moment they start and end, by webhook. Gitea 1.27 and 28.x are tested.

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
   - Trigger on custom events: **Workflow Run**, **Workflow Job** and **Pull Request**
6. Gitea refuses webhooks to private addresses by default. Allow the bridge host in Gitea's `app.ini`, then
   restart Gitea:

   ```ini
   [webhook]
   ALLOWED_HOST_LIST = private
   ```

The bridge rejects any request without a valid `X-Gitea-Signature`. Step progress comes from polling the Actions
API every 5 seconds while a job runs, and open PRs are re-polled every 60 seconds as a safety net. Cancelled runs
show no alert. `interrupt` limits which repositories may take over the screen (`true`, `false`, or a list of
`owner/repo` names) and `alias` gives repositories short panel labels. For several Gitea instances add one block
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

1. Create a fine-grained personal access token limited to the repositories you want on the panel, with read-only
   permissions **Metadata**, **Pull requests** and **Actions**. Nothing else is needed. Set an expiry you will
   remember; when it lapses the panel shows the `warn` badge.
2. Store it: `/etc/deskwatch/credentials/github-personal` (mode 600), and add
   `LoadCredential=github-personal:/etc/deskwatch/credentials/github-personal` to the unit.
3. Add the block, one per account or Enterprise instance:

   ```toml
   [[source.github]]
   name = "personal"
   token_file = "github-personal"
   repos = ["your-user/repo-a"]
   ```

A refused token or an unreachable server shows as the `warn` badge; polling backs off (1, 2, 5 minutes) and slows
down when less than 10 % of the rate limit is left. Runs that finished before the bridge started show on the
pipelines page and count in the red badge but do not take over the screen. `interrupt` and `alias` work as for
Gitea; a new non-draft PR flashes "New PR" unless `notify_new_prs = false`. Only the first 100 open PRs per
repository are counted.

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

The built-in `stats` page shows the machine the bridge runs on, read from `/proc`. If you run Prometheus with
node_exporter (and cAdvisor for containers), a `[[source.prometheus]]` block adds one `stats:<name>` page per
machine and a `containers` page listing expected containers that stopped. The bridge only reads, over the HTTP
API. A bearer token (`token_file`) is only needed behind a reverse proxy.

```toml
[[source.prometheus]]
name = "home"
url = "http://prometheus.example.lan:9090"
hosts = [
  { instance = "node-exporter:9100", name = "home", cadvisor = "cadvisor:8080", expect_containers = ["db", "web"] },
]

[[rotation]]
page = "stats:home"
dwell_s = 15
skip_when_empty = true

[[rotation]]
page = "containers"
dwell_s = 10
skip_when_empty = true
```

`instance` is the label Prometheus gives the node_exporter target. A stopped container disappears from cAdvisor,
so only names in `expect_containers` can be reported, and they must match the container name exactly. The pages
only join the rotation when you list them in `[[rotation]]`. Hosts and containers that are down count in the
`server` header badge. The PromQL for each field is in `bridge/src/prometheus/mod.rs` and can be replaced per field
with `queries`.

## 6. Home Assistant

Home Assistant raises and clears alerts on `deskpanel/alert` through two script blueprints. Setup (broker
login limited to that one topic, MQTT integration, importing the blueprints, creating the scripts) is in
[home-assistant.md](home-assistant.md). Alerts are on by default in the bridge; add `page = "alerts"` to
`[[rotation]]` to show the list in the idle rotation. Without Home Assistant, any script can do the same with
`mosquitto_pub`, see the [README](../README.md#alerts-from-home-assistant-and-scripts).

## 7. Start and check

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now deskwatch-bridge
sudo journalctl -u deskwatch-bridge -f
```

Look for `loaded config from /etc/deskwatch/bridge.toml` and `connected to MQTT broker`. The bridge builds all
sources before it connects, so a missing credential or a port already in use stops it right away with a message
naming the problem.

Then watch what it publishes, with the login that may read `deskpanel/#`:

```sh
mosquitto_sub -h broker.example.lan -u deskwatch-bridge -P '...' -t 'deskpanel/#' -v
```

You should see `deskpanel/bridge/status online`, a `deskpanel/badges` message and a `deskpanel/screen` message
with the stats page that changes every few seconds. To test a Gitea webhook, use "Test delivery" on the webhook in
Gitea and look for a 2xx answer there; run a workflow to see a job page appear.

## 8. Try it without hardware

Until the firmware exists, the desktop simulator draws the exact pixels the panel will. On a Linux desktop or
laptop (not the server) with Rust and SDL2 (`sudo apt install libsdl2-dev`); on Windows the release zip has
`deskwatch-sim.exe` with SDL2 built in, see [windows.md](windows.md#3-quick-test-the-demo-and-the-simulator):

```sh
export DESKWATCH_MQTT_PASSWORD='...'    # the panel login's password
cargo run -p deskwatch-sim -- --host broker.example.lan --user deskwatch-panel
```

Or play the built-in demo: `deskwatch-bridge --demo` publishes a loop of fake data to a broker on localhost
(`[mqtt]` from a config file if you pass one), `cargo run -p deskwatch-sim -- --host localhost` shows it. The
[README](../README.md#demo-mode) has the details of both.

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
| `warn` badge on the panel | A source cannot log in or connect: check the journal for the source's name; a GitHub token may have expired |
| Panel shows nothing new after a Gitea run | Check the repository is not excluded by `interrupt`, and that the webhook has the **Workflow Run** and **Workflow Job** events |
| Stats page shows `--` for CPU on the first screen | Normal: CPU use and network rates need two samples |

Logs are verbose on request: set `Environment=RUST_LOG=debug` in the unit (or `deskwatch_bridge=debug` to leave
other crates quiet).
