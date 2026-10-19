//! Device settings (Wi-Fi network, password, printer location) and the setup portal.
//!
//! - [`Settings`] are stored as one small checksummed record at the start of a flash
//!   region ([`load`], [`save`]); anything else found there reads as "no settings".
//! - [`form`] parses and renders the setup form.
//! - [`portal`] serves it with edge-http, either as a captive portal on the device's own
//!   access point or at `/setup` next to the IPP server.

#![no_std]
// The futures run on single-threaded executors (embassy, async-io); `Send` is not needed.
#![allow(async_fn_in_trait)]

extern crate alloc;

pub mod form;
pub mod portal;
mod region;

pub use region::{FlashRegion, RegionError};

use alloc::string::String;
use alloc::vec::Vec;

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};

/// Location used until the user sets one.
pub const DEFAULT_LOCATION: &str = "Kitchen";

pub const MAX_SSID: usize = 32;
/// WPA2 passphrases are 8–63 characters; empty means an open network.
pub const MIN_PASSWORD: usize = 8;
pub const MAX_PASSWORD: usize = 63;
pub const MAX_LOCATION: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub ssid: String,
    pub password: String,
    pub location: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { ssid: String::new(), password: String::new(), location: String::from(DEFAULT_LOCATION) }
    }
}

impl Settings {
    /// Whether there is a network to join.
    pub fn has_wifi(&self) -> bool {
        !self.ssid.is_empty()
    }

    /// Checks the limits Wi-Fi and the record format impose. Errors are user-facing.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ssid.is_empty() {
            return Err("Choose or enter a Wi-Fi network.");
        }
        if self.ssid.len() > MAX_SSID {
            return Err("The network name is too long (at most 32 bytes).");
        }
        if !self.password.is_empty() && !(MIN_PASSWORD..=MAX_PASSWORD).contains(&self.password.len()) {
            return Err("The password must be 8 to 63 characters, or empty for an open network.");
        }
        if self.location.trim().is_empty() {
            return Err("Enter a location, for example Kitchen.");
        }
        if self.location.len() > MAX_LOCATION {
            return Err("The location is too long (at most 63 bytes).");
        }
        Ok(())
    }
}

const MAGIC: &[u8; 4] = b"INKS";
const VERSION: u8 = 1;
/// Upper bound of an encoded record: header, three length-prefixed fields, CRC.
const RECORD_MAX: usize = 4 + 1 + 3 * (1 + 255) + 4;

/// `MAGIC, VERSION, (len u8, bytes) × 3 [ssid, password, location], CRC-32 (LE)`.
fn encode(s: &Settings) -> Vec<u8> {
    let mut out = Vec::with_capacity(RECORD_MAX);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    for field in [&s.ssid, &s.password, &s.location] {
        out.push(field.len() as u8);
        out.extend_from_slice(field.as_bytes());
    }
    out.extend_from_slice(&crc32(&out).to_le_bytes());
    out
}

fn decode(bytes: &[u8]) -> Option<Settings> {
    if bytes.get(..4)? != MAGIC || *bytes.get(4)? != VERSION {
        return None;
    }
    let mut pos = 5;
    let mut fields: [String; 3] = Default::default();
    for field in &mut fields {
        let len = *bytes.get(pos)? as usize;
        let data = bytes.get(pos + 1..pos + 1 + len)?;
        *field = String::from(core::str::from_utf8(data).ok()?);
        pos += 1 + len;
    }
    let crc = u32::from_le_bytes(bytes.get(pos..pos + 4)?.try_into().ok()?);
    if crc != crc32(&bytes[..pos]) {
        return None;
    }
    let [ssid, password, location] = fields;
    Some(Settings { ssid, password, location })
}

/// Reads the settings record at offset 0. `Ok(None)` if the region holds no valid record
/// (erased, foreign data, corrupted).
pub fn load<F: ReadNorFlash>(flash: &mut F) -> Result<Option<Settings>, F::Error> {
    // Reads must be whole read units (4 bytes on the ESP32); the buffer is a multiple of 4.
    let mut buf = [0u8; RECORD_MAX.next_multiple_of(4)];
    let len = buf.len().min(flash.capacity()) / F::READ_SIZE * F::READ_SIZE;
    flash.read(0, &mut buf[..len])?;
    Ok(decode(&buf[..len]))
}

/// Erases the first sector and writes the record there.
pub fn save<F: NorFlash>(flash: &mut F, settings: &Settings) -> Result<(), F::Error> {
    let mut record = encode(settings);
    // Writes must be whole write units; 0xFF leaves the padding erased.
    record.resize(record.len().next_multiple_of(F::WRITE_SIZE), 0xFF);
    flash.erase(0, F::ERASE_SIZE as u32)?;
    flash.write(0, &record)
}

/// CRC-32 (IEEE 802.3), bitwise: the record is tiny.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::vec;
    use embedded_storage::nor_flash::{ErrorType, NorFlashError, NorFlashErrorKind};

    /// NOR flash in RAM: erase sets 0xFF, writes can only clear bits.
    pub struct RamFlash(pub Vec<u8>);

    #[derive(Debug, PartialEq)]
    pub struct RamError;

    impl NorFlashError for RamError {
        fn kind(&self) -> NorFlashErrorKind {
            NorFlashErrorKind::OutOfBounds
        }
    }

    impl ErrorType for RamFlash {
        type Error = RamError;
    }

    /// Like esp-storage: reads in whole 4-byte words only.
    impl ReadNorFlash for RamFlash {
        const READ_SIZE: usize = 4;
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), RamError> {
            if !offset.is_multiple_of(4) || !bytes.len().is_multiple_of(4) {
                return Err(RamError);
            }
            let src = self.0.get(offset as usize..offset as usize + bytes.len()).ok_or(RamError)?;
            bytes.copy_from_slice(src);
            Ok(())
        }
        fn capacity(&self) -> usize {
            self.0.len()
        }
    }

    impl NorFlash for RamFlash {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 4096;
        fn erase(&mut self, from: u32, to: u32) -> Result<(), RamError> {
            self.0.get_mut(from as usize..to as usize).ok_or(RamError)?.fill(0xFF);
            Ok(())
        }
        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), RamError> {
            assert_eq!(offset as usize % 4, 0);
            assert_eq!(bytes.len() % 4, 0);
            let dst = self.0.get_mut(offset as usize..offset as usize + bytes.len()).ok_or(RamError)?;
            for (d, s) in dst.iter_mut().zip(bytes) {
                *d &= s;
            }
            Ok(())
        }
    }

    pub fn settings() -> Settings {
        Settings { ssid: "Home Wi-Fi".into(), password: "correct horse".into(), location: "Kök".into() }
    }

    #[test]
    fn roundtrip_over_old_contents() {
        let mut flash = RamFlash(vec![0x5A; 8192]); // e.g. leftover ESP-IDF NVS data
        assert_eq!(load(&mut flash).unwrap(), None);
        save(&mut flash, &settings()).unwrap();
        assert_eq!(load(&mut flash).unwrap(), Some(settings()));
        let mut other = settings();
        other.location = "Hall".into();
        save(&mut flash, &other).unwrap();
        assert_eq!(load(&mut flash).unwrap(), Some(other));
    }

    #[test]
    fn rejects_erased_and_corrupted_records() {
        let mut flash = RamFlash(vec![0xFF; 4096]);
        assert_eq!(load(&mut flash).unwrap(), None);
        save(&mut flash, &settings()).unwrap();
        flash.0[8] ^= 0x01;
        assert_eq!(load(&mut flash).unwrap(), None);
    }

    #[test]
    fn validation() {
        assert_eq!(settings().validate(), Ok(()));
        let with = |f: fn(&mut Settings)| {
            let mut s = settings();
            f(&mut s);
            s.validate()
        };
        assert!(with(|s| s.ssid.clear()).is_err());
        assert!(with(|s| s.ssid = "x".repeat(33)).is_err());
        assert!(with(|s| s.password = "short".into()).is_err());
        assert_eq!(with(|s| s.password.clear()), Ok(())); // open network
        assert!(with(|s| s.location = "  ".into()).is_err());
    }

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
