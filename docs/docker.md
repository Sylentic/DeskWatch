# DeskWatch in Docker

DeskWatch runs in one small container. There are two ways to use it, and they differ only in the config file:

- **The dashboard** (this page's quick start): the full-size status page in a browser, served by the bridge. Put the
  container on your homelab, open `http://<docker host>:8787/` from any screen. It needs **no MQTT broker and no ESP**.
- **The bridge for the ESP panel**: the same container publishing screens to your Mosquitto broker for the desk
  panel (sections 4 to 9). It can serve the dashboard page at the same time.

Sources (Gitea, GitHub, Azure DevOps, Prometheus), Mosquitto logins and Home Assistant are the same as in
[install.md](install.md); only how the bridge is started and fed its files changes. All host names below are
placeholders.

## Quick start: the dashboard with fake data

Releases publish an image to `ghcr.io` (section 10). Without one, Docker builds it from the source once (a few minutes, the Rust compile). Then:

```sh
git clone https://github.com/Sylentic/DeskWatch.git && cd DeskWatch
docker compose -f deploy/docker-compose.demo.yml up --build
```

Open <http://localhost:8787/>. You see a busy dashboard with demo data (a running job, a failed run, PRs, an
alert). Nothing else is needed: no config, no broker, no secrets. Stop it with Ctrl+C. The same without Compose:

```sh
docker build -t deskwatch-bridge .
docker run --rm -p 8787:8787 deskwatch-bridge --demo --kiosk --no-mqtt
```

(The log warns that the page is "open to the network without kiosk.token_file". That is expected for the demo: the
Compose file publishes the port on `127.0.0.1` only. Section 3 covers the token for a real setup.)

Contents: [1. Build the image](#1-build-the-image) · [2. Try the demo](#2-try-the-demo) ·
[3. The dashboard on your homelab](#3-the-dashboard-on-your-homelab) ·
[4. Config and secrets](#4-config-and-secrets) · [5. Compose for the ESP bridge](#5-compose-for-the-esp-bridge) ·
[6. Reaching the broker](#6-reaching-the-broker) · [7. Webhooks](#7-webhooks-gitea) ·
[8. The stats page](#8-the-stats-page-in-a-container) · [9. Health and restarts](#9-health-and-restarts) ·
[10. The published image](#10-the-published-image) · [11. Update and remove](#11-update-and-remove)

## 1. Build the image

If no published image fits (section 10), build it from the source. The [`Dockerfile`](../Dockerfile) at the repository root
has two stages: the official Rust image compiles the release binary (`cargo build --release --locked`, Rust 1.88
like the rest of the project), and only that binary is copied into a `debian:bookworm-slim` image together with the CA
certificates. The result is about 130 MB, runs as user `deskwatch` (uid 10001, not root).

```sh
git clone https://github.com/Sylentic/DeskWatch.git
cd DeskWatch
docker build -t deskwatch-bridge .
```

The first build takes a few minutes (it compiles all dependencies). The Dockerfile builds in two steps: first the
dependencies, from the manifests and `Cargo.lock` only, then your sources. A rebuild after a change in the bridge,
`sim` or `ui` code therefore reuses the cached dependency layer and only compiles the bridge itself; the dependencies
are rebuilt only when a `Cargo.toml` or `Cargo.lock` changes. The image is for the CPU you build it on, so
building on a Raspberry Pi gives an arm64 image with no extra steps; `docker buildx` can build for another CPU. CI
builds the image for amd64 and arm64 on every change (nothing pushed). The build stage cross-compiles instead of
emulating the other CPU, so a multi-arch build is not much slower than a single one. The Pi side is in
[raspberry-pi.md](raspberry-pi.md).

## 2. Try the demo

`--demo` plays fake data instead of real sources. Two variants:

```sh
# The dashboard page only (no broker): open http://localhost:8787/
docker run --rm -p 8787:8787 deskwatch-bridge --demo --kiosk --no-mqtt
```

For the ESP panel or the desktop simulator the demo needs a broker. With one reachable as `broker.example.lan`:

```sh
cat > demo.toml <<'TOML'
[mqtt]
host = "broker.example.lan"
TOML
docker run --rm -v "$PWD/demo.toml:/etc/deskwatch/bridge.toml:ro" deskwatch-bridge --demo /etc/deskwatch/bridge.toml
```

You should see `connected to MQTT broker` and `demo mode: playing fake data`. Look at it with the simulator, see
[install.md](install.md#8-try-it-without-hardware). If your broker needs a login, use the config and secrets from
section 4.

## 3. The dashboard on your homelab

This is the setup for a container that shows your real CI and host data on a page you open from any browser. The
files: [`deploy/docker-compose.dashboard.yml`](../deploy/docker-compose.dashboard.yml) and
[`deploy/dashboard.example.toml`](../deploy/dashboard.example.toml).

```sh
git clone https://github.com/Sylentic/DeskWatch.git && cd DeskWatch
mkdir -p dashboard/credentials
cp deploy/dashboard.example.toml dashboard/bridge.toml          # edit: display_name, sources
openssl rand -hex 24 > dashboard/credentials/kiosk-token        # the page token
chmod 644 dashboard/credentials/kiosk-token                     # readable by the container user
# one file per source secret, for example a read-only GitHub token (see section 4 for the file rules):
#   (umask 077; printf %s 'THE-TOKEN' > dashboard/credentials/github-personal) && sudo chown 10001 dashboard/credentials/github-personal
docker compose -f deploy/docker-compose.dashboard.yml up -d --build
docker compose -f deploy/docker-compose.dashboard.yml logs
```

The Compose file reads the config and secrets from `${DESKWATCH_CONFIG_DIR}`, which defaults to `../dashboard` (the
folder from the steps above, next to the file). To run it from anywhere else, or paste it into Portainer or Dockge,
point the variable at an absolute folder that holds `bridge.toml` and `credentials/` (`DESKWATCH_CONFIG_DIR=/srv/deskwatch
docker compose -f docker-compose.dashboard.yml up -d`, or a line in a `.env` file next to the Compose file) and use the
published image instead of `build:`, see section 10.

Then open `http://<docker host>:8787/?token=<the token>`. The page removes the token from the address bar as soon as
it loads and keeps it for that browser tab only, so reloads work and the token stays out of the history. A bookmark
of the page therefore needs the `?token=...` added by hand; for a kiosk browser put the full address in the start
script ([kiosk.md](kiosk.md#4-autostart-on-a-raspberry-pi) shows the Chromium kiosk start script; it takes the
address in `DESKWATCH_KIOSK_URL`).

**What the config does.** `[mqtt] enabled = false` makes the bridge skip the broker completely: no connection, no
warnings every few seconds. It keeps working without one; the only things that need MQTT are the ESP panel, its
button and alerts sent over MQTT (Home Assistant). `[kiosk] enabled = true` serves the page, and `[http] listen =
"0.0.0.0:8787"` is the right bind inside a container, because the port you *publish* decides who can reach it. The
layout and widgets are in [kiosk.md](kiosk.md#3-choose-the-widgets). The dashboard and the ESP panel can share one
bridge: set `enabled = true` (or delete the line) and give `[mqtt]` the broker's host, as in section 6.

**Your own data.** Without any `[[source.*]]` block the page shows the container's own stats and nothing else. Add
the sources you use (GitHub, Azure DevOps, Gitea, Prometheus) exactly as in
[bridge/config.example.toml](../bridge/config.example.toml) and [install.md](install.md#5-sources), with their
secrets in `dashboard/credentials/`, section 4 explains how. Only a Gitea source needs an incoming connection
(its webhook, section 7); the others only make outgoing requests.

### Protecting the page

The page is read-only (nothing on it can change anything), but it shows PR titles, pipeline names, host stats and
alert text. Choose one:

1. **A token (the example's default).** `token_file` makes the data routes answer 401 without `?token=...`. The
   page file itself loads without it, but holds no data. The first request still carries the token in its address
   (`?token=...`), so it can appear in a reverse proxy's access log; the page then removes it from the address bar
   and later requests send it as a query value on the data routes only. Over plain HTTP it keeps casual visitors on
   your LAN out; it does not stop someone who can watch your network traffic. Do not log or share the first address.
   A one-time login that sets an `HttpOnly` cookie would avoid the address altogether; that is not built yet.
2. **A reverse proxy.** Publish the port to the host only (`"127.0.0.1:8787:8787"` in the Compose file) and let
   your proxy (Caddy, Traefik, nginx) terminate TLS and do the access control (basic auth, or your SSO). Then the
   token is optional, but keeping both costs nothing. Make the proxy pass WebSocket upgrades on `/api/kiosk/ws`;
   without them the page falls back to polling `/api/kiosk` and still works, only slower to update.
3. **Local only.** Publish `"127.0.0.1:8787:8787"` and open the page on the Docker host itself (a Pi with a screen
   running the container).

#### Reverse proxy examples

All three examples terminate TLS for `dashboard.example.lan` (a placeholder), ask for a username and password, and
pass the WebSocket on `/api/kiosk/ws`. The bridge listens on `127.0.0.1:8787` (published as in option 2) or, when
the proxy runs in Docker too, on `deskwatch:8787` over a shared network. The browser sends the basic auth login on
the WebSocket by itself, and the bridge ignores a `Basic` header, so keeping `token_file` set still works: open
`https://dashboard.example.lan/?token=<the token>`. Create the login hash with `htpasswd -nbB desk 'a-password'`
(package `apache2-utils`) or, for Caddy, `caddy hash-password`.

**Caddy.** `reverse_proxy` upgrades WebSockets without extra lines, and Caddy gets the certificate by itself when the
name is public. For a LAN-only name use `tls internal`:

```caddyfile
dashboard.example.lan {
	tls internal
	basicauth {
		desk <hash from `caddy hash-password`>
	}
	reverse_proxy 127.0.0.1:8787
}
```

**nginx.** Two things are needed for the WebSocket: HTTP/1.1 with the `Upgrade` headers, and a long `proxy_read_timeout`
(the default 60 s closes a quiet socket; the bridge sends a frame every 5 s, but a long timeout also survives a
stalled source):

```nginx
# in the http block
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    listen 443 ssl;
    server_name dashboard.example.lan;
    ssl_certificate     /etc/nginx/tls/dashboard.crt;
    ssl_certificate_key /etc/nginx/tls/dashboard.key;

    auth_basic           "DeskWatch";
    auth_basic_user_file /etc/nginx/deskwatch.htpasswd;   # from htpasswd -nbB

    location / {
        proxy_pass http://127.0.0.1:8787;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_read_timeout 1h;
    }
}
```

**Traefik (Docker labels).** Traefik handles WebSockets without a setting. Put the bridge on the network Traefik uses
and publish no port; the entrypoint (`websecure`) and certificate resolver (`letsencrypt`) are the names from your
Traefik configuration. In a Compose file every `$` in the hash must be written `$$`:
`htpasswd -nbB desk 'a-password' | sed -e 's/\$/\$\$/g'`.

```yaml
services:
  deskwatch:
    image: ghcr.io/sylentic/deskwatch-bridge:<version>
    # volumes, read_only, cap_drop: as in deploy/docker-compose.dashboard.yml; no `ports:`
    networks: [proxy]
    labels:
      - traefik.enable=true
      - traefik.docker.network=proxy
      - traefik.http.routers.deskwatch.rule=Host(`dashboard.example.lan`)
      - traefik.http.routers.deskwatch.entrypoints=websecure
      - traefik.http.routers.deskwatch.tls.certresolver=letsencrypt
      - traefik.http.routers.deskwatch.middlewares=deskwatch-auth
      - traefik.http.middlewares.deskwatch-auth.basicauth.users=desk:$$2y$$05$$<rest of the hash>
      - traefik.http.services.deskwatch.loadbalancer.server.port=8787

networks:
  proxy:
    external: true
```

How these were checked: Caddy 2.8, nginx 1.24 and Traefik 3.1 each proxied a running bridge with these routes and
this login: no login gave 401, the login gave the page data, and the WebSocket upgrade answered `101` and delivered
a frame. Traefik was tested with its file provider using the same router, middleware and service values as the
labels, not through the Docker provider, and TLS was not part of the check. Check the hostnames, the entrypoint and
the resolver names against your own setup.

The live-update WebSocket is capped: at most 16 at once (`[kiosk] max_ws_clients`, one per open tab or screen; a
further one gets `503` and that page polls instead), and a client that has not taken a frame for 10 seconds is
dropped, after which the page reconnects. This keeps one stuck browser from holding the bridge, it is not a login.

Do **not** publish the port on the internet. There is no login screen, no request rate limiting and no TLS in the bridge.
If you want the page away from home, put it behind a VPN (WireGuard, Tailscale) or a proxy that requires a real
login. A source alias keeps work project names off a visible screen
([kiosk.md](kiosk.md#5-security)).

## 4. Config and secrets

The container expects two things, both mounted **read-only**:

| Mount | In the container | What |
|---|---|---|
| `bridge.toml` | `/etc/deskwatch/bridge.toml` | The config (the image's default argument). Start from [`bridge/config.example.toml`](../bridge/config.example.toml) |
| a folder of secret files | `/run/credentials/` | One file per secret |

The image sets `CREDENTIALS_DIRECTORY=/run/credentials`, so a config key like `token_file = "github-personal"` reads
`/run/credentials/github-personal`: exactly how the systemd unit's `LoadCredential=` works, with the same
file names. A trailing newline in a secret file is ignored. So the pattern is the same as in
[install.md](install.md#2-install-the-bridge):

1. One file per secret, for example `credentials/mqtt-password`, mode 600 (`umask 077` while writing, so the secret
   never touches your shell history).
2. Name it in `bridge.toml` with a key ending in `_file`: `password_file = "mqtt-password"`.
3. Never put a secret in `bridge.toml`, in an `environment:` line or in a `docker run -e`, because those show up in
   `docker inspect` and in compose output.

The folder is read by uid 10001 inside the container. With a bind mount that means the files must be readable by
that uid: either owned by it (`sudo chown -R 10001 credentials`, mode 400 or 600), or mode 644 inside a folder only
you can enter (`chmod 700` on the parent). Docker's own secrets also work and land in `/run/secrets` instead, so
point `CREDENTIALS_DIRECTORY` there, or name the secret with an absolute path (`token_file = "/run/secrets/token"`):

```yaml
services:
  deskwatch-bridge:
    secrets: [github-personal]
    environment:
      CREDENTIALS_DIRECTORY: /run/secrets
secrets:
  github-personal:
    file: ./credentials/github-personal
```

(A file-based Compose secret keeps the host file's owner and mode, so the same uid rule applies.)

## 5. Compose for the ESP bridge

[`deploy/docker-compose.yml`](../deploy/docker-compose.yml) is a ready example: it pulls the published image (pinned to a release, section 10), mounts the
config and the `credentials/` folder read-only, runs with a read-only root filesystem, drops all capabilities, and
restarts unless stopped. Copy it next to your config:

```sh
mkdir -p deskwatch/credentials && cd deskwatch
cp /path/to/DeskWatch/deploy/docker-compose.yml compose.yaml
cp /path/to/DeskWatch/bridge/config.example.toml bridge.toml       # edit it, see install.md step 3
(umask 077; printf %s 'THE-BRIDGE-PASSWORD' > credentials/mqtt-password)
docker compose up -d
docker compose logs -f
```

Look for `loaded config from /etc/deskwatch/bridge.toml` and `connected to MQTT broker`. As in the systemd setup,
the example config ships with a Gitea block switched on: delete it (or add the `gitea-webhook-secret` file) or the
bridge exits at start naming the missing credential. Changing `bridge.toml` or a secret needs
`docker compose restart`.

## 6. Reaching the broker

Set `[mqtt] host` and `port` in `bridge.toml`. Which host to use depends on where Mosquitto runs:

| Broker | `host =` | Extra step |
|---|---|---|
| On another machine | its name or address | none |
| On the Docker host itself | `"host.docker.internal"` | Keep the `extra_hosts` line of the Compose file. Mosquitto must listen on more than localhost (a `listener 1883` line, with logins, see [mosquitto.md](mosquitto.md)) and the host firewall must allow the Docker network |
| In the same Compose file | the service name, for example `"mosquitto"` | Uncomment the `mosquitto` service in the example and put it in the same file. The panel reaches it through the published port 1883 |
| On the Docker host, simplest | `"localhost"` | Run the bridge with `network_mode: host` instead of the default network (Linux only). Then `ports:` and `extra_hosts` are not used |

Broker logins and TLS work exactly as described in [mosquitto.md](mosquitto.md); put the password in
`credentials/mqtt-password` and, for a private CA, mount the CA file read-only and name it with `ca_file`:

```yaml
    volumes:
      - ./ca.pem:/etc/deskwatch/ca.pem:ro
```

```toml
[mqtt]
tls = true
port = 8883
ca_file = "/etc/deskwatch/ca.pem"
```

## 7. Webhooks (Gitea)

Only a Gitea source opens the webhook listener (`[http] listen`, default `0.0.0.0:8787`). Publish it in Compose with
`ports: ["8787:8787"]`, and use `http://<docker host>:8787/webhook/gitea/<name>` as the webhook URL in Gitea (the
rest, including `ALLOWED_HOST_LIST`, is in [install.md](install.md#gitea-actions-and-pull-requests)). Allow that port in the firewall from the Gitea host only. GitHub, Azure DevOps and
Prometheus sources only make outgoing requests and need no published port.

## 8. The stats page in a container

The built-in `stats` page reads `/proc` and `/sys`, and a container sees only part of the real machine:

- **Host name:** the container id by default. Set `display_name = "homeserver"` under `[server]`.
- **RAM and uptime:** the host's numbers (Docker does not virtualize `/proc/meminfo` and `/proc/uptime`).
- **CPU load:** the host's, as the kernel computes it for everything.
- **Network:** the container's own interface, so the rates are the bridge's traffic, not the host's. With
  `network_mode: host` they are the host's.
- **Disk:** the filesystem under the container's root, not a host disk. Mount the host path you care about
  read-only (for example `- /srv:/hostdisk:ro`) and set `disk_path = "/hostdisk"`.
- **CPU temperature:** often empty, because the thermal sensors are not always visible inside a container.

If you want exact numbers for several machines, use a Prometheus source with node_exporter instead
([install.md](install.md#prometheus-host-stats-and-containers)) and leave the local page as is.

## 9. Health and restarts

The image has a `HEALTHCHECK`, and `docker ps` shows `healthy` or `unhealthy`. There is no curl in the image, so the
bridge checks itself: `deskwatch-bridge --healthcheck` reads the same config and asks its own HTTP listener over
loopback every 30 seconds. The page file carries no data and needs no token, and `/healthz` only answers `ok` or
`mqtt down`, so the probe needs no secret. The `Host` header it sends is the address it connects to.

- **Dashboard on** (`[kiosk] enabled = true`): healthy when the page answers 200.
- **MQTT on** (the default): also healthy only while the bridge is connected to the broker. `GET /healthz` answers
  200 `ok` then and 503 `mqtt down` otherwise, so an ESP bridge with a dead broker shows `unhealthy` in `docker ps`.
  This route is added whenever MQTT is on, which means the HTTP listener (`[http] listen`, port 8787 by default)
  also opens for a bridge that has no dashboard or Gitea source. It serves nothing else; to keep it off the network
  publish no port for it, or set `[http] listen = "127.0.0.1:8787"` outside a container. With `[mqtt] enabled =
  false` there is no such check. For a few seconds after a start the connection is not up yet; the image's
  start period covers that.
- **Gitea source only**: healthy when the listener answers HTTP at all.
- **None of these** (a dashboard-less, broker-less bridge with only GitHub or Azure DevOps sources): there is no
  listener, so the check passes trivially.
- **Demo started with flags** (`--demo --kiosk`): the check cannot see command line flags. Set the matching
  environment variables instead (`DESKWATCH_DEMO=1`, `DESKWATCH_KIOSK=1`, `DESKWATCH_NO_MQTT=1`), which the bridge
  and its health check both read; `deploy/docker-compose.demo.yml` does this.
- **Startup problems end the process.** A missing credential, a bad config key or a taken port makes the bridge
  exit immediately with a message naming the problem, so `restart: unless-stopped` (or `docker ps` showing
  `Restarting`) is the signal. A source that cannot reach its service does not stop the bridge; it shows as the
  `warn` badge, and on the dashboard the `health` widget names it.
- **The broker is the health signal for the panel.** The bridge publishes `deskpanel/bridge/status` (`online`,
  and a retained `offline` as its last will when the connection drops). The panel already shows a red bar when it
  goes `offline`, and Home Assistant or a monitor can watch that topic.

The check does not restart a container by itself (plain Docker only reports it); an autoheal container or a
monitor such as Uptime Kuma pointed at the page can act on it.

## 10. The published image

Every version tag (from 0.9.6 on) is built for amd64 and arm64 by the `docker` job in
[`release.yml`](../.github/workflows/release.yml) and pushed to GitHub's registry:

```sh
docker pull ghcr.io/sylentic/deskwatch-bridge:<version>      # for example 0.9.8; no "v"
docker run --rm -p 8787:8787 ghcr.io/sylentic/deskwatch-bridge:<version> --demo --kiosk --no-mqtt
```

Tags are the version without the `v` and, for tags without a suffix such as `-rc.1`, `latest`. Pin the version in
production (`latest` moves). [`deploy/docker-compose.yml`](../deploy/docker-compose.yml) already uses the image,
pinned to a release. The dashboard and demo Compose files build from the checkout they sit in, which is
repeatable as long as you stay on one commit; to use the image there, replace their `build:` block with
`image: ghcr.io/sylentic/deskwatch-bridge:<version>`. An updater such as Renovate or Watchtower can bump the tag.
A new package on `ghcr.io` starts private: make it public once in the package settings (Package settings, Change
visibility) so servers can pull it without a login. CI (the `docker` job in [`ci.yml`](../.github/workflows/ci.yml))
only checks that the image builds and starts; nothing is pushed from pull requests or branches.

If the image does not suit you (your own changes, another CPU), section 1 builds it from the source.

## 11. Update and remove

With the published image (the default in `deploy/docker-compose.yml`): read [CHANGELOG.md](../CHANGELOG.md) first, a
new release may add config keys, then change the version in the `image:` line and run:

```sh
docker compose pull && docker compose up -d
```

When you build from the source instead:

```sh
cd DeskWatch && git pull                    # or check out the tag you want
docker compose build --pull && docker compose up -d
```

Remove with
`docker compose down --rmi local`, then delete the `credentials/` folder and the broker users you created.
