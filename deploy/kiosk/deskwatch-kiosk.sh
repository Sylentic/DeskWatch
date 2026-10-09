#!/bin/sh
# Opens the DeskWatch kiosk page full screen in Chromium. Started from the
# desktop autostart on a Raspberry Pi (see docs/kiosk.md, section 4).
#
#   install -m 755 deploy/kiosk/deskwatch-kiosk.sh ~/deskwatch-kiosk.sh
#
# URL: the bridge on this Pi, or http://<bridge host>:8787/?token=<kiosk token>
# for a bridge on another machine. Override with DESKWATCH_KIOSK_URL.
URL="${DESKWATCH_KIOSK_URL:-http://localhost:8787/}"

# The browser is called `chromium` on Raspberry Pi OS Bookworm and newer, and
# `chromium-browser` on older releases.
BROWSER=chromium
command -v "$BROWSER" >/dev/null 2>&1 || BROWSER=chromium-browser

# Wait until the bridge answers, so boot order does not matter. The page also
# reconnects by itself later, this only avoids a browser error page at boot.
until curl -fs -o /dev/null --max-time 3 "$URL"; do sleep 2; done

# Wayland (labwc, the default on Bookworm and newer). On X11 set
# DESKWATCH_KIOSK_FLAGS="" (empty) to drop the Wayland hint.
FLAGS="${DESKWATCH_KIOSK_FLAGS---ozone-platform=wayland}"

# shellcheck disable=SC2086  # FLAGS is a list of flags on purpose
exec "$BROWSER" --kiosk $FLAGS \
  --noerrdialogs --disable-infobars --no-first-run \
  --disable-session-crashed-bubble --hide-crash-restore-bubble \
  --password-store=basic --check-for-update-interval=31536000 \
  --user-data-dir="$HOME/.cache/deskwatch-kiosk" \
  "$URL"
