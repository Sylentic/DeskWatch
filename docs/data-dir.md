# The data folder

By default the bridge only reads: `bridge.toml` and the credential files you mount. The **data folder** is an optional,
writable folder where the bridge keeps files it writes itself. It is the base for the setup page that is planned
(adding sources and replacing tokens in the browser); today it also works by hand. Without it nothing changes.

Turn it on with `[data] dir = "/var/lib/deskwatch"` in `bridge.toml`, or the `DESKWATCH_DATA_DIR` environment
variable. The bridge creates the folder and stops at startup, with the reason, if it cannot write to it.

```text
/var/lib/deskwatch/
  sources.toml          [[source.*]] blocks, same format as in bridge.toml
  credentials/<name>    one secret per file, mode 0600, folder mode 0700
```

- **`sources.toml`** holds `[[source.gitea]]`, `[[source.github]]`, `[[source.azure_devops]]` and
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

The image runs as uid 10001 with a read-only root. Mount a writable volume for the folder and make that user the
owner:

```yaml
services:
  deskwatch:
    environment:
      DESKWATCH_DATA_DIR: /var/lib/deskwatch
    volumes:
      - ./dashboard/data:/var/lib/deskwatch      # sudo chown 10001 dashboard/data
```

(`deploy/docker-compose.dashboard.yml` has this as a commented example.) Back this folder up with the rest of your
config; it is the only state the bridge keeps. On a systemd install, give the unit `StateDirectory=deskwatch` and set
`dir = "/var/lib/deskwatch"`.

## Not covered yet

Changes to `sources.toml` are read at startup, so edit and restart. Editing in the browser, testing a connection and
reloading without a restart come in later steps.
