use alloc::vec::Vec;

use crate::GrayImage;

/// Brightness steps for [`adjust_brightness`]: -2 (much darker) … +2 (much lighter).
pub const BRIGHTNESS_RANGE: core::ops::RangeInclusive<i8> = -2..=2;

/// Lightens (positive `level`) or darkens (negative) the midtones, keeping black and white:
/// `y = x + k·x·(255 − x)/255` with k = 0.3 per step. Monotonic for |k| ≤ 1, integer only.
/// Mostly affects photos and gray areas; black text stays black.
pub fn adjust_brightness(img: &mut GrayImage, level: i8) {
    let k = i32::from(level.clamp(*BRIGHTNESS_RANGE.start(), *BRIGHTNESS_RANGE.end())) * 3;
    if k == 0 {
        return;
    }
    let lut: [u8; 256] = core::array::from_fn(|x| {
        let x = x as i32;
        (x + k * x * (255 - x) / (255 * 10)).clamp(0, 255) as u8
    });
    for p in &mut img.pixels {
        *p = lut[*p as usize];
    }
}

/// Maps 8-bit gray to 8 levels (0 = black … 7 = white), one level per byte.
///
/// Text looks crisper without dithering at 8 levels; add error diffusion here if photos
/// turn out to band.
pub fn quantize_gray3(img: &GrayImage) -> Vec<u8> {
    img.pixels.iter().map(|&p| ((p as u16 * 7 + 127) / 255) as u8).collect()
}

/// Floyd–Steinberg dither to 1 bit; returns one byte per pixel, 0 = black, 1 = white.
pub fn dither_mono(img: &GrayImage) -> Vec<u8> {
    let w = img.width as usize;
    let mut err_cur: Vec<i16> = alloc::vec![0; w + 2];
    let mut err_next: Vec<i16> = alloc::vec![0; w + 2];
    let mut out = Vec::with_capacity(img.pixels.len());
    for y in 0..img.height {
        for (x, &p) in img.row(y).iter().enumerate() {
            let v = (p as i16 + err_cur[x + 1] / 16).clamp(0, 255);
            let (bit, q) = if v >= 128 { (1, 255) } else { (0, 0) };
            out.push(bit);
            let e = v - q;
            err_cur[x + 2] += e * 7;
            err_next[x] += e * 3;
            err_next[x + 1] += e * 5;
            err_next[x + 2] += e;
        }
        core::mem::swap(&mut err_cur, &mut err_next);
        err_next.fill(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_moves_midtones_only() {
        let ramp = || GrayImage { width: 256, height: 1, pixels: (0..=255).collect() };
        let at = |level| {
            let mut img = ramp();
            adjust_brightness(&mut img, level);
            img.pixels
        };
        assert_eq!(at(0), ramp().pixels);
        for level in [-2, -1, 1, 2] {
            let p = at(level);
            assert_eq!((p[0], p[255]), (0, 255), "level {level}: black and white stay");
            assert!(p.windows(2).all(|w| w[0] <= w[1]), "level {level}: monotonic");
        }
        // Mid-gray: 128 → about 166 at +2 (0.6 × 64), 90 at -2.
        assert_eq!((at(-2)[128], at(-1)[128], at(1)[128], at(2)[128]), (90, 109, 147, 166));
        // Out-of-range levels clamp.
        assert_eq!(at(9), at(2));
    }

    #[test]
    fn gray3_endpoints() {
        let img = GrayImage { width: 3, height: 1, pixels: alloc::vec![0, 128, 255] };
        assert_eq!(quantize_gray3(&img), [0, 4, 7]);
    }

    #[test]
    fn mono_mid_gray_is_half_black() {
        let img = GrayImage { width: 16, height: 16, pixels: alloc::vec![128; 256] };
        let ones = dither_mono(&img).iter().filter(|&&b| b == 1).count();
        assert!((100..=156).contains(&ones), "{ones}");
    }
}
