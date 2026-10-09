# DeskWatch panel firmware

Rust firmware for the panel: an ESP32-S3 SuperMini with a 4" ST7796S SPI display. It is a dumb MQTT client. It joins
Wi-Fi, connects to the broker, subscribes to the bridge's retained topics (`screen`, `badges`, `bridge/status`) and
draws whatever arrives with the shared [`ui/`](../ui) crate. The contract is [docs/mqtt-schema.md](../docs/mqtt-schema.md),
the wiring is Part 6 of the project's wiring notes (pins below).

Stack (decided, see the firmware stack notes in the project files): `esp-hal` 1.1 + `esp-rtos` (embassy), `esp-radio`
Wi-Fi, `embassy-net`, `rust-mqtt` (MQTT 5), `mipidsi`, `embedded-graphics`.

## Status: stage 1, not yet run on hardware

The display has not arrived, so none of this has run on a board. It builds for `xtensa-esp32s3-none-elf`, passes
clippy, and CI builds it. Nothing below is verified on hardware:

| Part | State |
|---|---|
| Builds for the ESP32-S3, clippy clean | Done, checked in CI |
| Compile-time config from `config.toml` | Done, host-side build script only |
| Wi-Fi station, DHCP (`src/net.rs`) | Written, **untested** |
| MQTT connect, Last Will, subscribe, keep-alive, reconnect (`src/mqtt.rs`) | Written, **untested** (not run against a broker either) |
| Serial log of every received message | Written, **untested** |
| Display init and drawing (`src/display.rs`) | Written, **untested**. Orientation, colour order, inversion and SPI speed are guesses, see `[display]` in the config |
| Button, touch, LED, backlight dimming, SNTP clock | Not started |
| Setup mode (stage 2) and flash storage | Designed below, not started |
| OTA updates | Last, as planned |

Things to know before the first boot: the whole screen is cleared and redrawn on every change, so expect flicker until
partial redraws land; SPI runs blocking, without DMA; the clock is unknown, so job elapsed times are left out.

## What the panel stores, and what it does not

The panel only holds:

- the Wi-Fi network name and password,
- its own MQTT login (`deskwatch-panel`), which the broker limits with an ACL to reading `screen`, `badges` and
  `bridge/status` and writing `panel/status` and `panel/event` ([docs/mosquitto.md](../docs/mosquitto.md)).

CI tokens (Gitea, GitHub, Azure DevOps) stay in the bridge and never reach the panel. A stolen panel can therefore
see what is on the screen and press its own button, and nothing more: it cannot raise alerts and it has no token to
leak. If one is lost, change its Mosquitto password and the Wi-Fi password.

## Building

The firmware is its own Cargo project (not part of the root workspace, so `cargo test --workspace` stays
toolchain-free). It needs the Xtensa fork of Rust, because rustc upstream has no ESP32-S3 target.

```sh
cargo install espup espflash
espup install --targets esp32s3      # installs the "esp" toolchain; follow its note about the export file
cd firmware
cp config.example.toml config.toml   # then edit it, see below
cargo build --release
```

`rust-toolchain.toml` selects the `esp` toolchain inside this folder. The first build compiles `core` and `alloc`
itself (`build-std`), so it takes a few minutes.

To flash the board over its native USB port and open the log:

```sh
cargo run --release
```

If the board does not show up, hold the BOOT button while plugging it in to enter the download mode.

## Settings (stage 1: compiled in)

Copy `config.example.toml` to `config.toml` and fill it in. `config.toml` is gitignored; nothing secret is ever in the
repository. Without it the build falls back to the example file and the panel logs that it is not configured and
stops. The values:

| Key | Meaning |
|---|---|
| `wifi.ssid`, `wifi.password` | The network (2.4 GHz) |
| `mqtt.host` | Broker IPv4 address or a name your DNS resolves. Use the broker's address on your own network |
| `mqtt.port` | The plain listener, 1883 by default. **TLS is not supported yet**, so use a LAN-only listener with a login |
| `mqtt.username`, `mqtt.password` | The `deskwatch-panel` login. Leave empty only while the broker allows anonymous clients |
| `mqtt.topic_prefix` | Same as the bridge, `deskpanel` by default |
| `display.bgr`, `display.invert_colors`, `display.spi_mhz` | Screen quirks to settle on the real module |

Changing a value means rebuilding and flashing. That is why stage 2 exists.

## Settings (stage 2: stored in flash, set through a setup mode)

Not built yet. The code is already shaped for it: everything outside `main` receives a `Config` from a `ConfigSource`
(`src/config.rs`). Stage 1 has one source, `CompiledConfig`; stage 2 adds `FlashConfig` and a setup mode without
touching the Wi-Fi, MQTT or display code.

The plan, in short:

1. **Storage.** The settings live in a dedicated flash partition (NVS style, key/value) next to the app, so an OTA
   update or a reflash of the app does not wipe them. The MQTT password is stored there as is; ESP32 flash
   encryption is possible but off by default, so treat a lost panel as a lost password, as above.
2. **When setup mode starts.** Only when nothing is stored yet, or when the button (GPIO4) is held while powering on, also on an already configured panel (see "Changing settings later").
   Otherwise the panel boots straight into normal operation and **no web server ever runs**.
3. **What it does.** The panel starts its own Wi-Fi access point (name `deskwatch-setup-XXXX` from the chip's MAC,
   with a random WPA2 password drawn on the screen so only someone looking at the panel can join) and serves one
   small page on a fixed address: Wi-Fi name and password, broker host and port, panel login, topic prefix. It also
   scans for networks so the name can be picked from a list.
4. **Finishing.** Saving writes the settings to flash and restarts the panel into normal operation. A timeout (about 10
   minutes) with nothing saved also restarts it, so a forgotten setup mode closes by itself. If the stored Wi-Fi
   cannot be joined for several minutes, the panel falls back to setup mode too.
5. **Compile-time config stays** as a development fallback that wins over flash when present, so the stage 1 workflow
   keeps working.

### Changing settings later without wiping anything

A configured panel can be reconfigured at any time, with no erase and no reflash:

- **Re-enter setup mode:** hold the button (GPIO4) while powering on or pressing reset. Setup mode starts even
  though settings are stored. Release the button after the setup network name appears on the screen.
- **Overwrite single values:** the setup page is pre-filled with the stored values (the passwords show as blank
  with "keep current"). Only the fields you change are written; everything else stays as it was. A new Wi-Fi
  network, a new Mosquitto password or a new broker address is one visit to the page.
- **Wipe is a separate, deliberate action:** a "forget all settings" button on the same page, plus a second
  hold-at-boot with the button held for about 10 seconds. Neither is needed for ordinary changes.
- **Cancelling is safe:** leaving setup mode without saving (or the timeout) keeps the old settings untouched.
- **Safety net:** if new Wi-Fi settings fail to connect, the panel returns to setup mode instead of staying dead.

CI and DevOps tokens are not on the panel at all, so a PAT that expires or a change of employer never touches the
firmware. Swap the PAT in the bridge's credential file and restart the bridge, see
[docs/azure-devops.md](../docs/azure-devops.md). The panel needs no change and keeps showing the `warn` badge until
the bridge can log in again.

Open points for that PR: whether to use `esp-storage` with `sequential-storage` or the ESP-IDF NVS partition format
(the latter would let `esptool` or `espflash` pre-load settings from a CSV), and whether to show the setup password
as a QR code.

## Layout

| File | Job |
|---|---|
| `src/main.rs` | Start-up: heap, scheduler, config, tasks |
| `src/config.rs` | `Config`, the `ConfigSource` trait and the compile-time source |
| `build.rs` | Turns `config.toml` into constants, passes linker arguments |
| `src/net.rs` | Wi-Fi station, reconnect loop, embassy-net with DHCP |
| `src/mqtt.rs` | Broker session, Last Will, subscriptions, keep-alive, reconnect |
| `src/display.rs` | ST7796S setup on SPI2 and the task that feeds messages to `deskwatch_ui::Panel` and draws |

## Pins

From Part 6 of the wiring notes (display and touch share SPI2; touch is not wired into the firmware yet):

| GPIO | Use |
|---|---|
| 12 | SPI clock |
| 11 | SPI MOSI |
| 13 | SPI MISO (touch only) |
| 10 | Display chip select |
| 9 | Display data/command |
| 8 | Display reset |
| 7 | Backlight (plain on/off for now, PWM later) |
| 6 | Touch chip select (unused so far) |
| 2 | Touch interrupt (unused so far) |
| 4 | Button (unused so far) |

## Next steps once the screen is here

1. Flash, confirm the serial log shows the chip, Wi-Fi joining and the broker connection.
2. Settle `display.bgr`, `display.invert_colors` and the SPI speed on the real screen.
3. Publish a test screen from the bridge's `--demo` mode and check the page changes within a second.
4. Button on GPIO4 (publishes `short` and `long` to `panel/event`), then partial redraws and DMA, then touch, the
   status LED, SNTP, TLS, stage 2 settings, and OTA last.
