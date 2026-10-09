//! Panel settings: Wi-Fi, MQTT login and display quirks.
//!
//! The rest of the firmware only sees [`Config`] and asks a [`ConfigSource`]
//! for it at boot. Stage 1 has one source, [`CompiledConfig`], which reads the
//! values `build.rs` generated from the gitignored `config.toml`. Stage 2 adds
//! a flash (NVS) source and a setup mode behind the same trait, so nothing
//! outside `main` changes (see README.md).

use heapless::String;

#[allow(dead_code)] // the TLS spike constants are only read by the tls-spike binary
mod generated {
    include!(concat!(env!("OUT_DIR"), "/config_gen.rs"));
}

/// Everything the panel needs to join the network and talk to the broker.
/// The panel never holds CI tokens, those stay in the bridge.
#[derive(Clone)]
pub struct Config {
    pub wifi_ssid: String<32>,
    pub wifi_password: String<64>,
    /// IPv4 address or a name the network's DNS can resolve.
    pub mqtt_host: String<64>,
    pub mqtt_port: u16,
    /// Empty when the broker still allows anonymous clients.
    pub mqtt_username: String<64>,
    pub mqtt_password: String<64>,
    pub mqtt_client_id: String<32>,
    pub topic_prefix: String<32>,
    pub keep_alive_s: u16,
    pub display: DisplayConfig,
}

/// Display quirks that can only be settled on the real screen.
#[derive(Clone, Copy)]
pub struct DisplayConfig {
    pub bgr: bool,
    pub invert_colors: bool,
    pub spi_mhz: u32,
}

/// Why a [`ConfigSource`] could not give a usable [`Config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// Nothing is stored yet (stage 2: start the setup mode).
    NotConfigured,
    /// A value is too long for its field.
    TooLong(&'static str),
}

/// Where the settings come from.
pub trait ConfigSource {
    fn load(&mut self) -> Result<Config, ConfigError>;
}

/// Settings compiled into the firmware from `config.toml`.
pub struct CompiledConfig;

impl ConfigSource for CompiledConfig {
    fn load(&mut self) -> Result<Config, ConfigError> {
        use generated::*;

        if !CONFIGURED {
            return Err(ConfigError::NotConfigured);
        }
        Ok(Config {
            wifi_ssid: fit(WIFI_SSID, "wifi.ssid")?,
            wifi_password: fit(WIFI_PASSWORD, "wifi.password")?,
            mqtt_host: fit(MQTT_HOST, "mqtt.host")?,
            mqtt_port: MQTT_PORT,
            mqtt_username: fit(MQTT_USERNAME, "mqtt.username")?,
            mqtt_password: fit(MQTT_PASSWORD, "mqtt.password")?,
            mqtt_client_id: fit(MQTT_CLIENT_ID, "mqtt.client_id")?,
            topic_prefix: fit(MQTT_TOPIC_PREFIX, "mqtt.topic_prefix")?,
            keep_alive_s: MQTT_KEEP_ALIVE_S,
            display: DisplayConfig {
                bgr: DISPLAY_BGR,
                invert_colors: DISPLAY_INVERT,
                spi_mhz: DISPLAY_SPI_MHZ,
            },
        })
    }
}

fn fit<const N: usize>(s: &str, field: &'static str) -> Result<String<N>, ConfigError> {
    String::try_from(s).map_err(|_| ConfigError::TooLong(field))
}

/// Settings of the TLS spike binary (`[tls_spike]` in `config.toml`). The panel
/// firmware does not use them.
#[allow(dead_code)]
pub struct SpikeConfig {
    /// HTTPS address to fetch, `https://host[:port]/path`.
    pub url: String<128>,
    /// SNTP server name.
    pub ntp_host: String<64>,
    /// How many times to repeat the request in one boot.
    pub runs: u8,
    /// Use the ESP32-S3 AES, SHA and RSA units for the TLS maths.
    pub hw_accel: bool,
}

#[allow(dead_code)]
impl CompiledConfig {
    pub fn load_spike(&self) -> Result<SpikeConfig, ConfigError> {
        use generated::*;

        Ok(SpikeConfig {
            url: fit(TLS_SPIKE_URL, "tls_spike.url")?,
            ntp_host: fit(TLS_SPIKE_NTP_HOST, "tls_spike.ntp_host")?,
            runs: TLS_SPIKE_RUNS.clamp(1, 10),
            hw_accel: TLS_SPIKE_HW_ACCEL,
        })
    }
}
