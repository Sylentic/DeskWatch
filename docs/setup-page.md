# The setup page

A page in the browser that shows how the bridge is set up: sources, their credentials, the MQTT broker, the data
folder, and whether every source works right now. Every source and the broker have a **Test connection** button.
**It does not change anything yet.** Adding and editing sources and replacing tokens come in later releases.

It is **off by default**. Turn it on by giving it its own token:

```toml
[setup]
token_file = "setup-token"      # a credential, like kiosk-token
```

```sh
openssl rand -hex 24 > credentials/setup-token      # not a CI token; chmod so the bridge user can read it
```

Restart the bridge and open `http://<host>:8787/setup`. The page asks for the token once and keeps a login cookie for
12 hours (`HttpOnly`, `SameSite=Strict`, `Secure` behind an HTTPS proxy that sends `X-Forwarded-Proto`).

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

## Security

- **Its own token**, separate from the dashboard's. A dashboard login is useless here even with the same token text.
- **`kiosk.trusted_networks` never applies.** The peer address is not reliable enough (Docker, reverse proxies) to
  decide who may see credentials and, later, change them.
- Wrong tokens are delayed, and five in a minute pause logins for a minute for everybody. Requests whose `Origin` is
  another site are refused.
- The connection is plain HTTP unless you put a TLS reverse proxy in front ([docker.md](docker.md) has examples). The
  bridge logs a warning when the page is reachable from the network. Until you have TLS, keep the listener on
  `127.0.0.1` or on a network you trust.
- The page is built with `textContent` only, so nothing from the config can inject markup, and it loads nothing from
  other sites (CSP).
