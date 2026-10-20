//! Display task: shows the last page of each print job, and a status screen on
//! Identify-Printer.

use alloc::vec::Vec;

use board::{Board, PixelKind, Selected};
use embassy_futures::select::{Either, Either4, select, select4};
use embassy_time::{Duration, Instant, Timer};
use epd_inkplate::inkplate6::Inkplate6Panel;
use epd_inkplate::{EpdDriver, Frame};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use embedded_hal_bus::i2c::RefCellDevice;
use esp_hal::i2c::master::I2c;
use log::{info, warn};
use render::GrayImage;

use crate::device::{self, REDRAW};
use crate::net::{IDENTIFY, PAGES, TEXT, status_lines};
use crate::panel_bus::I2sPanelBus;

pub type Panel = Inkplate6Panel<Selected, I2sPanelBus, RefCellDevice<'static, I2c<'static, Blocking>>, Delay>;

/// Crop whitespace margins so a page printed on large paper fills the panel.
const AUTO_CROP: bool = true;

/// A refresh is synchronous and stalls the network tasks for a few seconds, so give the
/// IPP response time to go out first.
const SETTLE: Duration = Duration::from_millis(500);

/// How long Identify-Printer shows the status screen before the page comes back.
const IDENTIFY_FOR: Duration = Duration::from_secs(15);

/// Panel data in the board's pixel format, ready for [`Frame`]. `brightness` adjusts the
/// midtones (see `render::adjust_brightness`).
fn panel_data(page: &GrayImage, crop: bool, brightness: i8) -> Vec<u8> {
    let mut image = render::prepare_for_panel::<Selected>(page, crop);
    render::adjust_brightness(&mut image, brightness);
    match Selected::PIXEL {
        PixelKind::Mono1 => render::dither_mono(&image),
        PixelKind::Gray3 | PixelKind::Color7 => render::quantize_gray3(&image),
    }
}

fn show(panel: &mut Panel, data: &[u8], what: &str) {
    let frame = match Selected::PIXEL {
        PixelKind::Mono1 => Frame::Mono(data),
        PixelKind::Gray3 | PixelKind::Color7 => Frame::Gray3(data),
    };
    let start = Instant::now();
    match panel.show(frame) {
        Ok(()) => info!("display: {what} shown in {} ms", start.elapsed().as_millis()),
        Err(e) => warn!("display: refresh failed: {e:?}"),
    }
}

fn burn_in_clean(panel: &mut Panel) {
    let start = Instant::now();
    match panel.burn_in_clean() {
        Ok(()) => info!("display: burn-in clean in {} ms", start.elapsed().as_millis()),
        Err(e) => warn!("display: burn-in clean failed: {e:?}"),
    }
}

fn text_screen(lines: &[(alloc::string::String, u32)]) -> Vec<u8> {
    let text: Vec<render::Line<'_>> = lines.iter().map(|(text, scale)| render::Line { text, scale: *scale }).collect();
    let (w, h) = Selected::logical_size();
    panel_data(&render::text_screen(w as u32, h as u32, &text), false, 0)
}

/// A printed page as the panel shows it, with the current brightness setting.
fn page_data(page: &GrayImage) -> Vec<u8> {
    panel_data(page, AUTO_CROP, device::current().brightness)
}

#[embassy_executor::task]
pub async fn display_task(mut panel: Panel) {
    // The last page shown, kept as gray so it can be redrawn: after the status screen, or
    // with a new brightness. Before the first job the panel shows whatever it showed before
    // boot, which cannot be redrawn.
    let mut last_page: Option<GrayImage> = None;
    // Text screens' saturated black survives a normal refresh as a ghost, so whatever
    // replaces them gets a burn-in clean first.
    let mut status_shown = false;
    loop {
        match select4(PAGES.wait(), IDENTIFY.wait(), TEXT.wait(), REDRAW.wait()).await {
            Either4::Third(lines) => {
                show(&mut panel, &text_screen(&lines), "message");
                status_shown = true;
            }
            Either4::Fourth(()) => {
                if let Some(page) = &last_page {
                    if core::mem::take(&mut status_shown) {
                        burn_in_clean(&mut panel);
                    }
                    show(&mut panel, &page_data(page), "page (new brightness)");
                }
            }
            Either4::First(mut page) => {
                Timer::after(SETTLE).await;
                // Another job finished meanwhile: only its page matters.
                if let Some(newer) = PAGES.try_take() {
                    page = newer;
                }
                drop(last_page.take()); // free it before rendering the new one
                let data = page_data(&page);
                if core::mem::take(&mut status_shown) {
                    burn_in_clean(&mut panel);
                }
                show(&mut panel, &data, "page");
                last_page = Some(page);
            }
            Either4::Second(()) => {
                Timer::after(SETTLE).await;
                show(&mut panel, &text_screen(&status_lines()), "status screen");
                status_shown = true;

                match select(PAGES.wait(), Timer::after(IDENTIFY_FOR)).await {
                    // A new page replaces the status screen right away (next iteration).
                    Either::First(page) => PAGES.signal(page),
                    Either::Second(()) => {
                        if let Some(page) = &last_page {
                            burn_in_clean(&mut panel);
                            status_shown = false;
                            show(&mut panel, &page_data(page), "page (restored)");
                        }
                    }
                }
            }
        }
    }
}
