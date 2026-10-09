//! DeskWatch panel firmware (ESP32-S3 SuperMini, 4" ST7796S display).
//!
//! Stage 1: join Wi-Fi, connect to the broker, subscribe to the bridge's
//! retained topics, log what arrives over the serial port and draw it with the
//! shared `deskwatch-ui` crate. The Wi-Fi and MQTT path and the display path
//! are both written but not yet run on the real board; see README.md.

#![no_std]
#![no_main]

extern crate alloc;

mod config;
mod display;
mod mqtt;
mod net;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::timer::timg::TimerGroup;
use static_cell::StaticCell;

use config::{CompiledConfig, ConfigSource};
use mqtt::Message;

esp_bootloader_esp_idf::esp_app_desc!();

/// MQTT task -> display task. Four messages is plenty: the bridge sends a
/// screen, the badges and the status at most once every few seconds.
static MESSAGES: StaticCell<Channel<CriticalSectionRawMutex, Message, 4>> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    log::info!("DeskWatch panel firmware {}", env!("CARGO_PKG_VERSION"));

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // Wi-Fi needs a heap. The first region reuses RAM the bootloader is done
    // with.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 96 * 1024);

    // The scheduler must run before the radio starts.
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    let cfg = match CompiledConfig.load() {
        Ok(cfg) => cfg,
        Err(e) => {
            // Stage 2 will start the setup mode here instead of stopping.
            log::error!(
                "config: {e:?}. Copy firmware/config.example.toml to config.toml, fill it in and rebuild."
            );
            loop {
                Timer::after(Duration::from_secs(60)).await;
            }
        }
    };

    let messages = MESSAGES.init(Channel::new());

    // Display first, so something is on screen while Wi-Fi comes up.
    let lcd = display::init(
        display::Pins {
            spi: peripherals.SPI2,
            sck: peripherals.GPIO12,
            mosi: peripherals.GPIO11,
            cs: peripherals.GPIO10,
            dc: peripherals.GPIO9,
            rst: peripherals.GPIO8,
            backlight: peripherals.GPIO7,
        },
        cfg.display,
    );
    spawner.spawn(display::run(lcd, messages.receiver()).expect("spawn"));

    let stack = net::start(&spawner, peripherals.WIFI, &cfg);
    spawner.spawn(mqtt::run(stack, cfg, messages.sender()).expect("spawn"));

    // Everything runs in tasks now.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}
