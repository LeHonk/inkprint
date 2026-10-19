use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// 8-bit grayscale image, 0 = black, 255 = white, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl GrayImage {
    /// A white image.
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, pixels: vec![255; (width * height) as usize] }
    }

    pub fn full_rect(&self) -> Rect {
        Rect { x: 0, y: 0, width: self.width, height: self.height }
    }

    pub fn row(&self, y: u32) -> &[u8] {
        let w = self.width as usize;
        &self.pixels[y as usize * w..(y as usize + 1) * w]
    }

    pub fn row_mut(&mut self, y: u32) -> &mut [u8] {
        let w = self.width as usize;
        &mut self.pixels[y as usize * w..(y as usize + 1) * w]
    }

    /// Bounding box of pixels darker than `threshold`, grown by `padding`. `None` if blank.
    pub fn content_bbox(&self, threshold: u8, padding: u32) -> Option<Rect> {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..self.height {
            let row = self.row(y);
            let Some(first) = row.iter().position(|&p| p < threshold) else { continue };
            let last = row.iter().rposition(|&p| p < threshold).unwrap();
            x0 = x0.min(first as u32);
            x1 = x1.max(last as u32);
            y0 = y0.min(y);
            y1 = y;
        }
        if x0 == u32::MAX {
            return None;
        }
        let x = x0.saturating_sub(padding);
        let y = y0.saturating_sub(padding);
        let x_end = (x1 + 1 + padding).min(self.width);
        let y_end = (y1 + 1 + padding).min(self.height);
        Some(Rect { x, y, width: x_end - x, height: y_end - y })
    }

    /// Scales `src` (a region of self) to fit `width`×`height`, preserving aspect ratio and
    /// centering on white. Downscaling averages source pixels; upscaling samples nearest.
    pub fn fit_into(&self, src: Rect, width: u32, height: u32) -> GrayImage {
        let mut out = GrayImage::new(width, height);
        // scale = min(width / src.width, height / src.height), as a fraction num/den.
        let (num, den) = if width as u64 * src.height as u64 <= height as u64 * src.width as u64 {
            (width as u64, src.width as u64)
        } else {
            (height as u64, src.height as u64)
        };
        let dst_w = (src.width as u64 * num / den).max(1) as u32;
        let dst_h = (src.height as u64 * num / den).max(1) as u32;
        let off_x = (width - dst_w) / 2;
        let off_y = (height - dst_h) / 2;

        for dy in 0..dst_h {
            let sy0 = src.y as u64 + dy as u64 * den / num;
            let sy1 = (src.y as u64 + (dy as u64 + 1) * den / num).max(sy0 + 1);
            for dx in 0..dst_w {
                let sx0 = src.x as u64 + dx as u64 * den / num;
                let sx1 = (src.x as u64 + (dx as u64 + 1) * den / num).max(sx0 + 1);
                let mut sum = 0u64;
                for sy in sy0..sy1 {
                    let row = self.row(sy as u32);
                    sum += row[sx0 as usize..sx1 as usize].iter().map(|&p| p as u64).sum::<u64>();
                }
                let n = (sy1 - sy0) * (sx1 - sx0);
                out.row_mut(off_y + dy)[(off_x + dx) as usize] = (sum / n) as u8;
            }
        }
        out
    }

    /// Rotates 90° clockwise.
    pub fn rotate_cw90(&self) -> GrayImage {
        let mut out = GrayImage::new(self.height, self.width);
        for y in 0..self.height {
            for x in 0..self.width {
                let nx = self.height - 1 - y;
                out.pixels[(x * out.width + nx) as usize] = self.pixels[(y * self.width + x) as usize];
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_with_padding_is_clamped() {
        let mut img = GrayImage::new(10, 10);
        img.row_mut(1)[2] = 0;
        img.row_mut(4)[6] = 0;
        assert_eq!(img.content_bbox(128, 2), Some(Rect { x: 0, y: 0, width: 9, height: 7 }));
        assert_eq!(GrayImage::new(5, 5).content_bbox(128, 2), None);
    }

    #[test]
    fn rotate_cw90() {
        let img = GrayImage { width: 2, height: 1, pixels: vec![1, 2] };
        let r = img.rotate_cw90();
        assert_eq!((r.width, r.height, r.pixels), (1, 2, vec![1, 2]));
        let img = GrayImage { width: 1, height: 2, pixels: vec![1, 2] };
        assert_eq!(img.rotate_cw90().pixels, vec![2, 1]);
    }

    #[test]
    fn fit_letterboxes_and_centers() {
        let img = GrayImage { width: 2, height: 1, pixels: vec![0, 0] };
        let out = img.fit_into(img.full_rect(), 4, 4);
        assert_eq!(out.row(0), &[255; 4]);
        assert_eq!(out.row(1), &[0; 4]);
        assert_eq!(out.row(2), &[0; 4]);
        assert_eq!(out.row(3), &[255; 4]);
    }
}
