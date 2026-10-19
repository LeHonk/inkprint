// SPDX-License-Identifier: LGPL-3.0-only
//! Inkplate 6 parallel panel bus on the ESP32: I2S1 in LCD mode with DMA for the 8 data
//! lines and CL, plain GPIOs for CKV, SPH and LE.
//!
//! Port of `system/UtilI2S/UtilI2S.cpp` and the GPIO parts of `pinsAsOutputs` /
//! `pinsZstate` from Soldered's Inkplate-Arduino-library (LGPL-3.0). The I2S registers are
//! programmed exactly as upstream (same FIFO mode and clock divider), so the bytes produced
//! by `epd_inkplate` reach the panel in the same order and at the same rate.
//!
//! esp-hal has no driver for this mode with the FIFO packing upstream relies on, so the
//! registers are accessed through the `esp32` PAC. Exclusivity is guaranteed by owning
//! esp-hal's `I2S1` singleton and every pin used here.

use core::sync::atomic::{Ordering, compiler_fence};

use board::{Board, Selected};
use epd_inkplate::PanelBus;
use esp_hal::gpio::{AnyPin, DriveStrength, Flex, OutputConfig, Pin};
use esp_hal::peripherals::I2S1;
use static_cell::StaticCell;

const ROW_BYTES: usize = Selected::WIDTH as usize / 4;
/// Upstream sends 16 bytes more than a row (`E_INK_WIDTH / 4 + 16`); the end-of-frame
/// interrupt fires when DMA has read the buffer, before the FIFO has drained, so the
/// padding makes sure the real row is fully clocked out before SPH goes high.
const DMA_BYTES: usize = ROW_BYTES + 16;

/// GPIO matrix output signals (ESP32 TRM, GPIO signal list).
const SIG_I2S1O_BCK: u16 = 24;
const SIG_I2S1O_DATA_0: u16 = 166;
/// Detaches a pin from peripheral signals (plain GPIO output).
const SIG_GPIO: u16 = 256;

/// I2S clock divider: 80 MHz / 5 with BCK divider 2 (upstream default).
const CLKM_DIV_NUM: u8 = 5;

/// `lldesc_t` from ESP-IDF `rom/lldesc.h`.
#[repr(C)]
struct DmaDescriptor {
    /// size:12, length:12, offset:5, sosf:1, eof:1, owner:1 (LSB first).
    flags: u32,
    buf: u32,
    next: u32,
}

#[repr(C, align(4))]
struct DmaRow {
    desc: DmaDescriptor,
    buf: [u8; DMA_BYTES],
}

/// Lives in internal DRAM (statics are never placed in PSRAM), which the I2S DMA requires.
static DMA_ROW: StaticCell<DmaRow> = StaticCell::new();

/// The panel's ESP32 pins on Inkplate 6 / 6 V2.
pub struct PanelPins<'d> {
    /// I2S1 BCK → CL, GPIO0.
    pub cl: AnyPin<'d>,
    /// D0..D7: GPIO4, 5, 18, 19, 23, 25, 26, 27.
    pub data: [AnyPin<'d>; 8],
    /// GPIO2.
    pub le: AnyPin<'d>,
    /// GPIO32.
    pub ckv: AnyPin<'d>,
    /// GPIO33.
    pub sph: AnyPin<'d>,
}

pub struct I2sPanelBus {
    dma: &'static mut DmaRow,
    /// CL first, then D0..D7: pin, GPIO number, I2S1 output signal.
    bus_pins: [(Flex<'static>, u8, u16); 9],
    le: Flex<'static>,
    ckv: Flex<'static>,
    sph: Flex<'static>,
    _i2s: I2S1<'static>,
}

fn i2s() -> &'static esp32::i2s0::RegisterBlock {
    // SAFETY: we own esp-hal's I2S1 singleton, so nothing else touches these registers.
    unsafe { &*esp32::I2S1::ptr() }
}

fn gpio_regs() -> &'static esp32::gpio::RegisterBlock {
    // SAFETY: only the matrix entries of pins this driver owns are written.
    unsafe { &*esp32::GPIO::ptr() }
}

fn route(pin: u8, signal: u16) {
    gpio_regs().func_out_sel_cfg(pin as usize).write(|w| unsafe {
        w.out_sel().bits(signal);
        w.inv_sel().clear_bit();
        w.oen_sel().clear_bit()
    });
}

impl I2sPanelBus {
    /// Sets up I2S1 (`I2SInit`) and leaves every panel pin high impedance.
    pub fn new(i2s1: I2S1<'static>, pins: PanelPins<'static>) -> Self {
        let dma = DMA_ROW.init(DmaRow { desc: DmaDescriptor { flags: 0, buf: 0, next: 0 }, buf: [0; DMA_BYTES] });

        let strong = OutputConfig::default().with_drive_strength(DriveStrength::_40mA);
        let signals = [SIG_I2S1O_BCK].into_iter().chain((0..8).map(|i| SIG_I2S1O_DATA_0 + i));
        let mut bus_pins = core::iter::once(pins.cl).chain(pins.data).zip(signals).map(|(pin, sig)| {
            let number = pin.number();
            let mut flex = Flex::new(pin);
            flex.apply_output_config(&strong);
            (flex, number, sig)
        });
        let bus_pins = core::array::from_fn(|_| bus_pins.next().unwrap());
        let control = |pin: AnyPin<'static>| {
            let mut flex = Flex::new(pin);
            flex.apply_output_config(&OutputConfig::default());
            flex
        };

        init_i2s();
        let mut bus = Self {
            dma,
            bus_pins,
            le: control(pins.le),
            ckv: control(pins.ckv),
            sph: control(pins.sph),
            _i2s: i2s1,
        };
        bus.disable();
        bus
    }
}

/// `UtilI2S::I2SInit` with the default clock divider.
fn init_i2s() {
    // periph_module_enable + periph_module_reset.
    // SAFETY: single read-modify-write of the I2S1 bits only.
    let dport = unsafe { &*esp32::DPORT::ptr() };
    dport.perip_clk_en().modify(|_, w| w.i2s1_clk_en().set_bit());
    dport.perip_rst_en().modify(|_, w| w.i2s1_rst().set_bit());
    dport.perip_rst_en().modify(|_, w| w.i2s1_rst().clear_bit());

    let r = i2s();
    r.conf().modify(|_, w| w.rx_fifo_reset().set_bit());
    r.conf().modify(|_, w| w.rx_fifo_reset().clear_bit());
    r.conf().modify(|_, w| w.tx_fifo_reset().set_bit());
    r.conf().modify(|_, w| w.tx_fifo_reset().clear_bit());

    r.lc_conf().modify(|_, w| w.in_rst().set_bit());
    r.lc_conf().modify(|_, w| w.in_rst().clear_bit());
    r.lc_conf().modify(|_, w| w.out_rst().set_bit());
    r.lc_conf().modify(|_, w| w.out_rst().clear_bit());

    r.conf().modify(|_, w| w.rx_reset().set_bit().tx_reset().set_bit());
    r.conf().modify(|_, w| w.rx_reset().clear_bit().tx_reset().clear_bit());

    // SAFETY (all `bits` calls below): values are the ones upstream writes and fit the fields.
    // LCD mode: data on the parallel bus, WR toggled twice per sample.
    r.conf2().write(|w| unsafe { w.bits(0) }.lcd_en().set_bit().lcd_tx_wrx2_en().set_bit().lcd_tx_sdx2_en().clear_bit());

    r.sample_rate_conf().write(|w| unsafe {
        w.bits(0);
        w.rx_bits_mod().bits(8);
        w.tx_bits_mod().bits(8);
        w.rx_bck_div_num().bits(2);
        w.tx_bck_div_num().bits(2)
    });

    // No APLL: 80 MHz / CLKM_DIV_NUM.
    r.clkm_conf().write(|w| unsafe {
        w.bits(0);
        w.clka_ena().clear_bit();
        w.clkm_div_b().bits(0);
        w.clkm_div_a().bits(1);
        w.clkm_div_num().bits(CLKM_DIV_NUM)
    });

    // FIFO mode 1 ("0A0B_0C0D", 8-bit samples packed four to a word). The driver's byte
    // order within each 4-byte group compensates for this packing.
    r.fifo_conf().write(|w| unsafe {
        w.bits(0);
        w.rx_fifo_mod_force_en().set_bit();
        w.tx_fifo_mod_force_en().set_bit();
        w.tx_fifo_mod().bits(1);
        w.rx_data_num().bits(1);
        w.tx_data_num().bits(1);
        w.dscr_en().set_bit()
    });

    // Clock runs continuously while the bus is enabled; PCM path bypassed.
    r.conf1().write(|w| unsafe { w.bits(0) }.tx_stop_en().clear_bit().tx_pcm_bypass().set_bit());

    r.conf_chan().write(|w| unsafe {
        w.bits(0);
        w.tx_chan_mod().bits(1);
        w.rx_chan_mod().bits(1)
    });

    r.conf().modify(|_, w| w.tx_right_first().clear_bit().rx_right_first().clear_bit());
    r.timing().write(|w| unsafe { w.bits(0) });
}

impl PanelBus for I2sPanelBus {
    #[inline]
    fn set_ckv(&mut self, high: bool) {
        self.ckv.set_level(high.into());
    }

    #[inline]
    fn set_sph(&mut self, high: bool) {
        self.sph.set_level(high.into());
    }

    #[inline]
    fn set_le(&mut self, high: bool) {
        self.le.set_level(high.into());
    }

    /// `UtilI2S::sendDataI2S`.
    fn send_row(&mut self, row: &[u8]) {
        self.dma.buf[..ROW_BYTES].copy_from_slice(row);
        let len = DMA_BYTES as u32;
        self.dma.desc = DmaDescriptor {
            // size, length, sosf, eof, owner = DMA.
            flags: len | (len << 12) | (1 << 29) | (1 << 30) | (1 << 31),
            buf: self.dma.buf.as_ptr() as u32,
            next: 0,
        };
        let desc_addr = &self.dma.desc as *const DmaDescriptor as u32;
        // Internal DRAM is not cached: ordering the writes before the DMA start suffices.
        compiler_fence(Ordering::SeqCst);

        let r = i2s();
        // Stop anything in flight, reset FIFO, DMA and TX.
        r.out_link().modify(|_, w| w.outlink_stop().set_bit().outlink_start().clear_bit());
        r.conf().modify(|_, w| w.tx_start().clear_bit());
        r.conf().modify(|_, w| w.tx_fifo_reset().set_bit());
        r.conf().modify(|_, w| w.tx_fifo_reset().clear_bit());
        r.lc_conf().modify(|_, w| w.out_rst().set_bit());
        r.lc_conf().modify(|_, w| w.out_rst().clear_bit());
        r.conf().modify(|_, w| w.tx_reset().set_bit());
        r.conf().modify(|_, w| w.tx_reset().clear_bit());

        r.lc_conf().write(|w| unsafe { w.bits(0) }.out_data_burst_en().set_bit().outdscr_burst_en().set_bit());
        // SAFETY: 20-bit offset of a DRAM address, as upstream.
        r.out_link().modify(|_, w| unsafe { w.outlink_addr().bits(desc_addr & 0x000F_FFFF) });
        r.out_link().modify(|_, w| w.outlink_start().set_bit());

        // Start pushing data into the row.
        self.sph.set_low();
        self.ckv.set_high();
        r.conf().modify(|_, w| w.tx_start().set_bit());
        while r.int_raw().read().out_total_eof().bit_is_clear() {}
        self.sph.set_high();

        let raw = r.int_raw().read().bits();
        r.int_clr().write(|w| unsafe { w.bits(raw) });
        r.out_link().modify(|_, w| w.outlink_stop().set_bit().outlink_start().clear_bit());
    }

    /// GPIO part of `pinsAsOutputs` (`setI2S1pin` for CL and data, outputs for the rest).
    fn enable(&mut self) {
        for (flex, number, signal) in &mut self.bus_pins {
            route(*number, *signal);
            flex.set_output_enable(true);
        }
        for flex in [&mut self.le, &mut self.ckv, &mut self.sph] {
            flex.set_output_enable(true);
        }
        i2s().conf1().modify(|_, w| w.tx_stop_en().clear_bit());
    }

    /// GPIO part of `pinsZstate`.
    fn disable(&mut self) {
        for flex in [&mut self.le, &mut self.ckv, &mut self.sph] {
            flex.set_output_enable(false);
        }
        for (flex, number, _) in &mut self.bus_pins {
            flex.set_output_enable(false);
            route(*number, SIG_GPIO);
        }
        i2s().conf1().modify(|_, w| w.tx_stop_en().clear_bit());
    }
}
