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
green, and open PRs show as a header badge and a rotation page. GitHub and Azure DevOps come next, followed
by a firmware spike once the screen arrives.

## Running the bridge

Needs a stable Rust toolchain and a Mosquitto broker.

```sh
cargo run -p deskwatch-bridge -- bridge/config.example.toml
mosquitto_sub -t 'deskpanel/#' -v   # in another terminal
```

The config path can also come from `DESKWATCH_CONFIG`; the default is `/etc/deskwatch/bridge.toml`. The MQTT
password, if any, is read from `DESKWATCH_MQTT_PASSWORD`. A sample systemd unit is in
[deploy/deskwatch-bridge.service](deploy/deskwatch-bridge.service).

## Gitea setup

1. Add a `[gitea]` block to the bridge config (see `bridge/config.example.toml`).
2. Pick a random webhook secret and put it in the bridge's env file as
   `DESKWATCH_GITEA_WEBHOOK_SECRET`. For private repos, also create a read-only token
   (scopes `read:repository`) and set `DESKWATCH_GITEA_TOKEN`.
3. In Gitea, add a webhook (per repo, per organisation, or system-wide in the site admin):
   - Target URL: `http://<bridge-host>:8787/webhook/gitea`, method POST, content type `application/json`
   - Secret: the same value as `DESKWATCH_GITEA_WEBHOOK_SECRET`
   - Trigger on custom events: **Workflow Run**, **Workflow Job** and **Pull Request**
4. Gitea (1.27 or 28.x) refuses webhooks to private addresses by default. Allow the bridge host in `app.ini`:

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
