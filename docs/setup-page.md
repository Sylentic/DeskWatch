# The setup page

A page in the browser that shows how the bridge is set up: sources, their credentials, the MQTT broker, the data
folder, and whether every source works right now. Every source and the broker have a **Test connection** button.
With a [data folder](data-dir.md) it also adds, edits and removes sources, replaces tokens and changes the broker
settings (see below). Without one it only shows and tests.

It is **off by default**. Turn it on in `bridge.toml`, restart the bridge and open `http://<host>:8787/setup`:

```toml
[setup]
enabled = true
```

Until you set an administrator password ([auth.md](auth.md)) there is no login. Whoever can reach the port can use the
page, so it belongs on a network you trust (your home network). The bridge logs a warning at startup when the page is
reachable from the network without one. **Never expose the port to the internet** without the built-in login (and HTTPS). The page never
shows a secret (only whether a credential exists), token fields are write-only, and a page from another website cannot
make changes in your browser (the `Origin` check, below).

The page uses the whole screen: a menu on the left (**Sources**, **MQTT broker**, **Dashboard**, **Bridge and
restart**, **Stored secrets**) and the chosen section next to it, in several columns on a wide screen and one column on
a phone. **Add a source** is the first thing in the first section. The menu entry is kept in the address
(`/setup#mqtt`), so a link opens that section.

When the dashboard is on too, it shows a small **Setup** link in its header that opens this page, and this page has a
**Dashboard** link back.

The page is dark. The **Light mode** button in the header switches to light and remembers the choice in that
browser, for the dashboard too; the system's own light or dark setting is not followed.

## What it shows, and what it never shows

- Per source: type, address, repositories or projects, whether it comes from `bridge.toml` or the data folder
  ([data-dir.md](data-dir.md)), and its health (`working`, `token refused`, `not reachable`).
- Per credential: its **name** and whether it is `found` (mounted file or data folder) or `missing`. **Never the value,
  not even masked.** Nothing in the status response contains a secret, and a test guards that.
- The MQTT broker: address, TLS, login name, whether the password is set, and whether the bridge is connected.

## Test connection

The button makes the bridge itself try the connection, with the real credentials and the same client code as the real
source, and lists every check with a plain message. A test is read-only and makes a few cheap requests:

| Source | Checks |
| --- | --- |
| GitHub | the token can see each repository (up to 10); each `runners` scope, with a hint about the extra rights it needs |
| Gitea | the server answers; the token is accepted; each repository; each `runners` scope; the webhook secret exists (its value cannot be checked from here) |
| Azure DevOps | builds of each project; pull requests if enabled (Code: Read); each `agent_pools` entry exists (Agent Pools: Read) |
| Prometheus | the server answers a query; each host's `instance` is a known target |
| MQTT broker | connects and logs in (see below) |

A refused token says what to look at (a missing scope, an expired PAT). Messages are shortened and never contain a
credential. Only one test runs at a time and one takes at most 45 seconds.

The MQTT test uses its own client id (`<client_id>-setup-test`) so it does not kick the bridge off the broker, and sets
no Last Will. It proves that host, port, TLS and the login work; it does **not** check what the login may publish or
subscribe to, so an ACL mistake still shows up as a source of trouble in the broker log.

## Adding, editing and removing sources

With a data folder configured ([data-dir.md](data-dir.md)) the page can change sources that live in the data folder:

- **Add a source** (GitHub, Gitea, Azure DevOps, Prometheus), **Edit** it, **Remove** it.
- A token field is write-only. Leave it empty on an edit to keep the current token; fill it in to replace it. The value
  goes to the bridge once and is stored as `credentials/<kind>-<name>-<key>` (0600). It is never sent back.
- **Validate before save**: the whole configuration, as it would be, must load, and the connection test runs with the
  new values. If the test fails nothing is written and the failing checks are shown; **Save anyway** writes it
  regardless (for a server that is down right now).
- Settings the page does not know (hand-added keys in `sources.toml`) are kept on an edit.
- Sources from `bridge.toml` are shown but cannot be edited or deleted from the page, because the page never writes
  `bridge.toml` (it can be a read-only mount, and it is yours). Instead they get **Switch off here**: the source
  stops, leaves the dashboard and the panel, and its id is listed under `hidden = [...]` in `sources.toml`. Its
  token file belongs to `bridge.toml` and stays. **Switch on again** undoes it. To delete such a source for good,
  remove its block from `bridge.toml` and restart. A mounted credential with the same name
  as a page-managed one is refused, because the mounted one would win and hide the change.
- **Everything that deletes asks first**, in a box on the page (not a browser dialog): removing a source, switching
  one off, removing the broker login, resetting the broker settings, deleting a stored secret and restarting.
- Every save or removal is logged (source id, which secret files were written; never a value).
- **No restart needed.** A saved source starts at once, an edited one is replaced (its facts and warnings are dropped
  first), a removed one stops and disappears from the dashboard, the panel and the `warn` badge. A new Gitea source
  gets its webhook route (`/webhook/gitea/<name>`) immediately, and a removed one answers 404. If a saved source
  cannot be started (for example a credential file vanished), the old version keeps running, the page says why, and a
  restart will show the same error. Settings outside `[[source.*]]` (the MQTT broker, the listen address, the rotation)
  are still read at startup.

## Dashboard hosts

With a data folder, the **Dashboard hosts** card chooses how the built-in dashboard layout shows the hosts:
**detailed** (one host in detail on top, every host as a list below, tap a host to switch) or
**compact** (only the list of hosts, with their network rates). It is stored as `host_view` in a `[kiosk]` table in `sources.toml` and
applies after a restart, like the broker settings. Panels set with `[[kiosk.panel]]` in `bridge.toml` are not
touched. See [kiosk.md](kiosk.md#hosts-detailed-or-compact).

## Alerts

The **Alerts** section has three cards.

- **Alerts in the header** (needs a data folder): show the latest alert pill or not, whether
  "via <source>" is shown, the most alerts kept (1 to 50) and whether the Alerts menu on the dashboard has Dismiss buttons. They are stored
  under `[alerts]` in `sources.toml` (`ticker`, `show_source`, `max_active`, `dismiss`) and apply
  after a restart. `enabled` and `critical_ids` stay in `bridge.toml`.
  The same card has **Accept alerts on the web address** (`http_input`): `POST /api/alert` with the MQTT JSON, off by
  default, see [home-assistant.md](home-assistant.md#without-mqtt-the-web-address).
- **Try it**: **Send test alert** raises one fixed warning ("Test alert", source "setup") that goes away by itself after a
  minute, and **Clear it now** removes it. The page sends no text of its own, only the action `send` or `clear`, so there
  is no way to create any other alert from here (without a password the setup page has no login). It needs no data folder.
- **Where alerts come from**: whether MQTT is on and connected. Alerts arrive on the MQTT topic `deskpanel/alert`
  ([home-assistant.md](home-assistant.md)). Failed pipeline runs are not alerts; they count in the Failed badge.

## MQTT broker settings

With a data folder, **Change broker settings** edits `enabled`, `host`, `port`, `tls`, the login name and the
password. It works like a source: **Test and save** tries the connection with the new values first (the same test as
the Test connection button) and writes nothing if it fails, unless you press **Save anyway**.

- The settings go in an `[mqtt]` table in `sources.toml` and are applied **on top of** `bridge.toml`: only the keys
  the page sets are replaced. Everything else (`client_id`, `topic_prefix`, `ca_file`, client certificates) keeps
  coming from `bridge.toml`, and the page does not edit those. The page shows where the settings come from.
- The password is stored as the credential `mqtt-password` (0600) in the data folder and is never sent back. Leave the
  field empty to keep the saved one. A mounted credential with that name would win over it, so the page refuses to
  save a password while one exists. A login name that is emptied switches the login off.
- **The MQTT connection is made once at startup, so a saved change needs a restart.** The page says so. Restart the
  bridge yourself (`docker compose restart`, `systemctl restart deskwatch-bridge`), or turn on the button below.
- The test does not check what the login may publish or subscribe to (see Test connection).

### Removing the broker login or the saved settings

On the **MQTT broker** section, **Remove login** clears the login name and deletes the saved password;
**Reset to bridge.toml** removes everything the page saved for the broker (host, port, TLS, login, password), so
`bridge.toml` decides again. A password file that is not the page's own (`mqtt-password`) is never deleted. Both
apply after a restart.

## Stored secrets

The **Stored secrets** section lists the files in the data folder's `credentials/` with what uses each one (a source,
the MQTT login, or nothing). Values are never shown. Removing a source or the login deletes its secrets; a secret
that nothing uses any more (left over from a hand edit or an older version) can be deleted here. One that is in use
is refused.

## Restart

The **Bridge and restart** section has a **Restart bridge** button, and the banner after a change that needs a
restart has a **Restart now** button. Both ask for a confirm click, then the bridge shuts down cleanly (it tells the
broker it is going offline) and exits with code 75. The page waits until a new process answers (the status carries a
`boot` id that differs after a start) and carries on by itself; after a minute without an answer it says that nothing
started the bridge again.

**What it relies on.** The bridge only exits; something else must start it again: Docker with `restart:
unless-stopped` (or `always`, or `on-failure`; the compose files in `deploy/` have `unless-stopped`) or the systemd
unit in `deploy/deskwatch-bridge.service` (`Restart=on-failure`). A container without a restart policy (Docker's
default is `no`) or a bridge started by hand just stops. **What it cannot do:** restart the Docker host, pull a newer
image, or change the compose file; for an update you still run `docker compose pull && docker compose up -d`.

**It is opt-in.** `[setup] allow_restart` is off by default because it stops the bridge. While it is off, the page
shows a disabled button and says how to turn it on: add `allow_restart = true` under `[setup]` in `bridge.toml` and
restart once by hand (`docker compose restart deskwatch-bridge`). It needs the same origin like every change.

## Not yet

- **Setup on the ESP panel itself** (a setup-mode access point with its own page, for the bridgeless variant) is not
  built. It waits for the screen and the firmware work on real hardware.
- The kiosk layout, the rotation and the listen address stay in `bridge.toml`; the page does not edit them.
- Everything here is tested against mock servers and a fake broker, not against a real GitHub, Azure DevOps or
  Mosquitto.

## Security

- **Local network only by default.** The page and its `/api/setup/*` routes answer only loopback, private
  (10/8, 172.16/12, 192.168/16), link-local, IPv6 unique-local and the Tailscale/CGNAT range (100.64/10) addresses; any
  other client gets a `403`. Behind a reverse proxy the client is the last `X-Forwarded-For` entry (the one the proxy
  added), so a public visitor behind a LAN proxy is refused too. `[http] allow_public = true` lifts this; do that only
  with TLS and a proxy that has a login. The bridge warns at startup when it is on without TLS.
- **A login is available** ([auth.md](auth.md)): set an administrator password under **Access** and the page, the
  dashboard and every change need a sign-in; the address rule above then steps aside. Until you do, there is no login.
- **No login until you set a password (default).** Anyone who can reach the port from the local network can open the page and, with a data folder, change sources and store
  tokens. Keep the port on a trusted network. A website you have open in your browser cannot use the page for you:
  requests whose `Origin` is another site are refused.
- Secrets never reach the browser, whatever the network.
- **Removed: `[setup] token_file`.** A config that still sets it is refused at startup with a message that names the
  line to delete. Use the built-in login ([auth.md](auth.md)) or a proxy with one instead.
- The connection is plain HTTP unless you turn on HTTPS, in the bridge or with a TLS reverse proxy in front ([https.md](https.md)). The
  bridge logs a warning when the page is reachable from the network. Until you have TLS, keep the listener on
  `127.0.0.1` or on a network you trust.
- The page is built with `textContent` only, so nothing from the config can inject markup, and it loads nothing from
  other sites (CSP).

## Open points

- Authentication for the ESP panel's own setup mode (step 8 of the plan) is still to be worked out when that option
  exists.
