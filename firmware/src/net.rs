//! Wi-Fi station, embassy-net with DHCP, mDNS/DNS-SD advertisement and the IPP server.
//!
//! Wi-Fi credentials are compiled in from `WIFI_SSID` / `WIFI_PASSWORD` (put them in the
//! git-ignored `firmware/.env`, see README).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::net::{Ipv4Addr, SocketAddr};
use core::sync::atomic::{AtomicU32, Ordering};

use board::{Board, Selected};
use dnssd::{HostAnswersMdnsHandler, PRINTER_SUBTYPES, PrinterService};
use edge_http::io::server::Server;
use edge_mdns::buf::VecBufAccess;
use edge_mdns::io::{IPV4_DEFAULT_SOCKET, Mdns};
use edge_nal::{TcpBind, UdpSplit};
use edge_nal_embassy::{Tcp, TcpBuffers, Udp, UdpBuffers};
use embassy_futures::select::select;
use embassy_net::{DhcpConfig, Runner, Stack, StackResources};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::once_lock::OnceLock;
use embassy_sync::signal::Signal;
use embedded_io_async::Read;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::rng::Rng;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Config as WifiConfig, Interface, WifiController};
use ipp_http::{DiscardDocuments, DocumentSink, IppHandler, ShowLastPage};
use render::GrayImage;
use ipp_printer::{Config, JobError, JobState, PendingJob, Printer};
use log::{info, warn};
use static_cell::StaticCell;

/// IPP's registered port.
pub const IPP_PORT: u16 = 631;
/// Concurrent HTTP connections (each holds a TCP socket while waiting in accept).
const HTTP_TASKS: usize = 2;
/// DHCP + mDNS + HTTP handlers, plus one spare.
const SOCKETS: usize = HTTP_TASKS + 3;
const MDNS_BUF: usize = 1500;

pub type SharedPrinter = Mutex<CriticalSectionRawMutex, RefCell<Printer<Selected>>>;

/// Last page of the most recent job, for the display task. A newer page replaces one not
/// yet shown.
pub static PAGES: Signal<CriticalSectionRawMutex, GrayImage> = Signal::new();

/// Identify-Printer requests, for the display task.
pub static IDENTIFY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// What the status screen shows; set once the printer exists.
struct NetInfo {
    identity: &'static Identity,
    printer: &'static SharedPrinter,
}

static NET_INFO: OnceLock<NetInfo> = OnceLock::new();
/// Current IPv4 address (0 until DHCP completes).
static IPV4: AtomicU32 = AtomicU32::new(0);

/// Records the printer for [`status_lines`].
pub fn publish(identity: &'static Identity, printer: &'static SharedPrinter) {
    let _ = NET_INFO.init(NetInfo { identity, printer });
}

/// Text for the Identify-Printer status screen: (line, font scale).
pub fn status_lines() -> Vec<(String, u32)> {
    let mut lines = alloc::vec![("Inkprint".into(), 3)];
    let Some(info) = NET_INFO.try_get() else {
        lines.push(("No Wi-Fi configured".into(), 2));
        return lines;
    };
    let ip = Ipv4Addr::from_bits(IPV4.load(Ordering::Relaxed));
    lines.push((info.identity.instance.into(), 2));
    lines.push((String::new(), 1));
    if ip.is_unspecified() {
        lines.push(("Connecting...".into(), 2));
    } else {
        lines.push(("Ready to print".into(), 2));
    }
    lines.push((format!("{}.local", info.identity.hostname), 2));
    if !ip.is_unspecified() {
        lines.push((format!("IP {ip}"), 2));
    }
    lines.push((String::new(), 1));
    let now = uptime();
    let last = info.printer.lock(|p| {
        let p = p.borrow();
        p.jobs()
            .iter()
            .filter(|j| j.state == JobState::Completed)
            .max_by_key(|j| j.id)
            .map(|j| (j.name.clone(), j.pages, j.completed.unwrap_or(now)))
    });
    match last {
        Some((name, pages, at)) => {
            let ago = match now.saturating_sub(at) / 60 {
                0 => "just now".into(),
                m if m < 120 => format!("{m} min ago"),
                m => format!("{} h ago", m / 60),
            };
            lines.push((format!("Last job: {name}"), 2));
            lines.push((format!("{pages} page{}, {ago}", if pages == 1 { "" } else { "s" }), 2));
        }
        None => lines.push(("No jobs yet".into(), 2)),
    }
    lines
}

/// Sends pages to the display task and Identify-Printer to [`IDENTIFY`].
struct DisplaySink(ShowLastPage<fn(GrayImage)>);

impl DocumentSink for DisplaySink {
    async fn print<R: Read>(&self, job: &PendingJob, first: &[u8], rest: &mut R) -> Result<u32, JobError> {
        self.0.print(job, first, rest).await
    }

    fn identify(&self) {
        IDENTIFY.signal(());
    }
}

/// Width bound for captured pages: A4 at 150 dpi (1240 px) is halved to 620, enough to
/// crop margins and still fill the 600 px wide portrait canvas. Two pages of at most
/// 900 × 1270 px (about 1.1 MB each) live in PSRAM at a time.
const CAPTURE_MAX_WIDTH: u32 = 900;

/// Compile-time Wi-Fi credentials, if set.
pub fn credentials() -> Option<(&'static str, &'static str)> {
    Some((option_env!("WIFI_SSID")?, option_env!("WIFI_PASSWORD").unwrap_or("")))
}

/// Per-device names, derived from the station MAC address so several Inkplates can share
/// a network.
pub struct Identity {
    /// mDNS host label, e.g. "inkplate-a1b2".
    pub hostname: &'static str,
    /// Printer and DNS-SD instance name, e.g. "Inkplate 6 (a1b2)".
    pub instance: &'static str,
    pub uuid: &'static str,
}

impl Identity {
    pub fn from_mac(mac: [u8; 6]) -> Self {
        let suffix = format!("{:02x}{:02x}", mac[4], mac[5]);
        let mac_hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        Self {
            hostname: format!("inkplate-{suffix}").leak(),
            instance: format!("{} ({suffix})", Selected::MODEL).leak(),
            // Fixed namespace + MAC: stable across reboots and firmware updates.
            uuid: format!("urn:uuid:6c1f3f5e-1d9a-4c1e-9a52-{mac_hex}").leak(),
        }
    }

    pub fn printer_config(&self) -> Config {
        Config {
            name: self.instance.into(),
            uuid: self.uuid.into(),
            uri: format!("ipp://{}.local:{IPP_PORT}/ipp/print", self.hostname),
            location: "Kitchen".into(),
            more_info: format!("http://{}.local:{IPP_PORT}/", self.hostname),
            icon: Some(format!("http://{}.local:{IPP_PORT}{}", self.hostname, ipp_http::ICON_PATH)),
        }
    }
}

/// Creates the network stack on the Wi-Fi station interface, with DHCP sending our hostname.
pub fn stack(interface: Interface, identity: &Identity, seed: u64) -> (Stack<'static>, Runner<'static, Interface>) {
    static RESOURCES: StaticCell<StackResources<SOCKETS>> = StaticCell::new();
    let mut dhcp = DhcpConfig::default();
    dhcp.hostname = heapless::String::try_from(identity.hostname).ok();
    embassy_net::new(interface, embassy_net::Config::dhcpv4(dhcp), RESOURCES.init(StackResources::new()), seed)
}

/// Keeps the station connected, retrying every 5 s.
#[embassy_executor::task]
pub async fn wifi_task(mut controller: WifiController<'static>, ssid: &'static str, password: &'static str) {
    let auth = if password.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(password.try_into().expect("Wi-Fi password too long"))
    };
    let config = StationConfig::default().with_ssid(ssid.try_into().expect("Wi-Fi SSID too long")).with_authentication(auth);
    if let Err(e) = controller.set_config(&WifiConfig::Station(config)) {
        warn!("Wi-Fi config rejected: {e:?}");
        return;
    }
    loop {
        info!("Wi-Fi: connecting to {ssid:?}");
        match controller.connect_async().await {
            Ok(_) => {
                info!("Wi-Fi: connected");
                let reason = controller.wait_for_disconnect_async().await;
                warn!("Wi-Fi: disconnected: {reason:?}");
            }
            Err(e) => warn!("Wi-Fi: connect failed: {e:?}"),
        }
        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

/// Answers mDNS queries for the printer. Restarts with the new address whenever DHCP
/// hands out a different one.
#[embassy_executor::task]
pub async fn mdns_task(stack: Stack<'static>, identity: &'static Identity, txt: &'static [(&'static str, &'static str)]) {
    static UDP_BUFFERS: StaticCell<UdpBuffers<1, MDNS_BUF, MDNS_BUF, 2>> = StaticCell::new();
    let udp = Udp::new(stack, UDP_BUFFERS.init(UdpBuffers::new()));
    let recv_buf = VecBufAccess::<NoopRawMutex, MDNS_BUF>::new();
    let send_buf = VecBufAccess::<NoopRawMutex, MDNS_BUF>::new();
    let signal = Signal::<NoopRawMutex, ()>::new();

    loop {
        stack.wait_config_up().await;
        let Some(ipv4) = stack.config_v4().map(|c| c.address.address()) else { continue };
        IPV4.store(ipv4.to_bits(), Ordering::Relaxed);
        let service = PrinterService {
            instance: identity.instance,
            hostname: identity.hostname,
            ipv4: ipv4.octets(),
            port: IPP_PORT,
            subtypes: PRINTER_SUBTYPES,
            txt,
        };
        info!("mDNS: advertising {:?} as {}.local ({ipv4})", identity.instance, identity.hostname);

        let respond = async {
            let mut socket = match edge_mdns::io::bind(&udp, IPV4_DEFAULT_SOCKET, Some(ipv4), None).await {
                Ok(s) => s,
                Err(e) => {
                    warn!("mDNS: bind failed: {e:?}");
                    return;
                }
            };
            let (recv, send) = socket.split();
            let mdns = Mdns::new(Some(ipv4), None, recv, send, &recv_buf, &send_buf, Rng::new(), &signal);
            if let Err(e) = mdns.run(HostAnswersMdnsHandler::new(&service)).await {
                warn!("mDNS: stopped: {e:?}");
            }
        };
        select(respond, stack.wait_config_down()).await;
        IPV4.store(0, Ordering::Relaxed);
        Timer::after(Duration::from_secs(1)).await;
    }
}

fn uptime() -> u32 {
    Instant::now().as_secs() as u32
}

/// Serves IPP on port 631. With `display`, job pages go to [`PAGES`]; without (no panel
/// driver for this board, or panel init failed) documents are discarded and jobs abort.
#[embassy_executor::task]
pub async fn http_task(stack: Stack<'static>, printer: &'static SharedPrinter, display: bool) {
    static TCP_BUFFERS: StaticCell<TcpBuffers<HTTP_TASKS, 1536, 2048>> = StaticCell::new();
    static SERVER: StaticCell<Server<HTTP_TASKS, 2048, 32>> = StaticCell::new();
    let tcp = Tcp::new(stack, TCP_BUFFERS.init(TcpBuffers::new()));
    let server = SERVER.init(Server::new());
    let to_display: fn(GrayImage) = |page| PAGES.signal(page);
    let show = IppHandler::new(printer, DisplaySink(ShowLastPage { max_width: CAPTURE_MAX_WIDTH, show: to_display }), uptime);
    let discard = IppHandler::new(printer, DiscardDocuments, uptime);
    loop {
        match tcp.bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), IPP_PORT)).await {
            Ok(acceptor) => {
                info!("IPP: listening on port {IPP_PORT}");
                let result = if display {
                    server.run(Some(30_000), acceptor, &show).await
                } else {
                    server.run(Some(30_000), acceptor, &discard).await
                };
                if let Err(e) = result {
                    warn!("IPP: server stopped: {e:?}");
                }
            }
            Err(e) => warn!("IPP: bind failed: {e:?}"),
        }
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// TXT records for the printer, leaked to `'static` (built once at boot).
pub fn txt_records(config: &Config) -> &'static [(&'static str, &'static str)] {
    let records: Vec<(&'static str, &'static str)> = ipp_printer::mdns_txt_records::<Selected>(config)
        .into_iter()
        .map(|(k, v)| (k, &*v.leak()))
        .collect();
    records.leak()
}
