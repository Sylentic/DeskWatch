# DeskWatch in Docker

DeskWatch runs in one small container. There are two ways to use it, and they differ only in the config file:

- **The dashboard** (this page's quick start): the full-size status page in a browser, served by the bridge. Put the
  container on your homelab, open `http://<docker host>:8787/` from any screen. It needs **no MQTT broker**.
- **The dashboard with a broker**: the same container also listens for alerts on your Mosquitto broker, for example
  from Home Assistant (sections 4 to 9).

Sources (Gitea, GitHub, GitLab, Bitbucket Cloud, Azure DevOps, Prometheus), Mosquitto logins and Home Assistant are the same as in
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

(The log may warn that the page "has no login". That is expected for the demo: no password is set and the Compose file
publishes the port on `127.0.0.1` only. Section 3 covers who can see the page in a real setup.)

Contents: [1. Build the image](#1-build-the-image) · [2. Try the demo](#2-try-the-demo) ·
[3. The dashboard on your homelab](#3-the-dashboard-on-your-homelab) (also [Komodo and Portainer](#komodo-portainer-and-inline-configs)) ·
[4. Config and secrets](#4-config-and-secrets) · [5. Compose with a broker](#5-compose-with-a-broker) ·
[6. Reaching the broker](#6-reaching-the-broker) · [7. Webhooks](#7-webhooks-gitea) ·
[8. Host stats](#8-host-stats-in-a-container) · [9. Health and restarts](#9-health-and-restarts) ·
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
dependencies, from the manifests and `Cargo.lock` only, then your sources. A rebuild after a change in the bridge
code therefore reuses the cached dependency layer and only compiles the bridge itself; the dependencies
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

## 3. The dashboard on your homelab

This is the setup for a container that shows your real CI and host data on a page you open from any browser. The
files: [`deploy/docker-compose.dashboard.yml`](../deploy/docker-compose.dashboard.yml) and
[`deploy/dashboard.example.toml`](../deploy/dashboard.example.toml).

```sh
git clone https://github.com/Sylentic/DeskWatch.git && cd DeskWatch
mkdir -p dashboard/credentials
cp deploy/dashboard.example.toml dashboard/bridge.toml          # edit: display_name, sources
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

Then open `http://<docker host>:8787/`. With a new, empty data volume the first visit asks you to create the
administrator account (or to continue without a login). Until a password is set there is no login, so a kiosk that
should need nothing typed on first boot needs `[auth] required = false` in `bridge.toml` ([auth.md](auth.md)). **Keep it on your
home network** while it is like that: do not forward the port from your router. To use it beyond the home network, turn
on the [built-in login](auth.md) (and HTTPS, [https.md](https.md)); see "Protecting the page" below. Without a password
the bridge logs one warning at startup that the page has no login.

**What the config does.** `[mqtt] enabled = false` makes the bridge skip the broker completely: no connection, no
warnings every few seconds. It keeps working without one; the only thing that needs MQTT is
alerts sent over MQTT (Home Assistant). `[kiosk] enabled = true` serves the page, and `[http] listen =
"0.0.0.0:8787"` is the right bind inside a container, because the port you *publish* decides who can reach it. The
layout and widgets are in [kiosk.md](kiosk.md#3-choose-the-widgets). To also take alerts from MQTT,
set `enabled = true` (or delete the line) and give `[mqtt]` the broker's host, as in section 6.

**Your own data.** Without any `[[source.*]]` block the page is empty. Add
the sources you use (GitHub, Azure DevOps, Gitea, Prometheus) exactly as in
[bridge/config.example.toml](../bridge/config.example.toml) and [install.md](install.md#5-sources), with their
secrets in `dashboard/credentials/`, section 4 explains how. Only a Gitea source needs an incoming connection
(its webhook, section 7); the others only make outgoing requests.

### Managing sources in the browser (optional)

The [setup page](setup-page.md) at `/setup` shows and tests every source and, with a writable data folder, adds, edits
and removes sources and tokens without editing files. It is off by default. To use it with the dashboard Compose
file, create the folder owned by the container user (uid 10001), uncomment the `data` volume and the
`DESKWATCH_DATA_DIR` variable in `deploy/docker-compose.dashboard.yml`, and add `[setup] enabled = true` to
`bridge.toml`:

```sh
mkdir -p dashboard/data && sudo chown 10001 dashboard/data
```

A named volume (`data:/var/lib/deskwatch` plus a top-level `volumes: { data: }`) needs no `chown` from 0.9.14 on, and is
the better choice when a stack manager (Komodo, Portainer) deploys the Compose file from git, because a relative
`./data` would then sit inside the manager's checkout. See [data-dir.md](data-dir.md#docker).

The setup page needs the administrator login once you have set a password ([auth.md](auth.md)). Without one it shares
the rule of the dashboard: home network only, never published to the internet. `[setup] allow_restart = true` adds a "Restart now" button, which works because
the Compose files use `restart: unless-stopped`. Back up `dashboard/data` with the rest of your config. Details:
[data-dir.md](data-dir.md).

### Komodo, Portainer and inline configs

Stack managers such as Komodo and Portainer can run a Compose file with the config and secrets written into it
(`configs: ... content:`) instead of files next to it. DeskWatch works that way, with three things to know:

1. **`read_only: true` can be refused.** With inline `configs: content:` mounts, Compose may stop with an error that
   `file` is the sole supported option, so the hardened service from the examples does not start. Leave `read_only`
   out and keep `cap_drop: [ALL]` and `no-new-privileges`; the container still runs as uid 10001 without privileges.
2. **A changed inline config is not noticed.** When only `configs.<name>.content` changed, Compose leaves the old
   container running. Put the same text in a label through a YAML anchor and alias, and the container is recreated on
   every edit:

   ```yaml
   x-bridge-toml: &bridge_toml |
     [mqtt]
     enabled = false
     [kiosk]
     enabled = true
     [http]
     listen = "0.0.0.0:8787"

   services:
     deskwatch:
       image: ghcr.io/sylentic/deskwatch-bridge:latest
       restart: unless-stopped
       cap_drop: [ALL]
       security_opt: ["no-new-privileges:true"]
       ports: ["8787:8787"]
       environment:
         DESKWATCH_DATA_DIR: /var/lib/deskwatch
       labels:
         deskwatch.config: *bridge_toml     # only here to force a recreate when the text changes
       configs:
         - source: bridge_toml
           target: /etc/deskwatch/bridge.toml
       volumes:
         - data:/var/lib/deskwatch          # named volume, see below

   configs:
     bridge_toml:
       content: *bridge_toml

   volumes:
     data:
   ```

3. **Secrets taken from the manager's variables are not covered by that trick.** The label only follows text that is
   in the file. If you inline a secret from a stack variable, a rotated secret does not recreate the container; stop
   and start the stack (destroy and redeploy) after rotating one. Prefer the setup page's secret store in the data
   folder for tokens ([setup-page.md](setup-page.md)), so a token never sits in the stack at all.

**The data folder.** Use a named volume as in the example, not `./data`: under a stack manager `.` is its checkout of
your repository, and an update can wipe or conflict with a folder inside it. A named volume is writable for the bridge
without a `chown` from 0.9.14 on ([data-dir.md](data-dir.md#docker)).

**Who can open it.** Until you set an administrator password ([auth.md](auth.md)) the port has no login, and neither
does the setup page. The setup page and `POST /api/alert` also refuse clients outside the local network (`[http]
allow_public`, [setup-page.md](setup-page.md#security)). With no password, publish it to a trusted network only. With a
password and HTTPS you can open it further ([Protecting the page](#protecting-the-page)). Never publish it to the
internet without a password.

### Protecting the page

The page is read-only (nothing on it can change anything), but it shows PR titles, pipeline names, host stats and
alert text. Out of the box there is no login, which is fine on a home network and is why a new install just works.
The bridge has a built-in login that is off until you set an administrator password ([auth.md](auth.md)); the old
`token_file` setting was removed and a config that still sets it is refused at startup. The live WebSocket refuses a
request whose `Origin` is another site. If other people can reach your network, or you open the page beyond it, choose
one or more of these:

1. **The built-in login.** Set an administrator password (and, if you like, more users with a role each) under Setup, Access, or
   with `deskwatch-bridge auth set-password`. Screens and scripts use API keys. Needed anyway before `[http]
   allow_public = true`. Use it together with HTTPS.
2. **HTTPS.** Both a reverse proxy and the bridge itself can serve HTTPS, see [https.md](https.md).
3. **A reverse proxy.** Publish the port to the host only (`"127.0.0.1:8787:8787"` in the Compose file) and let
   your proxy (Caddy, Traefik, nginx) terminate TLS and do the access control (basic auth, or your SSO). Without a
   proxy login, do not publish the port to the internet. Make the proxy pass WebSocket upgrades on `/api/kiosk/ws`;
   without them the page falls back to polling `/api/kiosk` and still works, only slower to update.
4. **Local only.** Publish `"127.0.0.1:8787:8787"` and open the page on the Docker host itself (a Pi with a screen
   running the container).

#### Reverse proxy examples

All three examples terminate TLS for `dashboard.example.lan` (a placeholder), ask for a username and password, and
pass the WebSocket on `/api/kiosk/ws`. The bridge listens on `127.0.0.1:8787` (published as in option 3) or, when
the proxy runs in Docker too, on `deskwatch:8787` over a shared network. The browser sends the basic auth login on
the WebSocket by itself, and the bridge ignores a `Basic` header. The proxy must pass the browser's `Host` (with the port, if it is not 443) or send
`X-Forwarded-Host`, because the bridge compares the WebSocket's `Origin` with it. Create the login hash with `htpasswd -nbB desk 'a-password'`
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
        proxy_set_header Host $http_host;
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
    image: ghcr.io/sylentic/deskwatch-bridge:latest
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

Do **not** publish the port on the internet without the built-in login (or a proxy login) and HTTPS ([https.md](https.md)).
The login limits wrong passwords per address, but nothing rate-limits the read-only page itself. If you want the page
away from home, a VPN (WireGuard, Tailscale) is the simplest route; otherwise use the login behind HTTPS or a proxy that
requires a real login. A source alias keeps work project names off a visible screen
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

## 5. Compose with a broker

[`deploy/docker-compose.yml`](../deploy/docker-compose.yml) is a ready example: it pulls the published image (the `latest` tag, section 10), mounts the
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
| In the same Compose file | the service name, for example `"mosquitto"` | Uncomment the `mosquitto` service in the example and put it in the same file. Other clients reach it through the published port 1883 |
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

Only a Gitea source needs an incoming connection from outside. The listener it uses (`[http] listen`, default `0.0.0.0:8787`) is shared with the dashboard, the setup page and `/healthz` (section 9). Publish it in Compose with
`ports: ["8787:8787"]`, and use `http://<docker host>:8787/webhook/gitea/<name>` as the webhook URL in Gitea (the
rest, including `ALLOWED_HOST_LIST`, is in [install.md](install.md#gitea-actions-and-pull-requests)). Allow that port in the firewall from the Gitea host only. GitHub, Azure DevOps and
Prometheus sources only make outgoing requests and need no published port.

## 8. Host stats in a container

The bridge does not read the machine it runs on: from inside a container it would only see part of the real
machine (the container's network, no sensors). Host numbers come from Prometheus with node_exporter on each machine
(Linux) or windows_exporter (Windows), which are exact, including for the Docker host itself. Add a Prometheus source
under **Sources**, then choose the machines on the **Hosts** page ([hosts.md](hosts.md)).

## 9. Health and restarts

The image has a `HEALTHCHECK`, and `docker ps` shows `healthy` or `unhealthy`. There is no curl in the image, so the
bridge checks itself: `deskwatch-bridge --healthcheck` reads the same config and asks its own HTTP listener over
loopback every 30 seconds (over HTTPS when `[http.tls]` is set, without checking the certificate). The page file carries no data and needs no token, and `/healthz` only answers `ok` or
`mqtt down`, so the probe needs no secret. The `Host` header it sends is the address it connects to.

- **Dashboard on** (`[kiosk] enabled = true`): healthy when the page answers 200.
- **MQTT on** (the default): also healthy only while the bridge is connected to the broker. `GET /healthz` answers
  200 `ok` then and 503 `mqtt down` otherwise, so a bridge with a dead broker shows `unhealthy` in `docker ps`.
  This route is added whenever MQTT is on, which means the HTTP listener (`[http] listen`, port 8787 by default)
  also opens for a bridge that has no dashboard or Gitea source. It serves nothing else; to keep it off the network
  publish no port for it, or set `[http] listen = "127.0.0.1:8787"` outside a container. With `[mqtt] enabled =
  false` there is no such check. For a few seconds after a start the connection is not up yet; the image's
  start period covers that.
- **Setup page only** (`[setup] enabled = true`, no dashboard, MQTT off): healthy when `/setup` answers 200. A
  dead or missing listener shows `unhealthy`, where the old check passed.
- **Gitea source only**: healthy when the listener answers HTTP at all.
- **None of these** (a dashboard-less, setup-less, broker-less bridge with only GitHub or Azure DevOps sources): there is no
  listener, so the check passes trivially.
- **Demo started with flags** (`--demo --kiosk`): the check cannot see command line flags. Set the matching
  environment variables instead (`DESKWATCH_DEMO=1`, `DESKWATCH_KIOSK=1`, `DESKWATCH_NO_MQTT=1`), which the bridge
  and its health check both read; `deploy/docker-compose.demo.yml` does this.
- **Startup problems end the process.** A missing credential, a bad config key or a taken port makes the bridge
  exit immediately with a message naming the problem, so `restart: unless-stopped` (or `docker ps` showing
  `Restarting`) is the signal. A source that cannot reach its service does not stop the bridge; it shows as the
  `warn` badge, and on the dashboard the `health` widget names it.
- **The broker shows the bridge's state.** The bridge publishes `deskpanel/bridge/status` (`online`,
  and a retained `offline` as its last will when the connection drops). Home Assistant or a monitor can watch that topic.

The check does not restart a container by itself (plain Docker only reports it); an autoheal container or a
monitor such as Uptime Kuma pointed at the page can act on it.

## 10. The published image

Every version tag (from 0.9.6 on, except 0.9.8, which was never built) is built for amd64 and arm64 by the
`publish image` job in [`pipeline.yml`](../.github/workflows/pipeline.yml) and pushed to GitHub's registry:

```sh
docker pull ghcr.io/sylentic/deskwatch-bridge:<version>      # for example 1.0.10; no "v"
docker run --rm -p 8787:8787 ghcr.io/sylentic/deskwatch-bridge:<version> --demo --kiosk --no-mqtt
```

Tags are the version without the `v` and, for tags without a suffix such as `-rc.1`, `latest`. `latest` moves with
each release, so `docker compose pull` updates it; to stay on one version, use the version tag instead.
[`deploy/docker-compose.yml`](../deploy/docker-compose.yml) already uses the image with `latest`. The dashboard and demo Compose files build from the checkout they sit in, which is
repeatable as long as you stay on one commit; to use the image there, replace their `build:` block with
`image: ghcr.io/sylentic/deskwatch-bridge:latest`. An updater such as Renovate or Watchtower can bump the tag.
The package on `ghcr.io` is public, so servers pull it without a login. (A fork publishing its own image has to make
the package public once in its package settings, because a new package starts private.) The image exists only after
the pipeline has finished for that tag; check that the pull works before you update. For pull
requests the `docker image` job only checks that the image builds and starts (and, for pull requests into main, that it
builds for both CPUs); nothing is pushed from pull requests or branches.

### The dev image: try what is not released yet

Every merge into the `dev` branch publishes `ghcr.io/sylentic/deskwatch-bridge:dev` (always the newest) and
`:dev-<short commit>` (to go back to one), for amd64 and arm64, once the checks are green. It never touches `latest`
or a version tag, so a server that pins a release stays where it is. Run it next to your normal container, on another
port, with the same config read-only and a data folder of its own:

```yaml
services:
  deskwatch-dev:
    image: ghcr.io/sylentic/deskwatch-bridge:dev
    container_name: deskwatch-dev
    restart: unless-stopped
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    volumes:
      - ./bridge.toml:/etc/deskwatch/bridge.toml:ro
      - ./credentials:/run/credentials:ro
      - dev-data:/var/lib/deskwatch        # never the data folder of the normal container
    environment:
      DESKWATCH_DATA_DIR: /var/lib/deskwatch
      DESKWATCH_NO_MQTT: "1"               # leave the broker to the normal bridge
    ports:
      - "8788:8787"
volumes:
  dev-data:
```

Open `http://<docker host>:8788/` and update with `docker compose pull && docker compose up -d` (or let Komodo,
Watchtower or Renovate do it). Webhooks (Gitea) still go to the normal bridge, so the dev container learns about runs
by polling, a little later. Only use the dev image to try things: it can break between two merges.

If the image does not suit you (your own changes, another CPU), section 1 builds it from the source.

## 11. Update and remove

With the published image (the default in `deploy/docker-compose.yml`): read [CHANGELOG.md](../CHANGELOG.md) first, a
new release may add config keys, then run (the Compose file uses `latest`; if you pinned a version, change it in the
`image:` line first):

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
