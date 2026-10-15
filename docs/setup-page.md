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

There is no login. Whoever can reach the port can use the page, so it belongs on a network you trust (your home
network). The bridge logs a warning at startup when the page is reachable from the network. **Never expose the port to
the internet**, and do not publish it through a reverse proxy or tunnel unless that has its own login. The page never
shows a secret (only whether a credential exists), token fields are write-only, and a page from another website cannot
make changes in your browser (the `Origin` check, below).

When the dashboard is on too, it shows a small **Setup** link in its header that opens this page, and this page has a
**Dashboard** link back.

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
- Sources from `bridge.toml` are shown but not editable; change those by hand. A mounted credential with the same name
  as a page-managed one is refused, because the mounted one would win and hide the change.
- Every save or removal is logged (source id, which secret files were written; never a value).
- **No restart needed.** A saved source starts at once, an edited one is replaced (its facts and warnings are dropped
  first), a removed one stops and disappears from the dashboard, the panel and the `warn` badge. A new Gitea source
  gets its webhook route (`/webhook/gitea/<name>`) immediately, and a removed one answers 404. If a saved source
  cannot be started (for example a credential file vanished), the old version keeps running, the page says why, and a
  restart will show the same error. Settings outside `[[source.*]]` (the MQTT broker, the listen address, the rotation)
  are still read at startup.

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

### Restart now

With `[setup] allow_restart = true` the banner gets a **Restart now** button. The bridge then shuts down cleanly (it
tells the broker it is going offline) and exits with code 75. That only helps when something starts it again:
Docker with `restart: unless-stopped` (the compose files in `deploy/` have it) or the systemd unit in
`deploy/deskwatch-bridge.service` (`Restart=on-failure`). A bridge started by hand just stops, so the button is off by
default. It needs the same origin like every change.

## Not yet

- **Setup on the ESP panel itself** (a setup-mode access point with its own page, for the bridgeless variant) is not
  built. It waits for the screen and the firmware work on real hardware.
- The kiosk layout, the rotation and the listen address stay in `bridge.toml`; the page does not edit them.
- Everything here is tested against mock servers and a fake broker, not against a real GitHub, Azure DevOps or
  Mosquitto.

## Security

- **No login.** Anyone who can reach the port can open the page and, with a data folder, change sources and store
  tokens. Keep the port on a trusted network. A website you have open in your browser cannot use the page for you:
  requests whose `Origin` is another site are refused.
- Secrets never reach the browser, whatever the network.
- **Removed: `[setup] token_file`.** A config that still sets it is refused at startup with a message that names the
  line to delete. The page has no login of its own; use a proxy with one if you need it.
- The connection is plain HTTP unless you put a TLS reverse proxy in front ([docker.md](docker.md) has examples). The
  bridge logs a warning when the page is reachable from the network. Until you have TLS, keep the listener on
  `127.0.0.1` or on a network you trust.
- The page is built with `textContent` only, so nothing from the config can inject markup, and it loads nothing from
  other sites (CSP).

## Open points

- Authentication for the ESP panel's own setup mode (step 8 of the plan) is still to be worked out when that option
  exists.
