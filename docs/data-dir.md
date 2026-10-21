# The data folder

By default the bridge only reads: `bridge.toml` and the credential files you mount. The **data folder** is an optional,
writable folder where the bridge keeps files it writes itself. It is where the [setup page](setup-page.md) saves what you add or change in the browser (sources, tokens, broker
settings); it also works by hand. Without it nothing changes.

Turn it on with `[data] dir = "/var/lib/deskwatch"` in `bridge.toml`, or the `DESKWATCH_DATA_DIR` environment
variable. The bridge creates the folder and stops at startup, with the reason, if it cannot write to it.

```text
/var/lib/deskwatch/
  sources.toml          [[source.*]] blocks, same format as in bridge.toml
  credentials/<name>    one secret per file, mode 0600, folder mode 0700
  auth.toml             login: password hashes and API key hashes, mode 0600 (auth.md)
  layouts.json          each signed-in user's own dashboard layout, mode 0600 (kiosk.md)
  oauth.toml            GitHub/Microsoft sign-in rules, no secrets (auth.md)
```

- **`sources.toml`** holds `[[source.gitea]]`, `[[source.github]]`, `[[source.gitlab]]`, `[[source.bitbucket]]`, `[[source.azure_devops]]` and
  `[[source.prometheus]]` blocks and nothing else. They are added to the ones in `bridge.toml` and checked together,
  so a name used twice for the same type is an error (nothing is replaced silently). `bridge.toml` itself is never
  rewritten, so your comments stay.
- **`credentials/<name>`** is found by a plain `token_file = "<name>"`, like the mounted credentials. If the same name
  exists in the mounted credentials folder, the mounted file wins.
- Files are written to a temporary file and renamed into place, so a crash never leaves half a secret.
- Secrets are write-only: the bridge never prints or serves one back.

Create a secret by hand the same way the bridge will:

```sh
(umask 077; printf %s 'THE-TOKEN' > /var/lib/deskwatch/credentials/github-extra)
```

## Docker

The image runs as uid 10001 with a read-only root. Mount a writable volume for the folder. A **named volume** is the
simplest: the image contains an empty `/var/lib/deskwatch` owned by uid 10001, and Docker gives a new, empty named
volume that owner, so nothing needs a `chown`:

```yaml
services:
  deskwatch:
    environment:
      DESKWATCH_DATA_DIR: /var/lib/deskwatch
    volumes:
      - data:/var/lib/deskwatch

volumes:
  data:
```

This is also the right choice under a stack manager that deploys from git (Komodo, Portainer): there `.` is the
manager's checkout of your repository, so a relative bind mount such as `./data` puts the data folder inside a git
working tree, where an update can wipe it or conflict with it. A named volume lives outside it. (A volume that already
existed and is owned by root is not changed. Fix it once with
`docker run --rm -v <volume>:/d busybox chown 10001:10001 /d`; this applies to a volume created with an image older than
0.9.14 too.)

A **bind mount** works as well, but then the host folder decides the owner and you make uid 10001 the owner yourself:

```yaml
    volumes:
      - ./dashboard/data:/var/lib/deskwatch      # sudo chown 10001 dashboard/data
```

(`deploy/docker-compose.dashboard.yml` has this as a commented example.) Back this folder up with the rest of your
config; it is the only state the bridge keeps. On a systemd install, give the unit `StateDirectory=deskwatch` and set
`dir = "/var/lib/deskwatch"`.

## The `[mqtt]` table

`sources.toml` can also hold an `[mqtt]` table with `enabled`, `host`, `port`, `tls`, `username` and `password_file`.
Those keys replace the ones from `bridge.toml` (an empty `username` means no login); every other `[mqtt]` setting stays
as `bridge.toml` has it. The setup page writes this table (see setup-page.md). The broker is connected once at
startup, so a change needs a restart.

## The `[kiosk]` table

`sources.toml` can also hold `[kiosk]` with `host_view = "detailed"` or `"compact"` (set on the setup page). It
replaces `[kiosk] host_view` from `bridge.toml`; nothing else of `[kiosk]` can be set here.

## The `hidden` list

`sources.toml` can also hold a top-level `hidden = ["github:work", "gitea:home"]`: sources from `bridge.toml`
(`type:name`) that the bridge leaves out. The setup page writes it with **Switch off here** and removes the entry with
**Switch on again**. `bridge.toml` and the token files it names are never touched. An id that matches no source is
ignored; an id that is not `type:name` makes the file invalid.

## Secrets the page stores

`credentials/<name>` files are written for sources and the broker password the page saved. Removing the source (or
the login) deletes its files; the page can also delete a file that nothing uses any more. A mounted credential of the
same name is never deleted by the page.

## Not covered yet

`sources.toml` is read at startup and whenever the setup page saves a change; a hand edit of the file is picked up at the
next save or restart. The listen address and the kiosk settings stay in `bridge.toml`.
