# Alertmanager

Prometheus Alertmanager can send its alerts to the bridge as a webhook. Firing alerts show up in the Alerts list and
header like any other alert; when Alertmanager says an alert is resolved, the bridge removes it.

> **Not tested against a real Alertmanager yet.** It is built on the documented webhook format (version 4) and tested
> with mocked requests only. Please report what a real one does differently.

## Set it up

1. Turn on `http_input` (Setup > Alerts, or `[alerts] http_input = true` in `bridge.toml`). The Alertmanager address
   uses the same switch, the same local-network rule and the same alert keys as `POST /api/alert`
   (see [home-assistant.md](home-assistant.md) and [auth.md](auth.md)).
2. Add a receiver to `alertmanager.yml`:

```yaml
route:
  receiver: deskwatch        # or add a sub-route for the alerts you want here
receivers:
  - name: deskwatch
    webhook_configs:
      - url: http://<bridge>:8787/api/alertmanager
        send_resolved: true  # needed, or alerts never leave the screen
        http_config:         # only with the login on: an alert key
          authorization:
            type: Bearer
            credentials: <alert key>
```

`send_resolved: true` is what lets the bridge clear an alert; without it, use Dismiss or a `ttl`.

## What shows

| Alertmanager                     | DeskWatch                                                              |
|----------------------------------|------------------------------------------------------------------------|
| label `severity` critical, page, error | critical (red)                                                   |
| `severity` warning, or missing/other   | warning (yellow)                                                 |
| `severity` info, informational, none   | info (blue)                                                      |
| annotation `summary`             | title (else the `alertname` label)                                     |
| annotation `description`         | text (else `message`, else the `instance` label)                       |
| status `resolved`                | the alert is cleared                                                   |

The id of an alert is `am:<alertname>:<fingerprint>` and its source is `alertmanager`. `generatorURL` is not used.
At most 100 alerts of one call and 256 KB are taken; the answer is `202`, `400` for a body that is not a webhook,
`413` when too big and `503` when the bridge is busy (Alertmanager then retries).

If you set `critical_ids` in `bridge.toml`, add `"am:*"` (a trailing `*` matches any ending) or Alertmanager criticals
show as warnings.

A warning or critical stays until resolved or dismissed. Alertmanager repeats a firing alert every `repeat_interval`
(4 hours by default), which raises it again if you dismissed it in the meantime.
