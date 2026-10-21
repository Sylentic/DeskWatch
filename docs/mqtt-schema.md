# DeskWatch MQTT: the alert topic

The bridge reads alerts from one MQTT topic, `deskpanel/alert`, and announces itself on `deskpanel/bridge/status`.
Nothing else goes over MQTT. Home Assistant and scripts raise and clear alerts here; the kiosk page shows them.
The same JSON can be sent to the bridge over HTTP, see [home-assistant.md](home-assistant.md#without-mqtt-the-web-address).

Every JSON sample in [`schema/`](schema/) is checked by `bridge/tests/schema.rs` (the bridge accepts exactly that
payload). Change a sample and the code together, in one PR.

The topic prefix `deskpanel` comes from `[mqtt] topic_prefix` in `bridge.toml`. It kept its old name, so existing
Home Assistant blueprints and broker ACLs keep working.

## 1. Topics

| Topic | Direction | Retained | QoS | Meaning |
|---|---|---|---|---|
| `deskpanel/bridge/status` | bridge -> anyone | yes | 1 | `online` after every (re)connect, `offline` as the Last Will and on a clean shutdown |
| `deskpanel/alert` | Home Assistant, scripts -> bridge | no | 1 | Raise or clear an alert |

The bridge only subscribes to `deskpanel/alert`, and only while `[alerts] enabled` is true. With `[mqtt] enabled =
false` (or `--no-mqtt`) it does not connect at all.

Older versions also published `deskpanel/screen` and `deskpanel/badges` for the ESP32 desk panel, and read
`deskpanel/panel/status` and `deskpanel/panel/event`. The panel was removed, so those topics are gone. A retained
`deskpanel/screen` or `deskpanel/badges` message left on your broker is harmless; clear it with
`mosquitto_pub -r -n -t deskpanel/screen` if you like.

## 2. `deskpanel/alert`

Raise ([sample](schema/alert-raise.json)) or clear ([sample](schema/alert-clear.json)) an alert. Every payload has
`"v": 2`.

| Field | Meaning |
|---|---|
| `id` | Optional. The same id replaces the earlier alert; needed to clear. Missing means a new alert each time |
| `severity` | `info`, `warning` or `critical`. Default `info` |
| `title`, `message` | Text shown, cut to 40 characters. Title defaults to `Alert` |
| `ttl_s` | Optional lifetime in seconds. Defaults: `info` 1800, `warning` and `critical` until cleared |
| `source` | Free text label, default `mqtt` |
| `url` | Optional web address the alert links to |
| `clear` | `true` removes the alert with this `id` |

How each severity shows on the kiosk page:

| Severity | Banner | After it leaves the banner |
|---|---|---|
| `critical` | Red, above running jobs, until it is dismissed or cleared | Alerts pill and list until cleared or `ttl_s` |
| `warning` | Amber, until it is dismissed or 10 min | Alerts pill and list until cleared or `ttl_s` |
| `info` | Short notice for 5 s | Alerts list until cleared or `ttl_s` (no pill) |

Limits on the bridge side (`[alerts]` in the config): at most `max_active` alerts (default 10, the least urgent,
oldest one is dropped first); one update per id per second; payloads over 1 KB and other schema versions are
dropped; `critical_ids`, when set, lists the only ids that may be critical, any other critical alert is shown as a
warning. The broker should only let alert publishers write `deskpanel/alert`, see [mosquitto.md](mosquitto.md).
