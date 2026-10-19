// SPDX-License-Identifier: LGPL-3.0-only
extern crate std;

use std::cell::RefCell;
use std::rc::Rc;
use std::vec::Vec;

use board::{Board, DriverFamily, IoExpander, Media, PixelKind, Rotation};
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::{ErrorType, I2c, Operation};

use super::*;

/// A tiny panel so whole refreshes can be recorded: 2 groups of 16 pixels × 3 rows.
struct Tiny;
impl Board for Tiny {
    const MODEL: &'static str = "test";
    const WIDTH: u16 = 32;
    const HEIGHT: u16 = 3;
    const ROTATION: Rotation = Rotation::None;
    const PIXEL: PixelKind = PixelKind::Gray3;
    const DRIVER: DriverFamily = DriverFamily::ParallelEpd;
    const IO_EXPANDER: IoExpander = IoExpander::Mcp23017;
    const RESOLUTIONS: &'static [u16] = &[150];
    const MEDIA: &'static [Media] = &[];
}
impl Inkplate6Board for Tiny {
    const VARIANT: Variant = INKPLATE6;
}
const W: usize = 32;
const H: usize = 3;
const CLEAN_FRAMES: usize = 77;

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Enable,
    Disable,
    Row(Vec<u8>),
}

#[derive(Clone, Default)]
struct Bus(Rc<RefCell<Vec<Ev>>>);

impl Bus {
    fn rows(&self) -> Vec<Vec<u8>> {
        self.0
            .borrow()
            .iter()
            .filter_map(|e| if let Ev::Row(r) = e { Some(r.clone()) } else { None })
            .collect()
    }
}

impl PanelBus for Bus {
    fn set_ckv(&mut self, _: bool) {}
    fn set_sph(&mut self, _: bool) {}
    fn set_le(&mut self, _: bool) {}
    fn send_row(&mut self, row: &[u8]) {
        self.0.borrow_mut().push(Ev::Row(row.to_vec()));
    }
    fn enable(&mut self) {
        self.0.borrow_mut().push(Ev::Enable);
    }
    fn disable(&mut self) {
        self.0.borrow_mut().push(Ev::Disable);
    }
}

/// Records writes; answers reads like freshly powered expanders and a PMIC whose rails
/// come up (or not) when enabled.
/// (address, bytes) of every I2C write.
type WriteLog = Rc<RefCell<Vec<(u8, Vec<u8>)>>>;

#[derive(Clone)]
struct MockI2c {
    writes: WriteLog,
    rails_come_up: bool,
    rails_on: bool,
    /// Addresses that NACK.
    absent: Vec<u8>,
}

impl MockI2c {
    fn new(rails_come_up: bool) -> Self {
        Self { writes: Default::default(), rails_come_up, rails_on: false, absent: Vec::new() }
    }

    fn pmic_writes(&self) -> Vec<Vec<u8>> {
        self.writes.borrow().iter().filter(|(a, _)| *a == tps::ADDRESS).map(|(_, b)| b.clone()).collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Nack;

impl embedded_hal::i2c::Error for Nack {
    fn kind(&self) -> embedded_hal::i2c::ErrorKind {
        embedded_hal::i2c::ErrorKind::NoAcknowledge(embedded_hal::i2c::NoAcknowledgeSource::Address)
    }
}

impl ErrorType for MockI2c {
    type Error = Nack;
}

impl I2c for MockI2c {
    fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), Self::Error> {
        if self.absent.contains(&address) {
            return Err(Nack);
        }
        let mut reg = 0;
        for op in ops {
            match op {
                Operation::Write(bytes) => {
                    reg = bytes[0];
                    if address == tps::ADDRESS && reg == tps::REG_ENABLE {
                        self.rails_on = bytes[1] == tps::ENABLE_RAILS;
                    }
                    self.writes.borrow_mut().push((address, bytes.to_vec()));
                }
                Operation::Read(buf) => {
                    let v = match (address, reg) {
                        (tps::ADDRESS, tps::REG_PWRGOOD) if self.rails_on && self.rails_come_up => tps::PWR_GOOD_OK,
                        (tps::ADDRESS, tps::REG_TEMP) => 23,
                        (tps::ADDRESS, _) => 0,
                        _ => 0xFF, // expander power-on state: all pins inputs
                    };
                    buf.fill(v);
                }
            }
        }
        Ok(())
    }
}

struct NoDelay;
impl DelayNs for NoDelay {
    fn delay_ns(&mut self, _: u32) {}
}

fn panel(rails_come_up: bool) -> (Inkplate6Panel<Tiny, Bus, MockI2c, NoDelay>, Bus, MockI2c) {
    let bus = Bus::default();
    let i2c = MockI2c::new(rails_come_up);
    let p = Inkplate6Panel::new(bus.clone(), i2c.clone(), NoDelay).unwrap();
    (p, bus, i2c)
}

/// Independent model of the bus layout derived from upstream's loops: rows go out from
/// the last to the first; within a row, bus bytes come in groups of four covering 16
/// pixels from the right, in slot order [+4, +0, +12, +8]; each byte holds 4 consecutive
/// pixels with the leftmost in the lowest two bits.
fn expected_row(sent_row: usize, code: impl Fn(usize, usize) -> u8) -> Vec<u8> {
    let y = H - 1 - sent_row;
    let mut out = Vec::new();
    for g in 0..W / 16 {
        let base = W - 16 * (g + 1);
        for start in [base + 4, base, base + 12, base + 8] {
            out.push((0..4).fold(0, |b, m| b | (code(start + m, y) << (2 * m))));
        }
    }
    out
}

fn level(x: usize, y: usize) -> u8 {
    ((x * 5 + y * 3 + x / 7) % 8) as u8
}

#[test]
fn init_never_touches_the_panel() {
    let (_p, bus, i2c) = panel(true);
    assert!(bus.0.borrow().is_empty());
    // PMIC got its sequence but rails stayed off.
    assert_eq!(i2c.pmic_writes(), [vec![tps::REG_UPSEQ0, 0x1B, 0x00, 0x1B, 0x00]]);
}

#[test]
fn gray3_refresh_matches_upstream_layout() {
    let (mut p, bus, i2c) = panel(true);
    let frame: Vec<u8> = (0..H).flat_map(|y| (0..W).map(move |x| level(x, y))).collect();
    p.show(Frame::Gray3(&frame)).unwrap();

    let rows = bus.rows();
    assert_eq!(rows.len(), (CLEAN_FRAMES + GRAY3_PHASES + 1) * H);
    for k in 0..GRAY3_PHASES {
        for i in 0..H {
            let got = &rows[(CLEAN_FRAMES + k) * H + i];
            let want = expected_row(i, |x, y| luts::INKPLATE6_GRAY3[level(x, y) as usize][k]);
            assert_eq!(got, &want, "phase {k} row {i}");
        }
    }
    // Clean frames: whiten once, then darken 18×.
    assert!(rows[..H].iter().all(|r| r.iter().all(|&b| b == 0xAA)));
    assert!(rows[H..19 * H].iter().all(|r| r.iter().all(|&b| b == 0x55)));
    // Trailing skip frame, then powered down with pins released.
    assert!(rows[rows.len() - H..].iter().all(|r| r.iter().all(|&b| b == 0xFF)));
    let events = bus.0.borrow();
    assert_eq!(events.first(), Some(&Ev::Enable));
    assert_eq!(events.last(), Some(&Ev::Disable));
    assert_eq!(i2c.pmic_writes().last(), Some(&vec![tps::REG_ENABLE, 0]));
}

#[test]
fn mono_refresh_matches_upstream_layout() {
    let (mut p, bus, _) = panel(true);
    let black = |x: usize, y: usize| (x + y).is_multiple_of(3);
    let frame: Vec<u8> = (0..H).flat_map(|y| (0..W).map(move |x| if black(x, y) { 0 } else { 1 })).collect();
    p.show(Frame::Mono(&frame)).unwrap();

    let rows = bus.rows();
    let darken = INKPLATE6.mono_phases;
    assert_eq!(rows.len(), (CLEAN_FRAMES + darken + 2) * H);
    for f in 0..darken + 2 {
        for i in 0..H {
            let got = &rows[(CLEAN_FRAMES + f) * H + i];
            let want = expected_row(i, |x, y| match (f, black(x, y)) {
                (f, _) if f == darken + 1 => 0b00, // discharge
                (_, true) => 0b01,
                (f, false) if f < darken => 0b11, // skip
                (_, false) => 0b10,               // whiten in the final phase
            });
            assert_eq!(got, &want, "frame {f} row {i}");
        }
    }
}

#[test]
fn power_good_timeout_powers_down_without_scanning() {
    let (mut p, bus, i2c) = panel(false);
    let frame = vec![0u8; W * H];
    assert_eq!(p.show(Frame::Gray3(&frame)), Err(Error::PowerGood));
    assert_eq!(*bus.0.borrow(), [Ev::Enable, Ev::Disable]);
    assert_eq!(i2c.pmic_writes().last(), Some(&vec![tps::REG_ENABLE, 0]));
}

#[test]
fn rejects_wrong_frame_size() {
    let (mut p, bus, _) = panel(true);
    assert_eq!(p.show(Frame::Mono(&[0; 10])), Err(Error::FrameSize));
    assert!(bus.0.borrow().is_empty());
}

#[test]
fn reads_temperature_without_leaving_rails_on() {
    let (mut p, _, i2c) = panel(true);
    assert_eq!(p.temperature().unwrap(), 23);
    assert!(i2c.pmic_writes().contains(&vec![tps::REG_TMST1, 0x80]));
}

#[test]
fn missing_external_expander_is_tolerated() {
    let mut i2c = MockI2c::new(true);
    i2c.absent.push(INKPLATE6.ext_address);
    let p = Inkplate6Panel::<Tiny, _, _, _>::new(Bus::default(), i2c.clone(), NoDelay).unwrap();
    assert!(!p.has_ext_expander());
    assert!(i2c.writes.borrow().iter().all(|(a, _)| *a != INKPLATE6.ext_address));
}

#[test]
fn init_errors_name_the_device() {
    for (address, device) in [(INKPLATE6.int_address, Device::IoExpander), (tps::ADDRESS, Device::Pmic)] {
        let mut i2c = MockI2c::new(true);
        i2c.absent.push(address);
        let err = Inkplate6Panel::<Tiny, _, _, _>::new(Bus::default(), i2c, NoDelay).err();
        assert_eq!(err, Some(Error::Init { device, error: Nack }));
    }
}
