# The login

DeskWatch has a built-in login. It is **off until you set an administrator password**, so an existing install keeps
working exactly as before (the setup page and `/api/alert` still only answer the local network, see
[setup-page.md](setup-page.md#security)). Once a password exists, everything needs a sign-in: the dashboard, its live
WebSocket, the setup page and every change. Webhooks keep their own signature check and `/healthz` stays open.

It needs the [data folder](data-dir.md), because the password hash is saved there (`auth.toml`, mode 0600).

## Turn it on

Open the page from the local network and go to **/login** (or Setup, **Access**): the first-run form asks for a user name
and a password of at least 12 characters (a few words are fine). Only a client on the local network may do this first
step. On a server without a browser, do it on the host instead:

```sh
printf '%s\n' 'a long password' | deskwatch-bridge auth set-password --user admin /etc/deskwatch/bridge.toml
```

In Docker: `docker compose exec deskwatch deskwatch-bridge auth set-password ...` (restart the container afterwards).

## Roles

- **admin**: everything.
- **viewer** (optional): the dashboard only, no Setup, no dismiss or mute. Set its password under Setup, Access; the
  user name is always `viewer`.

## Scripts and screens use keys, not passwords

Setup, Access, **API keys** creates a key (shown once, only its hash is stored).

- An **alert** key lets a script `POST /api/alert` with `Authorization: Bearer <key>` (or `X-API-Key: <key>`). Home
  Assistant: put that header in the `rest_command` ([home-assistant.md](home-assistant.md)).
- A **viewer** key lets a screen open the dashboard once: `https://host/?key=<key>` signs the browser in (a viewer
  session for seven days) and redirects to the page without the key. Use it for the Raspberry Pi kiosk URL
  (`DESKWATCH_KIOSK_URL`, [kiosk.md](kiosk.md)).

## Sessions and safety

- A session is a random 256-bit id in a cookie (`HttpOnly`, `SameSite=Lax`, `Secure` over HTTPS or behind a proxy
  that sends `X-Forwarded-Proto: https`). Sessions live in memory: a restart signs everyone out. They end after seven
  idle days, with **Sign out**, or with **Sign out everywhere**.
- Every change needs the header `X-DeskWatch: 1` (the pages add it) besides the existing `Origin` check, so a page on
  another site cannot act for you.
- Five wrong passwords make that address wait five minutes (then ten, up to an hour). A wrong user name answers like a
  wrong password.
- Passwords are Argon2id hashes. Sign-ins and failures are logged with the address, never the password.
- **Use HTTPS** before anyone outside a trusted network signs in ([https.md](https.md)): the login itself works over HTTP,
  but then the password and cookie cross the network in the clear. The bridge warns about it at startup.
- `[http] allow_public = true` (setup page and alerts reachable from any address) now **requires** a password: without one
  the bridge refuses to start. With a password set the address check steps aside, because the login protects the pages.

## Forgot the password

No email. On the host: `deskwatch-bridge auth reset /etc/deskwatch/bridge.toml`, restart the bridge, and the first-run form
returns. Whoever can run that or reach the data volume owns the install, which is the right boundary. Keys stay.

## Not built yet

OAuth sign-in with GitHub or Microsoft, several named accounts, and an audit page. See the design notes in the project files.
