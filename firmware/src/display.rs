//! Display task: shows the last page of each print job, and a status screen on
//! Identify-Printer.

use alloc::vec::Vec;

use board::{Board, PixelKind, Selected};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use epd_inkplate::inkplate6::Inkplate6Panel;
use epd_inkplate::{EpdDriver, Frame};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::i2c::master::I2c;
use log::{info, warn};
use render::GrayImage;

use crate::net::{IDENTIFY, PAGES, status_lines};
use crate::panel_bus::I2sPanelBus;

pub type Panel = Inkplate6Panel<Selected, I2sPanelBus, I2c<'static, Blocking>, Delay>;

/// Crop whitespace margins so a page printed on large paper fills the panel.
const AUTO_CROP: bool = true;

/// A refresh is synchronous and stalls the network tasks for a few seconds, so give the
/// IPP response time to go out first.
const SETTLE: Duration = Duration::from_millis(500);

/// How long Identify-Printer shows the status screen before the page comes back.
const IDENTIFY_FOR: Duration = Duration::from_secs(15);

/// Panel data in the board's pixel format, ready for [`Frame`].
fn panel_data(page: &GrayImage, crop: bool) -> Vec<u8> {
    let image = render::prepare_for_panel::<Selected>(page, crop);
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

#[embassy_executor::task]
pub async fn display_task(mut panel: Panel) {
    // The last page shown, to restore after the status screen. Before the first job the
    // panel shows whatever it showed before boot, which cannot be restored.
    let mut current: Option<Vec<u8>> = None;
    // The status screen's saturated black text survives a normal refresh as a ghost, so
    // whatever replaces it gets a burn-in clean first.
    let mut status_shown = false;
    loop {
        match select(PAGES.wait(), IDENTIFY.wait()).await {
            Either::First(mut page) => {
                Timer::after(SETTLE).await;
                // Another job finished meanwhile: only its page matters.
                if let Some(newer) = PAGES.try_take() {
                    page = newer;
                }
                let data = panel_data(&page, AUTO_CROP);
                drop(page);
                if core::mem::take(&mut status_shown) {
                    burn_in_clean(&mut panel);
                }
                show(&mut panel, &data, "page");
                current = Some(data);
            }
            Either::Second(()) => {
                Timer::after(SETTLE).await;
                let lines = status_lines();
                let text: Vec<render::Line<'_>> =
                    lines.iter().map(|(text, scale)| render::Line { text, scale: *scale }).collect();
                let (w, h) = Selected::logical_size();
                let screen = render::text_screen(w as u32, h as u32, &text);
                show(&mut panel, &panel_data(&screen, false), "status screen");
                drop(screen);
                status_shown = true;

                match select(PAGES.wait(), Timer::after(IDENTIFY_FOR)).await {
                    // A new page replaces the status screen right away (next iteration).
                    Either::First(page) => PAGES.signal(page),
                    Either::Second(()) => {
                        if let Some(data) = &current {
                            burn_in_clean(&mut panel);
                            status_shown = false;
                            show(&mut panel, data, "page (restored)");
                        }
                    }
                }
            }
        }
    }
}
