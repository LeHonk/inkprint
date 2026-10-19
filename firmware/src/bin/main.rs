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
