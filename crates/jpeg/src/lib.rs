//! Streaming baseline JPEG decoder producing grayscale lines.
//!
//! Built for a printer with 4 MB of RAM receiving phone photos: the file is read as it
//! arrives and only one row of 8×8 blocks is decoded at a time, so memory use depends on
//! the image width, not the file size. Only luminance is reconstructed (the panel is
//! gray); chroma blocks are entropy-decoded to keep the bitstream in step, but never
//! transformed.
//!
//! Supported: baseline and extended sequential Huffman JPEG, 8-bit, 1 or 3 components
//! (YCbCr) with any sampling, interleaved or not, restart markers, EXIF orientation.
//! Rejected as [`Error::Unsupported`]: progressive, arithmetic coding, 12-bit, lossless,
//! CMYK, Adobe RGB-transform files.

#![no_std]
// The futures run on single-threaded executors; `Send` is not needed.
#![allow(async_fn_in_trait)]

extern crate alloc;

mod exif;
mod huffman;
mod idct;
mod reader;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use embedded_io_async::Read;
use raster::{ColorSpace, PageInfo, RasterSink};

use huffman::{Huffman, extend};
use idct::{ZIGZAG, idct_block};
use reader::{Bits, Source};

/// Largest width or height accepted (also advertised as `jpeg-x/y-dimension-supported`).
pub const MAX_DIMENSION: u16 = 16_384;

/// Starts with SOI followed by a marker.
pub fn sniff(prefix: &[u8]) -> bool {
    prefix.starts_with(&[0xFF, 0xD8, 0xFF])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Reading the input failed.
    Io,
    NotJpeg,
    /// A valid JPEG this decoder does not handle.
    Unsupported(&'static str),
    Malformed(&'static str),
}

/// Clockwise rotation that makes the image upright (from EXIF).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    pub width: u16,
    pub height: u16,
    pub orientation: Orientation,
}

struct Component {
    id: u8,
    h: usize,
    v: usize,
    quant: usize,
    dc: usize,
    ac: usize,
    pred: i32,
}

/// Decodes one JPEG into `sink` as a single 8-bit gray page. `first` holds bytes already
/// read past whatever came before (e.g. the IPP header); `rest` streams the remainder.
/// Stops after the luminance is complete; trailing data is left unread.
pub async fn decode<R: Read, S: RasterSink>(first: &[u8], rest: &mut R, sink: &mut S) -> Result<Decoded, Error> {
    let mut src = Source::new(first, rest);
    if src.byte().await? != 0xFF || src.byte().await? != 0xD8 {
        return Err(Error::NotJpeg);
    }

    let mut quant = [[0u16; 64]; 4];
    // Boxed: ~1.4 KB each, and the decode future lives in long-lived (static) task memory.
    let mut dc_tables: [Option<Box<Huffman>>; 4] = [None, None, None, None];
    let mut ac_tables: [Option<Box<Huffman>>; 4] = [None, None, None, None];
    let mut components: Vec<Component> = Vec::new();
    let (mut width, mut height) = (0usize, 0usize);
    let mut restart_interval = 0usize;
    let mut orientation = Orientation::Normal;
    let mut adobe_transform: Option<u8> = None;
    let mut dpi = 72u32;
    let mut pending_marker: Option<u8> = None;
    let mut luma: Option<LumaRows> = None;

    loop {
        let marker = match pending_marker.take() {
            Some(m) => m,
            None => next_marker(&mut src).await?,
        };
        match marker {
            0xD8 => return Err(Error::Malformed("SOI inside image")),
            0xD9 => return Err(Error::Malformed("image ended before luminance was complete")),
            0x01 | 0xD0..=0xD7 => continue, // TEM, stray RST: no length
            _ => {}
        }
        let len = src.u16().await? as usize;
        let len = len.checked_sub(2).ok_or(Error::Malformed("bad segment length"))?;
        match marker {
            // SOF0 baseline, SOF1 extended sequential (Huffman).
            0xC0 | 0xC1 => {
                if !components.is_empty() {
                    return Err(Error::Malformed("second frame"));
                }
                if src.byte().await? != 8 {
                    return Err(Error::Unsupported("12-bit precision"));
                }
                height = src.u16().await? as usize;
                width = src.u16().await? as usize;
                let n = src.byte().await? as usize;
                if height == 0 {
                    return Err(Error::Unsupported("height defined later (DNL)"));
                }
                if width == 0 || width > MAX_DIMENSION as usize || height > MAX_DIMENSION as usize {
                    return Err(Error::Unsupported("image too large"));
                }
                if !(n == 1 || n == 3) || len != 6 + 3 * n {
                    return Err(Error::Unsupported("only gray and YCbCr images"));
                }
                for _ in 0..n {
                    let id = src.byte().await?;
                    let hv = src.byte().await?;
                    let quant = src.byte().await? as usize;
                    let (h, v) = ((hv >> 4) as usize, (hv & 15) as usize);
                    if !(1..=4).contains(&h) || !(1..=4).contains(&v) || quant > 3 {
                        return Err(Error::Malformed("bad component"));
                    }
                    components.push(Component { id, h, v, quant, dc: 0, ac: 0, pred: 0 });
                }
                if n == 3 && adobe_transform == Some(0) {
                    return Err(Error::Unsupported("RGB JPEG"));
                }
            }
            0xC2 | 0xC6 | 0xCA | 0xCE => return Err(Error::Unsupported("progressive JPEG")),
            0xC3 | 0xC5 | 0xC7 | 0xC9..=0xCB | 0xCD | 0xCF => return Err(Error::Unsupported("lossless or arithmetic-coded JPEG")),
            0xC4 => {
                let mut left = len;
                while left > 0 {
                    let tc_th = src.byte().await?;
                    let mut counts = [0u8; 16];
                    for c in &mut counts {
                        *c = src.byte().await?;
                    }
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    let mut values = vec![0u8; total];
                    for v in &mut values {
                        *v = src.byte().await?;
                    }
                    let table = Some(Box::new(Huffman::new(&counts, &values)?));
                    let id = (tc_th & 15) as usize;
                    match (tc_th >> 4, id) {
                        (0, 0..=3) => dc_tables[id] = table,
                        (1, 0..=3) => ac_tables[id] = table,
                        _ => return Err(Error::Malformed("bad Huffman table id")),
                    }
                    left = left.checked_sub(17 + total).ok_or(Error::Malformed("bad DHT length"))?;
                }
            }
            0xDB => {
                let mut left = len;
                while left > 0 {
                    let pq_tq = src.byte().await?;
                    let id = (pq_tq & 15) as usize;
                    if id > 3 {
                        return Err(Error::Malformed("bad quantization table id"));
                    }
                    let wide = pq_tq >> 4 != 0;
                    for q in quant[id].iter_mut() {
                        *q = if wide { src.u16().await? } else { u16::from(src.byte().await?) };
                    }
                    left = left.checked_sub(if wide { 129 } else { 65 }).ok_or(Error::Malformed("bad DQT length"))?;
                }
            }
            0xDD => {
                if len < 2 {
                    return Err(Error::Malformed("bad DRI length"));
                }
                restart_interval = src.u16().await? as usize;
                src.skip(len - 2).await?;
            }
            0xE0 => {
                // JFIF density: units 1 = dots per inch.
                let mut seg = vec![0u8; len];
                for b in &mut seg {
                    *b = src.byte().await?;
                }
                if seg.starts_with(b"JFIF\0") && seg.len() >= 12 && seg[7] == 1 {
                    dpi = u32::from(u16::from_be_bytes([seg[8], seg[9]])).max(1);
                }
            }
            0xE1 => {
                let mut seg = vec![0u8; len];
                for b in &mut seg {
                    *b = src.byte().await?;
                }
                if let Some(o) = exif::orientation(&seg) {
                    orientation = o;
                }
            }
            0xEE => {
                let mut seg = vec![0u8; len];
                for b in &mut seg {
                    *b = src.byte().await?;
                }
                if seg.starts_with(b"Adobe") && seg.len() >= 12 {
                    adobe_transform = Some(seg[11]);
                    if components.len() == 3 && seg[11] == 0 {
                        return Err(Error::Unsupported("RGB JPEG"));
                    }
                }
            }
            0xDA => {
                if components.is_empty() {
                    return Err(Error::Malformed("scan before frame"));
                }
                let n = src.byte().await? as usize;
                if n == 0 || n > components.len() || len != 4 + 2 * n {
                    return Err(Error::Malformed("bad scan header"));
                }
                let mut in_scan = Vec::with_capacity(n);
                for _ in 0..n {
                    let id = src.byte().await?;
                    let tables = src.byte().await?;
                    let c = components.iter().position(|c| c.id == id).ok_or(Error::Malformed("unknown scan component"))?;
                    components[c].dc = (tables >> 4) as usize;
                    components[c].ac = (tables & 15) as usize;
                    if components[c].dc > 3 || components[c].ac > 3 {
                        return Err(Error::Malformed("bad table selector"));
                    }
                    in_scan.push(c);
                }
                // Spectral selection and successive approximation: fixed in sequential mode.
                src.skip(3).await?;

                let rows = luma.get_or_insert_with(|| LumaRows::new(width, height, &components));
                if !rows.started {
                    sink.begin_page(&PageInfo {
                        width: width as u32,
                        height: height as u32,
                        bits_per_pixel: 8,
                        bytes_per_line: width as u32,
                        color_space: ColorSpace::SGray,
                        dpi_x: dpi,
                        dpi_y: dpi,
                    });
                    rows.started = true;
                }
                let tables = Tables { quant: &quant, dc: &dc_tables, ac: &ac_tables };
                let marker = scan(&mut src, &mut components, &in_scan, restart_interval, &tables, rows, sink).await?;
                if rows.done() {
                    sink.end_page();
                    return Ok(Decoded { width: width as u16, height: height as u16, orientation });
                }
                pending_marker = marker;
            }
            _ => src.skip(len).await?,
        }
    }
}

/// Skips fill bytes and returns the next marker code.
async fn next_marker<R: Read>(src: &mut Source<'_, R>) -> Result<u8, Error> {
    if src.byte().await? != 0xFF {
        return Err(Error::Malformed("expected a marker"));
    }
    loop {
        match src.byte().await? {
            0xFF => continue,
            0x00 => return Err(Error::Malformed("expected a marker")),
            m => return Ok(m),
        }
    }
}

struct Tables<'a> {
    quant: &'a [[u16; 64]; 4],
    dc: &'a [Option<Box<Huffman>>; 4],
    ac: &'a [Option<Box<Huffman>>; 4],
}

/// Luminance rows of the block row being decoded, and how many lines went out.
struct LumaRows {
    width: usize,
    height: usize,
    /// Luma block columns across the image (padded), and pixel rows per block row.
    blocks_w: usize,
    rows_per_band: usize,
    buf: Vec<u8>,
    emitted: usize,
    started: bool,
}

impl LumaRows {
    fn new(width: usize, height: usize, components: &[Component]) -> Self {
        let (hmax, vmax) = max_sampling(components);
        let mcu_w = 8 * hmax;
        let blocks_w = width.div_ceil(mcu_w) * hmax;
        let rows_per_band = 8 * vmax;
        Self { width, height, blocks_w, rows_per_band, buf: vec![0; blocks_w * 8 * rows_per_band], emitted: 0, started: false }
    }

    fn stride(&self) -> usize {
        self.blocks_w * 8
    }

    fn done(&self) -> bool {
        self.emitted >= self.height
    }

    /// Sends the first `rows` decoded rows of the band, cropped to the image.
    fn emit<S: RasterSink>(&mut self, rows: usize, sink: &mut S) {
        let stride = self.stride();
        for r in 0..rows {
            if self.emitted >= self.height {
                break;
            }
            sink.line(self.emitted as u32, &self.buf[r * stride..r * stride + self.width]);
            self.emitted += 1;
        }
    }
}

fn max_sampling(components: &[Component]) -> (usize, usize) {
    (components.iter().map(|c| c.h).max().unwrap_or(1), components.iter().map(|c| c.v).max().unwrap_or(1))
}

/// Decodes one scan. Returns a marker the entropy decoder already ran into, if any.
async fn scan<R: Read, S: RasterSink>(
    src: &mut Source<'_, R>,
    components: &mut [Component],
    in_scan: &[usize],
    restart_interval: usize,
    tables: &Tables<'_>,
    luma: &mut LumaRows,
    sink: &mut S,
) -> Result<Option<u8>, Error> {
    let (hmax, vmax) = max_sampling(components);
    let width = luma.width;
    let height = luma.height;
    let mut bits = Bits::new();
    for &c in in_scan {
        components[c].pred = 0;
        let c = &components[c];
        if tables.dc[c.dc].is_none() || tables.ac[c.ac].is_none() {
            return Err(Error::Malformed("missing Huffman table"));
        }
    }
    // The first frame component is luminance; it must not be subsampled.
    if components[0].h != hmax || components[0].v != vmax {
        return Err(Error::Unsupported("subsampled luminance"));
    }

    // A single-component scan walks that component's own block grid, one block per MCU.
    let single = in_scan.len() == 1;
    let (mcus_x, mcus_y) = if single {
        let c = &components[in_scan[0]];
        (width.mul_ceil_div(c.h, hmax).div_ceil(8), height.mul_ceil_div(c.v, vmax).div_ceil(8))
    } else {
        (width.div_ceil(8 * hmax), height.div_ceil(8 * vmax))
    };
    let luma_in_scan = in_scan.contains(&0);
    let stride = luma.stride();
    let mut coef = [0i32; 64];
    let mut mcu = 0usize;

    for _ in 0..mcus_y {
        for mx in 0..mcus_x {
            if restart_interval > 0 && mcu > 0 && mcu.is_multiple_of(restart_interval) {
                bits.restart(src).await?;
                for &c in in_scan {
                    components[c].pred = 0;
                }
            }
            mcu += 1;
            for &ci in in_scan {
                let (bh, bv) = if single { (1, 1) } else { (components[ci].h, components[ci].v) };
                for by in 0..bv {
                    for bx in 0..bh {
                        let is_luma = ci == 0;
                        let c = &mut components[ci];
                        decode_block(src, &mut bits, c, tables, is_luma, &mut coef).await?;
                        if is_luma {
                            // Single scan: block row `my` of 8 lines; interleaved: band of MCUs.
                            let (x, y) = if single { (mx * 8, 0) } else { ((mx * bh + bx) * 8, by * 8) };
                            if x < stride {
                                idct_block(&coef, &mut luma.buf[y * stride + x..], stride);
                            }
                        }
                    }
                }
            }
        }
        if luma_in_scan {
            let rows = if single { 8 } else { luma.rows_per_band };
            luma.emit(rows, sink);
            if luma.done() {
                return Ok(None);
            }
        }
    }
    Ok(bits.marker)
}

/// Decodes one block's coefficients; dequantizes them into `coef` only for luminance.
async fn decode_block<R: Read>(
    src: &mut Source<'_, R>,
    bits: &mut Bits,
    c: &mut Component,
    tables: &Tables<'_>,
    keep: bool,
    coef: &mut [i32; 64],
) -> Result<(), Error> {
    let dc = tables.dc[c.dc].as_ref().unwrap();
    let ac = tables.ac[c.ac].as_ref().unwrap();
    let q = &tables.quant[c.quant];
    let size = u32::from(dc.decode(bits, src).await?);
    if size > 11 {
        return Err(Error::Malformed("bad DC size"));
    }
    let diff = extend(bits.receive(src, size).await?, size);
    c.pred += diff;
    if keep {
        coef.fill(0);
        coef[0] = c.pred * i32::from(q[0]);
    }
    let mut k = 1;
    while k < 64 {
        let rs = ac.decode(bits, src).await?;
        let (run, size) = ((rs >> 4) as usize, u32::from(rs & 15));
        if size == 0 {
            if run == 15 {
                k += 16;
                continue;
            }
            break; // end of block
        }
        k += run;
        if k > 63 {
            return Err(Error::Malformed("coefficient index out of range"));
        }
        let v = extend(bits.receive(src, size).await?, size);
        if keep {
            coef[ZIGZAG[k]] = v * i32::from(q[k]);
        }
        k += 1;
    }
    Ok(())
}

trait MulCeilDiv {
    fn mul_ceil_div(self, mul: usize, div: usize) -> usize;
}

impl MulCeilDiv for usize {
    /// ceil(self × mul / div): a component's size from the image size and its sampling.
    fn mul_ceil_div(self, mul: usize, div: usize) -> usize {
        (self * mul).div_ceil(div)
    }
}
