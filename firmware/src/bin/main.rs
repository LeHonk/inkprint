#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use log::info;

use board::{Board, Selected};
use ipp_printer::{Config, Printer};
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
use {
    epd_inkplate::inkplate6::Inkplate6Panel,
    esp_hal::delay::Delay,
    log::warn,
    esp_hal::gpio::Pin,
    esp_hal::i2c::master::{Config as I2cConfig, I2c},
    inkprint_firmware::panel_bus::{I2sPanelBus, PanelPins},
};

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.4.0
    // generator parameters: -o esp32 -o esp32-wrover-e -o unstable-hal -o alloc -o wifi -o embassy -o esp-backtrace -o log

    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // The following pins are used to bootstrap the chip. They are available
    // for use, but check the datasheet of the module for more information on them.
    // - GPIO0
    // - GPIO2
    // - GPIO5
    // - GPIO12
    // - GPIO15
    // These GPIO pins are in use by some feature of the module and should not be used.
    let _gpio6 = peripherals.GPIO6;
    let _gpio7 = peripherals.GPIO7;
    let _gpio8 = peripherals.GPIO8;
    let _gpio9 = peripherals.GPIO9;
    let _gpio10 = peripherals.GPIO10;
    let _gpio11 = peripherals.GPIO11;
    let _gpio16 = peripherals.GPIO16;
    let _gpio17 = peripherals.GPIO17;
    let _gpio20 = peripherals.GPIO20;

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98768);
    // Page buffers (~0.5-2 MB) are allocated from PSRAM.
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    info!("Embassy initialized!");

    #[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
    {
        let mut i2c = I2c::new(peripherals.I2C0, I2cConfig::default())
            .expect("I2C config")
            .with_sda(peripherals.GPIO21)
            .with_scl(peripherals.GPIO22);
        log_i2c_devices(&mut i2c);
        let bus = I2sPanelBus::new(
            peripherals.I2S1,
            PanelPins {
                cl: peripherals.GPIO0.degrade(),
                data: [
                    peripherals.GPIO4.degrade(),
                    peripherals.GPIO5.degrade(),
                    peripherals.GPIO18.degrade(),
                    peripherals.GPIO19.degrade(),
                    peripherals.GPIO23.degrade(),
                    peripherals.GPIO25.degrade(),
                    peripherals.GPIO26.degrade(),
                    peripherals.GPIO27.degrade(),
                ],
                le: peripherals.GPIO2.degrade(),
                ckv: peripherals.GPIO32.degrade(),
                sph: peripherals.GPIO33.degrade(),
            },
        );
        #[cfg(feature = "panel-test")]
        check_psram();
        // Does not touch the panel: whatever it shows stays visible.
        match Inkplate6Panel::<Selected, _, _, _>::new(bus, i2c, Delay::new()) {
            Ok(mut panel) => {
                if !panel.has_ext_expander() {
                    info!("no external IO expander found (optional)");
                }
                match panel.temperature() {
                    Ok(t) => info!("panel temperature: {t} °C"),
                    Err(e) => warn!("reading panel temperature: {e:?}"),
                }
                #[cfg(feature = "panel-test")]
                show_test_pattern(&mut panel);
                // TODO: hand `panel` to a display task fed by the IPP job pipeline.
            }
            Err(e) => warn!("panel init failed, running without display: {e:?}"),
        }
    }

    let _wifi_controller =
        esp_radio::wifi::WifiController::new(peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    let _wifi_interface = esp_radio::wifi::Interface::station();

    let (width, height) = Selected::logical_size();
    info!("{}: {}x{} portrait canvas", Selected::MODEL, width, height);

    let _printer: Printer<Selected> = Printer::new(Config {
        name: "Inkplate".into(),
        // TODO: derive from the eFuse MAC so each device has a stable, unique UUID.
        uuid: "urn:uuid:6c1f3f5e-1d9a-4c1e-9a52-000000000001".into(),
        // TODO: fill in the DHCP address / mDNS hostname once the network is up.
        uri: "ipp://inkplate.local:631/ipp/print".into(),
        location: "Kitchen".into(),
        more_info: "http://inkplate.local/".into(),
    });

    // TODO (milestones 2, 5, 6 in PLAN.md):
    // - display: implement epd_inkplate::ParallelBus with direct GPIO register writes,
    //   bring up the IO expander + TPS65186 and show a test pattern. Never clear at boot.
    // - wifi: connect using option_env!("WIFI_SSID") / option_env!("WIFI_PASSWORD"),
    //   run an embassy-net stack with DHCP.
    // - mdns: edge-mdns responder for _ipp._tcp (+ _print, _universal subtypes) with
    //   ipp_printer::mdns_txt_records::<Selected>().
    // - ipp: edge-http server on :631 POST /ipp/print -> Message::parse ->
    //   Printer::handle -> raster::Decoder -> render::PageCapture -> display task.
    let _ = spawner;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.2.2/examples
}

/// Gray ramp in 8 bands across the native panel width (black at native x = 0), with a
/// white square in the native top-left corner (inside the black band) to check orientation.
#[cfg(feature = "panel-test")]
fn show_test_pattern(panel: &mut impl epd_inkplate::EpdDriver<Error: core::fmt::Debug>) {
    let (w, h) = (Selected::WIDTH as usize, Selected::HEIGHT as usize);
    let frame: alloc::vec::Vec<u8> = (0..h)
        .flat_map(|y| (0..w).map(move |x| if x < 100 && y < 100 { 7 } else { (x * 8 / w) as u8 }))
        .collect();
    info!("drawing test pattern");
    match panel.show(epd_inkplate::Frame::Gray3(&frame)) {
        Ok(()) => info!("test pattern done"),
        Err(e) => warn!("test pattern failed: {e:?}"),
    }
}

/// Logs every 7-bit address that acknowledges on the bus. The PMIC (0x48) only answers
/// while its WAKEUP pin is high, so it normally does not show up here.
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
fn log_i2c_devices(i2c: &mut I2c<'_, esp_hal::Blocking>) {
    let mut found = alloc::vec::Vec::new();
    for address in 0x08..0x78u8 {
        // A 1-byte read is valid everywhere (esp-hal rejects some zero-length transfers).
        if i2c.read(address, &mut [0]).is_ok() {
            found.push(address);
        }
    }
    info!("I2C devices: {found:02x?}");
}

/// Writes and reads back 1 MB, which only fits in PSRAM. Rev 1 ESP32s have a PSRAM cache
/// bug that Rust code does not work around; mismatches here show it.
#[cfg(feature = "panel-test")]
fn check_psram() {
    const WORDS: usize = 256 * 1024;
    let pattern = |i: usize| (i as u32).wrapping_mul(0x9E37_79B9) ^ 0xA5A5_5A5A;
    let mut buf: alloc::vec::Vec<u32> = alloc::vec::Vec::with_capacity(WORDS);
    buf.extend((0..WORDS).map(pattern));
    let bad = buf.iter().enumerate().filter(|&(i, &v)| v != pattern(i)).count();
    info!("PSRAM check at {:p}: {bad} of {WORDS} words wrong", buf.as_ptr());
}
