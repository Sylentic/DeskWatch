//! Turns `config.toml` (or `config.example.toml` when there is none) into
//! constants the firmware includes at compile time, and passes the linker
//! script to the linker.

use std::{env, fmt::Write as _, fs, path::Path};

fn main() {
    // Link arguments live here, not in .cargo/config.toml, so that setting the
    // RUSTFLAGS environment variable (CI does) cannot silently drop them.
    // esp-hal's linker script. Without it the linker drops the interrupt tables.
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    println!("cargo:rustc-link-arg=-nostartfiles");
    // The Xtensa linker warns that the RAM segment is readable, writable and
    // executable; that is how this chip's memory is laid out.
    println!("cargo:rustc-link-arg=-Wl,--no-warn-rwx-segments");

    println!("cargo:rerun-if-changed=config.toml");
    println!("cargo:rerun-if-changed=config.example.toml");

    let configured = Path::new("config.toml").exists();
    let source = if configured {
        "config.toml"
    } else {
        println!(
            "cargo:warning=firmware/config.toml not found, using config.example.toml. \
             Copy it to config.toml and fill it in before flashing."
        );
        "config.example.toml"
    };
    let text = fs::read_to_string(source).unwrap_or_else(|e| panic!("cannot read {source}: {e}"));
    let doc: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("{source} is not valid TOML: {e}"));

    let mut out = String::new();
    writeln!(out, "pub const CONFIGURED: bool = {configured};").unwrap();
    string(&mut out, &doc, "wifi", "ssid", "WIFI_SSID", None);
    string(
        &mut out,
        &doc,
        "wifi",
        "password",
        "WIFI_PASSWORD",
        Some(""),
    );
    string(&mut out, &doc, "mqtt", "host", "MQTT_HOST", None);
    int(
        &mut out,
        &doc,
        "mqtt",
        "port",
        "MQTT_PORT",
        "u16",
        Some(1883),
    );
    string(
        &mut out,
        &doc,
        "mqtt",
        "username",
        "MQTT_USERNAME",
        Some(""),
    );
    string(
        &mut out,
        &doc,
        "mqtt",
        "password",
        "MQTT_PASSWORD",
        Some(""),
    );
    string(
        &mut out,
        &doc,
        "mqtt",
        "client_id",
        "MQTT_CLIENT_ID",
        Some("deskwatch-panel"),
    );
    string(
        &mut out,
        &doc,
        "mqtt",
        "topic_prefix",
        "MQTT_TOPIC_PREFIX",
        Some("deskpanel"),
    );
    int(
        &mut out,
        &doc,
        "mqtt",
        "keep_alive_s",
        "MQTT_KEEP_ALIVE_S",
        "u16",
        Some(30),
    );
    boolean(&mut out, &doc, "display", "bgr", "DISPLAY_BGR", true);
    boolean(
        &mut out,
        &doc,
        "display",
        "invert_colors",
        "DISPLAY_INVERT",
        false,
    );
    int(
        &mut out,
        &doc,
        "display",
        "spi_mhz",
        "DISPLAY_SPI_MHZ",
        "u32",
        Some(40),
    );

    // TLS spike settings (only the tls-spike binary reads these).
    string(
        &mut out,
        &doc,
        "tls_spike",
        "url",
        "TLS_SPIKE_URL",
        Some("https://example.com/"),
    );
    string(
        &mut out,
        &doc,
        "tls_spike",
        "ntp_host",
        "TLS_SPIKE_NTP_HOST",
        Some("pool.ntp.org"),
    );
    int(
        &mut out,
        &doc,
        "tls_spike",
        "runs",
        "TLS_SPIKE_RUNS",
        "u8",
        Some(2),
    );
    boolean(
        &mut out,
        &doc,
        "tls_spike",
        "hw_accel",
        "TLS_SPIKE_HW_ACCEL",
        true,
    );

    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("config_gen.rs");
    fs::write(dest, out).unwrap();
}

fn value<'a>(doc: &'a toml::Table, table: &str, key: &str) -> Option<&'a toml::Value> {
    doc.get(table)?.as_table()?.get(key)
}

fn string(
    out: &mut String,
    doc: &toml::Table,
    table: &str,
    key: &str,
    name: &str,
    default: Option<&str>,
) {
    let v = match value(doc, table, key) {
        Some(v) => v
            .as_str()
            .unwrap_or_else(|| panic!("[{table}] {key} must be a string")),
        None => default.unwrap_or_else(|| panic!("[{table}] {key} is required")),
    };
    writeln!(out, "pub const {name}: &str = {v:?};").unwrap();
}

fn int(
    out: &mut String,
    doc: &toml::Table,
    table: &str,
    key: &str,
    name: &str,
    ty: &str,
    default: Option<i64>,
) {
    let v = match value(doc, table, key) {
        Some(v) => v
            .as_integer()
            .unwrap_or_else(|| panic!("[{table}] {key} must be a number")),
        None => default.unwrap_or_else(|| panic!("[{table}] {key} is required")),
    };
    writeln!(out, "pub const {name}: {ty} = {v};").unwrap();
}

fn boolean(out: &mut String, doc: &toml::Table, table: &str, key: &str, name: &str, default: bool) {
    let v = match value(doc, table, key) {
        Some(v) => v
            .as_bool()
            .unwrap_or_else(|| panic!("[{table}] {key} must be true or false")),
        None => default,
    };
    writeln!(out, "pub const {name}: bool = {v};").unwrap();
}
