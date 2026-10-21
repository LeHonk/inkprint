//! Text screens (the Identify-Printer status screen), drawn with embedded-graphics' built-in
//! 10×20 font scaled up so it reads from across a kitchen.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

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

/// What the Identify-Printer status screen shows.
pub struct Status<'a> {
    /// Firmware version, shown small at the bottom.
    pub version: &'a str,
    /// `None` before Wi-Fi is configured.
    pub printer: Option<PrinterStatus<'a>>,
}

pub struct PrinterStatus<'a> {
    /// mDNS instance name, e.g. "Inkplate 6 (a1b2)".
    pub instance: &'a str,
    /// Without ".local".
    pub hostname: &'a str,
    /// `None` until DHCP completes.
    pub ip: Option<Ipv4Addr>,
    pub last_job: Option<LastJob<'a>>,
}

pub struct LastJob<'a> {
    pub name: &'a str,
    pub pages: u32,
    /// Seconds since it completed.
    pub age: u32,
}

/// Lines of the status screen: (text, font scale), for [`text_screen`].
pub fn status_lines(status: &Status<'_>) -> Vec<(String, u32)> {
    let mut lines = alloc::vec![("Inkprint".into(), 3)];
    match &status.printer {
        None => lines.push(("No Wi-Fi configured".into(), 2)),
        Some(p) => {
            lines.push((p.instance.into(), 2));
            lines.push((String::new(), 1));
            lines.push((if p.ip.is_some() { "Ready to print" } else { "Connecting..." }.into(), 2));
            lines.push((format!("{}.local", p.hostname), 2));
            if let Some(ip) = p.ip {
                lines.push((format!("IP {ip}"), 2));
            }
            lines.push((String::new(), 1));
            match &p.last_job {
                Some(job) => {
                    let ago = match job.age / 60 {
                        0 => "just now".into(),
                        m if m < 120 => format!("{m} min ago"),
                        m => format!("{} h ago", m / 60),
                    };
                    lines.push((format!("Last job: {}", job.name), 2));
                    lines.push((format!("{} page{}, {ago}", job.pages, if job.pages == 1 { "" } else { "s" }), 2));
                }
                None => lines.push(("No jobs yet".into(), 2)),
            }
        }
    }
    lines.push((String::new(), 1));
    lines.push((format!("Firmware {}", status.version), 1));
    lines
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
    fn status_ends_with_firmware_version() {
        let status = Status {
            version: "v0.1.0",
            printer: Some(PrinterStatus {
                instance: "Inkplate 6 (a1b2)",
                hostname: "inkplate-a1b2",
                ip: Some(Ipv4Addr::new(192, 168, 1, 42)),
                last_job: Some(LastJob { name: "Pancakes", pages: 1, age: 150 }),
            }),
        };
        let lines = status_lines(&status);
        let texts: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
        assert!(texts.contains(&"IP 192.168.1.42"));
        assert!(texts.contains(&"1 page, 2 min ago"));
        assert_eq!(lines.last().unwrap(), &(String::from("Firmware v0.1.0"), 1));

        let unconfigured = status_lines(&Status { version: "v0.1.0", printer: None });
        assert_eq!(unconfigured[1].0, "No Wi-Fi configured");
        assert_eq!(unconfigured.last().unwrap().0, "Firmware v0.1.0");
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
