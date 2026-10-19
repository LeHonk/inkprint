// SPDX-License-Identifier: LGPL-3.0-only
//! MCP23017 / PCAL6416A IO expanders, as used by the Arduino library: register values are
//! cached and only the changed register is written.
//!
//! The I2C bus is passed into every call instead of owned, because the expanders share it
//! with the TPS65186 PMIC.

use embedded_hal::i2c::I2c;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Inkplate 6 (original), 6PLUS, 10.
    Mcp23017,
    /// Inkplate 6 V2 and newer boards.
    Pcal6416a,
}

mod mcp {
    pub const IODIRA: u8 = 0x00;
    pub const GPPUA: u8 = 0x0C;
    pub const GPIOA: u8 = 0x12;
    /// Registers 0x00..=0x15 (IOCON.BANK = 0, A/B interleaved).
    pub const REG_COUNT: usize = 0x16;
}

mod pcal {
    pub const OUTPORT0: u8 = 0x02;
    pub const CFGPORT0: u8 = 0x06;
    pub const PUPDEN0: u8 = 0x46;
    pub const PUPDSEL0: u8 = 0x48;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Input,
    InputPullup,
    Output,
}

pub struct Expander {
    kind: Kind,
    address: u8,
    /// MCP23017: registers 0x00..=0x15. PCAL6416A: in0/1, out0/1, pol0/1, cfg0/1 at 0..8,
    /// pupden0/1 and pupdsel0/1 at 8..12.
    regs: [u8; mcp::REG_COUNT],
}

impl Expander {
    /// Probes the expander and reads its registers (`IOExpander::begin`).
    pub fn begin<I: I2c>(i2c: &mut I, kind: Kind, address: u8) -> Result<Self, I::Error> {
        let mut regs = [0; mcp::REG_COUNT];
        match kind {
            Kind::Mcp23017 => i2c.write_read(address, &[mcp::IODIRA], &mut regs)?,
            Kind::Pcal6416a => {
                // Read pair by pair: PCA95xx-style parts may toggle within a register pair
                // instead of auto-incrementing to the next one.
                for (idx, reg) in [(2, pcal::OUTPORT0), (6, pcal::CFGPORT0), (8, pcal::PUPDEN0), (10, pcal::PUPDSEL0)] {
                    i2c.write_read(address, &[reg], &mut regs[idx..idx + 2])?;
                }
            }
        }
        // The Arduino MCP23017 begin() also configures IOCON for the INT output; that only
        // matters for deep-sleep wake-up from the expander, which this firmware doesn't use.
        Ok(Self { kind, address, regs })
    }

    /// Cache index and register address for a per-port register.
    fn reg(&self, which: Reg, port: u8) -> (usize, u8) {
        match (self.kind, which) {
            (Kind::Mcp23017, Reg::Direction) => ((mcp::IODIRA + port) as usize, mcp::IODIRA + port),
            (Kind::Mcp23017, Reg::Output) => ((mcp::GPIOA + port) as usize, mcp::GPIOA + port),
            (Kind::Mcp23017, Reg::PullEnable) => ((mcp::GPPUA + port) as usize, mcp::GPPUA + port),
            (Kind::Mcp23017, Reg::PullSelect) => unreachable!("MCP23017 only has pull-ups"),
            (Kind::Pcal6416a, Reg::Direction) => (6 + port as usize, pcal::CFGPORT0 + port),
            (Kind::Pcal6416a, Reg::Output) => (2 + port as usize, pcal::OUTPORT0 + port),
            (Kind::Pcal6416a, Reg::PullEnable) => (8 + port as usize, pcal::PUPDEN0 + port),
            (Kind::Pcal6416a, Reg::PullSelect) => (10 + port as usize, pcal::PUPDSEL0 + port),
        }
    }

    fn update<I: I2c>(&mut self, i2c: &mut I, which: Reg, port: u8, f: impl FnOnce(u8) -> u8) -> Result<(), I::Error> {
        let (idx, addr) = self.reg(which, port);
        self.regs[idx] = f(self.regs[idx]);
        i2c.write(self.address, &[addr, self.regs[idx]])
    }

    /// `pinModeInternal`. Both chips use direction bit 1 = input.
    pub fn pin_mode<I: I2c>(&mut self, i2c: &mut I, pin: u8, mode: Mode) -> Result<(), I::Error> {
        if pin > 15 {
            return Ok(());
        }
        let (port, bit) = (pin / 8, 1u8 << (pin % 8));
        let set = |v: u8| v | bit;
        let clear = |v: u8| v & !bit;
        match (self.kind, mode) {
            (Kind::Mcp23017, Mode::Input) => {
                self.update(i2c, Reg::Direction, port, set)?;
                self.update(i2c, Reg::PullEnable, port, clear)
            }
            (Kind::Mcp23017, Mode::InputPullup) => {
                self.update(i2c, Reg::Direction, port, set)?;
                self.update(i2c, Reg::PullEnable, port, set)
            }
            (Kind::Mcp23017, Mode::Output) => {
                self.update(i2c, Reg::Direction, port, clear)?;
                self.update(i2c, Reg::PullEnable, port, clear)
            }
            (Kind::Pcal6416a, Mode::Input) => self.update(i2c, Reg::Direction, port, set),
            (Kind::Pcal6416a, Mode::InputPullup) => {
                self.update(i2c, Reg::Direction, port, set)?;
                self.update(i2c, Reg::PullEnable, port, set)?;
                self.update(i2c, Reg::PullSelect, port, set)
            }
            (Kind::Pcal6416a, Mode::Output) => {
                // Outputs power up high: drive the latch low before switching direction.
                self.update(i2c, Reg::Output, port, clear)?;
                self.update(i2c, Reg::Direction, port, clear)
            }
        }
    }

    /// `digitalWriteInternal`: ignored for pins configured as inputs, like upstream.
    pub fn write<I: I2c>(&mut self, i2c: &mut I, pin: u8, high: bool) -> Result<(), I::Error> {
        if pin > 15 {
            return Ok(());
        }
        let (port, bit) = (pin / 8, 1u8 << (pin % 8));
        let (dir, _) = self.reg(Reg::Direction, port);
        if self.regs[dir] & bit != 0 {
            return Ok(());
        }
        self.update(i2c, Reg::Output, port, |v| if high { v | bit } else { v & !bit })
    }
}

#[derive(Debug, Clone, Copy)]
enum Reg {
    Direction,
    Output,
    PullEnable,
    PullSelect,
}
