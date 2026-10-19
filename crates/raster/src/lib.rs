//! Streaming decoders for the two raster formats IPP clients send:
//!
//! * PWG Raster (`image/pwg-raster`, PWG 5102.4): `"RaS2"`, then per page a 1796-byte
//!   big-endian header followed by compressed lines.
//! * Apple Raster (`image/urf`): `"UNIRAST\0"`, a u32 page count, then per page a 32-byte
//!   header followed by compressed lines.
//!
//! Both use the same line compression: a line-repeat byte, then PackBits-style runs of
//! whole pixels. The decoder is a push state machine: feed it chunks of any size with
//! [`Decoder::push`] and it calls a [`RasterSink`] for every decoded line.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Pwg,
    Urf,
}

impl Format {
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/pwg-raster" => Some(Format::Pwg),
            "image/urf" => Some(Format::Urf),
            _ => None,
        }
    }

    /// Detects the format from the first bytes, for `application/octet-stream`.
    pub fn sniff(prefix: &[u8]) -> Option<Self> {
        if prefix.starts_with(PWG_SYNC) {
            Some(Format::Pwg)
        } else if prefix.starts_with(URF_SYNC) {
            Some(Format::Urf)
        } else {
            None
        }
    }

    fn sync(self) -> &'static [u8] {
        match self {
            Format::Pwg => PWG_SYNC,
            Format::Urf => URF_SYNC,
        }
    }

    fn page_header_len(self) -> usize {
        match self {
            Format::Pwg => 1796,
            Format::Urf => 32,
        }
    }
}

const PWG_SYNC: &[u8] = b"RaS2";
const URF_SYNC: &[u8] = b"UNIRAST\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// 8-bit gray, 0 = black (PWG sgray_8 / URF W8).
    SGray,
    /// 24-bit sRGB.
    SRgb,
    /// 1-bit or 8-bit black, 0 = white (PWG black_1 / black_8).
    Black,
    Other(u32),
}

impl ColorSpace {
    /// Byte value of a white pixel in this color space.
    pub fn white(self) -> u8 {
        match self {
            ColorSpace::Black => 0x00,
            _ => 0xff,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageInfo {
    pub width: u32,
    pub height: u32,
    pub bits_per_pixel: u32,
    pub bytes_per_line: u32,
    pub color_space: ColorSpace,
    pub dpi_x: u32,
    pub dpi_y: u32,
}

/// Receives decoded pages.
pub trait RasterSink {
    fn begin_page(&mut self, info: &PageInfo);
    /// One uncompressed line, `info.bytes_per_line` long. Called in order for y = 0..height.
    fn line(&mut self, y: u32, data: &[u8]);
    fn end_page(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BadSync,
    /// Page header with zero/oversized dimensions or an unsupported pixel size.
    BadHeader,
    /// A run or literal crosses the end of the line.
    BadLine,
    /// `finish` called in the middle of a header or page.
    Truncated,
}

/// Upper bound on bytes per line, so a bogus header cannot make us allocate megabytes.
pub const MAX_BYTES_PER_LINE: u32 = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Sync,
    UrfPageCount,
    PageHeader,
    LineRepeat,
    Control,
    /// Collecting one pixel that is then repeated `count` times.
    RepeatPixel { count: usize },
    /// Copying `remaining` literal bytes into the line.
    Literal { remaining: usize },
}

pub struct Decoder {
    format: Format,
    state: State,
    header: Vec<u8>,
    page: Option<PageInfo>,
    line: Vec<u8>,
    /// Bytes of `line` filled so far.
    pos: usize,
    /// Bytes per pixel for run-length purposes (1 for sub-byte depths).
    unit: usize,
    pixel: [u8; 8],
    pixel_fill: usize,
    repeat: u32,
    y: u32,
}

impl Decoder {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            state: State::Sync,
            header: Vec::new(),
            page: None,
            line: Vec::new(),
            pos: 0,
            unit: 1,
            pixel: [0; 8],
            pixel_fill: 0,
            repeat: 0,
            y: 0,
        }
    }

    pub fn push<S: RasterSink>(&mut self, mut data: &[u8], sink: &mut S) -> Result<(), Error> {
        while !data.is_empty() {
            match self.state {
                State::Sync => {
                    let sync = self.format.sync();
                    if self.fill_header(&mut data, sync.len()) {
                        if self.header != sync {
                            return Err(Error::BadSync);
                        }
                        self.header.clear();
                        self.state = match self.format {
                            Format::Pwg => State::PageHeader,
                            Format::Urf => State::UrfPageCount,
                        };
                    }
                }
                State::UrfPageCount => {
                    // Informational only; some clients send 0. Pages end when data ends.
                    if self.fill_header(&mut data, 4) {
                        self.header.clear();
                        self.state = State::PageHeader;
                    }
                }
                State::PageHeader => {
                    if self.fill_header(&mut data, self.format.page_header_len()) {
                        let info = match self.format {
                            Format::Pwg => parse_pwg_header(&self.header)?,
                            Format::Urf => parse_urf_header(&self.header)?,
                        };
                        self.header.clear();
                        self.start_page(info, sink);
                    }
                }
                State::LineRepeat => {
                    self.repeat = data[0] as u32 + 1;
                    data = &data[1..];
                    self.state = State::Control;
                }
                State::Control => {
                    let c = data[0];
                    data = &data[1..];
                    match c {
                        0..=127 => {
                            self.pixel_fill = 0;
                            self.state = State::RepeatPixel { count: c as usize + 1 };
                        }
                        128 => {
                            let white = self.page.unwrap().color_space.white();
                            self.line[self.pos..].fill(white);
                            self.pos = self.line.len();
                            self.after_run(sink);
                        }
                        _ => {
                            let remaining = (257 - c as usize) * self.unit;
                            if self.pos + remaining > self.line.len() {
                                return Err(Error::BadLine);
                            }
                            self.state = State::Literal { remaining };
                        }
                    }
                }
                State::RepeatPixel { count } => {
                    let n = (self.unit - self.pixel_fill).min(data.len());
                    self.pixel[self.pixel_fill..self.pixel_fill + n].copy_from_slice(&data[..n]);
                    self.pixel_fill += n;
                    data = &data[n..];
                    if self.pixel_fill == self.unit {
                        let bytes = count * self.unit;
                        if self.pos + bytes > self.line.len() {
                            return Err(Error::BadLine);
                        }
                        for chunk in self.line[self.pos..self.pos + bytes].chunks_exact_mut(self.unit)
                        {
                            chunk.copy_from_slice(&self.pixel[..self.unit]);
                        }
                        self.pos += bytes;
                        self.after_run(sink);
                    }
                }
                State::Literal { remaining } => {
                    let n = remaining.min(data.len());
                    self.line[self.pos..self.pos + n].copy_from_slice(&data[..n]);
                    self.pos += n;
                    data = &data[n..];
                    if n == remaining {
                        self.after_run(sink);
                    } else {
                        self.state = State::Literal { remaining: remaining - n };
                    }
                }
            }
        }
        Ok(())
    }

    /// Call at end of document data. Errors if the stream stopped mid-page.
    pub fn finish(&self) -> Result<(), Error> {
        match self.state {
            State::PageHeader if self.header.is_empty() => Ok(()),
            _ => Err(Error::Truncated),
        }
    }

    /// Appends from `data` until `header` holds `n` bytes; returns true when complete.
    fn fill_header(&mut self, data: &mut &[u8], n: usize) -> bool {
        let take = (n - self.header.len()).min(data.len());
        self.header.extend_from_slice(&data[..take]);
        *data = &data[take..];
        self.header.len() == n
    }

    fn start_page<S: RasterSink>(&mut self, info: PageInfo, sink: &mut S) {
        self.unit = (info.bits_per_pixel as usize / 8).max(1);
        self.line.clear();
        self.line.resize(info.bytes_per_line as usize, 0);
        self.pos = 0;
        self.y = 0;
        self.page = Some(info);
        sink.begin_page(&info);
        self.state = State::LineRepeat;
    }

    fn after_run<S: RasterSink>(&mut self, sink: &mut S) {
        if self.pos < self.line.len() {
            self.state = State::Control;
            return;
        }
        let height = self.page.unwrap().height;
        for _ in 0..self.repeat {
            if self.y >= height {
                break;
            }
            sink.line(self.y, &self.line);
            self.y += 1;
        }
        self.pos = 0;
        if self.y >= height {
            sink.end_page();
            self.page = None;
            self.state = State::PageHeader;
        } else {
            self.state = State::LineRepeat;
        }
    }
}

fn be_u32(b: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([b[offset], b[offset + 1], b[offset + 2], b[offset + 3]])
}

fn validate(info: PageInfo) -> Result<PageInfo, Error> {
    let min_bpl = (info.width as u64 * info.bits_per_pixel as u64).div_ceil(8);
    let ok = info.width > 0
        && info.height > 0
        && matches!(info.bits_per_pixel, 1 | 8 | 16 | 24 | 32 | 64)
        && info.bytes_per_line as u64 >= min_bpl
        && info.bytes_per_line <= MAX_BYTES_PER_LINE
        && (info.bits_per_pixel < 8 || info.bytes_per_line.is_multiple_of(info.bits_per_pixel / 8));
    if ok { Ok(info) } else { Err(Error::BadHeader) }
}

/// Field offsets from PWG 5102.4 section 4.3 (cups_page_header2_t layout).
fn parse_pwg_header(h: &[u8]) -> Result<PageInfo, Error> {
    let color_space = match be_u32(h, 400) {
        18 => ColorSpace::SGray,
        19 => ColorSpace::SRgb,
        3 => ColorSpace::Black,
        other => ColorSpace::Other(other),
    };
    validate(PageInfo {
        dpi_x: be_u32(h, 276),
        dpi_y: be_u32(h, 280),
        width: be_u32(h, 372),
        height: be_u32(h, 376),
        bits_per_pixel: be_u32(h, 388),
        bytes_per_line: be_u32(h, 392),
        color_space,
    })
}

/// URF page header: bpp, colorspace, duplex, quality (u8 each), 8 reserved bytes,
/// width, height, dpi (u32 BE), 8 reserved bytes.
fn parse_urf_header(h: &[u8]) -> Result<PageInfo, Error> {
    let bits_per_pixel = h[0] as u32;
    let color_space = match h[1] {
        0 | 4 => ColorSpace::SGray,
        1 | 5 => ColorSpace::SRgb,
        other => ColorSpace::Other(other as u32),
    };
    let width = be_u32(h, 12);
    let dpi = be_u32(h, 20);
    validate(PageInfo {
        width,
        height: be_u32(h, 16),
        bits_per_pixel,
        bytes_per_line: width.saturating_mul(bits_per_pixel / 8),
        color_space,
        dpi_x: dpi,
        dpi_y: dpi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[derive(Default)]
    struct Collect {
        pages: Vec<(PageInfo, Vec<Vec<u8>>)>,
        ended: usize,
    }

    impl RasterSink for Collect {
        fn begin_page(&mut self, info: &PageInfo) {
            self.pages.push((*info, Vec::new()));
        }
        fn line(&mut self, y: u32, data: &[u8]) {
            let lines = &mut self.pages.last_mut().unwrap().1;
            assert_eq!(lines.len() as u32, y);
            lines.push(data.to_vec());
        }
        fn end_page(&mut self) {
            self.ended += 1;
        }
    }

    fn pwg_header(width: u32, height: u32) -> Vec<u8> {
        let mut h = vec![0u8; 1796];
        let mut put = |off: usize, v: u32| h[off..off + 4].copy_from_slice(&v.to_be_bytes());
        put(276, 150);
        put(280, 150);
        put(372, width);
        put(376, height);
        put(384, 8);
        put(388, 8);
        put(392, width);
        put(400, 18);
        h
    }

    /// 4×3 sgray_8 page: two identical lines of black, then "a b c d".
    fn pwg_page() -> Vec<u8> {
        let mut d = pwg_header(4, 3);
        d.extend_from_slice(&[1, 3, 0x00]); // repeat ×2, run of 4 × 0x00
        d.extend_from_slice(&[0, 254, b'a', b'b', b'c', 0, b'd']); // 3 literals, run of 1
        d
    }

    fn decode(format: Format, data: &[u8], chunk: usize) -> Collect {
        let mut dec = Decoder::new(format);
        let mut sink = Collect::default();
        for c in data.chunks(chunk) {
            dec.push(c, &mut sink).unwrap();
        }
        dec.finish().unwrap();
        sink
    }

    #[test]
    fn pwg_two_pages_any_chunking() {
        let mut data = b"RaS2".to_vec();
        data.extend(pwg_page());
        data.extend(pwg_page());
        for chunk in [1, 3, 7, 1000, data.len()] {
            let out = decode(Format::Pwg, &data, chunk);
            assert_eq!(out.pages.len(), 2);
            assert_eq!(out.ended, 2);
            let (info, lines) = &out.pages[1];
            assert_eq!((info.width, info.height, info.dpi_x), (4, 3, 150));
            assert_eq!(info.color_space, ColorSpace::SGray);
            assert_eq!(lines, &[vec![0; 4], vec![0; 4], b"abcd".to_vec()]);
        }
    }

    #[test]
    fn urf_rgb_with_fill_to_white() {
        let mut data = b"UNIRAST\0".to_vec();
        data.extend(1u32.to_be_bytes());
        let mut h = vec![0u8; 32];
        h[0] = 24;
        h[1] = 1;
        h[12..16].copy_from_slice(&3u32.to_be_bytes());
        h[16..20].copy_from_slice(&1u32.to_be_bytes());
        h[20..24].copy_from_slice(&300u32.to_be_bytes());
        data.extend(h);
        data.extend([0, 0, 1, 2, 3, 128]); // one red-ish pixel, rest white
        let out = decode(Format::Urf, &data, 2);
        let (info, lines) = &out.pages[0];
        assert_eq!(info.color_space, ColorSpace::SRgb);
        assert_eq!(lines[0], [1, 2, 3, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn errors() {
        let mut sink = Collect::default();
        assert_eq!(Decoder::new(Format::Pwg).push(b"RaS3", &mut sink), Err(Error::BadSync));

        let mut data = b"RaS2".to_vec();
        data.extend(pwg_header(4, 1));
        data.extend([0, 10, 0x00]); // run of 11 pixels in a 4-pixel line
        assert_eq!(Decoder::new(Format::Pwg).push(&data, &mut sink), Err(Error::BadLine));

        let mut dec = Decoder::new(Format::Pwg);
        dec.push(b"RaS2", &mut sink).unwrap();
        dec.push(&pwg_header(4, 1)[..100], &mut sink).unwrap();
        assert_eq!(dec.finish(), Err(Error::Truncated));
    }

    #[test]
    fn sniff() {
        assert_eq!(Format::sniff(b"RaS2\0\0"), Some(Format::Pwg));
        assert_eq!(Format::sniff(b"UNIRAST\0"), Some(Format::Urf));
        assert_eq!(Format::sniff(b"%PDF-1.7"), None);
    }
}
