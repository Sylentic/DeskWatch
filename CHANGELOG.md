# Changelog

All notable changes to DeskWatch are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Version 1.0 is reserved for the first release with working firmware on the real
screen. Until then (0.x) the MQTT schema (v2) and the bridge config file aim to change only in compatible ways, but
may change if the firmware work needs it; any such change is listed here.

## [Unreleased]

### Added

- **Runner status on the kiosk dashboard.** A new `runners` widget lists runners and agents: offline first (red), then
  busy (orange), then idle (green); a runner an administrator switched off shows grey and never counts as offline.
  The header says how many are offline and the card greys out when the list stops updating.
- **Gitea runners** as the first source for it. `[[source.gitea]]` gets `runners = ["user", "org:<name>",
  "repo:<owner>/<name>", "admin"]` and `runner_poll_s` (default 30). Runners are off unless `runners` is set. The
  demo (`--demo`) shows four fake runners.
- **`docs/runners.md`**: the design for runners and agent pools across Gitea, GitHub and Azure DevOps, the token
  scopes each needs, and the order the other two sources follow in.
- The kiosk snapshot has a `runners` list (snapshot version stays 1; a page that does not know it ignores it).

## [0.9.8]

The Docker and kiosk hardening release: Compose uses the published image, WebSocket limits, reverse proxy examples.

### Added

- **Reverse proxy examples** for Caddy, nginx and Traefik (Docker labels) in `docs/docker.md`, each with TLS, basic
  auth and the WebSocket route on `/api/kiosk/ws`.
- **`[kiosk] max_ws_clients`** (default 16): the most live-update WebSockets open at once. A further client gets
  `503` and its page polls instead.

### Changed

- **`deploy/docker-compose.yml` uses the published image instead of building from `#main`.** It pulls
  `ghcr.io/sylentic/deskwatch-bridge` pinned to a release, so a restart or rebuild can no longer change the running
  version by surprise. Building from the source stays documented as a commented alternative. `docs/docker.md`
  (sections 5, 10 and 11) describes pulling and bumping the version. (Fixes #23)

### Fixed

- **The mouse pointer no longer disappears for good on the dashboard.** It is visible as normal and only hides
  after 3 seconds without movement (handy on a kiosk screen), then returns on the next movement.

### Security

- **A stalled dashboard client can no longer hold a WebSocket for ever.** The bridge drops a client that has not
  accepted a frame for 10 seconds; the page reconnects by itself. Together with the connection cap this keeps one
  broken or hostile client on the LAN from using up the bridge. There is still no request rate limiting.

## [0.9.7]

The dashboard fixes release: clearer connection states, running pipelines in Running now, and orange for running builds.

### Changed

- **Running builds are orange on the dashboard instead of blue.** The pulsing dot in Pipelines, the Running now
  progress bar, the header progress line and the header chip use orange. Failed stays red, success green, waiting for
  approval amber (not pulsing), queued and cancelled grey. The step name and the elapsed time of a running job are larger and brighter, so they
  can be read from across the room, and the progress bar is a plain orange fill without the moving stripes. Status colours are listed in `docs/kiosk.md`.

### Fixed

- **"Running now" on the dashboard listed nothing for GitHub and Azure DevOps pipelines that may not interrupt.** The
  Pipelines widget showed `running`, but the jobs behind it were only fetched for repositories and projects with
  `interrupt` on (Azure DevOps has it off by default), so the Running now widget said "No jobs running". Those
  sources now fetch the job progress of every running run or build; `interrupt` still only decides what takes over
  the ESP screen. Cost: Azure DevOps and GitHub now poll the running build every `job_poll_s` also for quiet projects.
  Gitea was not affected.
- **The dashboard no longer sits silently on `connecting`.** When the kiosk token is set and the page is opened without
  it, the data request answered 401 and the page showed an empty body forever. The page now asks for the token in a
  box (kept for that tab only) and says `token required` or `token rejected` in the header chip. It also names the
  other failures: `bridge unreachable`, `bridge error <status>`, `polling, no live socket` (WebSocket blocked, data
  still arrives by polling) and "Waiting for the first data". It fetches a snapshot once at start, so the reason is
  known even before the WebSocket gives up. Troubleshooting table in `docs/kiosk.md`.

## [0.9.6]

The homelab release: the bridge image is published to `ghcr.io`, and the Docker setup is hardened.

### Changed

- **Docker health check covers MQTT.** With MQTT on, the bridge serves `GET /healthz` (200 `ok` while the broker
  connection is up, 503 `mqtt down` otherwise) and `--healthcheck` requires it, so an ESP bridge with an unreachable
  broker shows `unhealthy` instead of `healthy`. This means the HTTP listener now also opens for a bridge without a
  dashboard or Gitea source (it serves only `/healthz`). The probe now sends the address it connects to as the `Host`
  header instead of a fixed `localhost`. (Fixes #25)
- **Compose files work from any folder.** `deploy/docker-compose.dashboard.yml` reads the config and secrets from
  `${DESKWATCH_CONFIG_DIR}`, default `../dashboard`. `--demo`, `--kiosk` and `--no-mqtt` can also be set as
  `DESKWATCH_DEMO`, `DESKWATCH_KIOSK` and `DESKWATCH_NO_MQTT`, so the demo Compose file no longer repeats the flags
  in a hand-written `healthcheck`. (Fixes #27)
- **The dashboard token leaves the address bar.** The page removes `?token=` from the URL with
  `history.replaceState` as soon as it loads and keeps the token in memory and in `sessionStorage` (this tab only,
  so the reload after a bridge upgrade still works). The first request still carries it. Documented in
  `docs/docker.md` and `docs/kiosk.md`; a one-time `HttpOnly` cookie login is a possible follow-up. (Fixes #24)

### Added

- **Docker image on release.** Pushing a version tag now also builds the bridge image for amd64 and arm64 and
  pushes it to `ghcr.io/sylentic/deskwatch-bridge` (tags `<version>` and, for tags without a suffix, `latest`).
  The Compose files and `docs/docker.md` can use it instead of building from the source.

## [0.9.5]

The big-screen release: a browser dashboard for a Raspberry Pi or a homelab container, and a TLS spike for the
firmware. Pre-release; the ESP panel firmware still has not run on a board.

### Added

- **Docker dashboard.** The kiosk page is now a first-class way to run DeskWatch in a container on a homelab, with
  no MQTT broker and no ESP. New `[mqtt] enabled = false` (and `--no-mqtt` on the command line) stops the bridge
  from connecting to or logging about a broker; the ESP panel, its button and MQTT alerts need it on, which stays
  the default. New `deploy/docker-compose.dashboard.yml` with `deploy/dashboard.example.toml` (token, published
  port, read-only config and secrets), and `deploy/docker-compose.demo.yml` for a first try with fake data and no
  config. `docs/docker.md` starts with a quick start and a homelab section on protecting the page (token, reverse
  proxy, not exposing it to the internet); the README now presents the three ways to use DeskWatch with the real
  status of each. Docker image: a `HEALTHCHECK` backed by a new `deskwatch-bridge --healthcheck`, which asks the
  bridge's own HTTP listener for `/` over loopback (nothing new is opened); it passes trivially when the bridge has
  no listener. CI starts the dashboard demo in the image and fetches the page and its data. Docker anchors renumbered:
  `docker.md` sections 3 to 10 are now 4 to 11.
- **Kiosk dashboard for big screens.** A new optional output of the bridge: one web page with many widgets at once
  and no rotation (server and host stats with a CPU graph, running jobs with progress, open PRs per repository,
  pipelines, alerts, containers that are down, source health), meant for a Raspberry Pi with a monitor or TV in a
  Chromium kiosk, readable from a 1280x720 screen up to 4K and on a phone. Failed runs, warnings and critical alerts
  show as a banner. It is built into the bridge (no extra install, no CDN or other internet resource), read-only,
  served on the existing `[http]` listener at `/`, and fed over a WebSocket (with polling as a fallback) from
  `/api/kiosk/ws` and `/api/kiosk`; it needs no MQTT broker, keeps the last data on screen when the bridge goes away
  and reconnects by itself. Off by default: `[kiosk] enabled = true`, a grid and `[[kiosk.panel]]` widget list in the
  same TOML file, and an optional `token_file`. `--kiosk` turns it on for `--demo`. The ESP panel and MQTT schema v2
  are unchanged. Guide: [docs/kiosk.md](docs/kiosk.md), with a Chromium autostart example for Raspberry Pi OS in
  `deploy/kiosk/`. **Not yet run on a Raspberry Pi.**
- **TLS spike for the firmware.** A separate `tls-spike` binary in `firmware/` (cargo feature `tls-spike`, so the
  panel firmware is unchanged) that joins Wi-Fi, sets the clock over SNTP and makes HTTPS requests with full
  certificate verification using mbedtls-rs, logging handshake time, response size and heap use over serial. Needs no
  screen or broker. CI builds and lints it. **Not yet run on hardware.** See "TLS spike" in `firmware/README.md`.

### Changed

- The bridge no longer waits for room in the MQTT queue when it publishes a screen. With the broker unreachable the
  queue used to fill up and stop the whole main loop; now the publish is dropped and retried on the next turn (the
  messages are retained, so only the newest matters), the failure is logged once instead of every turn, and a clean
  shutdown gives up after 2 seconds. Needed so the kiosk page keeps updating while the broker is down.
- `--demo` now also reports two extra hosts and healthy sources, so the kiosk widgets have something to show. The
  ESP panel's pages and badges in the demo are unchanged.

## [0.9.4]

### Added

- **Firmware crate (`firmware/`), stage 1.** Rust firmware for the ESP32-S3 SuperMini and the ST7796S display
  (esp-hal, embassy, esp-radio, rust-mqtt, mipidsi, reusing the `ui/` crate). It joins Wi-Fi, connects to the broker
  with the panel login and a Last Will, subscribes to `screen`, `badges` and `bridge/status`, logs what arrives over
  serial and draws it. Settings come from a gitignored `firmware/config.toml` compiled in; flash storage and a setup
  mode are designed in `firmware/README.md` for a later PR. The crate is outside the workspace (it needs the Xtensa
  toolchain) and has its own CI job that builds and lints it. **Not yet run on hardware**: Wi-Fi, MQTT and the
  display code are untested on a board and against a live broker.

## [0.9.3]

### Added

- **Raspberry Pi packaging.** The release workflow now also builds an `aarch64` Linux tarball (with sha256) for
  64-bit Raspberry Pi OS, natively on GitHub's Ubuntu 22.04 arm64 runner. CI builds the Docker image for `linux/amd64`
  and `linux/arm64` (build only, nothing pushed). New guide [docs/raspberry-pi.md](docs/raspberry-pi.md): systemd
  install from the release binary, the Docker route, Mosquitto on the Pi, and the demo. Not yet run on real Pi hardware.
- **Docker support** for the bridge: a small multi-stage `Dockerfile` (release binary, non-root user, config and secrets
  mounted read-only), a Compose example in `deploy/docker-compose.yml`, a guide in
  [docs/docker.md](docs/docker.md), and a CI job that checks the image builds and starts. No image is published.
### Changed

- Docs cleanup: the install guide and README now cover Linux and Windows and no longer carry stale 0.9 statements.

## [0.9.2]

### Added

- **Windows support** for the bridge and the simulator (x86_64). Linux and Windows are the supported systems;
  macOS is left to a contributor.
  - The bridge builds and runs on Windows: the default config is `%ProgramData%\DeskWatch\bridge.toml`, a plain
    credential name is read from `%ProgramData%\DeskWatch\credentials` (or `CREDENTIALS_DIRECTORY`), and Ctrl+C,
    closing the console, logoff and shutdown stop it cleanly. The local server stats page needs `/proc`, so on
    Windows it shows only the machine name.
  - The simulator links a bundled SDL2 on Windows, so `deskwatch-sim.exe` needs no DLL.
  - A Windows install and test guide, [docs/windows.md](docs/windows.md): Mosquitto, the demo with the
    simulator, running the bridge as a service with NSSM or a scheduled task.
  - CI runs clippy and the tests on `windows-latest`.
  - Releases add a Windows zip with `deskwatch-bridge.exe` and `deskwatch-sim.exe`, with a checksum file.

## [0.9.1]

### Added

- **Azure DevOps source** (`[[source.azure_devops]]`) for Azure DevOps Services, by polling with a read-only
  personal access token ([docs/azure-devops.md](docs/azure-devops.md)).
  - YAML pipeline runs on the pipelines page, with a running run's stage and task (`apply: terraform apply`) and
    progress on the job screen, and the failed task in the red alert.
  - A stage waiting for an approval raises an "Approval needed" notice and shows as a `review` row.
  - Open PRs per repository in the PR badge and `prs` page.
  - Work pipelines stay quiet by default (`interrupt = false`). A refused token or an unreachable service lights
    the `warn` badge.

## [0.9.0]

First release, a pre-1.0 one. The bridge, the panel UI and the desktop simulator are done; the firmware for the real panel is
not (see the README, "What is not in 0.9").

### Added

- **Bridge** (`deskwatch-bridge`), a service for a Linux home server that collects data, decides what the panel
  shows and publishes it over MQTT using schema v2 ([docs/mqtt-schema.md](docs/mqtt-schema.md)).
  - Screen priority (critical alert, running job, failed run, success flash, notice, idle rotation), retained
    screen and badge topics, online/offline status with a Last Will, and the button (short and long press).
  - Idle rotation of configurable pages: server stats, open pull requests, pipelines, alerts, and per-host stats
    and containers from Prometheus.
  - Local server stats read from `/proc` and `/sys`.
  - **Gitea source**: Actions run and job webhooks with signature checks, step progress polling, open PRs, and
    the latest run of every pipeline.
  - **GitHub source** for github.com, GitHub Enterprise Server and GHE.com, by polling with ETags and rate limit
    back-off.
  - **Prometheus source**: a stats page per machine from node_exporter, and stopped containers from cAdvisor.
  - **Alerts** on `deskpanel/alert` for Home Assistant and scripts, with severities info, warning and critical,
    a home badge and an alerts page.
  - Secrets only through systemd credentials (`LoadCredential=`) or absolute file paths, never in the config.
  - MQTT broker login and optional TLS, including a private CA and client certificates.
  - `--demo` mode that plays fake data in a loop.
- **Panel UI** (`deskwatch-ui`), a `no_std` drawing crate for the six page templates, header badges and footer
  on a 480x320 screen, with PNG snapshot tests.
- **Desktop simulator** (`deskwatch-sim`): the same pixels in an SDL2 window fed from MQTT, plus headless PNG
  rendering and a preview of example payloads.
- **Deployment**: a hardened systemd unit, an example config, a release binary for Linux x86_64, and an
  install guide ([docs/install.md](docs/install.md)).
- **Mosquitto**: example broker settings and ACL, and a migration guide for turning anonymous access off
  ([docs/mosquitto.md](docs/mosquitto.md)).
- **Home Assistant**: script blueprints "DeskWatch alert" and "DeskWatch clear"
  ([docs/home-assistant.md](docs/home-assistant.md)).
- **CI**: formatting, clippy and tests on stable Rust and on the minimum supported version (1.88).

### Known limits

- Only the first 100 open pull requests per GitHub repository are counted.
- The bridge is Linux only (server stats come from `/proc`; Windows support arrived in 0.9.2). Release binaries
  are Linux x86_64; build from source for other CPUs.
- Gitea webhooks are plain HTTP unless you put a reverse proxy in front of the bridge.

[Unreleased]: https://github.com/Sylentic/DeskWatch/compare/v0.9.8...HEAD
[0.9.8]: https://github.com/Sylentic/DeskWatch/compare/v0.9.7...v0.9.8
[0.9.7]: https://github.com/Sylentic/DeskWatch/compare/v0.9.6...v0.9.7
[0.9.6]: https://github.com/Sylentic/DeskWatch/compare/v0.9.5...v0.9.6
[0.9.5]: https://github.com/Sylentic/DeskWatch/compare/v0.9.4...v0.9.5
[0.9.4]: https://github.com/Sylentic/DeskWatch/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/Sylentic/DeskWatch/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.2
[0.9.1]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.1
[0.9.0]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.0
