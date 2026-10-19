// SPDX-License-Identifier: LGPL-3.0-only
//! Waveform and pixel lookup tables from the Arduino library
//! (`boards/Inkplate6/waveforms.h`, `graphics/GraphicsDefs.h`).
//!
//! Each pixel is driven with a 2-bit code per scan: `00` discharge/no drive, `01` towards
//! black, `10` towards white, `11` skip. Four pixels share a byte, lowest pixel in the
//! lowest bits.

use alloc::vec::Vec;

/// 3-bit grayscale waveform for Inkplate 6 and 6 V2 (identical upstream):
/// `[level][phase]`, level 0 = black … 7 = white, values are drive codes 0/1/2.
pub const INKPLATE6_GRAY3: [[u8; 9]; 8] = [
    [0, 0, 0, 0, 1, 1, 1, 1, 0],
    [0, 0, 0, 1, 1, 1, 1, 0, 0],
    [1, 1, 1, 1, 0, 2, 1, 0, 0],
    [1, 1, 1, 2, 2, 1, 1, 0, 0],
    [1, 1, 1, 1, 2, 2, 1, 0, 0],
    [0, 1, 1, 1, 2, 2, 1, 0, 0],
    [0, 0, 0, 0, 1, 1, 2, 0, 0],
    [0, 0, 0, 0, 0, 0, 2, 0, 0],
];

pub const GRAY3_PHASES: usize = 9;

/// 1-bit frame, 4 pixels (nibble, bit set = black) → drive black where set, skip elsewhere.
pub const LUTB: [u8; 16] = [
    0xFF, 0xFD, 0xF7, 0xF5, 0xDF, 0xDD, 0xD7, 0xD5, 0x7F, 0x7D, 0x77, 0x75, 0x5F, 0x5D, 0x57, 0x55,
];

/// 1-bit frame nibble → drive black where set, white elsewhere.
pub const LUT2: [u8; 16] = [
    0xAA, 0xA9, 0xA6, 0xA5, 0x9A, 0x99, 0x96, 0x95, 0x6A, 0x69, 0x66, 0x65, 0x5A, 0x59, 0x56, 0x55,
];

/// Fill bytes for `clean(c, …)`: 0 = whiten, 1 = darken, 2 = discharge, 3 = skip.
pub const CLEAN_FILL: [u8; 4] = [0b1010_1010, 0b0101_0101, 0b0000_0000, 0b1111_1111];

/// Clean sequence run before every full refresh: `(clean kind, repetitions)`.
pub const CLEAN_SEQUENCE: [(u8, u8); 9] =
    [(0, 1), (1, 18), (2, 1), (0, 18), (2, 1), (1, 18), (2, 1), (0, 18), (2, 1)];

/// One cycle of upstream's `burnInClean`: a stronger clean for ghosts of saturated
/// black-on-white content.
pub const BURN_IN_SEQUENCE: [(u8, u8); 8] = [(1, 21), (2, 1), (0, 12), (2, 1), (1, 21), (2, 1), (0, 12), (2, 1)];

/// Per-phase lookup tables for a 4-bit packed gray frame byte (two pixels, even pixel in
/// the high nibble), as `GLUT`/`GLUT2` in the Arduino driver: `low[phase][byte]` gives the
/// two pixels' codes in bits 0–3, `high` the same shifted into bits 4–7.
///
/// Heap-allocated (4.6 KB) to keep it off small embedded task stacks.
pub struct GrayLuts {
    pub low: Vec<[u8; 256]>,
    pub high: Vec<[u8; 256]>,
}

impl GrayLuts {
    pub fn new(waveform: &[[u8; GRAY3_PHASES]; 8]) -> Self {
        let codes = |i: usize, phase: usize| (waveform[i & 0x07][phase] << 2) | waveform[(i >> 4) & 0x07][phase];
        let low: Vec<[u8; 256]> =
            (0..GRAY3_PHASES).map(|phase| core::array::from_fn(|i| codes(i, phase))).collect();
        let high = low.iter().map(|l| l.map(|v| v << 4)).collect();
        Self { low, high }
    }
}
