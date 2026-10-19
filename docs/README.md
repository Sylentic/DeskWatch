# DeskWatch documentation

Start with the [project README](../README.md) for what DeskWatch is and what works today.

## Installing

- [install.md](install.md): the whole setup on a Linux server with systemd
- [docker.md](docker.md): the container, Compose files, the dashboard on a homelab
- [raspberry-pi.md](raspberry-pi.md): 64-bit Raspberry Pi OS
- [windows.md](windows.md): the release zip, Mosquitto, the simulator
- [releasing.md](releasing.md): how a release is made (for maintainers)
- [updates.md](updates.md): the quiet "update available" notice

## Using the dashboard

- [kiosk.md](kiosk.md): the big-screen web page, its widgets and settings
- [setup-page.md](setup-page.md): change sources and settings from the browser
- [data-dir.md](data-dir.md): the writable data folder the setup page and the login use
- [runners.md](runners.md): the runners widget

## Security

- [auth.md](auth.md): the built-in login, API keys, sessions
- [https.md](https.md): turning on HTTPS

## Sources

- [git-hosts.md](git-hosts.md): which Git hosts work, and which are tested
- [gitlab.md](gitlab.md), [bitbucket.md](bitbucket.md), [azure-devops.md](azure-devops.md): setup per source
- [azure-devops-verify.md](azure-devops-verify.md): checklist for trying Azure DevOps on a real organisation

## Alerts, MQTT and the ESP panel

- [home-assistant.md](home-assistant.md): alerts from Home Assistant and scripts, `POST /api/alert`
- [mosquitto.md](mosquitto.md): broker logins, ACL and TLS
- [mqtt-schema.md](mqtt-schema.md): the contract between bridge and panel, with samples in [schema/](schema/)
- [firmware README](../firmware/README.md): the firmware and the TLS spike
