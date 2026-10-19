# Development: running from source, the demo, the simulator and the firmware

For installing DeskWatch, start with [install.md](install.md), [docker.md](docker.md) or [windows.md](windows.md). This
page is for running it from a checkout and for working on it.

## Running the bridge by hand

Needs a stable Rust toolchain (1.88 or newer) and a Mosquitto broker (not for the kiosk dashboard alone: set
`[mqtt] enabled = false`).

```sh
cargo run -p deskwatch-bridge -- bridge/config.example.toml
mosquitto_sub -t 'deskpanel/#' -v   # in another terminal
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
Assistant: stats, open PRs, pipelines and alerts pages, notices, a running deploy with a second job, a failure cleared by
the button, a green success flash, and an amber and a red home alert. No sources run and the `[[rotation]]` blocks are
ignored; the rest of the config (`[mqtt]`, `[alerts]`, `[kiosk]`, ...) applies, and the config file is optional.

```sh
cargo run -p deskwatch-bridge -- --demo --kiosk --no-mqtt   # the dashboard on http://localhost:8787/
cargo run -p deskwatch-bridge -- --demo                     # for the simulator: broker on localhost
cargo run -p deskwatch-sim -- --host localhost
```

Pressing the simulator's button during the demo works as on the real panel. `--demo`, `--kiosk` and `--no-mqtt` can
also be set with `DESKWATCH_DEMO=1`, `DESKWATCH_KIOSK=1` and `DESKWATCH_NO_MQTT=1`.

## The dashboard page

The kiosk page is plain HTML, CSS and JavaScript in `bridge/kiosk/` (no build step), built into the bridge with
`include_str!`. The setup page is in `bridge/setup/`, the sign-in page in `bridge/auth/`. All three share the typefaces
in `bridge/fonts/`, served on `/fonts/`. After a change, rebuild the bridge and reload the page; `?static` on the address
stops all network use, and `DeskWatchKiosk.render(snapshot)` draws a snapshot of your own.

## Panel UI and simulator

The panel's drawing code lives in `ui/` (`deskwatch-ui`), a `no_std` crate that parses the MQTT payloads and draws the
six page templates (stats, job, alert, list, number, notice) plus the header badges and footer on a 480x320 landscape
screen. The firmware will use it unchanged; until the screen arrives, `sim/` (`deskwatch-sim`) draws the same pixels in a
desktop window.

The simulator needs SDL2 (`sudo apt install libsdl2-dev` on Debian or Ubuntu). On Windows SDL2 is built in, see
[windows.md](windows.md).

```sh
# Live: subscribe to the bridge's topics; click is the button (hold for a long press)
cargo run -p deskwatch-sim -- --host localhost

# Flip through the example payloads, no broker needed
cargo run -p deskwatch-sim -- preview ui/testdata/screens --badges ui/testdata/badges/default.json

# Headless PNG of one payload (also builds without SDL: --no-default-features)
cargo run -p deskwatch-sim -- render ui/testdata/screens/job_deploy.json -o job.png
```

You can also drive the live window by hand, before the bridge produces a page:

```sh
mosquitto_pub -r -t deskpanel/screen -f ui/testdata/screens/alert_failed.json
```

`ui/testdata/screens/` has one example payload per template and edge case. `cargo test -p deskwatch-ui` renders each
one and compares it with the approved image in `ui/tests/snapshots/`; after an intended layout change, re-approve with
`UPDATE_SNAPSHOTS=1 cargo test -p deskwatch-ui` and review the PNG diff in the PR.

## Firmware and the TLS spike

The panel firmware lives in [`firmware/`](../firmware/README.md). It is Rust (esp-hal, embassy), outside the Cargo
workspace because it needs the Xtensa toolchain, and has its own CI job. It builds and lints in CI but has not run on a
board yet; 1.0 waits for that.

Next to it is a **TLS spike**, a test binary that needs only the ESP32-S3 board, USB and Wi-Fi (no screen, button or
broker). It sets the clock over SNTP, makes HTTPS requests with full certificate verification and prints handshake
time, memory use and a result block over serial. Its purpose is to measure whether a standalone ESP that talks to HTTPS
APIs directly is realistic, before that option is built. It is also untested on hardware. To run it:

```sh
cd firmware
cp config.example.toml config.toml     # Wi-Fi settings; the [tls_spike] table is optional
cargo run --release --features tls-spike --bin tls-spike
```

Setup (Xtensa toolchain, `cmake` and `ninja`, Windows and Linux specifics), how to read the output and a results
template are in [firmware/README.md](../firmware/README.md#tls-spike).

## Checks before a pull request

The same as CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

How a release is made is in [releasing.md](releasing.md).
