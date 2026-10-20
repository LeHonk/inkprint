//! Canonical Huffman tables (JPEG Annex C) with a 9-bit lookup for short codes.

use embedded_io_async::Read;

use crate::Error;
use crate::reader::{Bits, Source};

const LOOKUP_BITS: u32 = 9;

pub(crate) struct Huffman {
    /// For each 9-bit prefix: (symbol, code length), length 0 = longer code.
    lookup: [(u8, u8); 1 << LOOKUP_BITS],
    /// Per code length 1–16: largest code (-1 if none), and index of its first value.
    maxcode: [i32; 17],
    valoffset: [i32; 17],
    values: [u8; 256],
}

impl Huffman {
    /// `counts[i]`: number of codes of length i+1; `values`: symbols in code order.
    pub(crate) fn new(counts: &[u8; 16], values: &[u8]) -> Result<Self, Error> {
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if total > 256 || total != values.len() {
            return Err(Error::Malformed("bad Huffman table"));
        }
        let mut t = Self { lookup: [(0, 0); 1 << LOOKUP_BITS], maxcode: [-1; 17], valoffset: [0; 17], values: [0; 256] };
        t.values[..total].copy_from_slice(values);
        let mut code = 0u32;
        let mut k = 0usize;
        for len in 1..=16u32 {
            let n = counts[len as usize - 1] as usize;
            if n > 0 {
                t.valoffset[len as usize] = k as i32 - code as i32;
                for _ in 0..n {
                    if len <= LOOKUP_BITS {
                        // Every 9-bit prefix starting with this code decodes to it.
                        let shift = LOOKUP_BITS - len;
                        for fill in 0..(1u32 << shift) {
                            t.lookup[((code << shift) | fill) as usize] = (values[k], len as u8);
                        }
                    }
                    code += 1;
                    k += 1;
                }
                t.maxcode[len as usize] = code as i32 - 1;
            }
            if code > (1 << len) {
                return Err(Error::Malformed("bad Huffman table"));
            }
            code <<= 1;
        }
        Ok(t)
    }

    pub(crate) async fn decode<R: Read>(&self, bits: &mut Bits, src: &mut Source<'_, R>) -> Result<u8, Error> {
        bits.fill(src).await?;
        let (symbol, len) = self.lookup[bits.peek(LOOKUP_BITS) as usize];
        if len > 0 {
            bits.consume(u32::from(len));
            return Ok(symbol);
        }
        for len in LOOKUP_BITS + 1..=16 {
            let code = bits.peek(len) as i32;
            if code <= self.maxcode[len as usize] {
                bits.consume(len);
                return Ok(self.values[(self.valoffset[len as usize] + code) as usize]);
            }
        }
        Err(Error::Malformed("bad Huffman code"))
    }
}

/// Sign-extends a `size`-bit magnitude (JPEG F.2.2.1).
pub(crate) fn extend(v: u32, size: u32) -> i32 {
    if size == 0 {
        0
    } else if v < 1 << (size - 1) {
        v as i32 - (1 << size) + 1
    } else {
        v as i32
    }
}
