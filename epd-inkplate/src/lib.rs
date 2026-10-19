//! Display drivers for the Inkplate family.
//!
//! Only the timing-critical bus access is board/HAL specific, behind [`ParallelBus`]; the
//! firmware implements it with direct GPIO register writes. Everything else uses
//! `embedded-hal` traits so this crate builds and unit-tests on the host.
//!
//! Drivers never clear the panel on init: the last printed page must survive reboots.
//!
//! References for porting: Soldered `Inkplate-Arduino-library` (Inkplate6 driver,
//! waveform tables, TPS65186 power sequence) and turgu1/ESP-IDF-InkPlate.

#![no_std]

pub mod parallel;
pub mod tps65186;

/// A frame in the panel's native orientation, one byte per pixel, `WIDTH × HEIGHT`.
#[derive(Debug, Clone, Copy)]
pub enum Frame<'a> {
    /// Levels 0 (black) … 7 (white).
    Gray3(&'a [u8]),
    /// 0 = black, 1 = white.
    Mono(&'a [u8]),
}

pub trait EpdDriver {
    type Error;

    /// Powers up the panel rails without changing what is displayed.
    fn init(&mut self) -> Result<(), Self::Error>;

    /// Full refresh (with clean cycles) to show `frame`.
    fn show(&mut self, frame: Frame<'_>) -> Result<(), Self::Error>;

    /// Turns off the high-voltage rails; the image stays visible.
    fn sleep(&mut self) -> Result<(), Self::Error>;
}

/// Low-level parallel EPD bus: 8 data lines (4 pixels × 2 bits per byte) plus the source
/// driver clock/latch and gate driver lines. Implementations must be fast — a full
/// refresh writes the whole frame once per waveform phase.
pub trait ParallelBus {
    /// Starts a frame scan (gate driver to first row).
    fn vscan_start(&mut self);
    /// Starts a row, clocking in its first data byte.
    fn hscan_start(&mut self, first: u8);
    /// Clocks one data byte into the source driver.
    fn write(&mut self, byte: u8);
    /// Latches the row and advances the gate driver.
    fn row_end(&mut self);
    /// Ends the frame scan.
    fn vscan_end(&mut self);
}
