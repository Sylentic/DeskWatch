# The setup page

A page in the browser that shows how the bridge is set up: sources, their credentials, the MQTT broker, the data
folder, and whether every source works right now. **This version is read-only.** Adding and editing sources, replacing
tokens and a "test connection" button come in later releases; the plan is in the project's setup-page advice.

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
