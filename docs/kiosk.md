# The kiosk dashboard (big screens)

The ESP panel shows one page at a time on a 4 inch screen. The **kiosk dashboard** is the other output: one web page
that shows everything at once, with no rotation, for a monitor or TV (a Raspberry Pi with a browser in kiosk mode, a
wall tablet, or any browser on your desk). It is built into the bridge, so there is nothing extra to install, and
it works on a LAN with no internet access (no CDN, fonts or scripts are loaded from elsewhere).

It reads the same facts as the ESP panel, so the two always agree, but it is its own design: the MQTT schema (v2)
and the panel do not change, and the page does not need a broker.

Contents: [1. What you get](#1-what-you-get) · [2. Turn it on](#2-turn-it-on) ·
[3. Choose the widgets](#3-choose-the-widgets) · [4. Autostart on a Raspberry Pi](#4-autostart-on-a-raspberry-pi) ·
[5. Security](#5-security) · [6. When something goes wrong](#6-when-something-goes-wrong) ·
[7. The data](#7-the-data-the-page-reads) · [8. What was and was not tested](#8-what-was-and-was-not-tested)

## 1. What you get

A grid of cards, 4 columns by 3 rows by default:

| Widget | Shows |
|---|---|
| `stats` | One host in detail: CPU, RAM, disk bars with warning colours, temperature, load, network rates, uptime, a CPU graph of the last few minutes |
| `hosts` | A compact table of every host (the bridge's own machine and every host a Prometheus source reports) |
| `jobs` | Jobs running now, with step, progress bar and a live elapsed time |
| `prs` | Open pull requests per repository, newest first, with the total |
| `pipelines` | The latest run of every pipeline: running and failed first, then the most recent |
| `alerts` | Active alerts (Home Assistant, scripts), most urgent first |
| `containers` | Containers and hosts that are down (needs the Prometheus source) |
| `health` | Whether every source is working, or why not (token refused, not reachable) |

Interrupts do not take over the whole screen as they do on the ESP panel, because the other widgets are still
useful. The bridge's normal priority decides (critical alert, running job, failed, warning, success, notice), and
the page shows the winner as a coloured **banner** under the header: red for a failure, deep red for a critical
alert, amber for a warning, green for a success, blue for a notice. While a job runs, a thin progress line under
the header follows it. The header also has the same counters as the panel badges (failed runs, open PRs, alerts,
things down), a connection chip, and a clock.

The size follows the screen width, so the same layout is readable on 1080p and 4K and on a small 1280x720 screen.
Under 900 px wide (a phone) it becomes a single scrolling column. The pointer is hidden on big screens.

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

`[[kiosk.panel]]` blocks place widgets on the grid, left to right and top to bottom, like the `[[rotation]]` list.
Without any, the default layout is used: `stats`, `hosts`, `jobs` (2 wide), `prs` (2 tall), `pipelines` (2 by 2),
`alerts`, `health`.

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
the screen allows: a 3x2 grid on a 4K TV gives very large cards. List widgets cut off what does not fit and the
last line says how many more there are. Configuration errors (a span larger than the grid, a `host` on a widget that
has none, an unknown widget) stop the bridge at startup with a message that names the block.

Layouts for day and night, drag and drop editing and per-widget filters are not built.

## 4. Autostart on a Raspberry Pi

Two parts: the bridge runs as in [raspberry-pi.md](raspberry-pi.md), and Chromium starts in kiosk mode when the Pi
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
`DESKWATCH_KIOSK_URL='http://bridge.example.lan:8787/?token=...' ~/deskwatch-kiosk.sh &` (the token is only needed when
the bridge has a [kiosk token](#5-security)). Older releases, or Bookworm switched to X11 with LXDE, read
`~/.config/lxsession/LXDE-pi/autostart` instead; see
[`deploy/kiosk/lxde-autostart`](../deploy/kiosk/lxde-autostart), which also turns the X11 screen blanking off.
If there is no checkout on the Pi, the script is short enough to copy from the repository page.

Things that make a kiosk last:

- **Screen blanking.** Turn it off in `sudo raspi-config` > Display Options > Screen Blanking (the menu name varies
  between releases), then check after an hour that the screen is still on. The LXDE file above covers X11.
- **Mouse pointer.** The page hides it over the page; `sudo apt install unclutter` is only needed if it shows up
  at the edges.
- **Chromium crash restore.** `--noerrdialogs --disable-session-crashed-bubble` keep the "restore pages" bubble
  away. A dedicated `--user-data-dir` keeps the kiosk's profile apart from yours.
- **Bridge restarts and upgrades need no action.** The page reconnects by itself, and after an upgrade it reloads
  itself once it sees a new version.
- **4K on a Pi 4.** The Pi 4 outputs 4K at 30 Hz unless you set `hdmi_enable_4kp60=1` in `/boot/firmware/config.txt`
  (it runs hotter). 30 Hz is plenty for this page, which only animates a progress bar and a small spinner.
- **Memory.** A Pi 4 with 2 GB or more, or a Pi 5, is the sensible floor for Chromium at 4K; use a 1080p output on
  anything smaller. A Pi Zero 2 W is not enough for a browser.
- **Burn-in.** The page is mostly static. On an OLED screen, switch the screen off at night
  (for example `wlr-randr --output HDMI-A-1 --off` from a cron job) rather than leaving it on for months.

## 5. Security

The page and its two data routes (`/api/kiosk` and `/api/kiosk/ws`) are read-only: the page cannot press buttons or
change anything. They show PR titles, pipeline names, host stats and alert text, so treat them like a dashboard.

- By default the listener is `0.0.0.0:8787` (it has to be reachable for Gitea webhooks). With the kiosk on and no
  token, anyone on your network can read the page, and the bridge logs a warning at startup. To keep it local, run
  the bridge on the Pi itself with `[http] listen = "127.0.0.1:8787"`, which is fine when the Pi has no webhook
  source.
- **Set a token** when the page is opened from another machine (or the listener is on the network): put a long random
  string in a credential file and name it, like every other secret:

  ```toml
  [kiosk]
  enabled = true
  token_file = "kiosk-token"     # a credential, loaded with LoadCredential= like the others
  ```

  ```sh
  openssl rand -hex 24 | sudo tee /etc/deskwatch/credentials/kiosk-token
  sudo chmod 600 /etc/deskwatch/credentials/kiosk-token
  ```

  Add `LoadCredential=kiosk-token:/etc/deskwatch/credentials/kiosk-token` to the unit, restart, and open
  `http://<host>:8787/?token=<the token>`. The page file itself has no data and loads without the token; the data
  routes answer 401 without it. Use hex or letters and digits for the token. The token is part of the page address
  (so it appears in the browser history and in your autostart file), and the connection is plain HTTP unless you put a
  reverse proxy with TLS in front, so the token keeps casual visitors out, not someone who can watch your network
  traffic.
- **In a container**, `0.0.0.0:8787` inside is correct and the published port decides who can reach the page:
  `"8787:8787"` is the whole network (keep the token), `"127.0.0.1:8787:8787"` is the Docker host only, for a reverse
  proxy that adds TLS and a login. Do not expose the port to the internet; use a VPN or an authenticating proxy.
  Details in [docker.md](docker.md#protecting-the-page).
- Names shown come from your sources after their `alias` setting. If a work project name should not appear on a
  visible screen, give it an alias, exactly as for the ESP panel. The page has no other redaction.
- The page sets a strict Content Security Policy: scripts and styles only from the bridge itself.

## 6. When something goes wrong

- **The bridge restarts or the network drops.** The page keeps the last data on screen, dims it, and shows
  "No data from the bridge for 0:42. Showing the last known state, reconnecting." in the banner row; the header chip
  turns red and says `offline`. It retries with a growing delay (up to 10 s) and also polls `/api/kiosk` in case a
  proxy blocks WebSockets, and carries on by itself when the bridge is back.
- **A source stops working** (expired token, service down). The `health` widget names it, the header chip says how
  many sources have a problem, and the widgets built from that source's data are dimmed with a "may be out of date"
  look. Hosts that Prometheus reports as down get a red dot and a dimmed card.
- **The bridge host's clock and the screen's clock differ.** Times such as "3 min ago" use the bridge's clock, so a
  Pi with a wrong clock still shows correct ages. The clock in the header is the screen's own.
- **A blank page or "connecting".** Open `http://<host>:8787/api/kiosk` in a browser: JSON means the bridge is fine
  and the problem is in the page or the browser; 401 means a token is needed; nothing means the bridge is not
  reachable (address, firewall, `[http] listen`). The bridge log shows `serving http://...` at startup.

## 7. The data the page reads

`GET /api/kiosk` returns the latest snapshot, and `/api/kiosk/ws` sends a snapshot over a WebSocket on every change
and at least every 5 seconds (a heartbeat, so the page can tell quiet from gone). Snapshots are versioned with `"v"`
(1), separately from the MQTT schema. Fields: `build` (bridge version; the page reloads when it changes), `now`
(Unix seconds), `layout` (the grid and the widgets from the config), `screen` (the composer's pick as an MQTT v2
screen payload, or `null` when idle), `badges`, `hosts`, `down`, `jobs`, `runs`, `pulls`, `alerts` and `sources`.
Lists are not capped to the ESP panel's five rows. The structure is in `bridge/src/kiosk.rs` (`snapshot`) and
covered by the tests next to it. Treat it as unstable until 1.0.

`?static` on the page address stops all network use; the page then waits for `DeskWatchKiosk.render(snapshot)`
to be called, which is how the screenshots and tests drive it.

## 8. What was and was not tested

Tested: the endpoints, the WebSocket (a real handshake and pushed changes), the token check, the config parsing and
the snapshot contents are covered by automated tests (`cargo test -p deskwatch-bridge`). The page was run in
Chromium against the live `--demo --kiosk` bridge and rendered at 1280x720, 1920x1080, 3840x2160 and a 420 px wide
phone, and was checked while the bridge was killed and started again: the page dimmed and said it was offline within 25 seconds, and recovered by itself within 14 seconds of the bridge returning. The reload on a new bridge version (`build` changing) is written but was not exercised.

**Not tested on real Raspberry Pi hardware**: Chromium on Raspberry Pi OS, the `labwc` and LXDE autostart snippets,
Wayland and X11 behaviour, screen blanking, 4K at 30 or 60 Hz, memory use at 4K, the pointer hiding, and touch
input. The memory and refresh-rate remarks above are estimates from the design notes, not measurements. Not tested
at all: Firefox, Safari, and Chromium older than about version 90 (the page uses plain ES2015 and CSS grid, with
no build step, so older browsers are expected to work but nobody has tried).
