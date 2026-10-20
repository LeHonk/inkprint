// SPDX-License-Identifier: LGPL-3.0-only
//! NXP PCF85063A real-time clock, fitted on newer Inkplate boards (not on early
//! Inkplate 6). Port of the Arduino library's `features/rtc`: 24-hour mode, years
//! 2000–2099, and the RAM byte set to 0xAA once the time has been written.
//!
//! In addition to upstream's RAM marker, the oscillator-stop flag (seconds bit 7) is
//! checked: the chip sets it when it lost power, and the time is then meaningless.

use clock::DateTime;
use embedded_hal::i2c::I2c;

pub const ADDRESS: u8 = 0x51;

const REG_RAM: u8 = 0x03;
const SET_MARKER: u8 = 0xAA;
const OSCILLATOR_STOPPED: u8 = 0x80;

fn bcd(v: u8) -> u8 {
    (v / 10) << 4 | (v % 10)
}

fn from_bcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}

pub struct Pcf85063<I2C> {
    i2c: I2C,
}

impl<I2C: I2c> Pcf85063<I2C> {
    pub fn new(i2c: I2C) -> Self {
        Self { i2c }
    }

    /// The current time, or `Ok(None)` if the clock was never set or lost power. An
    /// error (no acknowledge) usually means the board has no RTC.
    pub fn read(&mut self) -> Result<Option<DateTime>, I2C::Error> {
        // RAM byte, then seconds … years (0x04–0x0A).
        let mut r = [0u8; 8];
        self.i2c.write_read(ADDRESS, &[REG_RAM], &mut r)?;
        if r[0] != SET_MARKER || r[1] & OSCILLATOR_STOPPED != 0 {
            return Ok(None);
        }
        let t = DateTime {
            second: from_bcd(r[1] & 0x7F),
            minute: from_bcd(r[2] & 0x7F),
            hour: from_bcd(r[3] & 0x3F),
            day: from_bcd(r[4] & 0x3F),
            weekday: r[5] & 0x07,
            month: from_bcd(r[6] & 0x1F),
            year: 2000 + u16::from(from_bcd(r[7])),
        };
        // Garbage that happens to carry the marker.
        Ok(t.to_unix().map(|_| t))
    }

    /// Sets the clock (UTC). Writing the seconds also clears the oscillator-stop flag.
    pub fn set(&mut self, t: &DateTime) -> Result<(), I2C::Error> {
        let year = t.year.clamp(2000, 2099) - 2000;
        self.i2c.write(
            ADDRESS,
            &[
                REG_RAM,
                SET_MARKER,
                bcd(t.second),
                bcd(t.minute),
                bcd(t.hour),
                bcd(t.day),
                t.weekday,
                bcd(t.month),
                bcd(year as u8),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embedded_hal::i2c::{ErrorType, Operation};

    use super::*;

    /// Register file with an auto-incrementing pointer, like the chip.
    struct Chip {
        regs: [u8; 0x12],
        present: bool,
    }

    #[derive(Debug)]
    struct Nack;

    impl embedded_hal::i2c::Error for Nack {
        fn kind(&self) -> embedded_hal::i2c::ErrorKind {
            embedded_hal::i2c::ErrorKind::NoAcknowledge(embedded_hal::i2c::NoAcknowledgeSource::Address)
        }
    }

    impl ErrorType for Chip {
        type Error = Nack;
    }

    impl I2c for Chip {
        fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), Nack> {
            if !self.present || address != ADDRESS {
                return Err(Nack);
            }
            let mut ptr = 0usize;
            for op in ops {
                match op {
                    Operation::Write(bytes) => {
                        ptr = bytes[0] as usize;
                        for &b in &bytes[1..] {
                            self.regs[ptr] = b;
                            ptr += 1;
                        }
                    }
                    Operation::Read(buf) => {
                        for b in buf.iter_mut() {
                            *b = self.regs[ptr];
                            ptr += 1;
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn rtc() -> Pcf85063<Chip> {
        // Power-on state: oscillator-stop flag set, RAM cleared.
        let mut regs = [0u8; 0x12];
        regs[0x04] = OSCILLATOR_STOPPED;
        Pcf85063::new(Chip { regs, present: true })
    }

    #[test]
    fn unset_clock_reads_none_then_round_trips() {
        let mut rtc = rtc();
        assert_eq!(rtc.read().unwrap(), None);
        let t = DateTime::from_unix(1_792_438_861); // 2026-10-19T19:41:01Z, a Monday
        rtc.set(&t).unwrap();
        assert_eq!(rtc.read().unwrap(), Some(t));
        // BCD on the wire, as the datasheet wants.
        assert_eq!(&rtc.i2c.regs[0x03..0x0B], &[0xAA, 0x01, 0x41, 0x19, 0x19, 1, 0x10, 0x26]);
    }

    #[test]
    fn power_loss_invalidates_the_time() {
        let mut rtc = rtc();
        rtc.set(&DateTime::from_unix(1_792_438_861)).unwrap();
        rtc.i2c.regs[0x04] |= OSCILLATOR_STOPPED;
        assert_eq!(rtc.read().unwrap(), None);
    }

    #[test]
    fn missing_chip_is_an_error() {
        let mut rtc = Pcf85063::new(Chip { regs: [0; 0x12], present: false });
        assert!(rtc.read().is_err());
    }
}
