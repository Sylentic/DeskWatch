//! Wi-Fi station and the embassy-net stack on top of it.
//!
//! NOT TESTED ON HARDWARE.

use alloc::string::String;
use embassy_executor::Spawner;
use embassy_net::{Config as NetConfig, Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config as WifiConfig, Interface, WifiController};
use static_cell::StaticCell;

use crate::config::Config;

/// Sockets embassy-net can hold: the MQTT connection, DNS and DHCP.
const SOCKETS: usize = 5;
static RESOURCES: StaticCell<StackResources<SOCKETS>> = StaticCell::new();

/// Start the radio, the connection task and the network stack. DHCP runs
/// inside the stack; `Stack::wait_config_up` resolves once there is an address.
pub fn start(spawner: &Spawner, wifi: WIFI<'static>, cfg: &Config) -> Stack<'static> {
    let (mut controller, interfaces) =
        esp_radio::wifi::new(wifi, Default::default()).expect("wifi init");

    let station = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(cfg.wifi_ssid.as_str())
            .with_password(String::from(cfg.wifi_password.as_str())),
    );
    controller.set_config(&station).expect("wifi config");

    let seed = {
        let rng = Rng::new();
        (u64::from(rng.random()) << 32) | u64::from(rng.random())
    };
    let (stack, runner) = embassy_net::new(
        interfaces.station,
        NetConfig::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );

    spawner.spawn(connection(controller).expect("spawn"));
    spawner.spawn(net_task(runner).expect("spawn"));
    stack
}

/// Keep the station associated: connect, wait for a drop, wait, repeat.
#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    loop {
        log::info!("wifi: connecting");
        match controller.connect_async().await {
            Ok(_) => {
                log::info!("wifi: connected");
                // Returns when the access point goes away.
                let _ = controller.wait_for_disconnect_async().await;
                log::warn!("wifi: disconnected");
            }
            Err(e) => log::warn!("wifi: connect failed: {e:?}"),
        }
        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}
