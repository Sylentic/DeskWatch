# Changelog

All notable changes to DeskWatch are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). In 1.x the MQTT schema (v2) and the bridge config file only change
in compatible ways: new optional keys and new pages, no renamed or removed ones.

## [Unreleased]

## [1.0.0]

First release. The bridge, the panel UI and the desktop simulator are done; the firmware for the real panel is
not (see the README, "What is not in 1.0").

### Added

- **Bridge** (`deskwatch-bridge`), a service for a Debian home server that collects data, decides what the panel
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
- The bridge is Linux only (server stats come from `/proc`). Release binaries are Linux x86_64; build from
  source for other CPUs.
- Gitea webhooks are plain HTTP unless you put a reverse proxy in front of the bridge.

[Unreleased]: https://github.com/Sylentic/DeskWatch/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/Sylentic/DeskWatch/releases/tag/v1.0.0
