# DeskWatch

DeskWatch is an ESP32-S3 desk status panel. When idle it rotates through server stats, open pull requests and pipeline status. When a CI job runs it switches to live progress, shows a red alert on failure (cleared with a button) and a short green flash on success.

## How it fits together

- **Bridge** (Rust, runs on a Debian home server): collects server stats, receives Gitea and GitHub webhooks, polls Azure DevOps, decides what the panel should show and publishes it over MQTT.
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
state and online/offline status topics. The Gitea source is in: a running Actions job takes over the screen
with step progress, a failed run shows a red alert until the button is pressed, a successful run flashes
green, and open PRs show as a header badge and a rotation page. The panel UI and a desktop simulator are in `ui/` and
`sim/`. GitHub and Azure DevOps come next, followed by a firmware spike once the screen arrives.

## Running the bridge

Needs a stable Rust toolchain and a Mosquitto broker.

```sh
cargo run -p deskwatch-bridge -- bridge/config.example.toml
mosquitto_sub -t 'deskpanel/#' -v   # in another terminal
```

The config path can also come from `DESKWATCH_CONFIG`; the default is `/etc/deskwatch/bridge.toml`. The MQTT
password, if any, is read from `DESKWATCH_MQTT_PASSWORD`. A sample systemd unit is in
[deploy/deskwatch-bridge.service](deploy/deskwatch-bridge.service).

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

1. Add a `[gitea]` block to the bridge config (see `bridge/config.example.toml`).
2. Pick a random webhook secret and put it in the bridge's env file as
   `DESKWATCH_GITEA_WEBHOOK_SECRET`. For private repos, also create a read-only token
   (scopes `read:repository`) and set `DESKWATCH_GITEA_TOKEN`.
3. In Gitea, add a webhook (per repo, per organisation, or system-wide in the site admin):
   - Target URL: `http://<bridge-host>:8787/webhook/gitea`, method POST, content type `application/json`
   - Secret: the same value as `DESKWATCH_GITEA_WEBHOOK_SECRET`
   - Trigger on custom events: **Workflow Run**, **Workflow Job** and **Pull Request**
4. Gitea refuses webhooks to private addresses by default. Allow the bridge host in `app.ini`:

   ```ini
   [webhook]
   ALLOWED_HOST_LIST = private
   ```

The bridge rejects any request without a valid `X-Gitea-Signature`. Webhooks give instant job start and
end; step progress comes from polling the Actions jobs API every 5 seconds while a job runs, and the open PR
count is re-polled every 60 seconds as a safety net. Cancelled runs show no alert.

Button: a short press dismisses an alert or notice, or shows the next page during rotation. A long press
pins the current rotation page.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
