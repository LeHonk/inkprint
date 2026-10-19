//! Device settings in flash, restarts, and the backend for the setup form.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_bootloader_esp_idf::partitions::{self, DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType};
use esp_storage::FlashStorage;
use log::{info, warn};
use setup::portal::SetupBackend;
use setup::{FlashRegion, Settings};

/// The `nvs` partition; settings live in its first sector.
type Store = FlashRegion<FlashStorage<'static>>;

static STORE: Mutex<CriticalSectionRawMutex, RefCell<Option<Store>>> = Mutex::new(RefCell::new(None));
static CURRENT: Mutex<CriticalSectionRawMutex, RefCell<Option<Settings>>> = Mutex::new(RefCell::new(None));

/// Opens the settings store and returns the saved settings, if any.
pub fn open_store(flash: esp_hal::peripherals::FLASH<'static>) -> Option<Settings> {
    let mut storage = FlashStorage::new(flash);
    let mut table_buf = alloc::vec![0u8; PARTITION_TABLE_MAX_LEN];
    let nvs = partitions::read_partition_table(&mut storage, &mut table_buf)
        .ok()
        .and_then(|t| t.iter().find(|p| p.partition_type() == PartitionType::Data(DataPartitionSubType::Nvs)))
        .map(|p| (p.offset(), p.len()));
    let Some((offset, len)) = nvs else {
        warn!("settings: no nvs partition; settings cannot be saved");
        return None;
    };
    let mut region = FlashRegion::new(storage, offset, len);
    let saved = setup::load(&mut region).unwrap_or_else(|e| {
        warn!("settings: flash read failed: {e:?}");
        None
    });
    STORE.lock(|s| *s.borrow_mut() = Some(region));
    saved
}

pub fn set_current(settings: Settings) {
    CURRENT.lock(|c| *c.borrow_mut() = Some(settings));
}

pub fn current() -> Settings {
    CURRENT.lock(|c| c.borrow().clone()).unwrap_or_default()
}

/// Writes settings to flash and schedules a restart to apply them.
pub fn save(settings: &Settings) -> Result<(), &'static str> {
    STORE.lock(|s| match s.borrow_mut().as_mut() {
        Some(store) => setup::save(store, settings).map_err(|e| {
            warn!("settings: flash write failed: {e:?}");
            "Could not write the settings to flash."
        }),
        None => Err("This device has no settings partition."),
    })?;
    set_current(settings.clone());
    REBOOT.signal(());
    Ok(())
}

/// Survives a software reset (not power-on: then it holds garbage, hence the magic).
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut BOOT_REQUEST: u32 = 0;
const SETUP_REQUESTED: u32 = 0x5E7A_9B01;

/// Whether the previous run asked to boot into setup mode. Clears the request.
pub fn take_setup_request() -> bool {
    // SAFETY: single-threaded at boot; volatile because the value outlives the program.
    unsafe {
        let request = (&raw mut BOOT_REQUEST).read_volatile();
        (&raw mut BOOT_REQUEST).write_volatile(0);
        request == SETUP_REQUESTED
    }
}

/// Restarts into setup mode (access point and portal).
pub fn restart_into_setup() -> ! {
    // SAFETY: as above; nothing else touches the request.
    unsafe { (&raw mut BOOT_REQUEST).write_volatile(SETUP_REQUESTED) };
    esp_hal::system::software_reset()
}

/// Settings were saved; [`reboot_task`] restarts after the response went out.
pub static REBOOT: Signal<CriticalSectionRawMutex, ()> = Signal::new();

#[embassy_executor::task]
pub async fn reboot_task() {
    REBOOT.wait().await;
    Timer::after(Duration::from_secs(2)).await;
    info!("restarting to apply settings");
    esp_hal::system::software_reset()
}

/// The setup form's view of this device.
#[derive(Clone, Copy)]
pub struct DeviceSetup {
    pub name: &'static str,
    /// Networks from the scan at the start of setup mode (empty on the LAN form).
    pub networks: &'static [String],
}

impl SetupBackend for DeviceSetup {
    fn name(&self) -> String {
        self.name.into()
    }

    fn current(&self) -> Settings {
        current()
    }

    fn networks(&self) -> Vec<String> {
        self.networks.to_vec()
    }

    fn save(&self, settings: &Settings) -> Result<(), &'static str> {
        save(settings)
    }
}
