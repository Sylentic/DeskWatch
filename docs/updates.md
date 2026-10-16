# The "newer version" notice

When a newer DeskWatch release is out, the dashboard shows a small chip, "update available: v0.9.20", next to the Setup
link. It links to the release notes and has a × that hides that version in the browser you click it in (the next release
shows the chip again). The Setup page shows the same note behind the version under **Bridge and restart**.

- **What is sent:** once a day (the first look is 30 seconds after the bridge starts) the bridge asks
  `api.github.com` for the latest release of `Sylentic/DeskWatch` and compares the version number with its own. It is a
  plain GET: no id, no host name, no information about your install.
- **When it fails:** a missing internet connection, a refused request or an odd answer shows nothing and logs nothing
  above debug level. Only releases (not pre-releases) count, and only a release page on github.com/Sylentic/DeskWatch
  is ever linked.
- **Update:** the notice does not update anything. For Docker, change the image tag in your compose file (see
  `docs/docker.md`) and recreate the container.
- **Off switch:** for a server without internet, set `check = false` under `[updates]` in `bridge.toml`, or untick
  **Tell me about new versions** under Setup, **Bridge and restart** (needs a data folder and a restart). The demo
  (`--demo`) never checks.

```toml
[updates]
check = true   # false: never look for a newer release
```
