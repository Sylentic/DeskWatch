# DeskWatch MQTT schema v2

This is the contract between the bridge and the panel. Both sides are tested against it: every JSON sample in
[`schema/`](schema/) is checked by `bridge/tests/schema.rs` (the bridge produces exactly that payload, or accepts
it) and by `ui/tests/schema.rs` (the panel code parses it). Change a sample and the code together, in one PR.

## 1. The idea

The panel is a dumb MQTT client. It knows six **page templates** and draws whatever page the bridge sends. The
bridge owns everything else: collecting data, priority, which page is up and for how long.

```
 sources                          bridge                      panel
 -------                          ------                      -----
 server stats    (local)   --+
 Prometheus      (poll)    --+
 Gitea           (webhook) --+--> facts --> composer --> deskpanel/screen --> draws page
 GitHub / GHE    (poll)    --+              (priority,   deskpanel/badges --> draws header
 Azure DevOps    (poll)    --+               rotation) <-- deskpanel/panel/event (button)
 Home Assistant  (MQTT)    --+
```

New sources and new rotation rules never need a firmware update. A new kind of data reuses the `list` or `number`
template; only a genuinely new visual needs a new template.

## 2. Screen priority

The composer picks the screen in this order. A lower level pre-empts a higher one immediately.

| Level | Kind | Shows | Leaves the screen |
|---|---|---|---|
| 0 | `alert` (critical) | Red full screen from a critical alert | Button press (stays as a badge), `clear`, or its `ttl_s` |
| 1 | `job` | Live progress of a running job | Job finishes |
| 2 | `alert` (failed or warn) | Red for a failed job, amber for a warning alert | Button press, or after 10 min it drops to a badge |
| 3 | `alert` (success) | Green flash | 10 s |
| 4 | `notice` | Short flash, such as "New PR" or an info alert | 5 s |
| 5 | Rotation | Idle pages in turn | Never, it loops |

Several running jobs: the newest is shown and `others` counts the rest; a long press cycles through them.

Button, decided by the bridge from what is on screen:
- short press on an alert or notice: dismiss it (all alerts at that level at once);
- short press in rotation: next page now; long press: pin or unpin the current page;
- long press on a job: show the next running job.

## 3. Rotation

Configured on the bridge with `[[rotation]]` blocks (see `bridge/config.example.toml`). The stats page is the home
page; pages with nothing to show (`skip_when_empty`) are skipped; after an interrupt ends, rotation restarts at the
stats page. Pages the bridge fills: `stats` (this machine), `prs`, `pipelines`, `alerts`, and with a Prometheus source `stats:<host>` (one per machine) and `containers` (what is down).

## 4. Topics

| Topic | Direction | Retained | QoS | Purpose |
|---|---|---|---|---|
| `deskpanel/screen` | bridge -> panel | yes | 1 | The page to draw now, with all its data |
| `deskpanel/badges` | bridge -> panel | yes | 1 | Counters for the header strip |
| `deskpanel/bridge/status` | bridge -> panel | yes | 1 | `online` / `offline` (Last Will) |
| `deskpanel/panel/status` | panel -> bridge | yes | 1 | `online` / `offline` (Last Will) |
| `deskpanel/panel/event` | panel -> bridge | no | 1 | Button presses |
| `deskpanel/alert` | Home Assistant, scripts -> bridge | no | 1 | Raise or clear an alert. The panel does not subscribe |

`deskpanel` is the bridge's `mqtt.topic_prefix`.

General rules: every payload has `"v": 2`; timestamps are Unix seconds; unknown values are `null` (the panel draws
`--`); unknown fields are ignored; payloads stay under 1 KB; the bridge cuts titles and other short text to 40
characters and lists to 5 rows.

### `deskpanel/screen`

Envelope ([sample](schema/screen-job.json)):

| Field | Meaning |
|---|---|
| `template` | `stats`, `job`, `alert`, `list`, `number` or `notice`. The only field the panel switches on |
| `level` | 0 to 5 from section 2. The panel uses it only for styling |
| `page` | Name of the page, for logs and the footer |
| `pinned` | True when the user pinned this page |
| `position` | `[2, 3]` = page 2 of 3 in rotation, drawn as dots. `null` during interrupts |
| `stale` | Source data is old or the source is failing; the panel greys the content out |
| `seq` | Increments on every publish, so the panel can spot duplicates after a reconnect |

Template data, one sample each:

| Template | Sample | Notes |
|---|---|---|
| `stats` | [screen-stats.json](schema/screen-stats.json) | One host per page |
| `job` | [screen-job.json](schema/screen-job.json) | `progress` 0.0 to 1.0, or `null` for a spinner; `kind` is `build` or `deploy` |
| `alert` | [failed](schema/screen-alert-failed.json), [success](schema/screen-alert-success.json), [warn](schema/screen-alert-warn.json), [critical](schema/screen-alert-critical.json) | See below |
| `list` | [screen-list.json](schema/screen-list.json) | Row `status`: `ok`, `running`, `failed`, `open`, `review`, `neutral` |
| `number` | [screen-number.json](schema/screen-number.json) | One big figure |
| `notice` | [screen-notice.json](schema/screen-notice.json) | Short flash |

`alert` has two shapes. A finished job fills `project`, `pipeline` and `step`, and has no `title` or `message`. Any
other alert (Home Assistant, scripts) sets those three to `null` and fills `title` and `message`; `finished` is
`null` while the alert is active. The panel draws whichever pair is present. `status` is `failed` (red, also used
for critical alerts at level 0), `warn` (amber) or `success` (green).

### `deskpanel/badges`

[Sample](schema/badges.json). Drawn in the header on every page, including interrupts. Badges with `count` 0 are
left out by the bridge, at most 4 are sent (the first four that have a count), in this order:

1. `alerts` (icon `home`): active warning and critical alerts, red if any is critical, else amber (`review`)
2. `failed` (icon `pipeline`): failed runs not yet dismissed
3. `prs` (icon `pr`): open PRs over all sources
4. `server` (icon `server`): hosts and containers that are down (Prometheus source)
5. `warn` (icon `warn`): sources that cannot log in or connect

Icons: `pr`, `pipeline`, `server`, `warn`, `home`; unknown icons fall back to a dot.

### `deskpanel/panel/event`

[Sample](schema/panel-event.json). `action` is `short` or `long`; the bridge decides what it means.

### `deskpanel/alert` (inbound to the bridge)

Raise ([sample](schema/alert-raise.json)) or clear ([sample](schema/alert-clear.json)) an alert:

| Field | Meaning |
|---|---|
| `id` | Optional. The same id replaces the earlier alert; needed to clear. Missing means a new alert each time |
| `severity` | `info`, `warning` or `critical`. Default `info` |
| `title`, `message` | Text shown, cut to 40 characters. Title defaults to `Alert` |
| `ttl_s` | Optional lifetime in seconds. Defaults: `info` 1800, `warning` and `critical` until cleared |
| `source` | Free text label, default `mqtt` |
| `clear` | `true` removes the alert with this `id` |

How each severity shows:

| Severity | Screen | After it leaves the screen |
|---|---|---|
| `critical` | Level 0 red `alert`, above running jobs, until a button press | `home` badge and `alerts` page until cleared or `ttl_s` |
| `warning` | Level 2 amber `alert` until a button press or 10 min | `home` badge and `alerts` page until cleared or `ttl_s` |
| `info` | Level 4 `notice` for 5 s | `alerts` page until cleared or `ttl_s` (no badge) |

Limits on the bridge side (`[alerts]` in the config): at most `max_active` alerts (default 10, the least urgent,
oldest one is dropped first); one update per id per second; payloads over 1 KB and other schema versions are
dropped; `critical_ids`, when set, lists the only ids that may be critical, any other critical alert is shown as a
warning. The broker should only let alert publishers write `deskpanel/alert`, never `deskpanel/screen` or
`deskpanel/badges`.

## 5. Panel LED

| Screen | Onboard RGB LED |
|---|---|
| `job` | Slow blue pulse |
| `alert` failed | Solid red, fast blink at level 0 |
| `alert` warn | Solid amber |
| `alert` success | Green, fades out |
| anything else | Off or very dim white |

LED effects trigger only when `template` or `status` changes, so a retained message replayed after a reconnect does
not restart them.
