//! The ST7796S display on SPI2, and the task that draws the panel on it.
//!
//! NOT TESTED ON HARDWARE. The pins come from docs/schema-and-wiring.md
//! (Part 6); the orientation, colour order and inversion are the usual
//! values for this module family and may need the three `[display]` switches
//! in `config.toml`.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Receiver;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{GPIO7, GPIO8, GPIO9, GPIO10, GPIO11, GPIO12, SPI2};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use mipidsi::interface::SpiInterface;
use mipidsi::models::ST7796;
use mipidsi::options::{ColorInversion, ColorOrder, Orientation, Rotation};
use mipidsi::{Builder, Display};
use static_cell::StaticCell;

use deskwatch_ui::{Panel, Topic};

use crate::config::DisplayConfig;
use crate::mqtt::Message;

/// Pixels travel to the panel through this buffer, 512 bytes per SPI burst.
static SPI_BUF: StaticCell<[u8; 512]> = StaticCell::new();

type Bus = ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, embedded_hal_bus::spi::NoDelay>;
type Lcd = Display<SpiInterface<'static, Bus, Output<'static>>, ST7796, Output<'static>>;

/// The pins the display uses (Part 6 of docs/schema-and-wiring.md).
pub struct Pins {
    pub spi: SPI2<'static>,
    pub sck: GPIO12<'static>,
    pub mosi: GPIO11<'static>,
    pub cs: GPIO10<'static>,
    pub dc: GPIO9<'static>,
    pub rst: GPIO8<'static>,
    pub backlight: GPIO7<'static>,
}

/// Set up SPI2 and the controller, clear the screen and switch the backlight on.
/// Returns `None` (after logging why) when the display cannot be initialised,
/// so the rest of the firmware keeps running and logging over serial.
pub fn init(pins: Pins, cfg: DisplayConfig) -> Option<(Lcd, Output<'static>)> {
    let spi = match Spi::new(
        pins.spi,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(cfg.spi_mhz))
            .with_mode(Mode::_0),
    ) {
        Ok(spi) => spi.with_sck(pins.sck).with_mosi(pins.mosi),
        Err(e) => {
            log::error!("display: SPI config rejected: {e:?}");
            return None;
        }
    };

    let cs = Output::new(pins.cs, Level::High, OutputConfig::default());
    let dc = Output::new(pins.dc, Level::Low, OutputConfig::default());
    let rst = Output::new(pins.rst, Level::High, OutputConfig::default());
    // Backlight is plain on/off for now. PWM dimming (LEDC) comes with the
    // night-dimming work.
    let backlight = Output::new(pins.backlight, Level::Low, OutputConfig::default());

    let bus = match ExclusiveDevice::new_no_delay(spi, cs) {
        Ok(bus) => bus,
        Err(e) => {
            log::error!("display: could not claim the chip select pin: {e:?}");
            return None;
        }
    };
    let di = SpiInterface::new(bus, dc, SPI_BUF.init([0; 512]));

    let mut delay = Delay::new();
    let built = Builder::new(ST7796, di)
        .reset_pin(rst)
        // The controller is 320x480 portrait; Deg90 gives the 480x320 landscape
        // the UI crate draws.
        .display_size(320, 480)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .color_order(if cfg.bgr {
            ColorOrder::Bgr
        } else {
            ColorOrder::Rgb
        })
        .invert_colors(if cfg.invert_colors {
            ColorInversion::Inverted
        } else {
            ColorInversion::Normal
        })
        .init(&mut delay);

    match built {
        Ok(lcd) => {
            let mut backlight = backlight;
            backlight.set_high();
            log::info!("display: ST7796 initialised ({} MHz)", cfg.spi_mhz);
            Some((lcd, backlight))
        }
        Err(e) => {
            log::error!("display: init failed: {e:?}");
            None
        }
    }
}

/// Feed MQTT messages to the panel state and redraw when something changed.
/// Without a display it still parses and logs, which keeps the serial log
/// useful while the screen is unavailable or misbehaving.
#[embassy_executor::task]
pub async fn run(
    mut lcd: Option<(Lcd, Output<'static>)>,
    messages: Receiver<'static, CriticalSectionRawMutex, Message, 4>,
) {
    let mut panel = Panel::new();

    // First frame: "Waiting for the bridge".
    draw(&mut lcd, &panel);

    loop {
        let msg = messages.receive().await;
        let update = panel.handle(msg.topic, &msg.payload);
        log::info!(
            "panel: {} update, redraw={} led={:?} error={:?}",
            topic_name(msg.topic),
            update.redraw,
            update.led,
            panel.last_error(),
        );
        if update.redraw {
            draw(&mut lcd, &panel);
        }
    }
}

fn draw(lcd: &mut Option<(Lcd, Output<'static>)>, panel: &Panel) {
    if let Some((lcd, _)) = lcd {
        // No clock yet (SNTP is a later step), so elapsed times are left out.
        if panel.draw(lcd, None).is_err() {
            log::warn!("display: draw failed");
        }
    }
}

fn topic_name(topic: Topic) -> &'static str {
    topic.suffix()
}
