# Home Assistant alerts

Anyone with access to Home Assistant (HA) can put a message on the DeskWatch panel from an automation, without
typing JSON or topics. Two script blueprints do the work; they are thin wrappers around HA's `mqtt.publish` action
that send a message to the bridge's `deskpanel/alert` topic ([schema](mqtt-schema.md), section "deskpanel/alert").

| Blueprint | Creates | Does |
|---|---|---|
| [`deskwatch_alert.yaml`](../homeassistant/blueprints/script/deskwatch_alert.yaml) | script "DeskWatch alert" | Raise or replace an alert |
| [`deskwatch_clear.yaml`](../homeassistant/blueprints/script/deskwatch_clear.yaml) | script "DeskWatch clear" | Remove an alert by its id |

## One-time setup

1. **Broker login.** Create a broker user for HA limited to writing `deskpanel/alert`
   ([mosquitto.md](mosquitto.md)).
2. **MQTT integration.** In HA: Settings, Devices & services, Add integration, MQTT. Enter the broker host, port
   and the HA username and password.
3. **Import the blueprints.** Settings, Automations & scenes, Blueprints, Import blueprint, and paste the raw
   GitHub URL of each file above. (Or copy the files into `<config>/blueprints/script/`.)
4. **Create the scripts.** From each blueprint choose "Create script". Name them **DeskWatch alert** and
   **DeskWatch clear**. Leave the topic at `deskpanel/alert` unless you changed `topic_prefix` in the bridge.
5. **Bridge side.** Alerts are on by default (`[alerts]` in the bridge config). Add `page = "alerts"` to
   `[[rotation]]` if you want the alert list in the idle rotation.

## Build an alert

In any automation, add the action **Run script: DeskWatch alert** and fill in:

| Field | Meaning |
|---|---|
| Title | Required. Short heading, cut to 40 characters |
| Message | One line of detail, cut to 40 characters |
| Severity | `info` flashes a notice, `warning` shows amber until the button is pressed, `critical` shows red above running jobs |
| Id | Optional name such as `washer`. The same id replaces the earlier alert; needed to clear it |
| Duration | Optional minutes the alert stays. Empty: `info` 30 minutes, warning and critical until cleared |

Example in YAML (the UI builds the same thing):

```yaml
automation:
  - alias: Washing machine done
    triggers:
      - trigger: state
        entity_id: sensor.washer_state
        to: "finished"
    actions:
      - action: script.deskwatch_alert
        data:
          title: Washing machine
          message: Done
          severity: info
          id: washer
  - alias: Washing machine emptied
    triggers:
      - trigger: state
        entity_id: sensor.washer_state
        from: "finished"
    actions:
      - action: script.deskwatch_clear
        data:
          id: washer
```

A short press on the panel button takes a warning or critical alert off the screen; it stays in the `home` badge
and on the `alerts` page until the clear script runs with the same id (or its duration ends). Use `critical`
sparingly: it pre-empts everything. The bridge can restrict it to chosen ids (`critical_ids` in `[alerts]`), and
shows any other critical alert as a warning.

## Check it without an automation

Developer tools, Actions, `script.deskwatch_alert`, with `title: Test` and `severity: warning`. Or from any
shell with a broker login:

```sh
mosquitto_pub -u homeassistant -P '...' -t deskpanel/alert \
  -m '{"v":2,"id":"test","severity":"warning","title":"Test","message":"Hello"}'
mosquitto_pub -u homeassistant -P '...' -t deskpanel/alert -m '{"v":2,"id":"test","clear":true}'
```

## Without MQTT: the web address

Alerts can also arrive over HTTP, with the same JSON as on MQTT, for scripts, `curl`, Home Assistant's `rest_command`,
Uptime Kuma or a cron job. It is off by default. Switch it on with `http_input = true` in `[alerts]`, or with the
checkbox in the **Alerts** section of the [setup page](setup-page.md#alerts), then restart the bridge.

```sh
curl -X POST http://<bridge>:8787/api/alert \
  -d '{"v":2,"id":"nas","severity":"warning","title":"NAS is slow","message":"Disk 95% full"}'
curl -X POST http://<bridge>:8787/api/alert -d '{"v":2,"id":"nas","clear":true}'
```

The answer is `202` when the alert was accepted, `400` with the reason when the JSON is wrong (`v` must be 2, `clear`
needs an `id`), `413` over 1024 bytes and `503` when the bridge is busy. An alert without a `source` shows as "via http".
`max_active`, `critical_ids` and the other `[alerts]` settings apply as they do on MQTT.

With the [login](auth.md) on, send an alert key: `Authorization: Bearer <key>` in the `rest_command` headers. By default (no login) it only answers clients on the local network (loopback, private and Tailscale addresses); a sender from
elsewhere gets `403` unless `[http] allow_public = true`. Without a password, as with the dashboard and the setup page, use it on a trusted network only and do not publish the
port to the internet. A web page on another site cannot post to it (a request with a foreign `Origin` is refused); `curl` and
scripts send none. Over HTTP an alert can only be raised or cleared; it is not the panel button, and the ESP panel still
needs the MQTT broker to show anything.

## Notes

- The blueprints use the `action:` / `triggers:` syntax of recent HA releases (2024.10 and later).
- If a script run does nothing, look at the bridge log (`warn` lines about "ignoring alert") and at the MQTT
  integration's "Listen to a topic" page in HA, subscribed to `deskpanel/alert`.
- The bridge limits active alerts (default 10), ignores payloads over 1 KB and accepts one update per id per
  second, so a looping automation cannot flicker the panel.
