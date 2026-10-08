# DeskWatch

DeskWatch is an ESP32-S3 desk status panel. When idle it rotates through server stats, open pull requests and pipeline status. When a CI job runs it switches to live progress, shows a red alert on failure (cleared with a button) and a short green flash on success.

## How it fits together

- **Bridge** (Rust, runs on a Debian home server): collects server stats, receives Gitea webhooks, polls GitHub and (later) Azure DevOps, decides what the panel should show and publishes it over MQTT.
- **Firmware** (Rust, esp-hal + embassy): a deliberately dumb MQTT client on an ESP32-S3 SuperMini with a 4" ST7796S touch screen. It draws whatever page the bridge sends.
- **MQTT** (Mosquitto) sits between the two.

## Design docs

The design lives in the project's shared files for now:

- `mqtt-schema.md`: the MQTT contract between bridge and panel
- `schema-and-wiring.md`: hardware and pin wiring
- `firmware-stack.md`: firmware crate choices
- `display-options.md`: display research

## Status

The bridge is in `bridge/`. It publishes the server stats page and rotates through idle pages, with retained
state and online/offline status topics. Data sources share one framework: each `[[source.<type>]]` block
in the config runs as its own task, reads its secrets from credential files, and reports facts and its
health. The Gitea source is in: a running Actions job takes over the screen with step progress, a failed run
shows a red alert until the button is pressed, a successful run flashes green, open PRs show as a header
badge and a rotation page, and the latest run of every pipeline shows on the pipelines page. A source that
cannot log in or connect shows as a `warn` badge. The GitHub source does the same for github.com, GitHub
Enterprise Server and GHE.com by polling. The Prometheus source adds a stats page per machine (from node_exporter) and a list of stopped
containers (from cAdvisor), with the built-in `/proc` stats page as the fallback. The panel UI and a desktop simulator are in `ui/` and `sim/`. A
firmware spike follows once the screen arrives.

## Running the bridge

Needs a stable Rust toolchain and a Mosquitto broker.

```sh
cargo run -p deskwatch-bridge -- bridge/config.example.toml
mosquitto_sub -t 'deskpanel/#' -v   # in another terminal
```

The config path can also come from `DESKWATCH_CONFIG`; the default is `/etc/deskwatch/bridge.toml`. A sample
systemd unit is in [deploy/deskwatch-bridge.service](deploy/deskwatch-bridge.service).

Secrets never go in the config. Keys ending in `_file` name a credential: a plain name such as `gitea-token`
is read from `$CREDENTIALS_DIRECTORY`, which systemd fills from the unit's `LoadCredential=` lines; an
absolute path is read directly, which is handy when running by hand. The MQTT password can also come from
`DESKWATCH_MQTT_PASSWORD`.

## Host stats from Prometheus

The built-in `stats` page shows the machine the bridge runs on, read from `/proc`. If you run Prometheus with
node_exporter (and cAdvisor for containers), a `[[source.prometheus]]` block adds one `stats:<name>` page per
machine, and a `containers` page that lists expected containers that stopped and hosts that stopped answering.
Both pages only join the rotation when you list them in `[[rotation]]`; set `skip_when_empty` on `containers`
so a quiet day never shows it. The `server` badge counts what is down. The bridge only reads, over the HTTP API;
a bearer token (`token_file`) is optional and only needed behind a proxy.

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
```

`instance` is the label Prometheus gives the node_exporter target. A stopped container disappears from cAdvisor,
so only the names in `expect_containers` can be reported; they must match the container name exactly. When
Prometheus cannot be reached or refuses the token, the host pages are greyed out and the `warn` badge appears.
The PromQL for each field is in `bridge/src/prometheus/mod.rs` and can be replaced per field with `queries`.

## Panel UI and simulator

The panel's drawing code lives in `ui/` (`deskwatch-ui`), a `no_std` crate that parses the MQTT payloads and
draws the six page templates (stats, job, alert, list, number, notice) plus the header badges and footer on a
480x320 landscape screen. The firmware will use it unchanged; until the screen arrives, `sim/`
(`deskwatch-sim`) draws the same pixels in a desktop window.

The simulator needs SDL2 (`sudo apt install libsdl2-dev` on Debian or Ubuntu).

```sh
# Live: subscribe to the bridge's topics; click is the button (hold for a long press)
cargo run -p deskwatch-sim -- --host localhost

# Flip through the example payloads, no broker needed
cargo run -p deskwatch-sim -- preview ui/testdata/screens --badges ui/testdata/badges/default.json

# Headless PNG of one payload (also builds without SDL: --no-default-features)
cargo run -p deskwatch-sim -- render ui/testdata/screens/job_deploy.json -o job.png
```

You can also drive the live window by hand, before the bridge produces a page:

```sh
mosquitto_pub -r -t deskpanel/screen -f ui/testdata/screens/alert_failed.json
```

`ui/testdata/screens/` has one example payload per template and edge case. `cargo test -p deskwatch-ui`
renders each one and compares it with the approved image in `ui/tests/snapshots/`; after an intended layout
change, re-approve with `UPDATE_SNAPSHOTS=1 cargo test -p deskwatch-ui` and review the PNG diff in the PR.

## Gitea setup

1. Add a `[[source.gitea]]` block to the bridge config (see `bridge/config.example.toml`), for example
   with `name = "home"`.
2. Pick a random webhook secret and store it as the credential named by `webhook_secret_file`. For
   private repos, also create a read-only token (scope `read:repository`) and store it as the credential
   named by `token_file`. Add a `LoadCredential=` line for each to the systemd unit.
3. In Gitea, add a webhook (per repo, per organisation, or system-wide in the site admin):
   - Target URL: `http://<bridge-host>:8787/webhook/gitea/home` (the source's `name`), method POST,
     content type `application/json`
   - Secret: the same value as the webhook secret credential
   - Trigger on custom events: **Workflow Run**, **Workflow Job** and **Pull Request**
4. Gitea (1.27 or 28.x) refuses webhooks to private addresses by default. Allow the bridge host in `app.ini`:

   ```ini
   [webhook]
   ALLOWED_HOST_LIST = private
   ```

The bridge rejects any request without a valid `X-Gitea-Signature`. Webhooks give instant job start and
end; step progress comes from polling the Actions jobs API every 5 seconds while a job runs, and the open PR
count is re-polled every 60 seconds as a safety net. Cancelled runs show no alert.

`interrupt` decides which repositories may take over the screen: `true` (default), `false`, or a list of
`owner/repo` names. Runs from other repositories still show on the pipelines page, and their failures still
count in the red header badge until a green run of the same pipeline replaces them. `alias` gives
repositories short labels on the panel.

Several Gitea instances work too: add one `[[source.gitea]]` block per instance, each with its own name and
webhook URL.

## GitHub and GitHub Enterprise setup

GitHub cannot reach a server at home, so the bridge polls it: open PRs and the latest run of each workflow
every 60 seconds, and the jobs of a run every 5 seconds while it is in progress. Each request sends the last
`ETag`, so an unchanged answer does not count against the rate limit on github.com. One adapter covers
github.com, GitHub Enterprise Server and GHE.com; only `base_url` differs:

| Host | `base_url` |
|---|---|
| github.com, including Enterprise Cloud | leave out (`https://api.github.com`) |
| GitHub Enterprise Server | `https://ghe.example.com/api/v3` |
| GHE.com (data residency) | `https://api.your-subdomain.ghe.com` |

1. Create a fine-grained personal access token limited to the repositories you want on the panel, with
   read-only permissions **Metadata**, **Pull requests** and **Actions**. Nothing else is needed.
2. Store it as a credential, for example `/etc/deskwatch/credentials/github-personal` (root, mode 600), and
   add `LoadCredential=github-personal:/etc/deskwatch/credentials/github-personal` to the systemd unit.
3. Add a `[[source.github]]` block with `token_file = "github-personal"` and the `repos` to watch (see
   `bridge/config.example.toml`). Use one block per account or Enterprise instance.

A refused token or an unreachable server shows as the `warn` badge; polling then backs off (1, 2, 5 minutes)
and slows down when less than 10 % of the rate limit is left. Runs that finished before the bridge started
show on the pipelines page and count in the red badge, but do not take over the screen. `interrupt`,
`alias` and the pipelines page work as for Gitea; a new non-draft PR flashes "New PR" unless
`notify_new_prs = false`. Only the first 100 open PRs per repository are counted.

Button: a short press dismisses an alert or notice, or shows the next page during rotation. A long press
pins the current rotation page, or shows the next running job when several run at once.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
