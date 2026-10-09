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
| TLS spike binary (`tls-spike`, see below) | Written, builds and lints in CI, **untested** on hardware |
| Button, touch, LED, backlight dimming, SNTP clock in the panel | Not started |
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

## TLS spike

A separate binary, `tls-spike`, answers one question before the project commits to the standalone-ESP option
([docs/deployment-options.md](../docs/deployment-options.md) in the project files, if you have them): can this
ESP32-S3 make an HTTPS request with **full certificate verification**, how long does it take, and how much RAM and
flash does it need? It needs **no screen, no button and no MQTT broker**, only the board, a USB cable and Wi-Fi.

**Status: untested on hardware.** It compiles, passes clippy and builds in CI (feature `tls-spike`). Nothing in it has
run on a board yet: not the Wi-Fi join, the SNTP request, the hardware crypto setup, the handshake, or the numbers it
prints. Expect to fix something on the first run. The panel firmware is unaffected: without `--features tls-spike`
the TLS stack and the heap statistics are not built at all.

### What it does

1. Joins Wi-Fi with the same `config.toml` as the panel (`[wifi]`).
2. Sets the clock over SNTP. Certificates have start and end dates, so with no clock every certificate looks
   invalid. If the clock cannot be set, or the server answers with a date in the past, it logs `TIME NOT SET` or
   `TIME WRONG` in plain words.
3. Parses the bundled root certificates and logs what that costs in RAM.
4. Repeats `runs` times (default 2): resolve the name, open TCP, TLS handshake with verification **required**,
   `GET`, read the whole answer, close. Every run builds a fresh session, so the second run shows whether anything leaks
   and whether timings are repeatable.
5. Prints one result block to paste back (see below), then idles.

Verification covers the chain up to a bundled root, the host name and the validity dates. A failure is explained in
words: `NOT TRUSTED` (a root is missing, or a private CA), `EXPIRED` or `NOT YET VALID` (usually a wrong clock), `HOST
NAME MISMATCH`. The spike never falls back to an unverified connection.

### The TLS stack, and why

**mbedtls-rs 0.2** (the esp-rs crate, successor of `esp-mbedtls`; MbedTLS 3 compiled for the ESP32-S3).

- It verifies certificates (chain, host name, and, with the `hook-wall-clock` feature used here, the dates). Without
  that feature mbedtls-rs does not check validity dates at all, so the spike turns it on and feeds it from the RTC.
- It uses the S3's AES, SHA and RSA (big-number exponentiation) units through `esp-hal`'s crypto work queues. The S3 has
  **no** elliptic curve unit, so ECDHE and ECDSA run in software. Turn the hardware off with `hw_accel = false` to see
  how much it helps.
- It is async over `embedded-io-async`, which `embassy-net`'s TCP socket already implements.
- Release 0.2 is the one built for the `esp-hal` 1.1 family this crate already uses. 0.3 needs `esp-hal` 1.2 and a newer
  `esp-radio`, so moving to it is a separate, later change.

Not chosen: `embedded-tls` (TLS 1.3 only and its documentation does not describe certificate verification, so it
cannot be trusted with a token yet), `rustls` (no `no_std` client that is ready for this), and `esp-idf-svc` (works, but
means leaving the `no_std` stack the firmware is built on; it stays the fallback if mbedtls-rs fails here).

### Root certificates and their cost

`certs/roots.pem` holds 14 public roots (Let's Encrypt, DigiCert, Google Trust Services, Sectigo/USERTrust, Microsoft,
Amazon, SSL.com, GlobalSign), copied unchanged from the Mozilla store, see [certs/README.md](certs/README.md). It is
embedded as text (about 19 KB of flash) and parsed into RAM at start-up; the log prints the RAM it took. A private CA
(a self-hosted Gitea behind its own certificate) is not supported yet; that is a follow-up for the real client.

If your `url` shows `NOT TRUSTED`, the site's chain ends in a root that is not in this file. That is a bundle gap, not a
TLS problem.

### Flash and RAM, as built

Measured from the ELF section sizes of a release build (code, read-only data, initialised data; not a flashed
image, `espflash` prints the real image size when flashing, please report it):

| | Panel firmware | `tls-spike` | Difference |
|---|---|---|---|
| Flash (code + constants) | about 142 KiB | about 506 KiB | about **+365 KiB**, of which the root bundle is 19 KiB |
| Static RAM (including the heap) | about 105 KiB | about 180 KiB | the spike's heap is 224 KiB (64 reclaimed + 160) |

That is well inside the roughly 1.9 MB OTA slot. RAM per live session is what the run prints
(`tls_session_cost`); MbedTLS keeps two 16 KiB record buffers per connection by default, which is most of it. They can
be shrunk (`ssl-in-content-len-*` features of `mbedtls-rs-sys`), which forces another MbedTLS build.

### Build and flash

Prerequisites on top of the normal build (see "Building" above): `cmake` and `ninja`, because the date checks change
MbedTLS's configuration and the C library is compiled on your machine instead of using the prebuilt one. The Xtensa GCC
that `espup install` puts on the PATH does the compiling (open a new terminal after `espup`, or source the export file
it prints).

| | Linux (Debian, Ubuntu) | Windows |
|---|---|---|
| Install | `sudo apt install cmake ninja-build` | `winget install Kitware.CMake Ninja-build.Ninja` |
| USB access | add yourself to `dialout`: `sudo usermod -aG dialout $USER`, log out and in | none; the board appears as a COM port |

Then, with the board plugged into its native USB port (hold BOOT while plugging in if it is not found):

```sh
cd firmware
cp config.example.toml config.toml        # skip if you already have one; add a [tls_spike] table if you want to change defaults
cargo run --release --features tls-spike --bin tls-spike
```

`cargo run` flashes and opens the serial monitor in one go, so the first lines are not missed. The first build takes a few
minutes. To only read the log of an already flashed board: `espflash monitor` (press the board's RESET button once the
monitor is open, because the test starts at power-up). Leave with Ctrl+C. On Windows use the same commands in PowerShell
after running the `export-esp.ps1` that `espup` created; on Linux `source ~/export-esp.sh` (the path `espup` printed).

### Settings

Optional `[tls_spike]` table in `config.toml` (see `config.example.toml`):

| Key | Default | Meaning |
|---|---|---|
| `url` | `https://example.com/` | What to fetch. Try your Gitea or GitHub API address too. Only `https://host[:port]/path`. |
| `ntp_host` | `pool.ntp.org` | SNTP server for the clock. |
| `runs` | `2` | Requests per boot (1 to 10). |
| `hw_accel` | `true` | Use the AES, SHA and RSA units. |

### Reading the result

Everything is logged over serial. The part to paste back is the block between
`======== DESKWATCH TLS SPIKE RESULT` and `======== END OF RESULT ========`. Look for `verdict: ALL RUNS OK`.

| Number | Meaning |
|---|---|
| `dns`, `tcp` | Time to resolve the name and open the TCP connection |
| `handshake` | The TLS handshake alone: the number that matters |
| `first_byte`, `total` | After the request was sent, and for the whole run |
| `free_heap before / in_session / after` | Free heap before the run, with the TLS session open, and after it was closed |
| `tls_session_cost` | `before` minus `in_session`: RAM one TLS session holds |
| `peak_used_since_boot` | Highest heap use since power-up (Wi-Fi start-up counts too, so only a run that raised it says something about TLS) |
| `repeatability` | Free heap after run 1 versus after the last run. A drift of more than a few hundred bytes is a leak |

If the log stops right after `handshake: starting`, the hardware crypto probably hung: set `hw_accel = false`, rebuild
and tell us. If you see an allocation failure, raise `HEAP_MAIN` in `src/tls_spike/main.rs`.

Results template to paste into the project chat:

```text
TLS spike result
Board: ESP32-S3 SuperMini | OS: Windows / Linux | url: (default or yours) | hw_accel: true / false
Flashed image size (from espflash): ... KiB
Wi-Fi joined: yes/no | clock set by SNTP: yes/no
Verdict line: ...
Run 1: handshake ... ms, total ... ms, status ..., bytes ...
Run 2: handshake ... ms, total ... ms, status ..., bytes ...
tls_session_cost: ... bytes | repeatability drift: ... bytes | root_ram_bytes: ...
Anything that failed (paste the lines with ERROR or WARN):
```

## Layout

| File | Job |
|---|---|
| `src/main.rs` | Start-up: heap, scheduler, config, tasks |
| `src/config.rs` | `Config`, the `ConfigSource` trait and the compile-time source |
| `build.rs` | Turns `config.toml` into constants, passes linker arguments |
| `src/net.rs` | Wi-Fi station, reconnect loop, embassy-net with DHCP |
| `src/mqtt.rs` | Broker session, Last Will, subscriptions, keep-alive, reconnect |
| `src/display.rs` | ST7796S setup on SPI2 and the task that feeds messages to `deskwatch_ui::Panel` and draws |
| `src/tls_spike/` | The TLS spike binary (feature `tls-spike`): `main.rs` the test, `http.rs` a tiny GET client, `sntp.rs` the clock |
| `certs/roots.pem` | The root certificates the spike trusts |

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
