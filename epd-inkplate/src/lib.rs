// SPDX-License-Identifier: LGPL-3.0-only
//! Display drivers for the Inkplate family.
//!
//! Ported from Soldered's Inkplate-Arduino-library (LGPL-3.0), which is why this crate is
//! LGPL-3.0 while the rest of the workspace is MIT OR Apache-2.0:
//! <https://github.com/SolderedElectronics/Inkplate-Arduino-library>, `src/boards/Inkplate6`,
//! `src/features/TPS65186`, `src/system/{mcpExpander,pcalExpander,UtilI2S}`.
//!
//! The chip-specific part (I2S/DMA data bus and the GPIO-driven control lines) sits behind
//! [`PanelBus`]; the firmware implements it with direct register access. Everything else —
//! scan sequencing, waveforms, PMIC and IO expanders over `embedded-hal` I2C — lives here
//! and is unit-tested on the host.
//!
//! Drivers never clear the panel on init: the last printed page must survive reboots.

#![no_std]

extern crate alloc;

pub mod expander;
pub mod inkplate6;
pub mod luts;
pub mod rtc;
pub mod tps65186;

/// A frame in the panel's native orientation, one byte per pixel, `WIDTH × HEIGHT`, row-major.
#[derive(Debug, Clone, Copy)]
pub enum Frame<'a> {
    /// Levels 0 (black) … 7 (white).
    Gray3(&'a [u8]),
    /// 0 = black, anything else = white.
    Mono(&'a [u8]),
}

pub trait EpdDriver {
    type Error;

    /// Full refresh (with clean cycles) to show `frame`.
    fn show(&mut self, frame: Frame<'_>) -> Result<(), Self::Error>;

    /// Turns off the high-voltage rails; the image stays visible.
    fn sleep(&mut self) -> Result<(), Self::Error>;
}

/// The ESP32 side of the parallel panel interface.
///
/// Data goes out over 8 data lines plus the CL clock (I2S1 in LCD mode with DMA on the
/// Inkplate 6); CKV, SPH and LE are plain GPIOs. SPV, OE and GMOD are on the IO expander
/// and handled by the driver.
pub trait PanelBus {
    /// Gate driver clock (CKV).
    fn set_ckv(&mut self, high: bool);
    /// Source driver start pulse (SPH).
    fn set_sph(&mut self, high: bool);
    /// Source driver latch enable (LE).
    fn set_le(&mut self, high: bool);

    /// Clocks one row of `WIDTH / 4` bytes into the source driver, like the Arduino
    /// `sendDataI2S`: arm the DMA, SPH low, CKV high, transmit, wait for completion, SPH high.
    ///
    /// `row` is in the byte order the Arduino driver writes into its I2S DMA buffer; the
    /// implementation must use the same I2S FIFO mode (`tx_fifo_mod = 1`) so the bytes reach
    /// the panel in the same order.
    fn send_row(&mut self, row: &[u8]);

    /// Routes data/CL pins to the bus and makes CKV/SPH/LE outputs (`pinsAsOutputs`).
    fn enable(&mut self);
    /// Puts all panel pins in high impedance (`pinsZstate`).
    fn disable(&mut self);
}
