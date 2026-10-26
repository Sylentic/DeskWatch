# DeskWatch on a Raspberry Pi

A Raspberry Pi with a monitor or TV makes a good wall display for the dashboard. This page is only about the Pi; the
rest is the same as on any Linux machine, so it points to the other guides.

There are two ways to set it up, and the second is usually nicer:

- **The Pi only shows the page.** DeskWatch runs in a container on a homelab server
  ([docker.md](docker.md#3-the-dashboard-on-your-homelab)) and the Pi's browser opens its address. The Pi needs no
  DeskWatch software, only the autostart in [kiosk.md](kiosk.md#4-autostart-on-a-raspberry-pi) with
  `DESKWATCH_KIOSK_URL` set to the server.
- **The Pi runs DeskWatch itself.** Use Docker ([docker.md](docker.md); the published image is multi-arch and also
  runs on `arm64`) or the systemd release binary (below), and open `http://localhost:8787/` in the browser.

## Requirements

- **64-bit Raspberry Pi OS** (Debian 12 "Bookworm" or newer; with desktop if the Pi shows the page). Check with
  `uname -m`: it must print `aarch64`. A 32-bit OS (`armv7l`) is not supported by the release; re-flash with the
  64-bit image. Pi 3, 4, 5, Zero 2 W and 400 all support it.
- **A browser-capable Pi** if it shows the page: a Pi 4 with 2 GB or more, or a Pi 5 ([memory notes](kiosk.md#4-autostart-on-a-raspberry-pi)).
  A Pi Zero 2 W can run the bridge but is not enough for a browser.
- **A fixed address** (DHCP reservation) if other machines or webhooks reach the Pi.

## Running the bridge from the release binary

Each release has `deskwatch-bridge-<version>-aarch64-linux.tar.gz` next to the x86_64 one, built on Ubuntu 22.04
(glibc 2.35), so it runs on Bookworm and newer. Download and verify it as described in
[install.md](install.md#option-a-download-a-release) (take the aarch64 file), then follow the same systemd steps. The
unit file is the same one. Building from source on a Pi works but is slow; prefer the binary or the image.

The bridge's own host card (CPU, memory, disk, temperature) works on the Pi because it reads `/proc` and `/sys`; turn it on with `[server] enabled = true`.

## Showing the dashboard

Autostart of Chromium in kiosk mode, screen blanking, 4K and burn-in are in
[kiosk.md section 4](kiosk.md#4-autostart-on-a-raspberry-pi). A dashboard alone needs no broker: set
`[mqtt] enabled = false`.

## What was and was not tested

- The aarch64 binary is built natively on GitHub's arm64 runner, and the multi-arch image is built by CI.
- **Nothing here has run on real Raspberry Pi hardware yet.** If something does not work, please
  [open an issue](https://github.com/Sylentic/DeskWatch/issues/new/choose) with the Pi model and OS version, or tell us
  that it works.
