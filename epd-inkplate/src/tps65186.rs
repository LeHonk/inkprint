// SPDX-License-Identifier: LGPL-3.0-only
//! TI TPS65186 e-paper PMIC (high-voltage rails, VCOM, panel temperature).
//!
//! Port of `features/TPS65186`. WAKEUP, PWRUP and VCOM are IO expander pins, so the
//! sequences live on the driver, which owns the expander; this module holds the register
//! map and the I2C transactions.

use embedded_hal::i2c::I2c;

pub const ADDRESS: u8 = 0x48;

pub const REG_TEMP: u8 = 0x00;
pub const REG_ENABLE: u8 = 0x01;
pub const REG_UPSEQ0: u8 = 0x09;
pub const REG_DWNSEQ0: u8 = 0x0B;
pub const REG_TMST1: u8 = 0x0D;
pub const REG_PWRGOOD: u8 = 0x0F;

/// PG register value when all rails are up.
pub const PWR_GOOD_OK: u8 = 0b1111_1010;

/// Rail power-up/down sequence written by `TPS65186::begin` (UPSEQ0, UPSEQ1, DWNSEQ0, DWNSEQ1).
pub const BEGIN_SEQUENCE: [u8; 4] = [0x1B, 0x00, 0x1B, 0x00];
/// Written to UPSEQ0 / DWNSEQ0 on every power-up.
pub const POWER_UP_UPSEQ0: u8 = 0xE4;
pub const POWER_UP_DWNSEQ0: u8 = 0x1B;
/// ENABLE register value with all rails enabled.
pub const ENABLE_RAILS: u8 = 0x20;

pub fn write_reg<I: I2c>(i2c: &mut I, reg: u8, value: u8) -> Result<(), I::Error> {
    i2c.write(ADDRESS, &[reg, value])
}

pub fn read_reg<I: I2c>(i2c: &mut I, reg: u8) -> Result<u8, I::Error> {
    let mut buf = [0];
    i2c.write_read(ADDRESS, &[reg], &mut buf)?;
    Ok(buf[0])
}

pub fn write_begin_sequence<I: I2c>(i2c: &mut I) -> Result<(), I::Error> {
    let [a, b, c, d] = BEGIN_SEQUENCE;
    i2c.write(ADDRESS, &[REG_UPSEQ0, a, b, c, d])
}

pub fn enable_rails<I: I2c>(i2c: &mut I, enable: bool) -> Result<(), I::Error> {
    write_reg(i2c, REG_ENABLE, if enable { ENABLE_RAILS } else { 0 })
}

pub fn read_power_good<I: I2c>(i2c: &mut I) -> Result<u8, I::Error> {
    read_reg(i2c, REG_PWRGOOD)
}

/// Starts a temperature conversion; read [`REG_TEMP`] about 5 ms later.
pub fn start_temperature<I: I2c>(i2c: &mut I) -> Result<(), I::Error> {
    write_reg(i2c, REG_TMST1, 0x80)
}
