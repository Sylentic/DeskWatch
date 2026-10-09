# Changelog

All notable changes to DeskWatch are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Version 1.0 is reserved for the first release with working firmware on the real
screen. Until then (0.x) the MQTT schema (v2) and the bridge config file aim to change only in compatible ways, but
may change if the firmware work needs it; any such change is listed here.

## [Unreleased]

### Added

- **Docker support** for the bridge: a small multi-stage `Dockerfile` (release binary, non-root user, config and secrets
  mounted read-only), a Compose example in `deploy/docker-compose.yml`, a guide in
  [docs/docker.md](docs/docker.md), and a CI job that checks the image builds and starts. No image is published.

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

[Unreleased]: https://github.com/Sylentic/DeskWatch/compare/v0.9.2...HEAD
[0.9.2]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.2
[0.9.1]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.1
[0.9.0]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.0
