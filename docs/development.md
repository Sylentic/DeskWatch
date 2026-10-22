# Development: running from source and the demo

For installing DeskWatch, start with [install.md](install.md), [docker.md](docker.md) or [windows.md](windows.md). This
page is for running it from a checkout and for working on it.

## Running the bridge by hand

Needs a stable Rust toolchain (1.88 or newer) and a Mosquitto broker (not for the kiosk dashboard alone: set
`[mqtt] enabled = false`).

```sh
cargo run -p deskwatch-bridge -- bridge/config.example.toml
mosquitto_sub -t 'deskpanel/#' -v   # in another terminal: alerts and the bridge status
```

The config path can also come from `DESKWATCH_CONFIG`; the default is `/etc/deskwatch/bridge.toml`
(`%ProgramData%\DeskWatch\bridge.toml` on Windows). On a Linux server run it as a service, see
[deploy/deskwatch-bridge.service](../deploy/deskwatch-bridge.service). The example config has a Gitea block switched on;
remove it if you have no Gitea.

Secrets never go in the config. Keys ending in `_file` name a credential: a plain name such as `gitea-token` is read
from `$CREDENTIALS_DIRECTORY`, which systemd fills from the unit's `LoadCredential=` lines (on Windows it is
`%ProgramData%\DeskWatch\credentials`); an absolute path is read directly, which is handy when running by hand. The MQTT
password can also come from `DESKWATCH_MQTT_PASSWORD`.

## Demo mode

`--demo` plays a loop of fake data of a little over two minutes, so you can watch every page without Gitea or Home
Assistant: stats, open PRs, pipelines and alerts, notices, a running deploy with a second job, a failure, a green
success flash, and a yellow and a red home alert. No sources run; the rest of the config (`[mqtt]`, `[alerts]`, `[kiosk]`, ...) applies, and the config file is optional.

```sh
cargo run -p deskwatch-bridge -- --demo --kiosk --no-mqtt   # the dashboard on http://localhost:8787/
cargo run -p deskwatch-bridge -- --demo --kiosk             # the same, with MQTT alerts on a broker on localhost
```

`--demo`, `--kiosk` and `--no-mqtt` can
also be set with `DESKWATCH_DEMO=1`, `DESKWATCH_KIOSK=1` and `DESKWATCH_NO_MQTT=1`.

## The dashboard page

The kiosk page is plain HTML, CSS and JavaScript in `bridge/kiosk/` (no build step), built into the bridge with
`include_str!`. The setup page is in `bridge/setup/`, the sign-in page in `bridge/auth/`. All three share the typefaces
in `bridge/fonts/`, served on `/fonts/`. After a change, rebuild the bridge and reload the page; `?static` on the address
stops all network use, and `DeskWatchKiosk.render(snapshot)` draws a snapshot of your own.

## Branches and pull requests

Start every branch from `dev` and open the pull request into `dev` (GitHub suggests `main`, the default branch, so pick
`dev` as the base). `main` only takes a pull request from `dev`, once the work is tested there (see
[releasing.md](releasing.md)); a pull request from any other branch into `main` fails the `from dev` check. Neither
branch takes a direct push.

To test what is on `dev` before it goes to `main`, every merge into `dev` publishes the image
`ghcr.io/sylentic/deskwatch-bridge:dev`; [docker.md](docker.md#the-dev-image-try-what-is-not-released-yet) shows how
to run it next to the released one.

## Dependency updates

Dependabot (`.github/dependabot.yml`) opens one pull request a week per ecosystem (Cargo, GitHub Actions, the
Dockerfile), always into `dev`, so the pipeline checks every update first. They are never merged automatically. The
Rust version of the build image is raised by hand together with `rust-version` in `Cargo.toml`.

## Checks before a pull request

The same as the pipeline (`.github/workflows/pipeline.yml`):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

How a release is made is in [releasing.md](releasing.md).
