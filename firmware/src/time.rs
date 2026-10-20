//! Wall-clock time: the PCF85063A RTC (when the board has one) at boot, and SNTP once a
//! day, which also corrects the RTC.

use core::net::{IpAddr, SocketAddr};

use clock::{DateTime, sntp};
use edge_nal::{UdpBind, UdpReceive, UdpSend};
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_hal_bus::i2c::RefCellDevice;
use epd_inkplate::rtc::Pcf85063;
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use log::{info, warn};
use static_cell::StaticCell;

use crate::net::SharedPrinter;

/// The RTC on the shared I2C bus.
pub type Rtc = Pcf85063<RefCellDevice<'static, I2c<'static, Blocking>>>;

// TODO: prefer the NTP servers from the DHCP lease (option 42). embassy-net 0.9 does not
// expose DHCP options and its DHCP client receives every reply on port 68.
const NTP_SERVER: &str = "pool.ntp.org";
const SYNC_EVERY: Duration = Duration::from_secs(24 * 3600);
const RETRY_AFTER: Duration = Duration::from_secs(600);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

fn uptime() -> u32 {
    Instant::now().as_secs() as u32
}

/// Reads the RTC at boot: `None` if the board has no RTC (no acknowledge, as on early
/// Inkplate 6 boards), otherwise the current Unix time if the clock is set.
pub fn read_rtc(rtc: &mut Rtc) -> Option<Option<u64>> {
    match rtc.read() {
        Ok(Some(t)) => {
            info!("RTC: {}", fmt(&t));
            Some(t.to_unix())
        }
        Ok(None) => {
            info!("RTC: present but not set (or lost power)");
            Some(None)
        }
        Err(_) => {
            info!("RTC: none on this board; time comes from NTP only");
            None
        }
    }
}

fn fmt(t: &DateTime) -> alloc::string::String {
    alloc::format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

/// Syncs the printer's clock from NTP after the network comes up and then daily, writing
/// the RTC each time.
#[embassy_executor::task]
pub async fn ntp_task(stack: Stack<'static>, printer: &'static SharedPrinter, mut rtc: Option<Rtc>) {
    static BUFFERS: StaticCell<UdpBuffers<1, 128, 128, 2>> = StaticCell::new();
    let udp = Udp::new(stack, BUFFERS.init(UdpBuffers::new()));
    loop {
        stack.wait_config_up().await;
        match sync(stack, &udp).await {
            Ok(unix) => {
                printer.lock(|p| p.borrow_mut().set_wall_clock(unix, uptime()));
                let now = DateTime::from_unix(unix);
                info!("NTP: {}", fmt(&now));
                if let Some(rtc) = &mut rtc {
                    match rtc.set(&now) {
                        Ok(()) => info!("RTC: corrected"),
                        Err(e) => warn!("RTC: write failed: {e:?}"),
                    }
                }
                Timer::after(SYNC_EVERY).await;
            }
            Err(e) => {
                warn!("NTP: {e}; retrying in {} min", RETRY_AFTER.as_secs() / 60);
                Timer::after(RETRY_AFTER).await;
            }
        }
    }
}

/// One SNTP exchange: the server's time in Unix seconds.
async fn sync(stack: Stack<'static>, udp: &Udp<'_>) -> Result<u64, &'static str> {
    let server = match stack.dns_query(NTP_SERVER, DnsQueryType::A).await {
        Ok(addrs) => *addrs.first().ok_or("no address for the NTP server")?,
        Err(_) => return Err("DNS lookup of the NTP server failed"),
    };
    let server = SocketAddr::new(IpAddr::from(server), sntp::PORT);
    let mut socket = udp.bind(SocketAddr::new(core::net::Ipv4Addr::UNSPECIFIED.into(), 0)).await.map_err(|_| "UDP bind failed")?;

    let rng = esp_hal::rng::Rng::new();
    let cookie = u64::from(rng.random()) << 32 | u64::from(rng.random());
    socket.send(server, &sntp::request(cookie)).await.map_err(|_| "send failed")?;

    let mut buf = [0u8; 128];
    let receive = async {
        loop {
            match socket.receive(&mut buf).await {
                Ok((len, from)) if from == server => match sntp::parse_reply(&buf[..len], cookie) {
                    Ok(unix) => return Ok(unix),
                    Err(sntp::Error::NotOurs) => continue,
                    Err(_) => return Err("server refused or unsynchronised"),
                },
                Ok(_) => continue,
                Err(_) => return Err("receive failed"),
            }
        }
    };
    with_timeout(REPLY_TIMEOUT, receive).await.map_err(|_| "no reply")?
}
