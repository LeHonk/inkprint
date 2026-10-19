//! TI TPS65186 e-paper PMIC (high-voltage rails, VCOM, panel temperature).

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

pub const ADDRESS: u8 = 0x48;

pub struct Tps65186<I2C> {
    i2c: I2C,
}

impl<I2C: I2c> Tps65186<I2C> {
    pub fn new(i2c: I2C) -> Self {
        Self { i2c }
    }

    /// Brings up the panel rails.
    // TODO: port the sequence from the Inkplate library: wake/pwrup via the IO expander,
    // write the power-up sequence and enable registers, then poll for power-good.
    pub fn power_on(&mut self, delay: &mut impl DelayNs) -> Result<(), I2C::Error> {
        let _ = &mut self.i2c;
        delay.delay_ms(5);
        Ok(())
    }

    // TODO: port the power-down sequence.
    pub fn power_off(&mut self) -> Result<(), I2C::Error> {
        Ok(())
    }
}
