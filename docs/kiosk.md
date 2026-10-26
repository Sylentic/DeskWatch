# The kiosk dashboard (big screens)

The **kiosk dashboard** is one web page that shows everything at once, for a monitor or TV (a Raspberry Pi with a browser in kiosk mode, a
wall tablet, or any browser on your desk). It is built into the bridge, so there is nothing extra to install, and
it works on a LAN with no internet access (no CDN or scripts are loaded from elsewhere; the typefaces, Geist, Geist Mono
and Space Grotesk, are built into the bridge and served on `/fonts/`, see `bridge/fonts/`; the tab icon is served on
`/favicon.svg`, see `bridge/icons/`).

The page does not need a broker.

Contents: [1. What you get](#1-what-you-get) · [2. Turn it on](#2-turn-it-on) ·
[3. Choose the widgets](#3-choose-the-widgets) · [4. Autostart on a Raspberry Pi](#4-autostart-on-a-raspberry-pi) ·
[5. Security](#5-security) · [6. When something goes wrong](#6-when-something-goes-wrong) ·
[7. The data](#7-the-data-the-page-reads) · [8. What was and was not tested](#8-what-was-and-was-not-tested)

## 1. What you get

A grid of cards, 4 columns by 3 rows by default:

| Widget | Shows |
|---|---|
| `stats` | One host in detail: CPU, RAM, disk bars with warning colours, GPU and VRAM bars and GPU temperature (only for a host that reports a GPU, see [GPU](#gpu-on-the-host-card)), temperature, load, network rates, uptime, a CPU graph of the last few minutes |
| `hosts` | Every host (the bridge's own machine and every host a Prometheus source reports): a list, or with `view = "detailed"` the chosen host in detail on top of the list |
| `jobs` | Running now: a heading per run, and under it a row per job (parallel jobs, GitLab jobs, Bitbucket steps, Azure DevOps jobs) with its status colour, step, progress bar and a live elapsed time |
| `prs` | Open pull requests per repository, newest first, with the total |
| `pipelines` | The latest run of every pipeline, one boxed row each, tinted by status (failed red, success green, running orange, waiting for approval amber), newest first across all sources (a running run counts from its start, a finished one from its end), so the top rows are always the most recent activity; in the default layout it is two cells wide and scrolls |
| `alerts` | Active alerts (Home Assistant, scripts, new issues and PR comments from your Git hosts), most urgent first, each in a box coloured by severity |
| `containers` | Containers and hosts that are down (needs the Prometheus source) |
| `health` | Whether every source is working, or why not (token refused, not reachable) |
| `runners` | Runners and agents: offline first, then busy, then idle. Not in the default layout; see [runners.md](runners.md) |

### Click through, and scroll

Open PRs, pipeline runs, running jobs and runners are links: a click (or tap) opens the item on its own site in a new
tab, so the dashboard stays where it is. A row only becomes a link when the source gave an address the bridge could
trust; otherwise it stays plain text.

| Source | PR | Run and running job | Runner |
|---|---|---|---|
| Gitea | the pull request | the run (Gitea's own address when its webhook sends one, else the run by its number, else the repository's Actions page); a job opens its job page or the Actions page | the runners page of the configured scope (`admin`, `user`, `org:<name>`, `repo:<owner>/<name>`) |
| GitHub and GitHub Enterprise | the pull request | the workflow run; a job opens its job page | the runner settings of the repository or organisation (none for `admin` and `user`, GitHub has no such page) |
| GitLab | the merge request | the pipeline; a job opens its job page | the instance, group or project runner settings |
| Bitbucket Cloud | the pull request | the pipeline run | no link (Bitbucket has no stable runners page) |
| Azure DevOps | the pull request in its repository | the build results page | the agent pool in the organisation settings (a link per pool, not per agent) |

Addresses are built from the source's own configured address, never hard coded, and only `http` and `https` addresses
are used. The runner list links go to the page that lists the runners, because the APIs do not give a page per runner.

Every card scrolls inside itself when its list is longer than the card: mouse wheel, touch drag or the arrow keys. There
is no scrollbar (it clashes with the card edge and the night theme); a small "↓ N more" pill at the bottom edge shows how many items are below, and tapping it scrolls down.
Open PRs and Running now show everything unless you set `rows`, so a long list scrolls instead of being cut off with
"+N more". While you scroll a card, its contents are not redrawn (a redraw would throw your place away); about 8 seconds
after you let go it catches up, and a card you left scrolled goes back to the top after 30 seconds, so a wall display
shows the newest items again.

### GPU on the host card

A host that reports a GPU gets **GPU** and **VRAM** bars and a **GPU temp** line on its card; hosts without a GPU show
nothing extra. A GPU above 95 % puts the host in the "needs a look" group. Prometheus hosts read the figures from the
NVIDIA DCGM exporter by default; `gpu_pct`, `gpu_mem_pct` and `gpu_temp_c` can be replaced per host under `queries`
([install.md](install.md#prometheus-host-stats-and-containers)). The bridge machine itself reads AMD GPUs from sysfs;
Intel and NVIDIA GPUs on the bridge machine need a Prometheus host. Not yet tried on a real GPU.

### Hosts: detailed or compact

The built-in layout has one **Hosts** panel, two cells wide and two tall. In the `detailed` view (the default) a list
of every host sits on the left and the host you chose is shown in detail next to it (CPU, memory and disk bars, uptime,
temperature, load, network, cores and a CPU graph). Each line in the list shows the host's highest value (`DISK 88`);
hosts that need a look (down, over a warning line, or started less than an hour ago) come first, with an amber or red
pip. Tap or click a host to show it in detail; the choice is kept in the browser (localStorage), so a reload keeps it,
and a host that has disappeared falls back to the first one. The chosen host never changes by itself. When the panel is
narrow (one cell wide) the chosen host is shown above the list instead. In the `compact` view the panel is only the
table: uptime, CPU, memory and disk, and the download and upload rates (as columns when the panel is two cells wide,
else as a small line under each host).

`[kiosk] host_view = "detailed"` or `"compact"` chooses the view, or the [setup page](setup-page.md) ("Dashboard
hosts", needs a data folder and a restart of the bridge); the setup page's choice wins over `bridge.toml`. In your own
`[[kiosk.panel]]` blocks a `hosts` panel takes `view = "detailed"` or `"compact"` the same way; without `view` it is the
plain table (two cells wide it gets network columns). A `stats` panel keeps working as before: with a fixed `host` it
shows that host, without one it has arrows (`‹ 1/3 ›`) to step through the hosts.

### Failed runs: look and dismiss

The red **failed** counter in the header counts pipelines whose latest finished run failed. It used to stay until the
pipeline turned green, so an old
failure could sit there forever. Now the counter is a button: click or tap it for a list with, per failed run, the
source, project and pipeline, branch, failing step and how long ago, each opening the run on its source in a new tab.
**Dismiss** removes one run from the list and the counter, **Dismiss all** clears the list. Escape or a click outside
closes it. With nothing left the counter shows `0 failed` with a green dot.

- A *new* failure (a new run of the same pipeline) is not hidden by an earlier dismiss and shows up again.
- A pipeline that later succeeds drops off by itself, as before.
- A dismissal is kept on the bridge, so it clears the run on every screen at once, like dismissing an alert. It is
  forgotten once the failure leaves the data (the pipeline went green). It lives in memory only, so restarting the
  bridge shows the failures again. Without a password set ([auth.md](auth.md)) nothing needs a login: the page is for a trusted network.
- The snapshot no longer lists a dismissed run, and the failed badge counts what is left.
- API: `POST /api/kiosk/failed/dismiss {"ids": ["<key>@<started>", ...]}`, same-origin only.

### Source down

When a source cannot be checked, a pill appears in the header next to the alert pill: **SOURCE DOWN**, the source id
and why (amber "not answering", red "sign-in refused"), with "+N more" for further sources. It only means the bridge's
last check failed. A quiet source (no new PRs or runs for days) is fine and shows nothing. The source's entry in the
sources bar under the header turns amber or red, and the connection state at the end of that bar keeps saying
"N sources have a problem", for as long as the source is down.

**Mute** hides the pill on every screen (the bridge remembers it, in memory) until that source works again; the sources
bar still shows the problem, and the next outage shows the pill again. API: `POST /api/kiosk/source/mute {"id": "..."}`.

### Alerts in the header

The most urgent active alert is a coloured pill on the right of the header: the severity in colour (INFO blue,
WARNING yellow, CRITICAL red), what is wrong, the message and the source ("homeassistant"), with "+N more" when
there are more. With no alerts there is no pill. Click it, or the **Alerts** badge, to fold out the menu with all alerts.
The header badges (one bar next to the DESKWATCH name) run Open PRs, Failed runs, Alerts; each is gone at 0. While a run
result or notice is showing (see below), it takes the pill's place: the header shows one pill at a time.

The **Alerts** badge counts every active alert, coloured by the worst one. In the menu and in the Alerts card every alert
sits in a box in its severity colour (info blue, warning yellow, critical red; yellow is its own colour, apart from
running orange and waiting amber). Alerts that come from your Git hosts (a new issue, a new PR comment) are info alerts
with the source's mark; they link to the issue or PR and stay until dismissed or for a week. In the menu every alert has a **Dismiss**
button, plus **Dismiss all**. Dismissing clears the alert for every screen (the same as a `clear` message on MQTT,
unlike Failed runs, which are hidden per browser) and works without MQTT. It can only clear, never raise or change an
alert. Switch it off with `dismiss = false` in `[alerts]`.
The header pill on or off and "via source" are `[alerts]` settings too (`ticker`, `show_source`), also
changeable on the [setup page](setup-page.md#alerts).

### Customize your own dashboard

The grid icon in the header (next to the cogwheel for Setup) starts **edit mode**. Every panel gets a **Drag to move**
grip on top, an x to hide it and a resize handle in the bottom right corner.

- **Move:** drag the grip with the mouse or a finger. The panel floats with the pointer, a dashed **Drop here** slot shows
  where it lands and the other panels make room as you go. Esc cancels.
- **Resize:** drag the corner handle. A dashed outline snaps to the grid cells and shows the size (`2 × 1`); the panel
  changes when you let go. A panel can be as large as the grid.
- **Keyboard:** focus the grip and use the arrow keys to move the panel earlier or later; focus the handle and use the
  arrow keys to change its width and height.

A hidden panel leaves the grid and waits under **Hidden panels** in the bar on top; click it there to bring it back. The
bar counts the cells in use (`10 of 12`) and has **Reset to default**. **Done** leaves edit mode.

The administrator's layout (the built-in one or your `[[kiosk.panel]]` blocks) stays the default. A user only stores the
difference, so a panel the administrator adds later still shows up (at the end). Panels flow in order on the grid, so
a smaller panel can leave a gap.

Where the choice is kept:

| Situation | Kept |
|---|---|
| Signed in (local user or GitHub/Microsoft), data folder writable | On the bridge, per user (`layouts.json`, mode 0600), so it follows the user to any browser. Every role may save its own layout, nobody can change another user's. |
| No login, or a screen opened with a viewer key | In this browser (localStorage) |

API: `GET /api/auth/layout`, `PUT /api/auth/layout`, `POST /api/auth/layout/reset` (409 when the sign-in is not a person).

### Where things run: source marks and the sources bar

Every job, run, pull request, runner and alert carries a two-letter mark for its source: **Gt** Gitea, **GH** GitHub,
**GL** GitLab, **BB** Bitbucket, **AZ** Azure DevOps, **PM** Prometheus, **HA** Home Assistant, **LO** the bridge
machine or a script. The bar under the header lists every source with its mark, its name and its state: how many jobs
run there, `ok`, or what is wrong (`not answering`, `sign-in refused`). With the setup page on, each entry opens it.
At the end of the bar the connection state says whether the page gets live data (`all sources ok`, `offline`,
`polling, no live socket`).

### Run results in the header

Interrupts do not take over the whole screen, because the other widgets are still
useful. The bridge's normal priority decides (critical alert, running job, failed, warning, success, notice), and
the page shows the winner as a coloured **pill in the header**:

- **FAILED** (red) and **CRITICAL**: stay until dismissed; a new one pulses once (not with reduced motion).
- **WARNING** (yellow).
- **SUCCESS** (green), **FIXED** when the same pipeline failed before (the bridge has already replaced the red result),
  **DEPLOYED** for a pipeline with "deploy" in its name: these go away by themselves.
- **NOTICE** (blue).

While a job runs, a thin orange progress line under the header follows it. A run that finished well stays in Running
now for 30 seconds as **Done**, and a row in Pipelines that just finished is tinted red or green for 30 seconds. An
open PR younger than an hour carries **NEW**.

The size follows the screen width, so the same layout is readable on 1080p and 4K and on a small 1280x720 screen.
Under 900 px wide (a phone) it becomes a single scrolling column. The pointer hides after 3 seconds without mouse movement.

## 2. Turn it on

It is off by default. Add this to the bridge config (see `bridge/config.example.toml`):

```toml
[kiosk]
enabled = true
```

Restart the bridge and open `http://<bridge host>:8787/` in a browser (`[http] listen` sets the address; port 8787
by default). The page is served on the same listener as the webhooks, so there is no new port. On the Pi that runs
the bridge, use `http://localhost:8787/`.

To try it with fake data and no sources, run the demo:

```sh
deskwatch-bridge --demo --kiosk
```

`--kiosk` turns the page on without editing the config (the config file is optional with `--demo`). The page needs
no broker, but the bridge tries to reach one unless you tell it not to and logs a warning every few seconds until it
finds one. Add `--no-mqtt` (or `[mqtt] enabled = false` in the config) for a bridge that serves only the page:

```sh
deskwatch-bridge --demo --kiosk --no-mqtt
```

**In Docker** the same page is the main use: a container on a server that you open from any browser, with a
dashboard-only config and no broker. The Compose files, the token and the reverse proxy options are in
[docker.md](docker.md#3-the-dashboard-on-your-homelab).

## 3. Choose the widgets

`[[kiosk.panel]]` blocks place widgets on the grid, left to right and top to bottom.
Without any, the default layout is used: `jobs` (Running now, 2 wide, 2 tall) and one `hosts` panel (2 wide, 2 tall,
see [Hosts](#hosts-detailed-or-compact)) on top, `pipelines` (2 wide), `prs` and `alerts` (1 cell each) below them.
The Alerts panel shows every active alert as a coloured box with a Dismiss button (when `[alerts] dismiss` is on). There
is no Sources panel: the sources and their status are in the bar under the header, which opens the
[setup page](setup-page.md) when it is on. The `alerts` and `health` widgets still
exist for your own layout. Add a `runners` panel yourself (it needs a source with runner scopes set, see
[runners.md](runners.md)); the default grid has no free cell for it.

```toml
[kiosk]
enabled = true
columns = 4          # grid size, 1 to 12 each (default 4 x 3)
rows = 3

[[kiosk.panel]]
widget = "stats"
host = "homeserver"  # which host; default is the first one (the bridge's own machine)

[[kiosk.panel]]
widget = "stats"
host = "nas"         # a host name from a [[source.prometheus]] block

[[kiosk.panel]]
widget = "jobs"
span = [2, 1]        # [columns, rows] of the grid this widget covers

[[kiosk.panel]]
widget = "prs"
span = [1, 2]
rows = 12            # at most 12 list rows (default: as many as fit)
title = "Reviews"    # replace the heading
```

Each widget takes `span` cells (default `[1, 1]`) and the grid always fills the screen, so the cells are as big as
the screen allows: a 3x2 grid on a 4K TV gives very large cards. List widgets scroll inside their card (see Click through, and scroll), and with `rows` set they stop at that many rows. Configuration errors (a span larger than the grid, a `host` on a widget that
has none, an unknown widget) stop the bridge at startup with a message that names the block.

### Filter a widget by source or repository

The `jobs`, `prs` and `pipelines` widgets can show only part of what the bridge knows. Both filters are optional
and combine (a widget needs to match both):

```toml
[[kiosk.panel]]
widget = "prs"
title = "Work PRs"
source = "azure_devops"   # gitea, github, gitlab, bitbucket or azure_devops: the tag shown next to a repository

[[kiosk.panel]]
widget = "pipelines"
repo = "acme/*"           # one repository or project, or a prefix ending in *; case does not matter
```

`repo` is the name shown on the card (the project, after its `alias`). There is no wildcard in the middle, only a
trailing `*`. A widget whose filter matches nothing shows its normal empty text. Filters only change the widget:
the header counters and the banner still count everything, and `host` still belongs to `stats` only. Two copies of
a widget with different filters is the way to split, for example, work and home pipelines over two cards.

Users can also hide, move and resize panels for themselves, see [Customize your own dashboard](#customize-your-own-dashboard).

### Light mode, day and night theme

The page is dark. The **Light mode** button (the sun icon) on the [setup page](setup-page.md) switches the dashboard to light as well
(the choice is kept in that browser, and a dashboard open in another tab follows at once); `?scheme=light` or
`?scheme=dark` on the address overrides it for one screen. The night theme below wins over light mode at night.

A **night theme** keeps the layout and the colour meanings (green, red, running
orange) at about half the brightness, for a dark room:

```toml
[kiosk]
theme = "auto"        # day (default), night, or auto
night_from = "22:00"  # auto only, HH:MM; the window may cross midnight
night_to = "07:00"
```

`auto` uses the clock of the **browser** (the same one as the clock in the header), not the bridge's, so a Pi in
another time zone dims at its own bedtime. The page checks every second and switches without a reload.
`?theme=day`, `?theme=night` or `?theme=auto` on the page address overrides the config, which is handy to try the
night look in the afternoon.

### OLED protection (pixel shift)

```toml
[kiosk]
pixel_shift = true
pixel_shift_minutes = 5   # 1 to 120, default 5
```

Every `pixel_shift_minutes` the whole page moves one step around a ring of nine positions, at most 0.25 rem (4 px
at 1080p, 8 px at 4K) from the centre. That keeps the edges of cards and text from sitting on the same pixels for
months. The step follows the wall clock, so a reload does not restart the walk. The page edge that moves out of
view is cut off by a few pixels. Off by default. For an OLED you should still switch the screen off at night.

With `[setup] enabled = true` the header also shows a cogwheel that opens the [setup page](setup-page.md); without it
the icon is not there.

## 4. Autostart on a Raspberry Pi

Two parts: the bridge runs somewhere ([raspberry-pi.md](raspberry-pi.md) lists the choices), and Chromium starts in kiosk mode when the Pi
boots to the desktop. Use **Raspberry Pi OS with desktop, 64-bit**, set to log in automatically
(`sudo raspi-config` > System Options > Boot / Auto Login > Desktop Autologin) and install the browser if it is
missing: `sudo apt install chromium`. The package is called `chromium-browser` on older releases.

Copy the start script (it waits until the bridge answers, then opens Chromium full screen) and tell the desktop to
run it at login:

```sh
install -m 755 deploy/kiosk/deskwatch-kiosk.sh ~/deskwatch-kiosk.sh      # from a checkout of this repository
mkdir -p ~/.config/labwc
echo '~/deskwatch-kiosk.sh &' >> ~/.config/labwc/autostart                # Bookworm and newer (labwc, Wayland)
```

[`deploy/kiosk/deskwatch-kiosk.sh`](../deploy/kiosk/deskwatch-kiosk.sh) opens `http://localhost:8787/`. For a bridge on
another machine, set the address with an environment variable, for example
`DESKWATCH_KIOSK_URL='http://bridge.example.lan:8787/' ~/deskwatch-kiosk.sh &`. With the [login](auth.md) on, create a **viewer** key under Setup, Access and put `?key=<key>` in the URL once. Without a password the page has no login, so a Pi
on the same network needs nothing else ([security](#5-security)). Older releases, or Bookworm switched to X11 with LXDE, read
`~/.config/lxsession/LXDE-pi/autostart` instead; see
[`deploy/kiosk/lxde-autostart`](../deploy/kiosk/lxde-autostart), which also turns the X11 screen blanking off.
If there is no checkout on the Pi, the script is short enough to copy from the repository page.

Things that make a kiosk last:

- **Screen blanking.** Turn it off in `sudo raspi-config` > Display Options > Screen Blanking (the menu name varies
  between releases), then check after an hour that the screen is still on. The LXDE file above covers X11.
- **Mouse pointer.** The page hides it after 3 seconds without mouse movement and shows it again on the next
  movement; `sudo apt install unclutter` is only needed if it shows up at the edges.
- **Chromium crash restore.** `--noerrdialogs --disable-session-crashed-bubble` keep the "restore pages" bubble
  away. A dedicated `--user-data-dir` keeps the kiosk's profile apart from yours.
- **Bridge restarts and upgrades need no action.** The page reconnects by itself, and after an upgrade it reloads
  itself once it sees a new version.
- **4K on a Pi 4.** The Pi 4 outputs 4K at 30 Hz unless you set `hdmi_enable_4kp60=1` in `/boot/firmware/config.txt`
  (it runs hotter). 30 Hz is plenty for this page, which only animates a progress bar and a small spinner.
- **Memory.** A Pi 4 with 2 GB or more, or a Pi 5, is the sensible floor for Chromium at 4K; use a 1080p output on
  anything smaller. A Pi Zero 2 W is not enough for a browser.
- **Burn-in.** The page is mostly static. On an OLED screen, turn on `pixel_shift` and the night theme
  ([section 3](#3-choose-the-widgets)), and switch the screen off at night
  (for example `wlr-randr --output HDMI-A-1 --off` from a cron job) rather than leaving it on for months.

## 5. Security

The page and its two data routes (`/api/kiosk` and `/api/kiosk/ws`) only show data. The few things the page can change
(dismissing alerts and failed runs, muting a source, saving your own layout) are separate calls that need the right role
once a login is set ([auth.md](auth.md)). The page shows PR titles, pipeline names, host stats and alert text, so treat them like a dashboard.

- **No login until you set a password.** The listener is `0.0.0.0:8787` (it has to be reachable for Gitea webhooks and
  for other machines on your network). With the kiosk on and no password set, anyone who can reach the port can read the
  page. On a home network that is the intended trade-off, and the bridge logs one warning at startup saying so. **Never
  expose the port to the internet** (no router port forward, no tunnel or public reverse proxy) unless a login sits in
  front of it: the built-in one ([auth.md](auth.md), with HTTPS) or the proxy's own. To keep the page local to the Pi, run the bridge there with
  `[http] listen = "127.0.0.1:8787"`, which is fine when the Pi has no webhook source.
- **The built-in login** ([auth.md](auth.md)) replaces the old `[kiosk] token_file`, `trusted_networks` and
  `session_days` (removed; see [Removed login settings](#removed-login-settings) below). Independent of it, the
  WebSocket refuses an `Origin` that is not the page's own host, so a website open in your browser cannot read the page
  through it. The connection is plain HTTP unless you turn on [HTTPS](https.md) or put a reverse proxy with TLS in front.
- **In a container**, `0.0.0.0:8787` inside is correct and the published port decides who can reach the page:
  `"8787:8787"` is the whole network (trusted network only), `"127.0.0.1:8787:8787"` is the Docker host only, for a reverse
  proxy that adds TLS and a login. Do not expose the port to the internet; use a VPN or an authenticating proxy.
  Details in [docker.md](docker.md#protecting-the-page).
- Names shown come from your sources after their `alias` setting. If a work project name should not appear on a
  visible screen, give it an alias. The page has no other redaction.
- The page sets a strict Content Security Policy: scripts, styles and fonts only from the bridge itself.

## 6. When something goes wrong

- **The bridge restarts or the network drops.** The page keeps the last data on screen, dims it, and shows
  "No data from the bridge for 0:42. Showing the last known state, reconnecting." in the banner row; the connection state
  at the end of the sources bar turns red and says `offline`. It retries with a growing delay (up to 10 s) and also polls `/api/kiosk` in case a
  proxy blocks WebSockets, and carries on by itself when the bridge is back.
- **A source stops working** (expired token, service down). The `health` widget names it, the sources bar marks it
  and says how many sources have a problem, and the widgets built from that source's data are dimmed with a "may be out of date"
  look. Hosts that Prometheus reports as down get a red dot and a dimmed card.
- **The bridge host's clock and the screen's clock differ.** Times such as "3 min ago" use the bridge's clock, so a
  Pi with a wrong clock still shows correct ages. The clock in the header is the screen's own.
- <a id="removed-login-settings"></a>**The bridge refuses to start: "[kiosk] token_file is no longer supported".**
  `[kiosk] token_file`, `[kiosk] trusted_networks`, `[kiosk] session_days` and `[setup] token_file` were removed: the
  those old settings no longer do anything. Delete the line the message names from the config. The bridge stops
  instead of ignoring the line, so nobody keeps a login they believe is still active. If you need a login, set a
  password ([auth.md](auth.md)) or put a reverse proxy with its own in front ([docker.md](docker.md#protecting-the-page)).
- **The page says why it has no data.** The connection state (end of the sources bar) and a box in the page body
  name the cause:

  | Connection state | Meaning | What to do |
  | --- | --- | --- |
  | `bridge unreachable` | The request did not get an answer. | Check the container or service is running, the address and port, the firewall, the proxy. |
  | `bridge error 502` (any HTTP status) | Something answered, but with an error. | Usually a reverse proxy that cannot reach the bridge; see its log and the bridge log. |
  | `polling, no live socket` | The WebSocket is blocked (a proxy without WebSocket support) but the page still gets data by polling every 5 s. | Works as is. To get instant updates, let the proxy forward `Upgrade` headers on `/api/kiosk/ws`. |
  | box "Waiting for the first data" | The bridge is up but has not collected anything yet. | Wait a few seconds; the page fills in by itself. |

  If nothing helps, open `http://<host>:8787/api/kiosk` in a browser: JSON means the bridge is fine and the problem
  is in the page or the browser; 401 means a sign-in is needed ([auth.md](auth.md)); nothing means the bridge is not reachable (address,
  firewall, `[http] listen`). The bridge log shows `serving http://...` at startup.

## 7. The data the page reads

`GET /api/kiosk` returns the latest snapshot, and `/api/kiosk/ws` sends a snapshot over a WebSocket on every change
and at least every 5 seconds (a heartbeat, so the page can tell quiet from gone). Snapshots are versioned with `"v"`
(1), independent of the alert topic. Fields: `build` (bridge version; the page reloads when it changes), `now`
(Unix seconds), `layout` (the grid, the widgets with their filters, `theme` and `pixel_shift_s` from the config), `screen` (the composer's pick: `level`, `template`, `page` and `data`, or `null` when idle), `badges`, `failed` (the failed runs behind the badge, minus the dismissed ones: `key`, `source`, `project`, `pipeline`, `ref`, `step`, `started`, `finished`, `url`), `hosts`, `down`, `jobs`, `run_jobs`, `runs`, `pulls`, `runners`, `queued_jobs`, `alerts` and `sources` (`id`, `health`, and `muted: true` when the pill was muted). `jobs`, `runs`, `pulls[].items[]` and `runners` each carry a `url` (an `http(s)` address, or `null`). A job's `group` names its run; `run_jobs` maps that group to every job of the run (`name`, `status`, `step`, `progress`, `started`, `finished`, `url`), so jobs that run side by side are listed together.
The structure is in `bridge/src/kiosk.rs` (`snapshot`) and
covered by the tests next to it. It is made for this page and may change between versions; scripts should use `POST /api/alert` instead.

Status colours (the dots and boxes in Pipelines, the Running now rows and the header counters use the same ones): orange
and pulsing for running, red for failed, green for success, amber and still for waiting for approval, grey for queued,
cancelled and anything unknown. Orange is deliberately darker than amber so a running build and one waiting for
approval stay apart, and only the running dot pulses. Alerts use their own severity colours: blue, yellow and red.

`?static` on the page address stops all network use; the page then waits for `DeskWatchKiosk.render(snapshot)`
to be called, which is how the screenshots and tests drive it.

## 8. What was and was not tested

Tested: the endpoints, the WebSocket (a real handshake and pushed changes), the sign-in checks, the config parsing and
the snapshot contents are covered by automated tests (`cargo test -p deskwatch-bridge`). The failure states were run in Chromium against the live `--demo --kiosk` bridge (WebSocket blocked, bridge unreachable). The page was run in
Chromium against the live `--demo --kiosk` bridge and rendered at 1280x720, 1920x1080, 3840x2160 and a 420 px wide
phone, and was checked while the bridge was killed and started again: the page dimmed and said it was offline within 25 seconds, and recovered by itself within 14 seconds of the bridge returning. The reload on a new bridge version (`build` changing) is written but was not exercised.

The night theme, the `source` and `repo` filters and the pixel shift were run in headless Chromium at 1920x1080
against the live `--demo` bridge: `theme = "auto"` picked night and day by the configured window (checked for 23:00,
03:00, 06:59, 07:00 and 12:00, with the window crossing midnight), `?theme=` overrode the config, a `source` plus
prefix `repo` filter kept the matching runs and emptied the others, and the pixel shift walked the nine positions in
order and stayed at the centre when off. Not checked: how the night theme looks on a real OLED or in a dark room.

The links and scrolling were run in headless Chromium at 1280x720, 1920x1080 and 3840x2160 against the `--demo` bridge
and a hand-built snapshot: wheel scrolling in a long Open PRs list, the position kept across redraws and while a card is
held, the return to the top after 30 seconds, and a `javascript:` address staying plain text. Not checked: touch
scrolling on a real touchscreen, and the links of every source against a real server. The addresses are built from
documented URL patterns and covered by unit tests with sample data; see
[azure-devops-verify.md](azure-devops-verify.md) for what to click when you try Azure DevOps.

**Not tested on real Raspberry Pi hardware**: Chromium on Raspberry Pi OS, the `labwc` and LXDE autostart snippets,
Wayland and X11 behaviour, screen blanking, 4K at 30 or 60 Hz, memory use at 4K, the pointer hiding, and touch
input. The memory and refresh-rate remarks above are estimates from the design notes, not measurements. Not tested
at all: Firefox, Safari, and Chromium older than about version 90 (the page uses plain ES2015 and CSS grid, with
no build step, so older browsers are expected to work but nobody has tried).
