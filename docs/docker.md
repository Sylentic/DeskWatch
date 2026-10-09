# Running the bridge in Docker

The bridge runs fine in a container: one small image, no state on disk, config and secrets mounted read-only.
This page covers building the image, a Compose file, reaching your MQTT broker, secrets as files and what to know
about the stats page. Everything else (sources, Mosquitto logins, Home Assistant) is the same as in
[install.md](install.md); only how the bridge is started and fed its files changes.

All host names below are placeholders.

Contents: [1. Build the image](#1-build-the-image) · [2. Try the demo](#2-try-the-demo) ·
[3. Config and secrets](#3-config-and-secrets) · [4. Compose](#4-compose) · [5. Reaching the broker](#5-reaching-the-broker) ·
[6. Webhooks](#6-webhooks-gitea) · [7. The stats page](#7-the-stats-page-in-a-container) ·
[8. Health and restarts](#8-health-and-restarts) · [9. Publishing an image](#9-publishing-an-image-optional) ·
[10. Update and remove](#10-update-and-remove)

## 1. Build the image

No image is published yet, so you build it from the source. The [`Dockerfile`](../Dockerfile) at the repository root
has two stages: the official Rust image compiles the release binary (`cargo build --release --locked`, Rust 1.88
like the rest of the project), and only that binary is copied into a `debian:bookworm-slim` image together with the CA
certificates. The result is about 130 MB, runs as user `deskwatch` (uid 10001, not root).

```sh
git clone https://github.com/Sylentic/DeskWatch.git
cd DeskWatch
docker build -t deskwatch-bridge .
```

The first build takes a few minutes (it compiles all dependencies). The image is for the CPU you build it on, so
building on a Raspberry Pi gives an arm64 image with no extra steps; `docker buildx` can build for another CPU. CI
builds the image for amd64 and arm64 on every change (nothing pushed). The build stage cross-compiles instead of
emulating the other CPU, so a multi-arch build is not much slower than a single one. The Pi side is in
[raspberry-pi.md](raspberry-pi.md).

## 2. Try the demo

`--demo` plays fake data and needs only a broker. With one reachable as `broker.example.lan`:

```sh
cat > demo.toml <<'TOML'
[mqtt]
host = "broker.example.lan"
TOML
docker run --rm -v "$PWD/demo.toml:/etc/deskwatch/bridge.toml:ro" deskwatch-bridge --demo /etc/deskwatch/bridge.toml
```

You should see `connected to MQTT broker` and `demo mode: playing fake data`. Look at it with the simulator, see
[install.md](install.md#8-try-it-without-hardware). If your broker needs a login, use the config and secrets from the
next section.

## 3. Config and secrets

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

## 4. Compose

[`deploy/docker-compose.yml`](../deploy/docker-compose.yml) is a ready example: it builds the image, mounts the
config and the `credentials/` folder read-only, runs with a read-only root filesystem, drops all capabilities, and
restarts unless stopped. Copy it next to your config:

```sh
mkdir -p deskwatch/credentials && cd deskwatch
cp /path/to/DeskWatch/deploy/docker-compose.yml compose.yaml
cp /path/to/DeskWatch/bridge/config.example.toml bridge.toml       # edit it, see install.md step 3
(umask 077; printf %s 'THE-BRIDGE-PASSWORD' > credentials/mqtt-password)
docker compose up -d --build
docker compose logs -f
```

Look for `loaded config from /etc/deskwatch/bridge.toml` and `connected to MQTT broker`. As in the systemd setup,
the example config ships with a Gitea block switched on: delete it (or add the `gitea-webhook-secret` file) or the
bridge exits at start naming the missing credential. Changing `bridge.toml` or a secret needs
`docker compose restart`.

## 5. Reaching the broker

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

## 6. Webhooks (Gitea)

Only a Gitea source opens the webhook listener (`[http] listen`, default `0.0.0.0:8787`). Publish it in Compose with
`ports: ["8787:8787"]`, and use `http://<docker host>:8787/webhook/gitea/<name>` as the webhook URL in Gitea (the
rest, including `ALLOWED_HOST_LIST`, is in [install.md](install.md#gitea-actions-and-pull-requests)). Allow that port in the firewall from the Gitea host only. GitHub, Azure DevOps and
Prometheus sources only make outgoing requests and need no published port.

## 7. The stats page in a container

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

## 8. Health and restarts

The image has no `HEALTHCHECK`. The bridge has no status endpoint, and adding one only for Docker would open a
port for everyone. What it does instead:

- **Startup problems end the process.** A missing credential, a bad config key or a taken port makes the bridge
  exit immediately with a message naming the problem, so `restart: unless-stopped` (or `docker ps` showing
  `Restarting`) is the signal. A source that cannot reach its service does not stop the bridge; it shows as the `warn` badge.
- **The broker is the health signal.** The bridge publishes `deskpanel/bridge/status` (`online`, and a retained
  `offline` as its last will when the connection drops). The panel already shows a red bar when it goes `offline`,
  and Home Assistant or a monitor can watch that topic.

If you want a Docker-level check anyway, run a small MQTT client in a separate container that watches that topic.

## 9. Publishing an image (optional)

Nothing is published and no workflow pushes an image. CI only checks that the Docker image builds and starts (the
`docker` job in [`ci.yml`](../.github/workflows/ci.yml)). If you want a published image so people can skip the
build, a job like this in a release workflow does it for GitHub's registry (`ghcr.io`); it needs no secret beyond
the built-in token:

```yaml
  docker:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write
    steps:
      - uses: actions/checkout@v5
      - uses: docker/setup-qemu-action@v3        # only for the arm64 build below
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/build-push-action@v6
        with:
          context: .
          push: true
          platforms: linux/amd64,linux/arm64       # arm64 covers a Raspberry Pi
          tags: |
            ghcr.io/sylentic/deskwatch-bridge:${{ github.ref_name }}
            ghcr.io/sylentic/deskwatch-bridge:latest
```

Things to decide first: the tag scheme (only version tags, `latest` too?), whether the image should be public
(a package inherits the repository's visibility only after you set it in the package settings), and whether to
build arm64 (a cross build under QEMU is several times slower than the amd64 one). Add the job to
`release.yml` so it only runs on version tags. Later, `docker pull` replaces `docker build` in step 1.

## 10. Update and remove

```sh
cd DeskWatch && git pull                    # or check out the tag you want
docker compose build --pull && docker compose up -d
```

Read [CHANGELOG.md](../CHANGELOG.md) first; a new release may add config keys. Remove with
`docker compose down --rmi local`, then delete the `credentials/` folder and the broker users you created.
