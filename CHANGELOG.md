# Changelog

All notable changes to DeskWatch are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Version 1.0 is reserved for the first release with working firmware on the real
screen. Until then (0.x) the MQTT schema (v2) and the bridge config file aim to change only in compatible ways, but
may change if the firmware work needs it; any such change is listed here.

## [Unreleased]

### Added

- **HTTPS, opt-in.** `[http.tls]` with `cert` and `key` (PEM) makes the one HTTP listener serve HTTPS only: kiosk
  page, setup page, `/api/alert`, webhooks and the WebSocket (which becomes `wss`). A wrong path stops the bridge at
  startup, and the Docker health check follows. Plain HTTP stays the default, so nothing changes for existing installs.
  `deploy/docker-compose.https.yml` adds Caddy with an internal CA as a second route. Guide: [docs/https.md](docs/https.md).

## [0.9.17]

### Changed

- **The setup page uses the whole screen.** A menu on the left (Sources, MQTT broker, Dashboard, Bridge and restart,
  Stored secrets) instead of one long list, with the cards in several columns on a wide screen. **Add a source** is now
  the first thing on the page. Confirmations are boxes on the page instead of browser dialogs.

### Added

- **Restart button on the setup page** ("Bridge and restart", and in the banner after a change). It asks for a
  confirm click, then waits until the bridge answers again and says so if nothing started it again. Still opt-in with
  `[setup] allow_restart = true`; while it is off the page explains how to turn it on. It relies on a restart policy
  (see [setup-page.md](docs/setup-page.md#restart)).
- **Everything can be removed from the setup page.** Sources from `bridge.toml` can be switched off (and on again)
  without editing that file (`hidden` in `sources.toml`); the MQTT login and the saved broker settings can be
  removed, including the stored password; stored secrets that nothing uses can be deleted. Each asks for confirmation.
- `GET /api/setup/status` has `boot`, `secrets`, `hidden` and `hideable` on sources, and `removable` and `has_login`
  on `mqtt`. New routes: `POST /api/setup/source/hide`, `/api/setup/mqtt/delete`, `/api/setup/secret/delete`.

## [0.9.16]

### Changed

- **The default kiosk layout is new.** Open PRs is a tall column on the left, Running now is large on the right, one
  Hosts panel (1 wide, 2 tall) with a small Pipelines panel sits in the middle. Alerts are no longer a panel: they scroll
  through the page header, from left to right, with severity, message and source (they pause on hover or touch, and
  stand still with "reduce motion"). Sources are no longer a panel: the sources and their status are on the setup page,
  and the "sources" chip in the header opens it. Custom `[[kiosk.panel]]` blocks are untouched, and the `alerts` and
  `health` widgets still exist for them.
- **Stats and Hosts are one Hosts panel.** `host_view = "detailed"` (the default) shows the chosen host in detail on top
  and every host as a list below it; tap a host to switch (kept in the browser). `compact` is the list only, with network
  rates. The setup page chooses the view as before ("Dashboard hosts"). A `hosts` panel in your own layout can take
  `view = "detailed"` or `"compact"`; without it, it is the plain table as before.
- A running job list and the Pipelines panel no longer cut off at 3 jobs or 4 runs by default; they scroll.

### Added

- **Kiosk page: click through to the source.** Open PRs, pipeline runs, running jobs and runner lists are links that
  open the item on Gitea, GitHub (also Enterprise) or Azure DevOps in a new tab. The bridge now carries a web address
  for each (`url` in the kiosk snapshot), built from the source's own address, organisation and project, and only
  `http(s)` addresses become links. See [docs/kiosk.md](docs/kiosk.md#click-through-and-scroll).
- **Kiosk page: cards scroll.** Every card scrolls inside itself (wheel, touch, keys) with a fade instead of a
  scrollbar. Open PRs and Running now no longer cut off after 3 jobs: they show everything unless `rows` is set. A card
  is not redrawn while you scroll it and goes back to the top after 30 seconds.

## [0.9.15]

### Removed

- **BREAKING: `[kiosk] token_file`, `[kiosk] trusted_networks`, `[kiosk] session_days` and `[setup] token_file` are gone,
  together with the cookie login behind them.** Since 0.9.12 the dashboard and the setup page have no login (they are for
  a trusted network); these settings only kept the old behaviour alive with a warning. A config that still has one of
  them is now refused at startup with a message that names the section and key to delete, instead of quietly running
  with (or without) a login you did not expect. To upgrade, delete those lines from `bridge.toml`. If you still want a
  login, put a reverse proxy with its own in front ([docs/docker.md](docs/docker.md#protecting-the-page)).
  What stays: the `Origin` check (a website in your browser cannot reach the WebSocket or change the setup) and write-only
  secrets. Gone with it: `POST /api/login`, `POST /api/setup/login`, the `Authorization: Bearer` and `?token=` access to
  `/api/kiosk`, and the token box on both pages.

### Added

- **`deskwatch-bridge ado-capture`**: records what a real Azure DevOps organisation answers (builds, timelines, pull
  requests, agent pools, agents, job requests) into one file with every name, id, link and mail address replaced by
  stable placeholders, so the answers can be shared and compared with the hand-written test data. Read-only, the token
  is never printed or saved, and a final check refuses to write a file that still holds the organisation, a project
  name or the token. See `docs/azure-devops-verify.md`.
- `docs/mosquitto.md`: a step-by-step switch-over for a broker in Docker (section 7), plus notes that an ACL leaves every unlisted client without access and that Docker often publishes the broker port on all interfaces.

### Fixed

- The Docker healthcheck now covers a bridge that only serves the setup page (no dashboard, MQTT off): it checks `/setup` instead of passing without looking.
- Documentation audit against 0.9.14, line by line. `docs/raspberry-pi.md` no longer says no image is published (the
  image is on ghcr.io); `docs/docker.md`, `docs/install.md`, the example config, the Dockerfile and the code comments no
  longer say the HTTP port only opens for a Gitea source or the dashboard (it also opens for the setup page and, with MQTT
  on, the health check); `docs/azure-devops.md` had a broken scope table and now says a real organisation has been
  connected once but its data not yet verified; `docs/runners.md` lost its "first version" and "this PR" wording;
  `docs/releasing.md` lists every file a release PR changes; `docs/windows.md` mentions the dashboard-only setup;
  the README's demo and Docker rows are corrected; comments pointing at schema section numbers match `docs/mqtt-schema.md`.

### Changed

- New integration test: the setup page saves a source on an empty writable data folder (the Docker named volume case) and the source and its secrets come back after a restart. The CI Xtensa toolchain install now gets a GitHub token so it no longer hits the rate limit.
- CI and release workflows use current major versions of the Docker and artifact actions (Node 24), and the Dockerfile skips the `SecretsUsedInArgOrEnv` lint for `CREDENTIALS_DIRECTORY`, which is a path and not a secret (issue 74).

## [0.9.14]

### Changed

- Kiosk default layout: Running now is now the large panel (2 by 2, top right) and Pipelines the smaller one below it (2 wide, bottom right; it shows the 4 latest runs, more with a `rows` setting of your own), instead of the Pipelines-first order of 0.9.13 (PR 69). Your own `[[kiosk.panel]]` order is untouched.
- Kiosk Pipelines panel: the latest run is at the top. Runs are listed newest first across Gitea, GitHub and Azure DevOps instead of failed and running ones first; the MQTT pipelines page is unchanged.
- **Docs: running under Komodo or Portainer.** `docs/docker.md` has a new section on inline `configs: content:` (a refused
  `read_only`, a changed config that is not noticed, secrets from stack variables), the named volume for the data folder and
  who can reach the port.
- **Docker: a named volume now works for the data folder.** The image contains an empty `/var/lib/deskwatch` owned by
  uid 10001, so a new named volume mounted there is writable for the bridge and the setup page can save without a
  manual `chown`. `docs/data-dir.md` and `docs/docker.md` show the named volume next to the bind mount and explain why
  a relative path is a bad idea under Komodo or Portainer. A named volume created earlier is still root-owned (one-off
  fix in `docs/data-dir.md`).

## [0.9.13]

### Added

- **A "Setup" link in the dashboard header** that opens `/setup`, shown only when the setup page is on
  (`[setup] enabled = true`), so a wall screen without it stays clean. The setup page links back with "Dashboard".

### Changed

- **Azure DevOps address and project names**: an address that already ends in the organisation (`https://dev.azure.com/<organization>/`) no longer
  doubles it in the request path, and a project name pasted as `My%20Project` is no longer encoded twice. Both ended in a 404 from a real
  organisation. The setup page labels say what to enter.
- **Azure DevOps connection test**: a 404 now says `HTTP 404` and the path that was asked for, and lists the projects the
  token can see, so a mistyped project or organisation name is easy to spot.
- Kiosk default layout: the Pipelines panel now comes before Running now (Pipelines top right, Running now bottom right). Your own `[[kiosk.panel]]` order is untouched.
- Documentation audit against 0.9.12: the setup page and data folder are now covered in the README, the Docker guide, the install guide, the Compose files, the example config and the systemd unit; the Azure DevOps agent pool status is described as done (mock answers only); the authentication stance and the open ESP setup-mode question are stated in the README; one dead link in the firmware guide removed.

## [0.9.12]

The no-login release: the dashboard and the setup page no longer use token authentication. Both are meant for a trusted
network (your home network); never expose the port to the internet, and do not publish it through a proxy or tunnel
without a login. Built against mock servers only, not run in a real browser or in Docker. Authentication for the ESP
panel's setup mode is left for when that exists.

### Changed

- **The dashboard needs no login by default.** `[kiosk] token_file` is optional again for the Docker quick-start: the
  dashboard Compose file and `dashboard.example.toml` no longer create or require a token, so a new install opens at
  once and a Raspberry Pi kiosk needs nothing typed. The bridge logs one clear warning when the page has no login and
  listens on the network: keep it on your home network, never expose it to the internet or publish it through a proxy
  or tunnel without a login. `token_file` and the cookie login stay for anyone who wants them (nothing about them
  changed).
- **The setup page needs no login either.** It is turned on with `[setup] enabled = true` and is open to whoever can
  reach the port, so use it on a trusted network only (the bridge warns at startup). Secrets still never reach the
  browser, token fields stay write-only and a page from another website still cannot make changes (the `Origin`
  check). Authentication for the ESP panel's setup mode is left for when that exists.

### Deprecated

- **`[setup] token_file`** still works (it keeps a login in front of the page and turns the page on) but the bridge
  warns; use `[setup] enabled = true`.
- **`[kiosk] trusted_networks`** still works but the bridge now warns at startup and the docs advise removing it: the
  peer address it relies on is not the real client behind Docker or a reverse proxy.

## [0.9.11]

The setup release: sources, tokens and the MQTT broker can be set up in the browser instead of by hand-editing files.
Everything here is built against mock servers and fake brokers only, not yet checked against a real GitHub, Azure
DevOps or Mosquitto. The planned last step, a setup mode on the ESP panel itself, is **not** part of this release: it
waits for the screen (see `docs/setup-page.md`).

### Added

- **Setup page: MQTT broker settings.** With a data folder the page changes `enabled`, `host`, `port`, `tls`, the login
  name and the password, tests the connection first (nothing is written when it fails, unless "Save anyway") and
  stores the password as a 0600 credential. The settings are applied on top of `bridge.toml` through an `[mqtt]` table
  in `sources.toml`; they apply after a restart, because the broker connection is made at startup. New
  `[setup] allow_restart` adds a "Restart now" button (clean shutdown, exit code 75, for Docker or systemd).
- **Sources reload without a restart.** Adding, editing, removing a source or replacing its token on the setup page
  takes effect at once: the bridge starts, replaces or stops that source's task, drops what a stopped source reported
  (pages, badges, warnings) and adds or removes its Gitea webhook route. A source that cannot start leaves the old
  one running and the page shows why. A source's helper task now stops with it.
- **Setup page: add, edit and remove sources, replace tokens** (needs the data folder). Write-only token fields,
  validate-before-save with the connection test (with "Save anyway"), unknown keys in `sources.toml` are kept, every
  change is logged without values.
- **Setup page, read-only** (`[setup] token_file`, off by default): `/setup` shows each source (address, repositories,
  whether it comes from `bridge.toml` or the data folder, live health), whether each credential is found or missing
  (never its value), and the MQTT broker and its connection. It has its own token, login cookie (12 hours) and failure
  limit, and ignores `kiosk.trusted_networks`; see docs/setup-page.md.
- **Test connection** on the setup page, per source and for the MQTT broker: the bridge makes a few read-only requests
  with the real credentials and lists each check (repositories, projects, runners, agent pools, hosts, broker login),
  with a hint when the token is refused. One test at a time; messages never contain a credential. The MQTT test uses
  its own client id and no Last Will, so it never disturbs the live connection.
- **Data folder** (`[data] dir` or `DESKWATCH_DATA_DIR`, off by default): a writable folder holding `sources.toml`
  (more `[[source.*]]` blocks, merged with `bridge.toml`; a duplicate name is an error) and `credentials/` (secrets in
  0600 files, atomic writes, found by a plain `token_file` name; a mounted credential of the same name wins).
  `bridge.toml` is never rewritten. The setup page saves into it; see docs/data-dir.md.

### Changed

- **One-time login for the dashboard** (issue 32). With `[kiosk] token_file` set, the page asks for the token once and
  trades it (`POST /api/login`) for a session cookie (`HttpOnly`, `SameSite=Strict`, `Secure` behind an HTTPS proxy),
  valid `[kiosk] session_days` days (default 30). The token no longer has to be in an address. Replacing the token
  file logs every browser out. Wrong tokens are delayed and, after five in a minute, logins are refused for a minute.
  The live WebSocket refuses a foreign `Origin`. `Authorization: Bearer` keeps working for scripts; `?token=` still
  works but is deprecated and logs a warning once.

## [0.9.10]

The Azure DevOps release: agent pools, waiting jobs and non-approval checks. All three are built against mock
responses only, not yet checked against a real organisation.

### Added

- **Azure DevOps checks other than approvals** (branch control, business hours, invoked functions): a stage held by a
  `Checkpoint.*` record for two timeline polls in a row shows "Check pending" and a `review` row instead of looking like
  it runs. The record names are not verified against a real run.
- **Azure DevOps agent pools** on the kiosk `runners` widget. `[[source.azure_devops]]` gets `agent_pools = ["<pool name>"]`
  and `agent_poll_s` (default 60). Off unless `agent_pools` is set. Needs one more token scope, **Agent Pools (Read)**;
  a refused list is logged and does not change the source's health. Built against mock responses only.
- **Jobs waiting for an agent** per Azure DevOps pool: the kiosk snapshot gets a `queued_jobs` list (version stays 1) and
  the `runners` widget shows a `n waiting` row per pool and tag. Same token scope, same mock-only caveat.

## [0.9.9]

The kiosk release: runner status, night theme and widget filters, trusted networks, and a faster Docker build.

This is the first release since 0.9.7 that is actually built: it carries everything from 0.9.8 (see below) as well as
the changes listed here.

### Added

- **`[kiosk] trusted_networks`**: CIDR ranges (or single addresses, IPv4 and IPv6) whose clients skip the kiosk token
  for the data routes and the WebSocket. Judged only by the TCP peer address, never by `X-Forwarded-For`. Default
  empty, so nothing changes without config. Behind a proxy or Docker NAT the peer may not be the real client;
  `docs/kiosk.md` and `docs/docker.md` say how to check it (debug log of the peer address).
- **GitHub self-hosted runners** on the kiosk `runners` widget. `[[source.github]]` gets `runners = ["repo:<owner>/<name>",
  "org:<name>"]` and `runner_poll_s` (default 300). Off unless `runners` is set. Needs extra token rights:
  **Administration: read** for a repository's runners, **Self-hosted runners: read** for an organisation's. A token
  without them only loses the runner list; the PR and pipeline widgets keep working. Works on GitHub Enterprise
  Server and GHE.com too. See `docs/runners.md`.
- **Kiosk night theme**: `[kiosk] theme = "day" | "night" | "auto"` with `night_from` and `night_to` (default 22:00 to
  07:00, by the browser's clock). The night look is the same layout at about half the brightness. `?theme=` on the page
  address overrides the config. The default stays `day`, so nothing changes unless you set it.
- **Kiosk widget filters**: the `jobs`, `prs` and `pipelines` widgets take `source` (`gitea`, `github`,
  `azure_devops`) and `repo` (a name, or a prefix ending in `*`), so one card can show work and another home.
- **Kiosk pixel shift** for OLED screens: `pixel_shift = true` moves the page a few pixels every
  `pixel_shift_minutes` (default 5). Off by default.
- **Runner status on the kiosk dashboard.** A new `runners` widget lists runners and agents: offline first (red), then
  busy (orange), then idle (green); a runner an administrator switched off shows grey and never counts as offline.
  The header says how many are offline and the card greys out when the list stops updating.
- **Gitea runners** as the first source for it. `[[source.gitea]]` gets `runners = ["user", "org:<name>",
  "repo:<owner>/<name>", "admin"]` and `runner_poll_s` (default 30). Runners are off unless `runners` is set. The
  demo (`--demo`) shows four fake runners.
- **`docs/runners.md`**: the design for runners and agent pools across Gitea, GitHub and Azure DevOps, the token
  scopes each needs, and the order the other two sources follow in.
- The kiosk snapshot has a `runners` list (snapshot version stays 1; a page that does not know it ignores it).

### Changed

- **Documentation sweep.** README and the example config now cover `trusted_networks`, `max_ws_clients`, the `runners`
  widget and the published ghcr.io image (the README still said nothing is published).
- **Faster Docker rebuilds**: the `Dockerfile` compiles the dependencies in their own layer, so a change in the bridge,
  `sim` or `ui` source no longer recompiles them. Fixes #29.
- **Clearer kiosk token card.** The "needs a token" and "token rejected" cards now say it is the dashboard access
  token from the file named by `token_file` under `[kiosk]`, not a CI token, and show the `/?token=YOUR_TOKEN`
  example in code style so it no longer breaks mid-word. `docs/kiosk.md` and `docs/docker.md` explain the same.

## [0.9.8]

The Docker and kiosk hardening release: Compose uses the published image, WebSocket limits, reverse proxy examples.

**Never released.** The `v0.9.8` tag was pushed on the commit before this release was merged, so the release
workflow stopped at its version check. No binaries, Docker image or GitHub release exist for 0.9.8. Everything listed
here is part of [0.9.9](#099) and later; use 0.9.9.

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

[Unreleased]: https://github.com/Sylentic/DeskWatch/compare/v0.9.13...HEAD
[0.9.13]: https://github.com/Sylentic/DeskWatch/compare/v0.9.12...v0.9.13
[0.9.12]: https://github.com/Sylentic/DeskWatch/compare/v0.9.11...v0.9.12
[0.9.11]: https://github.com/Sylentic/DeskWatch/compare/v0.9.10...v0.9.11
[0.9.10]: https://github.com/Sylentic/DeskWatch/compare/v0.9.9...v0.9.10
[0.9.9]: https://github.com/Sylentic/DeskWatch/compare/v0.9.8...v0.9.9
[0.9.8]: https://github.com/Sylentic/DeskWatch/compare/v0.9.7...v0.9.8
[0.9.7]: https://github.com/Sylentic/DeskWatch/compare/v0.9.6...v0.9.7
[0.9.6]: https://github.com/Sylentic/DeskWatch/compare/v0.9.5...v0.9.6
[0.9.5]: https://github.com/Sylentic/DeskWatch/compare/v0.9.4...v0.9.5
[0.9.4]: https://github.com/Sylentic/DeskWatch/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/Sylentic/DeskWatch/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.2
[0.9.1]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.1
[0.9.0]: https://github.com/Sylentic/DeskWatch/releases/tag/v0.9.0
