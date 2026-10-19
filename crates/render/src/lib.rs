//! From decoded raster lines to a panel-ready image.
//!
//! 1. [`PageCapture`] implements [`raster::RasterSink`]: it converts lines to 8-bit gray,
//!    box-downsamples them by an integer factor so a page fits the memory budget, and keeps
//!    the most recently *completed* page (the "last page" of the document).
//! 2. [`prepare_for_panel`] crops whitespace margins, fits the content into the board's
//!    portrait canvas, rotates to native orientation and quantizes to the panel's levels.

#![no_std]

extern crate alloc;

mod gray;
mod quantize;

pub use gray::{GrayImage, Rect};
pub use quantize::{dither_mono, quantize_gray3};

use alloc::vec::Vec;
use board::{Board, Rotation};
use raster::{ColorSpace, PageInfo, RasterSink};

/// Captures pages from the raster decoder, downsampled so width ≤ `max_width`.
pub struct PageCapture {
    max_width: u32,
    current: Option<Capture>,
    last: Option<GrayImage>,
    pub pages_seen: u32,
}

struct Capture {
    info: PageInfo,
    factor: u32,
    image: GrayImage,
    /// Per-output-column sums for the band of `factor` source lines being averaged.
    acc: Vec<u32>,
}

impl PageCapture {
    /// `max_width` bounds memory: about `max_width² × 1.5` bytes per page for A-series.
    /// Two pages are alive at once (last completed + current).
    pub fn new(max_width: u32) -> Self {
        Self { max_width, current: None, last: None, pages_seen: 0 }
    }

    /// The last completed page, if any.
    pub fn last_page(&self) -> Option<&GrayImage> {
        self.last.as_ref()
    }

    pub fn take_last_page(&mut self) -> Option<GrayImage> {
        self.last.take()
    }
}

fn to_gray(cs: ColorSpace, bpp: u32, line: &[u8], x: usize) -> u8 {
    match (cs, bpp) {
        (ColorSpace::Black, 1) => {
            if line[x / 8] & (0x80 >> (x % 8)) != 0 { 0 } else { 255 }
        }
        (ColorSpace::Black, 8) => 255 - line[x],
        (ColorSpace::SRgb, 24) => {
            let p = &line[x * 3..x * 3 + 3];
            // Rec. 601 luma in integer arithmetic.
            ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8
        }
        (_, 8) => line[x],
        _ => line[x * (bpp as usize / 8)], // TODO: 16-bit and other spaces; take MSB.
    }
}

impl RasterSink for PageCapture {
    fn begin_page(&mut self, info: &PageInfo) {
        let factor = info.width.div_ceil(self.max_width).max(1);
        let w = info.width / factor;
        let h = info.height / factor;
        self.current = Some(Capture {
            info: *info,
            factor,
            image: GrayImage::new(w, h),
            acc: alloc::vec![0; w as usize],
        });
    }

    fn line(&mut self, y: u32, data: &[u8]) {
        let Some(c) = self.current.as_mut() else { return };
        let f = c.factor;
        let out_y = y / f;
        if out_y >= c.image.height {
            return;
        }
        for (ox, sum) in c.acc.iter_mut().enumerate() {
            for dx in 0..f as usize {
                let x = ox * f as usize + dx;
                *sum += to_gray(c.info.color_space, c.info.bits_per_pixel, data, x) as u32;
            }
        }
        if y % f == f - 1 {
            let n = f * f;
            let row = c.image.row_mut(out_y);
            for (px, sum) in row.iter_mut().zip(c.acc.iter_mut()) {
                *px = (*sum / n) as u8;
                *sum = 0;
            }
        }
    }

    fn end_page(&mut self) {
        if let Some(c) = self.current.take() {
            self.last = Some(c.image);
            self.pages_seen += 1;
        }
    }
}

/// Crop, fit, rotate and return an 8-bit gray image in the panel's native orientation.
///
/// Quantization to the panel's levels is left to the caller (see [`quantize_gray3`],
/// [`dither_mono`]) since the driver decides the bit packing.
pub fn prepare_for_panel<B: Board>(page: &GrayImage, auto_crop: bool) -> GrayImage {
    let content = if auto_crop {
        page.content_bbox(CONTENT_THRESHOLD, CROP_PADDING)
    } else {
        None
    }
    .unwrap_or(page.full_rect());
    let (w, h) = B::logical_size();
    let fitted = page.fit_into(content, w as u32, h as u32);
    match B::ROTATION {
        Rotation::None => fitted,
        Rotation::Cw90 => fitted.rotate_cw90(),
        Rotation::Cw180 => fitted.rotate_cw90().rotate_cw90(),
        Rotation::Cw270 => fitted.rotate_cw90().rotate_cw90().rotate_cw90(),
    }
}

/// Pixels darker than this count as content when cropping.
pub const CONTENT_THRESHOLD: u8 = 200;
/// Whitespace kept around the content after cropping, in source pixels.
pub const CROP_PADDING: u32 = 8;

#[cfg(test)]
mod tests {
    use super::*;
    use board::Inkplate6;

    fn gray_page(width: u32, height: u32, fill: impl Fn(u32, u32) -> u8) -> Vec<Vec<u8>> {
        (0..height).map(|y| (0..width).map(|x| fill(x, y)).collect()).collect()
    }

    fn info(width: u32, height: u32) -> PageInfo {
        PageInfo {
            width,
            height,
            bits_per_pixel: 8,
            bytes_per_line: width,
            color_space: ColorSpace::SGray,
            dpi_x: 300,
            dpi_y: 300,
        }
    }

    #[test]
    fn keeps_last_completed_page_downsampled() {
        let mut cap = PageCapture::new(100);
        for shade in [10u8, 200] {
            cap.begin_page(&info(400, 600));
            for (y, line) in gray_page(400, 600, |_, _| shade).iter().enumerate() {
                cap.line(y as u32, line);
            }
            cap.end_page();
        }
        let last = cap.last_page().unwrap();
        assert_eq!((last.width, last.height), (100, 150));
        assert!(last.pixels.iter().all(|&p| p == 200));
        assert_eq!(cap.pages_seen, 2);
    }

    #[test]
    fn crop_and_fit_fills_panel() {
        // A4-ish page at low res with a small black block in the middle.
        let mut page = GrayImage::new(210, 297);
        for y in 100..140 {
            page.row_mut(y)[80..130].fill(0);
        }
        let out = prepare_for_panel::<Inkplate6>(&page, true);
        assert_eq!((out.width, out.height), (800, 600));
        let dark = out.pixels.iter().filter(|&&p| p < 128).count();
        // Cropped content should cover a large share of the panel, not a tiny patch.
        assert!(dark > 800 * 600 / 4, "dark pixels: {dark}");
    }
}
