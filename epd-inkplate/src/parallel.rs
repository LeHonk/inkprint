//! Driver for the parallel-bus panels (Inkplate 6, 6PLUS, 10, 5).

use core::marker::PhantomData;

use board::Board;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

use crate::tps65186::Tps65186;
use crate::{EpdDriver, Frame, ParallelBus};

/// Per-pixel drive for one scan of the panel.
pub const DRIVE_NONE: u8 = 0b00;
pub const DRIVE_BLACK: u8 = 0b01;
pub const DRIVE_WHITE: u8 = 0b10;

/// A grayscale waveform: for each phase, the drive applied to a pixel of each gray level.
pub struct Waveform {
    pub phases: &'static [[u8; 8]],
}

// TODO: port the panel's 3-bit waveform from the Inkplate Arduino library (Inkplate6
// driver). Left empty rather than guessed: wrong values can stress the panel.
pub const INKPLATE6_GRAY3: Waveform = Waveform { phases: &[] };

#[derive(Debug)]
pub enum Error<E> {
    I2c(E),
    /// Frame length does not match the board's panel size.
    FrameSize,
    /// The waveform for this panel has not been ported yet.
    NoWaveform,
}

pub struct ParallelEpd<B: Board, BUS, I2C, D> {
    bus: BUS,
    pmic: Tps65186<I2C>,
    delay: D,
    waveform: &'static Waveform,
    _board: PhantomData<B>,
}

impl<B, BUS, I2C, D> ParallelEpd<B, BUS, I2C, D>
where
    B: Board,
    BUS: ParallelBus,
    I2C: I2c,
    D: DelayNs,
{
    pub fn new(bus: BUS, i2c: I2C, delay: D, waveform: &'static Waveform) -> Self {
        Self { bus, pmic: Tps65186::new(i2c), delay, waveform, _board: PhantomData }
    }

    /// Writes one full frame scan where each pixel gets `drive(x, y)`.
    fn scan(&mut self, mut drive: impl FnMut(usize) -> u8) {
        let (w, h) = (B::WIDTH as usize, B::HEIGHT as usize);
        self.bus.vscan_start();
        for y in 0..h {
            let row = y * w;
            let byte = |x: usize, drive: &mut dyn FnMut(usize) -> u8| {
                (drive(row + x) << 6) | (drive(row + x + 1) << 4) | (drive(row + x + 2) << 2) | drive(row + x + 3)
            };
            self.bus.hscan_start(byte(0, &mut drive));
            for x in (4..w).step_by(4) {
                self.bus.write(byte(x, &mut drive));
            }
            self.bus.row_end();
        }
        self.bus.vscan_end();
    }

    /// Clean cycles to reduce ghosting before drawing a new image.
    // TODO: match the Arduino library's clean sequence (alternating black/white scans).
    fn clean(&mut self) {
        for drive in [DRIVE_BLACK, DRIVE_WHITE, DRIVE_BLACK, DRIVE_WHITE] {
            self.scan(|_| drive);
        }
    }
}

impl<B, BUS, I2C, D> EpdDriver for ParallelEpd<B, BUS, I2C, D>
where
    B: Board,
    BUS: ParallelBus,
    I2C: I2c,
    D: DelayNs,
{
    type Error = Error<I2C::Error>;

    fn init(&mut self) -> Result<(), Self::Error> {
        // Deliberately no clear: the panel keeps showing the last page.
        Ok(())
    }

    fn show(&mut self, frame: Frame<'_>) -> Result<(), Self::Error> {
        let pixels = match frame {
            Frame::Gray3(p) | Frame::Mono(p) => p,
        };
        if pixels.len() != B::WIDTH as usize * B::HEIGHT as usize {
            return Err(Error::FrameSize);
        }
        if self.waveform.phases.is_empty() {
            return Err(Error::NoWaveform);
        }
        self.pmic.power_on(&mut self.delay).map_err(Error::I2c)?;
        self.clean();
        let phases = self.waveform.phases;
        for phase in phases {
            match frame {
                Frame::Gray3(p) => self.scan(|i| phase[p[i] as usize & 7]),
                // Mono uses the darkest and lightest entries of each phase.
                Frame::Mono(p) => self.scan(|i| phase[if p[i] == 0 { 0 } else { 7 }]),
            }
        }
        self.scan(|_| DRIVE_NONE);
        self.pmic.power_off().map_err(Error::I2c)
    }

    fn sleep(&mut self) -> Result<(), Self::Error> {
        self.pmic.power_off().map_err(Error::I2c)
    }
}
