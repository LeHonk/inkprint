use alloc::vec::Vec;

use crate::GrayImage;

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
