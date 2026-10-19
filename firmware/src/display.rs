//! Display task: shows the last page of each print job.

use board::{Board, PixelKind, Selected};
use embassy_time::{Duration, Instant, Timer};
use epd_inkplate::inkplate6::Inkplate6Panel;
use epd_inkplate::{EpdDriver, Frame};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::i2c::master::I2c;
use log::{info, warn};

use crate::net::PAGES;
use crate::panel_bus::I2sPanelBus;

pub type Panel = Inkplate6Panel<Selected, I2sPanelBus, I2c<'static, Blocking>, Delay>;

/// Crop whitespace margins so a page printed on large paper fills the panel.
const AUTO_CROP: bool = true;

/// A refresh is synchronous and stalls the network tasks for a few seconds, so give the
/// IPP response time to go out first.
const SETTLE: Duration = Duration::from_millis(500);

#[embassy_executor::task]
pub async fn display_task(mut panel: Panel) {
    loop {
        let mut page = PAGES.wait().await;
        Timer::after(SETTLE).await;
        // Another job finished meanwhile: only its page matters.
        if let Some(newer) = PAGES.try_take() {
            page = newer;
        }

        let start = Instant::now();
        let image = render::prepare_for_panel::<Selected>(&page, AUTO_CROP);
        drop(page);
        let result = match Selected::PIXEL {
            PixelKind::Mono1 => panel.show(Frame::Mono(&render::dither_mono(&image))),
            PixelKind::Gray3 | PixelKind::Color7 => {
                let levels = render::quantize_gray3(&image);
                drop(image);
                panel.show(Frame::Gray3(&levels))
            }
        };
        match result {
            Ok(()) => info!("display: page shown in {} ms", start.elapsed().as_millis()),
            Err(e) => warn!("display: refresh failed: {e:?}"),
        }
    }
}
