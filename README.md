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
state and online/offline status topics. Data sources share one framework: each `[[source.<type>]]` block
in the config runs as its own task, reads its secrets from credential files, and reports facts and its
health. The Gitea source is in: a running Actions job takes over the screen with step progress, a failed run
shows a red alert until the button is pressed, a successful run flashes green, open PRs show as a header
badge and a rotation page, and the latest run of every pipeline shows on the pipelines page. A source that
cannot log in or connect shows as a `warn` badge. GitHub and Azure DevOps come next, followed by a firmware
spike once the screen arrives.

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

Button: a short press dismisses an alert or notice, or shows the next page during rotation. A long press
pins the current rotation page, or shows the next running job when several run at once.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
