//! Text screens (the Identify-Printer status screen), drawn with embedded-graphics' built-in
//! 10×20 font scaled up so it reads from across a kitchen.

use alloc::string::String;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};

use crate::GrayImage;

/// One line of a text screen: text and integer scale of the 10×20 font.
pub struct Line<'a> {
    pub text: &'a str,
    pub scale: u32,
}

/// Draws onto a [`GrayImage`] with every font pixel blown up to `scale × scale`.
struct Scaled<'a> {
    image: &'a mut GrayImage,
    scale: u32,
}

impl OriginDimensions for Scaled<'_> {
    fn size(&self) -> Size {
        Size::new(self.image.width / self.scale, self.image.height / self.scale)
    }
}

impl DrawTarget for Scaled<'_> {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<BinaryColor>>>(&mut self, pixels: I) -> Result<(), Self::Error> {
        let s = self.scale;
        for Pixel(p, color) in pixels {
            if p.x < 0 || p.y < 0 || color == BinaryColor::Off {
                continue;
            }
            let (x0, y0) = (p.x as u32 * s, p.y as u32 * s);
            if x0 + s > self.image.width || y0 + s > self.image.height {
                continue;
            }
            for y in y0..y0 + s {
                self.image.row_mut(y)[x0 as usize..(x0 + s) as usize].fill(0);
            }
        }
        Ok(())
    }
}

/// White `width`×`height` image with `lines` stacked from the top, left-aligned with a
/// margin. Lines too long for the width are cut and end in "...".
pub fn text_screen(width: u32, height: u32, lines: &[Line<'_>]) -> GrayImage {
    const MARGIN: u32 = 24;
    let (char_w, char_h) = (FONT_10X20.character_size.width, FONT_10X20.character_size.height);
    let mut image = GrayImage::new(width, height);
    let mut y = MARGIN;
    for line in lines {
        let scale = line.scale.max(1);
        let max_chars = ((width - 2 * MARGIN) / (char_w * scale)) as usize;
        let text: String = if line.text.chars().count() > max_chars {
            line.text.chars().take(max_chars.saturating_sub(3)).chain("...".chars()).collect()
        } else {
            line.text.into()
        };
        let mut target = Scaled { image: &mut image, scale };
        let origin = Point::new((MARGIN / scale) as i32, (y / scale) as i32);
        let _ = Text::with_baseline(&text, origin, MonoTextStyle::new(&FONT_10X20, BinaryColor::On), Baseline::Top)
            .draw(&mut target);
        y += char_h * scale + char_h / 2;
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(img: &GrayImage, y0: u32, y1: u32) -> usize {
        (y0..y1).map(|y| img.row(y).iter().filter(|&&p| p == 0).count()).sum()
    }

    #[test]
    fn draws_scaled_lines_top_down() {
        let img = text_screen(600, 800, &[Line { text: "Inkprint", scale: 3 }, Line { text: "ready", scale: 2 }]);
        assert_eq!((img.width, img.height), (600, 800));
        // Title: 60 px tall from the margin; second line below it; nothing further down.
        assert!(ink(&img, 24, 84) > 1000);
        assert!(ink(&img, 94, 134) > 300);
        assert_eq!(ink(&img, 200, 800), 0);
        // Pure black text on white: nothing in between for the panel to dither.
        assert!(img.pixels.iter().all(|&p| p == 0 || p == 255));
    }

    #[test]
    fn long_lines_are_cut() {
        let long = "x".repeat(100);
        let img = text_screen(600, 200, &[Line { text: &long, scale: 2 }]);
        // 552 px usable / 20 px per char = 27 chars: no ink in the right margin.
        let right: usize = (0..200).map(|y| img.row(y)[576..].iter().filter(|&&p| p == 0).count()).sum();
        assert_eq!(right, 0);
    }
}
