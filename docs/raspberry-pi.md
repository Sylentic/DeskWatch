# DeskWatch on a Raspberry Pi

A Raspberry Pi is a good always-on home for the bridge: it idles at a few watts, and the bridge needs very little
(a Pi 3 or newer is plenty; a Pi Zero 2 W works too). It is also a fine always-on machine for the demo:
`deskwatch-bridge --demo` on the Pi plus the simulator on a PC gives you something to look at before the panel
hardware exists.

This page covers two ways to run the bridge on **Raspberry Pi OS 64-bit** (Debian 12 "Bookworm" or newer, the
Lite image is enough). Everything else (config keys, sources, Mosquitto logins, Home Assistant) is the same as on any
Linux server, so this page points to [install.md](install.md) for it. Every host name below is a placeholder.

Contents: [1. Before you start](#1-before-you-start) · [2. Route A: systemd](#2-route-a-systemd-release-binary) ·
[3. Route B: Docker](#3-route-b-docker) · [4. Mosquitto on the Pi](#4-mosquitto-on-the-pi) ·
[5. Run the demo or the simulator](#5-the-demo-and-the-simulator) · [6. Update and remove](#6-update-and-remove) ·
[7. What was and was not tested](#7-what-was-and-was-not-tested)

## 1. Before you start

- **64-bit OS.** The release binary is `aarch64`. Check with `uname -m`: it must print `aarch64`. A 32-bit Raspberry
  Pi OS (`armv7l`) is not supported by the release; re-flash with the 64-bit image (Raspberry Pi Imager lists it
  as "Raspberry Pi OS (64-bit)"). Pi 3, 4, 5, Zero 2 W and 400 all support it.
- **A broker.** Mosquitto on the Pi itself, or another machine, see [section 4](#4-mosquitto-on-the-pi).
- **Storage wear.** The bridge writes nothing to disk, so an SD card is fine. Journald logs are the only writes.
- **A fixed address.** Give the Pi a DHCP reservation on your router (or a static address) so the panel and Gitea
  webhooks keep finding it.

The local stats page (CPU, memory, disk, temperature of the machine the bridge runs on) works on the Pi because
it reads `/proc` and `/sys`, like on any Linux server.

## 2. Route A: systemd, release binary

Each release at <https://github.com/Sylentic/DeskWatch/releases> has a tarball for the Pi next to the
x86_64 one, named `deskwatch-bridge-<version>-aarch64-linux.tar.gz`, with a `.sha256` file. It is built on
Ubuntu 22.04 (glibc 2.35), so it runs on Raspberry Pi OS Bookworm and newer and needs nothing installed.

```sh
VERSION=v0.9.3      # the release you want; the aarch64 tarball first ships with 0.9.3
cd "$(mktemp -d)"
curl -fLO "https://github.com/Sylentic/DeskWatch/releases/download/$VERSION/deskwatch-bridge-$VERSION-aarch64-linux.tar.gz"
curl -fLO "https://github.com/Sylentic/DeskWatch/releases/download/$VERSION/deskwatch-bridge-$VERSION-aarch64-linux.tar.gz.sha256"
sha256sum -c "deskwatch-bridge-$VERSION-aarch64-linux.tar.gz.sha256"
tar xzf "deskwatch-bridge-$VERSION-aarch64-linux.tar.gz"
cd "deskwatch-bridge-$VERSION-aarch64-linux"

sudo install -m 755 deskwatch-bridge /usr/local/bin/deskwatch-bridge
sudo install -D -m 644 config.example.toml /etc/deskwatch/bridge.toml
sudo install -m 644 deploy/deskwatch-bridge.service /etc/systemd/system/
sudo install -d -m 700 /etc/deskwatch/credentials
```

From here it is the same as on any server: edit `/etc/deskwatch/bridge.toml` (delete the example Gitea block if you
have no Gitea), add secrets with `LoadCredential=` lines, then start it. Follow
[install.md sections 2 to 7](install.md#2-install-the-bridge); the unit file is the same one.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now deskwatch-bridge
sudo journalctl -u deskwatch-bridge -f
```

Look for `connected to MQTT broker`.

**Building from source instead** works on the Pi too ([install.md option B](install.md#option-b-build-from-source)),
but a first build takes a long time on a Pi 3 and wants 2 GB of memory or swap. Prefer the release binary, or
cross-build on a PC: `rustup target add aarch64-unknown-linux-gnu`, install `gcc-aarch64-linux-gnu`, then
`CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
cargo build --release --locked -p deskwatch-bridge --target aarch64-unknown-linux-gnu`. Build on a distribution with
glibc 2.36 or older if you want the binary to run on Bookworm.

## 3. Route B: Docker

Handy if the Pi already runs other containers. [docker.md](docker.md) is the full guide; on the Pi:

```sh
curl -fsSL https://get.docker.com | sudo sh      # or: sudo apt install docker.io docker-compose
sudo usermod -aG docker "$USER"                  # log out and in again afterwards

git clone https://github.com/Sylentic/DeskWatch.git
cd DeskWatch
docker build -t deskwatch-bridge .               # builds natively for arm64
```

The `Dockerfile` has no CPU-specific lines, so the same file builds for the Pi and for an x86_64 server. CI builds
it for both `linux/amd64` and `linux/arm64` on every change, but **no image is published** to any registry, so you
build it yourself (a first build compiles every dependency and takes a while on a Pi; later ones reuse the layers). To
build the arm64 image on a faster PC and copy it over:

```sh
docker buildx build --platform linux/arm64 -t deskwatch-bridge:arm64 --load .
docker save deskwatch-bridge:arm64 | ssh pi@raspberrypi.local docker load
```

(`--load` of a foreign-CPU image needs QEMU: `sudo apt install qemu-user-static binfmt-support` on Debian.)

Then continue with [docker.md section 3 onwards](docker.md#3-config-and-secrets): config and secrets mounted
read-only, the Compose example in [`deploy/docker-compose.yml`](../deploy/docker-compose.yml), reaching the broker.
Pointing `host` at the Pi's own broker needs the `host.docker.internal` or `network_mode: host` choice described there.

Which route? Systemd is lighter (no Docker daemon) and is the simplest if the Pi does
only this. Docker keeps the bridge self-contained and fits a Pi that already runs a container stack.

## 4. Mosquitto on the Pi

If the Pi is also the broker host:

```sh
sudo apt install mosquitto mosquitto-clients
```

Then follow [mosquitto.md](mosquitto.md) for logins and topic limits (do that before the panel arrives; an open
broker on a home network lets anything draw on the panel). The Pi's package is the same Mosquitto 2.0 the guide
assumes. The bridge's `[mqtt] host` is then `localhost` for Route A, or see the broker table in
[docker.md section 5](docker.md#5-reaching-the-broker) for Route B.

## 5. The demo and the simulator

With a broker running on the Pi, try the bridge with no CI at all:

```sh
deskwatch-bridge --demo        # plays fake data to a broker on localhost
```

and watch it from a PC with the desktop simulator (`cargo run -p deskwatch-sim -- --host raspberrypi.local`, or
`deskwatch-sim.exe --host raspberrypi.local` from the Windows zip), see
[install.md section 8](install.md#8-try-it-without-hardware). The simulator is a desktop program: it is not
shipped for the Pi, and it needs a window system. A Pi is, however, a fine always-on machine to leave the demo
running while you work on the layouts or the firmware.

## 6. Update and remove

Route A: download the new tarball as in section 2, `sudo install -m 755 deskwatch-bridge /usr/local/bin/`, then
`sudo systemctl restart deskwatch-bridge`. Route B: `git pull`, `docker build -t deskwatch-bridge .`, then
`docker compose up -d`. Removal and rollback are the same as in
[install.md section 9](install.md#9-update-roll-back-remove) and [docker.md section 10](docker.md#10-update-and-remove).

## 7. What was and was not tested

- The aarch64 binary was cross-compiled and checked as a valid ARM aarch64 ELF, and the release workflow builds
  it natively on GitHub's arm64 Ubuntu 22.04 runner.
- The multi-arch image build runs in CI (build only, nothing pushed).
- **Nothing here has run on real Raspberry Pi hardware yet.** The commands in this page follow the Linux guide and
  are expected to work as written; if one does not, please open an issue with the Pi model and OS version.
