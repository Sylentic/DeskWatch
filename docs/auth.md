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

## Sign in with GitHub or Microsoft

Optional, on top of the local password. Set it up on the setup page under **Sign-in providers** (needs the login to be
on, and a public address that starts with `https://`; plain `http://` only works for `localhost`). The settings go in
`oauth.toml` in the data folder (no secrets), the client secrets in `credentials/oauth-github` and
`credentials/oauth-microsoft`. The local admin account always keeps working, so a wrong rule never locks you out.

Nobody gets in by default: a person must match an allow rule. Roles are decided again at every sign-in, so removing
someone from a team takes effect at their next sign-in.

**GitHub.** Create an OAuth App (GitHub settings, Developer settings) with callback URL
`<public address>/auth/callback/github`. Paste the client id and secret. Allow individual users (by login), whole
organisations or teams (`org/team`), and name admin users or admin teams. People are recognised by their numeric id, so
a renamed account stays the same person. Organisations and teams are only visible when the organisation approved the
OAuth App; personal user rules work without that.

**Microsoft.** Register an app in Entra ID with redirect URI `<public address>/auth/callback/microsoft` (type Web), add a
client secret, and paste tenant id, client id and secret. Allow by group object id and name admin groups. Only work or
school accounts have groups. People are recognised by tenant id plus object id, never by e-mail.

Limits: no real-provider test has been run yet (the tests use mock providers); organisations and teams are read in pages
of 100; a user in more than 20 matching Microsoft groups is checked in batches.

## Forgot the password

No email. On the host: `deskwatch-bridge auth reset /etc/deskwatch/bridge.toml`, restart the bridge, and the first-run form
returns. Whoever can run that or reach the data volume owns the install, which is the right boundary. Keys stay.

## Not built yet

Several named accounts, and an audit page. See the design notes in the project files.
