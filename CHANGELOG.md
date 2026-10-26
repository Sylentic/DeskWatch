# Changelog

All notable changes to DeskWatch are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). From 1.0.0 on, a change that breaks an existing bridge config file or the
`POST /api/alert` format needs a new major version.

## [Unreleased]

## [1.0.6]

### Added

- **Free placement on a visible grid.** In edit mode the grid is drawn behind the panels. A panel can be put on any free
  cell and sized on the cells (no overlap, red when it does not fit), and the grid grows downwards when you want
  everything under each other. The bridge saves a column and row per panel (`pos` in `layouts.json`).
- **The `:dev` image shows it is a dev build.** The version on the setup page now reads e.g. `1.0.5-dev (abc1234)`
  instead of the plain number. Release images are unchanged.
- **Smoother customizing.** Moving a panel now shows a floating panel and a dashed drop slot while the others make
  room, and resizing is done by dragging a corner handle that snaps to the grid and shows the size. Both work with the
  mouse and with touch, and from the keyboard. This replaces the small W, H and arrow buttons (#165).
- **`[server] enabled = false` hides the bridge's own machine** from the dashboard, with a **Show this machine's own
  stats** switch under **Dashboard hosts** on the setup page (it applies after a restart). For a bridge in Docker
  whose Prometheus source already covers the machine, so the container's own numbers no longer show as a second host.
- **A red GPU line on the CPU graph** of a host that reports a GPU, on the same 0 to 100 % scale.

### Changed

- **Dependencies updated:** tokio 1.54.1 and the GitHub Actions checkout step (v7).

### Fixed

- **Customized layouts are saved more reliably.** A change the bridge could not store (for example a lost session or no
  writable data folder) used to be dropped without a word and was gone after a reload; the Customize bar now says so.
  Saves are sent one after the other, a late answer from the bridge no longer overwrites a change you just made, and
  the layout calls use relative addresses like the rest of the page, so they also work behind a proxy under a sub path.
- **Setup no longer shows an error after saving the Dashboard hosts view.** Changing Detailed to Compact (or back)
  was saved correctly, but the page said "Saved, but kiosk could not be started" because the answer did not say it
  was applied. It now shows the normal "Restart now" notice; the restart itself is expected, as the layout is built
  at startup.

## [1.0.5]

### Added

- **A new install starts on an account setup page.** With an empty data folder, `/` and `/setup` open the first-run
  page, which asks for a user name and a password; you then land on Setup to add sources. A **Continue without a
  login** link and `[auth] required = false` keep the old behaviour, for a home network or a Pi kiosk that needs
  nothing typed on first boot. Installs that already have files in their data folder, installs without a data folder
  and the demo are not redirected. (#158)

### Changed

- **Dependencies updated.** argon2 0.6 (the password code is adapted; hashes made by earlier versions still
  verify, a test covers it), base64, getrandom, serde_json, sha2, tokio and toml. The `rustls-webpki` 0.102.8
  advisories that the newest rumqttc still pulls in for an error type are ignored in the audit with a written reason.

### Fixed

- **Container links no longer replace the dashboard tab.** A link of a source with a Firefox container now opens in
  a new tab like every other link, so DeskWatch stays open.

## [1.0.4]

### Changed

- **`docs/raspberry-pi.md` is shorter.** It keeps what is special about a Pi (64-bit OS, the aarch64 binary, a Pi that only
  shows the page) and points to `docker.md` and `install.md` for the rest.
- **`deploy/docker-compose.yml` uses the `latest` image tag** instead of a fixed version, so `docker compose pull`
  gives the newest release and a release no longer needs a change in the Compose file. Pin a version number in the
  file to stay on one release. `docs/docker.md` and `docs/releasing.md` are updated.
- **A tidier Setup page.** The top bar has the dashboard's height and look, with icon buttons that name themselves in a
  tooltip: Dashboard, My account, Light/Dark mode (a sun in dark mode, a moon in light mode) and Sign out, last.
  Adding and changing things now happens in a pop-up on the page instead of a form in between the cards: a source
  (pick its type first), the broker settings, a user, a new password, an API key (shown once, with a Copy button) and
  the GitHub and Microsoft sign-in. Access shows the numbers of users, keys and signed-in browsers, and a cleaner list
  of users and keys. Removing anything still asks first. Nothing changes in what is saved.
- **My account opens as a pop-up in Setup** (your role, your own password, Sign out). The `/account` page stays, for a
  first password an administrator chose.
- **Scroll hint is a pill instead of a fade.** A card with more below no longer fades its bottom row; a small
  "↓ N more" pill shows how many items are out of view, and tapping it scrolls down. It only shows when the card can
  scroll.

### Added

- **Ways to give feedback.** Issue forms for bugs, feature requests and "I tested a source or feature", a
  `CONTRIBUTING.md`, a `SECURITY.md` and a pull request template. The README has a "Help testing" section listing what
  was only tried against mock answers, and every GitHub release now ends with "Something not working? Open an issue".
- **GitHub App sign-in.** A GitHub source can sign in as a GitHub App installed on the organisation (`auth = "app"`
  with `app_id`, `installation_id` and `private_key_file`; in Setup: **Sign in with: GitHub App**) instead of with
  one person's token. The bridge signs a short token with the App's private key and swaps it for an hour-long
  installation token. Tried against mock answers only. Details in `docs/git-hosts.md`.
- **Backup in Setup: export and import your settings.** Setup > Backup downloads sources, broker, alerts, dashboard
  and sign-in rules and everyone's dashboard layout as one file, by default **without secrets** (safe to keep and
  share). Optionally the stored tokens and client secrets go in too, locked with a password (Argon2id + XChaCha20-
  Poly1305). Passwords, API keys and sessions are never exported. Importing shows what is inside before anything
  changes, then replaces the settings; restart afterwards. Guide: [docs/backup.md](docs/backup.md).
- **Sign in with a service principal from the Setup page.** An Azure DevOps source has a **Sign in with** list:
  personal access token or Entra service principal (tenant id, application id, client secret). Until now the service
  principal could only be set in a config file. The list is built so another source type can get a choice of sign-in
  method later. Tried against mock answers only.
- **Alertmanager webhooks.** Prometheus Alertmanager can post to `POST /api/alertmanager` (same `http_input` switch
  and alert keys as `/api/alert`). Firing alerts appear with the severity from the `severity` label, `resolved` clears
  them. `critical_ids` accepts a trailing `*`. Mocks only, not tried against a real Alertmanager. See
  [docs/alertmanager.md](docs/alertmanager.md).
- **Open a source's links in a Firefox container.** Setup > Sources and `bridge.toml` get an optional `container`
  setting for Gitea, GitHub, GitLab, Bitbucket and Azure DevOps. In Firefox with the add-on "Open external links in a
  container", the links of that source open in the named container. Off by default; other browsers are not affected.
  See [docs/firefox-containers.md](docs/firefox-containers.md).

### Fixed

- **Test connection and Remove now work for an Azure DevOps source that signs in with Entra.** The test used to
  look for a token and fail, and removing such a source left its client secret behind.

## [1.0.3]

This release removes the ESP32 desk display (see Removed: a `[[rotation]]` block in `bridge.toml` is now ignored with a
warning) and makes the dashboard more useful day to day: alerts and pipelines are coloured by status, Running now shows
every job of a run, and each user can arrange their own dashboard.

### Removed

- **The ESP32 desk display is dropped.** Its firmware never ran on a board, so `firmware/` (with the TLS spike), the
  panel drawing crate `ui/` and the desktop simulator `sim/` (`deskwatch-sim`, also gone from the Windows zip) are
  removed, together with the firmware CI job and the SDL2 builds. The last commit that still has them is the one
  before this change in the history. MQTT stays for alerts (`deskpanel/alert`, Home Assistant).
- **The bridge no longer talks to an ESP desk panel.** It stops publishing `deskpanel/screen` and `deskpanel/badges`,
  no longer reads `deskpanel/panel/status` and `deskpanel/panel/event`, and the idle page rotation, the button
  handling and the page data types behind them are gone. The dashboard is unchanged: the bridge now only decides
  whether something interrupts the idle page (a running job, a failed or successful run, an alert, a notice).
  MQTT stays for alerts: `deskpanel/alert`, the Home Assistant blueprints, broker logins and TLS, and
  `deskpanel/bridge/status` (online and the Last Will) all work as before. If a script of your own listened to
  `deskpanel/screen` or `deskpanel/badges`, it will stop receiving them; clear the leftover retained messages with
  `mosquitto_pub -r -n -t deskpanel/screen` (and `deskpanel/badges`).
- **`[[rotation]]` is ignored.** A config that still has `[[rotation]]` blocks keeps starting; the bridge logs one
  warning and ignores them. Delete them from `bridge.toml`: the key will be refused in a later version.
  `bridge/config.example.toml` no longer contains them.
- **The `deskwatch-panel` broker login is no longer needed.** `deploy/mosquitto/acl.example` now gives the bridge
  `read deskpanel/alert` and `write deskpanel/bridge/status` only. An existing ACL that gives the bridge
  `readwrite deskpanel/#` keeps working. `docs/mqtt-schema.md` now describes only the alert topic.
- The sample payloads for screens, badges and panel events in `docs/schema/` are removed; the alert samples stay.

### Added

- **DeskWatch has an icon.** The dashboard, Setup, the login page and My account now show the status ring in the
  browser tab (`/favicon.svg`, with a PNG fallback and an `apple-touch-icon.png` for a phone home screen). The icon
  routes need no login. The logo and a GitHub social preview image are in `docs/logo/`.
- **Everyone can customize the dashboard.** The grid icon in the header opens edit mode: hide a panel, drag it (or use
  the arrows) to move it, and make it wider, narrower, taller or shorter. Hidden panels wait under "Hidden panels" in the
  edit bar, one click brings them back, and "Reset to default" restores the administrator's layout. The choice is kept per
  user on the bridge (`layouts.json` in the data folder), so it follows a signed-in user to any browser, and in the
  browser without a login. Every role, Viewer included, changes only their own layout.

### Changed

- **Setup in the header is a cogwheel icon**, and every item in the header bar (badges, pills, icons, update notice) is
  now the same height and centred on one line.
- **Alerts are coloured by severity** in the dashboard's Alerts card and the header Alerts menu: info alerts are blue
  (their box is back, as before 1.0.2), warnings are yellow and critical alerts red. Warning yellow is its own colour,
  apart from orange (running) and amber (waiting for approval).
- **Pipelines get the same boxes**: every run in the Pipelines card sits in a box tinted by its status (failed red,
  success green, running orange, waiting for approval amber), and a build that just finished in Running now gets a
  green box.
- **Running now lists the jobs of each run**: one heading per run (project, pipeline, branch, commit, how many jobs
  are done) with a coloured row per job under it, so parallel jobs show side by side with their own status, step and
  time. Works for GitHub and Gitea jobs, GitLab jobs (with their stage), Bitbucket steps and Azure DevOps jobs; a
  source that only knows the run as a whole shows one row. The Pipelines card stays one row per pipeline.

### Documentation

- The guides, the README and `config.example.toml` were swept for the removed ESP panel (pages, the `warn` badge, the
  `deskwatch-panel` login, the simulator), the new alert colours and the cogwheel and grid icons in the header. New:
  the GPU rows (`docs/kiosk.md`, `docs/install.md`), a table of what each Git host adds to the alerts list
  (`docs/git-hosts.md`), the Access and Sign-in sections of Setup, roles and My account (`docs/setup-page.md`,
  `docs/auth.md`), and links for GitLab merge requests and pipelines. The dashboard screenshot in `docs/` is new and
  the unused panel picture is gone.

### Fixed

- **The icons and the clock in the header stay on the right** on narrow windows, tablets and phones (up to 900 px wide).
  There they used to jump to the left, because the spacer that pushes them right is switched off at that width.
- **Signing in over the bridge's own HTTPS (`[http.tls]`) works in a browser.** Browsers talk HTTP/2 there and send no
  `Host` header, so the origin check refused the sign-in, Setup changes and dismissing alerts with "cross-origin request
  refused". The check now uses the request's authority too. Behind Caddy or another proxy nothing changes.
- **Setup, Access on a phone** no longer scrolls sideways: the user rows wrap and the forms keep to the card width. The
  "My account" link follows the theme colour.
- **Viewers and operators no longer see buttons that do nothing.** The dashboard hid nothing by role, so Dismiss and Mute
  (Viewer) and the Setup link (Viewer and Operator) were shown although the bridge answers those requests with 403. They
  are now left out for the roles that cannot use them.

## [1.0.2]

### Added

- **GPU usage, VRAM and GPU temperature on the host card** (#115). Prometheus hosts read them from the NVIDIA DCGM
  exporter by default, and `gpu_pct`, `gpu_mem_pct` and `gpu_temp_c` can be replaced under `queries`. The bridge machine
  reads AMD GPUs from sysfs. The rows only show for hosts that report a GPU. Intel and NVIDIA on the bridge machine
  itself need a Prometheus host.
- **New issues and new PR comments in the alerts list** (#118). GitHub, GitLab and Bitbucket (polled) and Gitea (the
  **Issues** webhook event) add a new issue (Bitbucket: issue tracker on, token with **Issues: Read**); Azure DevOps adds a new comment on an open PR, skipping your own. They show as info alerts with the
  source's mark, link to the issue or PR, and stay until dismissed (or a week). Existing issues and comments do not
  alert at startup. `alert_new_issues = false` or `alert_pr_comments = false` turns them off per source.
- **Azure DevOps can sign in as a Microsoft Entra service principal** instead of a personal access token:
  `auth = "entra"` with `tenant_id`, `client_id` and `client_secret_file`. The bridge fetches a short-lived token with
  the client-credentials flow, keeps it in memory, renews it before it expires and once when Azure DevOps refuses it.
  Nothing changes for existing `token_file` setups. Tested against mock Entra and Azure DevOps answers only. Research
  and the other options (managed identity, Entra OAuth, GitLab, Bitbucket, GitHub) in `docs/azure-devops-auth.md` and
  `docs/git-hosts.md`.
- **A bridge that only reads Azure DevOps (or only GitHub Enterprise) is documented**, and without `[[rotation]]` blocks
  the ESP rotation now adds the PR and pipeline pages when a Git host is configured.
- **Several users with their own role.** Setup › Access now lists local users. A **viewer** looks at the dashboard, a new
  **operator** may also dismiss alerts and failed runs, an **administrator** can do everything, including Setup, users
  and keys. A new user gets a password the administrator chooses and replaces it at the first sign-in; everyone has a
  **My account** page (`/account`) to change their own password. The last administrator cannot be removed or lowered.
- **A role per sign-in rule.** GitHub and Microsoft sign-in got operator users, teams and groups next to the existing
  viewer and administrator rules. With several matches the highest role wins.

### Changed

- **Info alerts are plain rows in the dashboard's Alerts card**, without the blue box. Warnings and critical alerts
  keep their amber and red boxes.

- An existing `auth.toml` is read as before: the administrator and the viewer password become the first two users
  (the viewer keeps the user name `viewer`). Without a password nothing changes.
- The single "viewer password" box is gone from Setup; add a viewer user instead.

### Fixed

- **Drop-downs on the setup page no longer jump back** to their first or saved value when the page refreshes itself
  every few seconds (#120), for example the source type under "Add a source". A refresh also waits while a drop-down
  is open.

## [1.0.1]

### Fixed

- **A failed run now always shows as a red pill in the dashboard header**, until it is dismissed (× in the pill, or
  Dismiss in the Failed list). Before, the pill only followed the panel's screen, so it stayed away while a job ran, for
  repositories without `interrupt`, and after ten minutes. A critical alert still comes first.
- **Light mode applies to the dashboard and the sign-in page too.** The Light mode button on the setup page now
  switches all three pages; the night theme still wins at night. `?scheme=light|dark` overrides it on one screen.
- **The CPU graph uses the full width from the start**, instead of a short line at the right edge for the first five
  minutes after a reload. Its label says how much time it covers.
- **A finished run shows its steps as Done**: the same segments it had while it ran, all green.
- **Setup › Dashboard describes the host views as they are now** (a list next to the chosen host, or one table).

### Changed (project)

- **One pipeline** (`.github/workflows/pipeline.yml`) for pull requests and releases, and a **`dev` branch**: new
  work goes into `dev` first, and `main` only takes a pull request from `dev`. `main` stays the default branch, the
  released state. See `docs/releasing.md`.

## [1.0.0]

The first stable release: the bridge, the dashboard (in a browser, on a Raspberry Pi or in Docker), the setup page and
the login. The ESP desk display stays experimental and is not part of this release.

### Changed

- **New look for the dashboard, the setup page and the sign-in page ("Graphite").** Darker, more compact cards with one
  set of text styles everywhere. The typefaces (Geist, Geist Mono, Space Grotesk; SIL Open Font License) are built into
  the bridge and served on `/fonts/`, so the pages still need no internet. The night theme keeps working.
- **Where something runs is always visible.** Every job, run, pull request, runner and alert carries a two-letter mark
  for its source (Gt, GH, GL, BB, AZ, PM, HA), and a bar under the header lists every source with its state ("2
  running", "ok", "not answering"). With the setup page on, it opens the setup page.
- **One event at a time in the header.** A run result, notice or alert is one pill: red for failed and critical, amber
  for warnings, green for success. A success right after a failure of the same pipeline says **Fixed**; a deploy says
  **Deployed**. A new red result pulses once (not with reduced motion).
- **Running now** shows one segment per step when the source tells the steps, and a run that just finished well stays
  for 30 seconds as "Done" before it moves to Pipelines. Just-finished rows in Pipelines are tinted red or green for a
  short while. Open PRs is one list, newest first, with a NEW tag for the first hour.
- **Hosts, detailed view:** a list to pick from next to the chosen host (above it when the panel is narrow). Hosts that
  need a look (down, over a warning line, or rebooted in the last hour) come first. Both views show uptime.
- **Default layout:** Running now and Hosts (2x2 each) on top; Pipelines (2x1), Open PRs and Alerts (1x1 each) below.
  The Alerts panel is new in the default layout: each alert is a coloured box with a Dismiss button. Layouts set with
  `[[kiosk.panel]]` are not touched.

## [0.9.20]

### Added

- **Sign in with GitHub or Microsoft.** Optional, configured on the setup page under Sign-in providers. Authorization
  code flow with PKCE, access by user, organisation, team or Microsoft group, admin rules, roles re-read at every
  sign-in, deny by default. The local admin always works. See docs/auth.md.

### Changed (security)

- **The setup page and `POST /api/alert` only answer the local network by default.** Clients with a loopback, private,
  link-local, IPv6 unique-local or Tailscale (100.64/10) address work as before; anything else gets `403`, also when it
  comes through a LAN reverse proxy (judged by the last `X-Forwarded-For` entry). `[http] allow_public = true` lifts the
  rule, and the bridge warns when that is on without `[http.tls]`. A normal home install is unaffected; a setup page or
  alert sender that is reached from the internet through a tunnel stops working until you set it. This is the first
  step of making the app safe to offer beyond the home network (a login follows). See `docs/setup-page.md`.
- `[http] allow_public = true` now needs a login password; without one the bridge refuses to start.

### Added

- **A built-in login** ([docs/auth.md](docs/auth.md)). Set an administrator password (first-run screen, Setup, Access, or
  `deskwatch-bridge auth set-password` on the host) and the dashboard, its WebSocket, the setup page and every change need a
  sign-in. Optional viewer password (dashboard only). Scripts and screens use API keys: an alert key for `POST /api/alert`
  (`Authorization: Bearer`), a viewer key for a Pi kiosk (`/?key=`). Argon2id hashes and key hashes in `auth.toml` in the data
  folder, in-memory sessions (HttpOnly, SameSite=Lax cookie, Secure over HTTPS), an `X-DeskWatch` header on every write, and
  rate limiting per address after five wrong tries. `deskwatch-bridge auth reset` forgets the passwords. Off until a password is
  set, so existing installs are unchanged. The health check accepts the 303 or 401 a guarded page answers.
- **GitLab source** (`[[source.gitlab]]`) for gitlab.com and self-managed GitLab, by polling with a read-only
  `read_api` token. Open merge requests count as the open PRs (exact count from `X-Total`), pipelines fill the
  pipelines page, and a running pipeline shows as one "Running now" card with its current stage (GitLab jobs have no
  steps, so the bar counts stages). A failed pipeline raises the red alert naming the failed job. Optional runner
  lists (`project:`, `group:`, `instance`) for the `runners` widget. Also in the Setup page ("Add a source", with a
  connection test), the kiosk `source` filter and `docs/gitlab.md`. **Only tested against mock answers written from
  the GitLab documentation; not yet tried on a real GitLab.** `docs/git-hosts.md` lists which Git hosts work today
  (Forgejo and Codeberg only in part).
- **Bitbucket Cloud source** (`[[source.bitbucket]]`), by polling with a read-only access token. Open pull requests
  count as the open PRs, pipelines fill the pipelines page, and a running pipeline shows as one "Running now" card
  with its current step and a progress bar. A failed pipeline raises the red alert naming the failed step. Optional
  runner lists (`repo:`, `workspace:`) for the `runners` widget (no busy state, Bitbucket does not send one). Also in
  the Setup page, the connection test, the kiosk `source` filter and `docs/bitbucket.md`. **Only tested against mock
  answers written from Atlassian's API description; not yet tried on a real Bitbucket.**
- **A "source down" pill in the kiosk header.** When a source cannot be checked (not answering, or its login
  refused) a pill next to the alerts says which one and why. A quiet source is not a down source. **Mute** hides the
  pill on every screen until the source works again; the "N sources have a problem" chip keeps counting it.
- **A quiet "update available" notice.** Once a day the bridge asks GitHub for the latest release number. When it is
  newer than the running version, the dashboard header shows a small chip "update available: vX.Y.Z" next to Setup
  (links to the release notes, × hides it for that version in that browser) and Setup shows it behind the version.
  Only the version number is compared; nothing about the install is sent, and a failed check is silent. Off with
  `[updates] check = false` or the checkbox under Setup, Bridge and restart. See `docs/updates.md`.
- **HTTPS, opt-in.** `[http.tls]` with `cert` and `key` (PEM) makes the one HTTP listener serve HTTPS only: kiosk
  page, setup page, `/api/alert`, webhooks and the WebSocket (which becomes `wss`). A wrong path stops the bridge at
  startup, and the Docker health check follows. Plain HTTP stays the default, so nothing changes for existing installs.
  `deploy/docker-compose.https.yml` adds Caddy with an internal CA as a second route. Guide: [docs/https.md](docs/https.md).

### Changed

- **HTTPS in the Docker examples.** Checked the Docker health check against `[http.tls]` (it works, it probes `https://`
  on loopback). `docker-compose.dashboard.yml` and `dashboard.example.toml` now show how to mount the certificate, and
  `docker.md` no longer says the bridge has no TLS.

- **Dismissing a failed run now happens on the bridge**, so it disappears on every screen at once, like an alert, instead
  of per browser (localStorage). A new failure of the same pipeline still shows up; restarting the bridge shows the
  failures again.

### Fixed

- The startup warnings that the kiosk page, the setup page and `POST /api/alert` "have no login" are no longer logged
  when an administrator password is set.
- **CI:** the firmware job's cache is keyed on the Xtensa GCC version, and the Xtensa toolchain install gets a GitHub
  token, so it no longer fails on API rate limits.

### Documentation

- A full pass over the documentation against the current code: the login, HTTPS, setup page, kiosk, Docker, Home
  Assistant and install pages no longer say the dashboard "has no login" without the "until you set a password"
  qualifier, the source lists name GitLab and Bitbucket Cloud, and the README states plainly what is tested on real systems
  and what is not (GitLab and Bitbucket: mock answers only; Forgejo and Codeberg: untested; OAuth sign-in: designed, not
  built). New `docs/README.md` indexes every page.

## [0.9.19]

### Changed

- **Alerts, run results and notices sit in the header as coloured pills** instead of moving text or a full-width banner
  under it. The most urgent alert is a pill next to the badges (label, title, message, source and "+N more"); a run
  result or notice (Success green, Failed red, Critical deep red, Warning amber, Notice blue) is a pill beside it.
- **The Alerts badge works like the Failed badge**: it (and the alert pill) fold out a menu with all alerts, each with a
  Dismiss button, plus Dismiss all. The `dismiss` setting now controls these buttons.
- **Header badge order** is Open PRs, Failed runs, Alerts, and each badge disappears when its count is 0.
- **The header no longer turns yellow or red** for an alert: the colour is in the pill, which would otherwise vanish
  against a red bar.
- **Setup, Alerts**: "Show the ticker" is now "Show the latest alert" (the header pill). The speed setting is gone;
  `ticker_speed` in `[alerts]` is still accepted but has no effect.

## [0.9.18]

### Changed

- **A warning or critical alert tints the header** yellow or red instead of showing a banner under it, while it is on
  screen. Failed runs and info alerts keep their banner.
- **The setup page is dark by default** and no longer follows the system's light or dark setting. A "Light mode" button in
  the header switches to light and is remembered in the browser.

### Added

- **Alerts over HTTP.** `POST /api/alert` takes the same JSON as the MQTT topic, so a script, `curl`, Home Assistant's
  `rest_command` or Uptime Kuma can put alerts on the dashboard without a broker. Off by default: switch it on with
  `http_input = true` in `[alerts]` or with the checkbox in the Alerts section of the setup page, then restart. No login
  (trusted network only); a request from a web page on another site is refused. See
  [home-assistant.md](docs/home-assistant.md#without-mqtt-the-web-address).
- **Alerts section on the setup page.** Ticker on or off, speed, "via source", the most alerts kept and an on/off
  switch for dismissing (saved in the data folder, applied after a restart), and a **Send test alert** button that raises
  one fixed test alert for a minute so the ticker can be seen working. The page can send no alert text of its own.
- **Dismiss on the dashboard.** Each alert in the ticker has a small x that clears it for every screen. It works without
  MQTT and can only clear. `[alerts]` gained `ticker`, `ticker_speed`, `show_source`, `dismiss` and `http_input`;
  `max_active` must be between 1 and 50.
- **The "failed" badge in the kiosk header can be dealt with.** Click it for a list of the failed runs (source,
  project and pipeline, branch, failing step, age; each opens the run on its source) with "Dismiss" per run and "Dismiss
  all". Dismissed failures stop counting; a new failure of the same pipeline shows up again. The choice is kept in the
  browser (localStorage), so a second screen has its own list. The snapshot has a new `failed` field.

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
