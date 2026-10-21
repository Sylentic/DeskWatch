# HTTPS for the dashboard and the setup page

HTTPS is **opt-in**. Without any setting the bridge serves plain HTTP, as before, and nothing changes for existing
installs. There are two ways to turn it on. Pick one.

| | Bridge serves TLS | Caddy in front |
|---|---|---|
| Setting | `[http.tls]` in `bridge.toml` | `deploy/docker-compose.https.yml` |
| Works for | Docker, Pi, Windows, Linux | Docker (or any host with Caddy) |
| Certificate | you supply the PEM files | Caddy makes one (internal CA or Let's Encrypt) |

## Route 1: the bridge serves HTTPS

```toml
[http]
listen = "0.0.0.0:8787"

[http.tls]
cert = "/etc/deskwatch/tls/dashboard.crt"   # certificate plus intermediates, PEM
key  = "/etc/deskwatch/tls/dashboard.key"   # private key, PEM
```

The one listener then speaks HTTPS only (kiosk page, setup page, `/api/alert`, webhooks). The page's live WebSocket
becomes `wss` by itself. A wrong path or a bad file stops the bridge at startup with a message that names the file.
The Docker health check follows (it skips the certificate check on its own loopback probe). In Docker, mount the two
files read-only into the container. The key file must be readable by the container user (uid 10001).

Changing the certificate (renewal) needs a restart of the bridge.

## Route 2: Caddy in front (Docker)

```
cp deploy/Caddyfile.example deploy/Caddyfile      # put your own name in it
docker compose -f deploy/docker-compose.https.yml up -d --build
```

DeskWatch publishes no port in that file; only Caddy answers, on 443, and passes the WebSocket through.

## Certificates on an internal network

You have no public DNS for `*.lan` names, so a normal certificate authority cannot issue one. The options:

- **A domain you own** (for example `dashboard.example.org`, pointing at the homelab's internal address).
  Let's Encrypt can issue with the DNS challenge, which needs only a DNS record and no open port. No browser
  warning and nothing to install. Caddy supports this with a DNS provider plugin. Best choice if you have a domain.
- **An internal CA** (Caddy `tls internal`, or `mkcert`): works for any name, but every browser and device must
  trust the root certificate once. Caddy keeps it in `/data/caddy/pki/authorities/local/root.crt` in its volume.
- **Self-signed:** a browser warning each time. Only as a last resort.

## What changes when HTTPS is on

- **Home Assistant and scripts that POST `/api/alert`** must use `https://` and trust the certificate (or skip the
  check, for example `curl -k`). With an internal CA, install the root in Home Assistant too.
- **Raspberry Pi kiosk:** the autostart URL (`deploy/kiosk/deskwatch-kiosk.sh`) defaults to
  `http://localhost:8787/`. On the Pi itself, keep plain HTTP on localhost; otherwise set `DESKWATCH_KIOSK_URL` to
  the `https://` name, which needs the root certificate on the Pi.
- **Bookmarks and compose files** that say `http://` have to change.
- The same-origin check on the WebSocket and POSTs keeps working: it compares `Origin` with `Host`.

Not covered here: TLS between the bridge and the MQTT broker (see [mosquitto.md](mosquitto.md)).
