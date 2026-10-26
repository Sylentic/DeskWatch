# DeskWatch documentation

Start with the [project README](../README.md) for what DeskWatch is, a quick demo and what works today.

## Installing

- [docker.md](docker.md): the container, Compose files, the dashboard on a homelab (the easiest start)
- [install.md](install.md): the whole setup on a Linux server with systemd
- [raspberry-pi.md](raspberry-pi.md): a Pi with a monitor or TV as a wall display
- [windows.md](windows.md): the release zip, Mosquitto
- [hosts.md](hosts.md): Linux and Windows hosts through Prometheus, pending updates
- [updates.md](updates.md): the quiet "update available" notice

## Using the dashboard

- [kiosk.md](kiosk.md): the dashboard page, its widgets, layout and settings
- [setup-page.md](setup-page.md): add and change sources and settings from the browser
- [backup.md](backup.md): export and import the settings, without secrets or locked with a password
- [data-dir.md](data-dir.md): the writable data folder the setup page and the login use
- [runners.md](runners.md): the runners widget

## Security

- [auth.md](auth.md): the built-in login, API keys, sessions, sign-in with GitHub or Microsoft
- [https.md](https.md): turning on HTTPS

## Sources

- [git-hosts.md](git-hosts.md): which Git hosts work, and which are tested
- [gitlab.md](gitlab.md), [bitbucket.md](bitbucket.md), [azure-devops.md](azure-devops.md): setup per source
- [azure-devops-verify.md](azure-devops-verify.md): checklist for trying Azure DevOps on a real organisation
- [firefox-containers.md](firefox-containers.md): open the links of a source in a Firefox container

## Alerts and MQTT

- [home-assistant.md](home-assistant.md): alerts from Home Assistant and scripts, `POST /api/alert`
- [alertmanager.md](alertmanager.md): alerts from Prometheus Alertmanager, `POST /api/alertmanager`
- [mosquitto.md](mosquitto.md): broker logins, ACL and TLS
- [mqtt-schema.md](mqtt-schema.md): the MQTT topics and the alert message format, with samples in [schema/](schema/)

## Working on DeskWatch

- [development.md](development.md): running from source, the demo, the dashboard files,
  the checks before a pull request
- [releasing.md](releasing.md): how a release is made (for maintainers)
- [CHANGELOG](../CHANGELOG.md): what changed in each version
