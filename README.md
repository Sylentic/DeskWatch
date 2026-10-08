# DeskWatch

DeskWatch is an ESP32-S3 desk status panel. When idle it rotates through server stats, open pull requests and pipeline status. When a CI job runs it switches to live progress, shows a red alert on failure (cleared with a button) and a short green flash on success.

## How it fits together

- **Bridge** (Rust, runs on a Debian home server): collects server stats, receives Gitea and GitHub webhooks, polls Azure DevOps, decides what the panel should show and publishes it over MQTT.
- **Firmware** (Rust, esp-hal + embassy): a deliberately dumb MQTT client on an ESP32-S3 SuperMini with a 4" ST7796S touch screen. It draws whatever page the bridge sends.
- **MQTT** (Mosquitto) sits between the two.

## Design docs

The design lives in the project's shared files for now:

- `mqtt-schema.md`: the MQTT contract between bridge and panel
- `schema-and-wiring.md`: hardware and pin wiring
- `firmware-stack.md`: firmware crate choices
- `display-options.md`: display research

## Status

No code yet. The bridge comes next, followed by a firmware spike once the screen arrives.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
