# Getting started

The shortest road from nothing to your own sources on the dashboard, with Docker. Each step names the page that has
the details.

**Words used in these docs.** The *dashboard* is the web page the bridge serves at `/` (also called the kiosk page; the
config section is `[kiosk]`). The *setup page* is `/setup`, where you add sources. *Kiosk mode* is only a browser
started full screen on a wall display. A *source* is one service the bridge reads (Gitea, GitHub,
Prometheus and so on).

## 1. Look at it first

```sh
docker run --rm -p 8787:8787 ghcr.io/sylentic/deskwatch-bridge:latest --demo --kiosk --no-mqtt
```

Open <http://localhost:8787/> to see fake data. Stop it with Ctrl+C.

## 2. Run your own

```sh
git clone https://github.com/Sylentic/DeskWatch.git && cd DeskWatch
mkdir -p dashboard/credentials
cp deploy/dashboard.example.toml dashboard/bridge.toml
```

To add sources from the browser you need two things, and the setup page does nothing without both:

1. In `dashboard/bridge.toml`, remove the `#` in front of `[data]`, `dir`, `[setup]` and `enabled = true`.
2. A writable data folder for the container. In `deploy/docker-compose.dashboard.yml`, uncomment the `data` volume
   line and create the folder for the container user:

   ```sh
   mkdir -p dashboard/data && sudo chown 10001 dashboard/data
   ```

   A named volume needs no `chown`, see [data-dir.md](data-dir.md#docker).

Then start it:

```sh
docker compose -f deploy/docker-compose.dashboard.yml up -d
```

## 3. Create the first account and add sources

Open `http://<docker host>:8787/`. A new install asks for the first administrator account (or lets you continue
without a login). Then open the cogwheel in the header, or `/setup`:

- **Sources**: add GitHub, Gitea, GitLab, Bitbucket Cloud or Azure DevOps. Each has a **Test connection** button
  ([setup-page.md](setup-page.md)).
- **Hosts**: add a Prometheus source first, then tick the machines to show ([hosts.md](hosts.md)).

## 4. Next

- Keep it on your home network, or set a password and HTTPS first: [auth.md](auth.md), [https.md](https.md).
- Alerts from Home Assistant or scripts: [home-assistant.md](home-assistant.md).
- A wall display: a Linux machine with a screen and a browser in kiosk mode, see [kiosk.md](kiosk.md#4-autostart-a-full-screen-browser-on-linux).
- Without Docker: [install.md](install.md) (Linux) or [windows.md](windows.md).
