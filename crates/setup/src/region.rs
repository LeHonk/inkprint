//! A window onto part of a flash device, so settings code sees offsets from 0.

use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash};

/// `len` bytes of `flash` starting at `offset` (e.g. the `nvs` partition).
pub struct FlashRegion<F> {
    flash: F,
    offset: u32,
    len: u32,
}

impl<F> FlashRegion<F> {
    pub fn new(flash: F, offset: u32, len: u32) -> Self {
        Self { flash, offset, len }
    }

    fn range(&self, from: u32, n: usize) -> Result<u32, RegionError<core::convert::Infallible>> {
        match from.checked_add(n as u32) {
            Some(end) if end <= self.len => Ok(self.offset + from),
            _ => Err(RegionError::OutOfBounds),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegionError<E> {
    OutOfBounds,
    Flash(E),
}

impl<E: NorFlashError> NorFlashError for RegionError<E> {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            RegionError::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            RegionError::Flash(e) => e.kind(),
        }
    }
}

fn bounds<E>(_: RegionError<core::convert::Infallible>) -> RegionError<E> {
    RegionError::OutOfBounds
}

impl<F: ErrorType> ErrorType for FlashRegion<F> {
    type Error = RegionError<F::Error>;
}

impl<F: ReadNorFlash> ReadNorFlash for FlashRegion<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let at = self.range(offset, bytes.len()).map_err(bounds)?;
        self.flash.read(at, bytes).map_err(RegionError::Flash)
    }

    fn capacity(&self) -> usize {
        self.len as usize
    }
}

impl<F: NorFlash> NorFlash for FlashRegion<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let start = self.range(from, to.saturating_sub(from) as usize).map_err(bounds)?;
        self.flash.erase(start, start + (to - from)).map_err(RegionError::Flash)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let at = self.range(offset, bytes.len()).map_err(bounds)?;
        self.flash.write(at, bytes).map_err(RegionError::Flash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{RamFlash, settings};
    use alloc::vec;

    #[test]
    fn settings_land_inside_the_region_only() {
        let mut region = FlashRegion::new(RamFlash(vec![0x11; 3 * 4096]), 4096, 4096);
        crate::save(&mut region, &settings()).unwrap();
        assert_eq!(crate::load(&mut region).unwrap(), Some(settings()));
        let raw = &region.flash.0;
        assert!(raw[..4096].iter().all(|&b| b == 0x11));
        assert!(raw[8192..].iter().all(|&b| b == 0x11));
        assert_eq!(&raw[4096..4100], b"INKS");
    }

    #[test]
    fn out_of_bounds() {
        let mut region = FlashRegion::new(RamFlash(vec![0; 8192]), 4096, 4096);
        assert_eq!(region.read(4092, &mut [0; 8]), Err(RegionError::OutOfBounds));
        assert_eq!(region.erase(0, 8192), Err(RegionError::OutOfBounds));
    }
}
