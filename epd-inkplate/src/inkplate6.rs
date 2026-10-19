// SPDX-License-Identifier: LGPL-3.0-only
//! Inkplate 6 / 6 V2 panel driver (ED060SC4, 800×600), ported from
//! `boards/Inkplate6/Inkplate6Driver.cpp`.
//!
//! Full refreshes only (`display3b` / `display1b`); partial updates aren't needed to show
//! a printed page. VCOM programming, touchpad, SD card and battery reading are left out.

use alloc::vec;
use alloc::vec::Vec;
use core::marker::PhantomData;

use board::Board;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

use crate::expander::{Expander, Kind, Mode};
use crate::luts::{self, GrayLuts, GRAY3_PHASES};
use crate::tps65186 as tps;
use crate::{EpdDriver, Frame, PanelBus};

/// Board-specific differences within the Inkplate 6 family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variant {
    pub expander: Kind,
    /// Internal expander (panel control, PMIC pins).
    pub int_address: u8,
    /// External expander (user pins; all driven low to save power).
    pub ext_address: u8,
    /// Number of "darken" phases in a 1-bit refresh.
    pub mono_phases: usize,
}

pub const INKPLATE6: Variant =
    Variant { expander: Kind::Mcp23017, int_address: 0x20, ext_address: 0x22, mono_phases: 4 };
pub const INKPLATE6_V2: Variant =
    Variant { expander: Kind::Pcal6416a, int_address: 0x20, ext_address: 0x21, mono_phases: 5 };

/// Boards this driver supports.
pub trait Inkplate6Board: Board {
    const VARIANT: Variant;
}

impl Inkplate6Board for board::Inkplate6 {
    const VARIANT: Variant = INKPLATE6;
}

impl Inkplate6Board for board::Inkplate6V2 {
    const VARIANT: Variant = INKPLATE6_V2;
}

/// Internal expander pins (`pins.h`, `defines.h`).
mod pin {
    pub const OE: u8 = 0;
    pub const GMOD: u8 = 1;
    pub const SPV: u8 = 2;
    pub const WAKEUP: u8 = 3;
    pub const PWRUP: u8 = 4;
    pub const VCOM: u8 = 5;
    pub const GPIO0_ENABLE: u8 = 8;
    pub const BATTERY_MOSFET: u8 = 9;
    pub const SD_PMOS: u8 = 10;
}

/// How long to wait for the PMIC power-good register, in ms.
const POWER_TIMEOUT_MS: u32 = 250;
/// Pause after each full-frame scan, in µs.
const FRAME_GAP_US: u32 = 230;

/// I2C devices the driver talks to, for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    IoExpander,
    ExtExpander,
    Pmic,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// A device did not respond during [`Inkplate6Panel::new`].
    Init { device: Device, error: E },
    I2c(E),
    /// The PMIC did not report all rails good within 250 ms.
    PowerGood,
    /// Frame length does not match the panel size.
    FrameSize,
}

impl<E> From<E> for Error<E> {
    fn from(e: E) -> Self {
        Error::I2c(e)
    }
}

pub struct Inkplate6Panel<B, BUS, I2C, D> {
    bus: BUS,
    i2c: I2C,
    delay: D,
    io: Expander,
    ext: Option<Expander>,
    on: bool,
    gray: GrayLuts,
    row: Vec<u8>,
    _board: PhantomData<B>,
}

impl<B, BUS, I2C, D> Inkplate6Panel<B, BUS, I2C, D>
where
    B: Inkplate6Board,
    BUS: PanelBus,
    I2C: I2c,
    D: DelayNs,
{
    const W: usize = B::WIDTH as usize;
    const H: usize = B::HEIGHT as usize;

    /// Sets up the expanders and PMIC (`gpioInit` + `initDriver`). Leaves the panel
    /// unpowered and untouched, so whatever it shows stays visible.
    pub fn new(bus: BUS, mut i2c: I2C, mut delay: D) -> Result<Self, Error<I2C::Error>> {
        let v = B::VARIANT;
        let init = |device| move |error| Error::Init { device, error };
        let mut io = Expander::begin(&mut i2c, v.expander, v.int_address).map_err(init(Device::IoExpander))?;
        // Upstream writes the battery pin here while it is still an input, which has no
        // effect; it is set up below instead.
        // Upstream ignores a missing external expander (and every I2C error); it only
        // holds user pins, so carry on without it.
        let mut ext = Expander::begin(&mut i2c, v.expander, v.ext_address).ok();

        for p in [pin::VCOM, pin::PWRUP, pin::WAKEUP, pin::GPIO0_ENABLE] {
            io.pin_mode(&mut i2c, p, Mode::Output)?;
        }
        io.write(&mut i2c, pin::GPIO0_ENABLE, true)?;

        // TPS65186::begin: program the rail sequence while awake.
        io.write(&mut i2c, pin::WAKEUP, true)?;
        delay.delay_ms(1);
        tps::write_begin_sequence(&mut i2c).map_err(init(Device::Pmic))?;
        delay.delay_ms(1);
        io.write(&mut i2c, pin::WAKEUP, false)?;

        // Unused pins as low outputs: upstream measured lower deep-sleep current this way.
        if let Some(ext) = &mut ext {
            for p in 0..15 {
                ext.pin_mode(&mut i2c, p, Mode::Output).map_err(init(Device::ExtExpander))?;
                ext.write(&mut i2c, p, false).map_err(init(Device::ExtExpander))?;
            }
        }
        for p in [14, 15] {
            io.pin_mode(&mut i2c, p, Mode::Output)?;
            io.write(&mut i2c, p, false)?;
        }
        match v.expander {
            // 6 V2: SD card supply off. (Upstream also floats ESP32 GPIO12–15; that is the
            // firmware's business.)
            Kind::Pcal6416a => io.pin_mode(&mut i2c, pin::SD_PMOS, Mode::Input)?,
            Kind::Mcp23017 => {}
        }

        // initDriver: panel control lines, touchpad / unused pins, battery divider off.
        for p in [pin::OE, pin::GMOD, pin::SPV] {
            io.pin_mode(&mut i2c, p, Mode::Output)?;
        }
        for p in [10, 11, 12] {
            match v.expander {
                Kind::Mcp23017 => io.pin_mode(&mut i2c, p, Mode::Input)?, // touchpad
                Kind::Pcal6416a => {
                    io.pin_mode(&mut i2c, p, Mode::Output)?;
                    io.write(&mut i2c, p, false)?;
                }
            }
        }
        io.pin_mode(&mut i2c, pin::BATTERY_MOSFET, Mode::Output)?;
        io.write(&mut i2c, pin::BATTERY_MOSFET, false)?;

        Ok(Self {
            bus,
            i2c,
            delay,
            io,
            ext,
            on: false,
            gray: GrayLuts::new(&luts::INKPLATE6_GRAY3),
            row: vec![0; Self::W / 4],
            _board: PhantomData,
        })
    }

    /// Whether the external IO expander answered during init.
    pub fn has_ext_expander(&self) -> bool {
        self.ext.is_some()
    }

    /// Panel temperature in °C from the PMIC (`readTemperature`).
    pub fn temperature(&mut self) -> Result<i8, Error<I2C::Error>> {
        let wake = !self.on;
        if wake {
            self.io_write(pin::WAKEUP, true)?;
            self.io_write(pin::PWRUP, true)?;
            self.delay.delay_ms(5);
        }
        tps::start_temperature(&mut self.i2c)?;
        self.delay.delay_ms(5);
        let t = tps::read_reg(&mut self.i2c, tps::REG_TEMP)? as i8;
        if wake {
            self.io_write(pin::PWRUP, false)?;
            self.io_write(pin::WAKEUP, false)?;
            self.delay.delay_ms(5);
        }
        Ok(t)
    }

    fn io_write(&mut self, p: u8, high: bool) -> Result<(), I2C::Error> {
        self.io.write(&mut self.i2c, p, high)
    }

    fn io_mode(&mut self, p: u8, mode: Mode) -> Result<(), I2C::Error> {
        self.io.pin_mode(&mut self.i2c, p, mode)
    }

    fn delay_us(&mut self, us: u32) {
        if us > 0 {
            self.delay.delay_us(us);
        }
    }

    /// `einkOn`: control lines to their idle state, then rails up via the PMIC.
    fn eink_on(&mut self) -> Result<(), Error<I2C::Error>> {
        if self.on {
            return Ok(());
        }
        self.pins_as_outputs()?;
        self.bus.set_le(false);
        self.bus.set_sph(true);
        self.io_write(pin::GMOD, true)?;
        self.io_write(pin::SPV, true)?;
        self.bus.set_ckv(false);
        self.io_write(pin::OE, false)?;
        self.on = true;

        if !self.pmic_power_up()? {
            self.eink_off()?;
            return Err(Error::PowerGood);
        }
        self.io_write(pin::OE, true)?;
        Ok(())
    }

    /// `einkOff`: control lines low, rails down, all panel pins high impedance.
    fn eink_off(&mut self) -> Result<(), Error<I2C::Error>> {
        if !self.on {
            return Ok(());
        }
        self.io_write(pin::OE, false)?;
        self.io_write(pin::GMOD, false)?;
        self.bus.set_le(false);
        self.bus.set_ckv(false);
        self.bus.set_sph(false);
        self.io_write(pin::SPV, false)?;
        self.pmic_power_down()?;
        self.pins_z_state()?;
        self.on = false;
        Ok(())
    }

    fn pins_as_outputs(&mut self) -> Result<(), I2C::Error> {
        for p in [pin::OE, pin::GMOD, pin::SPV] {
            self.io_mode(p, Mode::Output)?;
        }
        self.bus.enable();
        Ok(())
    }

    fn pins_z_state(&mut self) -> Result<(), I2C::Error> {
        for p in [pin::OE, pin::GMOD, pin::SPV] {
            self.io_mode(p, Mode::Input)?;
        }
        self.bus.disable();
        Ok(())
    }

    /// `TPS65186::powerUp`. Returns false on power-good timeout.
    fn pmic_power_up(&mut self) -> Result<bool, I2C::Error> {
        self.io_write(pin::WAKEUP, true)?;
        self.delay.delay_ms(5);
        tps::enable_rails(&mut self.i2c, true)?;
        tps::write_reg(&mut self.i2c, tps::REG_UPSEQ0, tps::POWER_UP_UPSEQ0)?;
        tps::write_reg(&mut self.i2c, tps::REG_DWNSEQ0, tps::POWER_UP_DWNSEQ0)?;
        self.io_write(pin::PWRUP, true)?;

        let mut waited = 0;
        loop {
            self.delay.delay_ms(1);
            waited += 1;
            if tps::read_power_good(&mut self.i2c)? == tps::PWR_GOOD_OK {
                break;
            }
            if waited >= POWER_TIMEOUT_MS {
                return Ok(false);
            }
        }
        self.io_write(pin::VCOM, true)?;
        Ok(true)
    }

    /// `TPS65186::powerDown`.
    fn pmic_power_down(&mut self) -> Result<(), I2C::Error> {
        self.io_write(pin::VCOM, false)?;
        self.io_write(pin::PWRUP, false)?;
        for _ in 0..POWER_TIMEOUT_MS {
            self.delay.delay_ms(1);
            if tps::read_power_good(&mut self.i2c)? == 0 {
                break;
            }
        }
        self.io_write(pin::WAKEUP, false)?;
        tps::enable_rails(&mut self.i2c, false)
    }

    /// Starts a frame and skips the first two (invisible) rows.
    fn vscan_start(&mut self) -> Result<(), I2C::Error> {
        self.bus.set_ckv(true);
        self.delay_us(7);
        self.io_write(pin::SPV, false)?;
        self.delay_us(10);
        self.bus.set_ckv(false);
        self.bus.set_ckv(true);
        self.delay_us(8);
        self.io_write(pin::SPV, true)?;
        self.delay_us(10);
        self.bus.set_ckv(false);
        self.bus.set_ckv(true);
        self.delay_us(18);
        self.bus.set_ckv(false);
        self.bus.set_ckv(true);
        self.delay_us(18);
        self.bus.set_ckv(false);
        self.bus.set_ckv(true);
        Ok(())
    }

    /// Ends the current row and latches it onto the panel.
    fn vscan_end(&mut self) {
        self.bus.set_ckv(false);
        self.bus.set_le(true);
        self.bus.set_le(false);
    }

    /// Sends the current `row` buffer as one row.
    fn send_row(&mut self) {
        self.bus.send_row(&self.row);
        self.vscan_end();
    }

    /// `clean(c, rep)`: `rep` frames with every pixel driven the same way.
    fn clean(&mut self, kind: u8, rep: u8) -> Result<(), Error<I2C::Error>> {
        self.eink_on()?;
        self.row.fill(luts::CLEAN_FILL[kind as usize]);
        for _ in 0..rep {
            self.vscan_start()?;
            for _ in 0..Self::H {
                self.send_row();
            }
            self.delay_us(FRAME_GAP_US);
        }
        Ok(())
    }

    fn clean_sequence(&mut self) -> Result<(), Error<I2C::Error>> {
        for (kind, rep) in luts::CLEAN_SEQUENCE {
            self.clean(kind, rep)?;
        }
        Ok(())
    }

    /// `display3b`: clean, then 9 waveform phases from a 4-bit packed frame.
    fn display_gray3(&mut self, levels: &[u8]) -> Result<(), Error<I2C::Error>> {
        // DMemory4Bit layout: two pixels per byte, even x in the high nibble.
        let fb: Vec<u8> = levels.as_chunks::<2>().0.iter().map(|[a, b]| ((a & 7) << 4) | (b & 7)).collect();

        self.eink_on()?;
        self.clean_sequence()?;
        for k in 0..GRAY3_PHASES {
            // Rows go out last to first, each row right to left, in groups of 16 pixels
            // (8 frame bytes → 4 bus bytes). This mirrors upstream's
            // `GLUT2[*(--dp)] | GLUT[*(--dp)]`, reading the two operands left to right.
            let mut dp = fb.len();
            self.vscan_start()?;
            for _ in 0..Self::H {
                let (low, high) = (&self.gray.low[k], &self.gray.high[k]);
                for j in (0..Self::W / 4).step_by(4) {
                    for slot in [2, 3, 0, 1] {
                        self.row[j + slot] = high[fb[dp - 1] as usize] | low[fb[dp - 2] as usize];
                        dp -= 2;
                    }
                }
                self.send_row();
            }
            self.delay_us(FRAME_GAP_US);
        }
        self.clean(3, 1)?;
        self.vscan_start()?;
        self.eink_off()
    }

    /// `display1b`: clean, darken phases, one black/white phase, one discharge phase.
    fn display_mono(&mut self, pixels: &[u8]) -> Result<(), Error<I2C::Error>> {
        // _partial layout: 8 pixels per byte, x % 8 = bit, set = black.
        let fb: Vec<u8> = pixels
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| p.iter().enumerate().fold(0, |b, (i, &v)| if v == 0 { b | (1 << i) } else { b }))
            .collect();

        self.eink_on()?;
        self.clean_sequence()?;
        let phases = [&luts::LUTB; 8][..B::VARIANT.mono_phases].iter().copied().chain([&luts::LUT2]);
        for lut in phases {
            let mut p = fb.len() - 1;
            self.vscan_start()?;
            for _ in 0..Self::H {
                for n in (0..Self::W / 4).step_by(4) {
                    let (dram1, dram2) = (fb[p], fb[p - 1]);
                    self.row[n] = lut[(dram2 >> 4) as usize];
                    self.row[n + 1] = lut[(dram2 & 0x0F) as usize];
                    self.row[n + 2] = lut[(dram1 >> 4) as usize];
                    self.row[n + 3] = lut[(dram1 & 0x0F) as usize];
                    p = p.wrapping_sub(2);
                }
                self.send_row();
            }
            self.delay_us(FRAME_GAP_US);
        }
        // Discharge frame.
        self.row.fill(0);
        self.vscan_start()?;
        for _ in 0..Self::H {
            self.send_row();
        }
        self.delay_us(FRAME_GAP_US);
        self.vscan_start()?;
        self.eink_off()
    }
}

impl<B, BUS, I2C, D> EpdDriver for Inkplate6Panel<B, BUS, I2C, D>
where
    B: Inkplate6Board,
    BUS: PanelBus,
    I2C: I2c,
    D: DelayNs,
{
    type Error = Error<I2C::Error>;

    fn show(&mut self, frame: Frame<'_>) -> Result<(), Self::Error> {
        let pixels = match frame {
            Frame::Gray3(p) | Frame::Mono(p) => p,
        };
        if pixels.len() != Self::W * Self::H {
            return Err(Error::FrameSize);
        }
        match frame {
            Frame::Gray3(p) => self.display_gray3(p),
            Frame::Mono(p) => self.display_mono(p),
        }
    }

    fn sleep(&mut self) -> Result<(), Self::Error> {
        self.eink_off()
    }
}

#[cfg(test)]
mod tests;
