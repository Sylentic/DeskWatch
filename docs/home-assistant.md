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

## Notes

- The blueprints use the `action:` / `triggers:` syntax of recent HA releases (2024.10 and later).
- If a script run does nothing, look at the bridge log (`warn` lines about "ignoring alert") and at the MQTT
  integration's "Listen to a topic" page in HA, subscribed to `deskpanel/alert`.
- The bridge limits active alerts (default 10), ignores payloads over 1 KB and accepts one update per id per
  second, so a looping automation cannot flicker the panel.
