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
use log::{info, warn};

use board::{Board, Selected};
use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex;
use esp_radio::wifi::{Interface, WifiController};
use inkprint_firmware::net::{self, Identity, SharedPrinter};
use inkprint_firmware::{device, portal};
use ipp_printer::Printer;
use static_cell::StaticCell;
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
use {
    epd_inkplate::inkplate6::{Inkplate6Board, Inkplate6Panel},
    esp_hal::delay::Delay,
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
    #[allow(unused_mut, reason = "boards with a display driver reborrow the I2C pins")]
    let mut peripherals = esp_hal::init(config);

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

    #[allow(unused_mut, reason = "only boards with a display driver set it")]
    let mut display = false;
    #[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
    {
        recover_i2c_bus(peripherals.GPIO21.reborrow(), peripherals.GPIO22.reborrow());
        let mut i2c = I2c::new(peripherals.I2C0, I2cConfig::default())
            .expect("I2C config")
            .with_sda(peripherals.GPIO21)
            .with_scl(peripherals.GPIO22);
        let found = log_i2c_devices(&mut i2c);
        if option_env!("INKPRINT_I2C_DIAG").is_some() {
            i2c_diagnostics(&mut i2c);
        }
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
        // Probe the optional external expander only if the scan saw it: the transfer after an
        // expected NACK has been seen to time out on this chip (see i2c_diagnostics).
        let ext_present = found.contains(&<Selected as Inkplate6Board>::VARIANT.ext_address);
        match Inkplate6Panel::<Selected, _, _, _>::new(bus, i2c, Delay::new(), ext_present) {
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
                spawner.spawn(inkprint_firmware::display::display_task(panel).expect("spawn display"));
                display = true;
            }
            Err(e) => warn!("panel init failed, running without display: {e:?}"),
        }
    }

    let (width, height) = Selected::logical_size();
    info!("{}: {}x{} portrait canvas", Selected::MODEL, width, height);

    // Settings: flash first, then the compiled-in development fallback.
    let saved = device::open_store(peripherals.FLASH);
    let have_saved = saved.is_some();
    let settings = saved.or_else(net::compiled_in_settings);
    match &settings {
        Some(s) if have_saved => info!("settings: network {:?}, location {:?}", s.ssid, s.location),
        Some(s) => info!("settings: compiled-in network {:?}", s.ssid),
        None => info!("settings: none saved"),
    }
    device::set_current(settings.clone().unwrap_or_default());
    let setup_requested = device::take_setup_request();
    spawner.spawn(device::reboot_task().expect("spawn reboot"));

    let controller = match WifiController::new(peripherals.WIFI, Default::default()) {
        Ok(c) => c,
        Err(e) => {
            warn!("Wi-Fi init failed: {e:?}");
            loop {
                Timer::after(Duration::from_secs(3600)).await;
            }
        }
    };
    static IDENTITY: StaticCell<Identity> = StaticCell::new();
    let station = Interface::station();
    let identity: &'static Identity = IDENTITY.init(Identity::from_mac(station.mac_address()));
    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    match settings {
        Some(s) if s.has_wifi() && !setup_requested => {
            start_network(spawner, controller, station, identity, seed, s, display).await;
        }
        settings => {
            info!("setup mode ({})", if setup_requested { "requested after losing Wi-Fi" } else { "no Wi-Fi settings" });
            drop(station);
            // Only a device that was never set up gets instructions on the panel: otherwise
            // it may still show a recipe worth keeping.
            portal::run(spawner, controller, identity, seed, display && settings.is_none()).await;
        }
    }

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

/// Brings up Wi-Fi and DHCP, then starts mDNS and the IPP server.
#[allow(clippy::large_stack_frames, reason = "runs once at boot on main's stack, like main")]
async fn start_network(
    spawner: Spawner,
    controller: WifiController<'static>,
    interface: Interface,
    identity: &'static Identity,
    seed: u64,
    settings: setup::Settings,
    display: bool,
) {
    static PRINTER: StaticCell<SharedPrinter> = StaticCell::new();

    let config = identity.printer_config(&settings.location);
    let txt = net::txt_records(&config);
    let printer: &'static SharedPrinter = PRINTER.init(Mutex::new(RefCell::new(Printer::new(config))));
    net::publish(identity, printer);

    let (stack, runner) = net::stack(interface, net::dhcp_config(identity), seed);
    let (ssid, password) = (settings.ssid.leak(), settings.password.leak());
    spawner.spawn(net::wifi_task(controller, ssid, password).expect("spawn wifi"));
    spawner.spawn(net::net_task(runner).expect("spawn net"));
    spawner.spawn(net::connectivity_watchdog(stack).expect("spawn watchdog"));

    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        info!("IP address {} — printer at ipp://{}.local:{}/ipp/print", cfg.address, identity.hostname, net::IPP_PORT);
    }
    spawner.spawn(net::mdns_task(stack, identity, txt).expect("spawn mdns"));
    spawner.spawn(net::http_task(stack, printer, identity, display).expect("spawn http"));
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

/// I2C bus recovery. The IO expander and PMIC stay powered when the ESP32 resets (flashing,
/// restart after saving settings), so a reset in the middle of a transfer can leave one of
/// them holding SDA low, and every later transfer times out. Clock SCL until SDA is
/// released (at most 9 bits), then send a STOP.
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
fn recover_i2c_bus(sda: impl Pin, scl: impl Pin) {
    use esp_hal::gpio::{DriveMode, Flex, OutputConfig};

    let open_drain = OutputConfig::default().with_drive_mode(DriveMode::OpenDrain);
    let (mut sda, mut scl) = (Flex::new(sda), Flex::new(scl));
    for line in [&mut sda, &mut scl] {
        line.apply_output_config(&open_drain);
        line.set_input_enable(true);
        line.set_high(); // released: the board's pull-ups hold the line high
        line.set_output_enable(true);
    }
    let delay = Delay::new();
    delay.delay_micros(10);
    let (sda_low, scl_low) = (sda.is_low(), scl.is_low());
    let mut pulses = 0;
    while sda.is_low() && pulses < 9 {
        scl.set_low();
        delay.delay_micros(5);
        scl.set_high();
        delay.delay_micros(5);
        pulses += 1;
    }
    // STOP: SDA rises while SCL is high.
    sda.set_low();
    delay.delay_micros(5);
    scl.set_high();
    delay.delay_micros(5);
    sda.set_high();
    delay.delay_micros(5);
    if sda_low || scl_low {
        warn!(
            "I2C: bus was stuck (SDA low: {sda_low}, SCL low: {scl_low}); {pulses} clock pulses, SDA now {}",
            if sda.is_high() { "released" } else { "still low" }
        );
    }
}

/// The step that failed in panel init was a write to the expander (0x20) right after the
/// absent second expander (0x22) NACKed. Replays that pair, logging only after each pair (a
/// log line takes milliseconds): immediately, and with a 1 ms pause. The write puts back
/// the IODIRA value just read, so nothing changes. Enabled with INKPRINT_I2C_DIAG=1.
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
#[allow(clippy::large_stack_frames, reason = "diagnostic run once at boot on main's stack")]
fn i2c_diagnostics(i2c: &mut I2c<'_, esp_hal::Blocking>) {
    let mut iodir = [0u8; 1];
    let read = i2c.write_read(0x20, &[0x00], &mut iodir);
    info!("diag: read 0x20 IODIRA: {read:?} -> {:02x}", iodir[0]);
    let restore = [0x00, iodir[0]];
    let mut absent = [0u8; 22];

    let (nack, write) = (i2c.write_read(0x22, &[0x00], &mut absent), i2c.write(0x20, &restore));
    info!("diag A immediate:   NACK {nack:?}, then write {write:?}");

    let nack = i2c.write_read(0x22, &[0x00], &mut absent);
    Delay::new().delay_millis(1);
    let write = i2c.write(0x20, &restore);
    info!("diag C 1 ms gap:    NACK {nack:?}, then write {write:?}");
}

/// Logs every 7-bit address that acknowledges on the bus. The PMIC (0x48) only answers
/// while its WAKEUP pin is high, so it normally does not show up here.
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
fn log_i2c_devices(i2c: &mut I2c<'_, esp_hal::Blocking>) -> alloc::vec::Vec<u8> {
    let mut found = alloc::vec::Vec::new();
    for address in 0x08..0x78u8 {
        // A 1-byte read is valid everywhere (esp-hal rejects some zero-length transfers).
        if i2c.read(address, &mut [0]).is_ok() {
            found.push(address);
        }
    }
    info!("I2C devices: {found:02x?}");
    found
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
