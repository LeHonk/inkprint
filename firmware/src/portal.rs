//! Setup mode: an open access point with DHCP, catch-all DNS and the settings form as a
//! captive portal.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{Ipv4Addr, SocketAddr};

use edge_dhcp::server::{Server as DhcpServer, ServerOptions};
use edge_http::io::server::Server;
use edge_nal::{TcpBind, UdpBind};
use edge_nal_embassy::{Tcp, TcpBuffers, Udp, UdpBuffers};
use embassy_executor::Spawner;
use embassy_net::{Ipv4Cidr, Stack, StaticConfigV4};
use embassy_time::{Duration, Instant, Timer};
use esp_radio::wifi::ap::AccessPointConfig;
use esp_hal::time::Duration as HalDuration;
use esp_radio::wifi::scan::{ScanConfig, ScanTypeConfig};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config as WifiConfig, Interface, WifiController};
use log::{info, warn};
use setup::portal::SetupHandler;
use static_cell::StaticCell;

use crate::device::{self, DeviceSetup};
use crate::net::{self, Identity, TEXT};

/// The access point's own address; clients get 192.168.4.50 and up.
const AP_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const PORTAL_URL: &str = "http://192.168.4.1/setup";
/// With saved settings, go back to trying them after this long without a submission.
const SETUP_TIMEOUT: Duration = Duration::from_secs(600);
const HTTP_TASKS: usize = 2;

/// Runs setup mode until the settings are saved (the reboot task restarts) or, when
/// settings exist, until [`SETUP_TIMEOUT`].
pub async fn run(spawner: Spawner, mut controller: WifiController<'static>, identity: &'static Identity, seed: u64, show_screen: bool) -> ! {
    device::enter_setup_mode();
    let networks: &'static [String] = scan(&mut controller).await.leak();
    info!("setup: {} networks found", networks.len());

    let ap = AccessPointConfig::default().with_ssid(identity.ap_ssid.try_into().expect("AP name length"));
    if let Err(e) = controller.set_config(&WifiConfig::AccessPoint(ap)) {
        warn!("setup: access point failed: {e:?}");
    }
    let config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(AP_IP, 24),
        gateway: Some(AP_IP),
        dns_servers: Default::default(),
    });
    let (stack, runner) = net::stack(Interface::access_point(), config, seed);
    spawner.spawn(net::net_task(runner).expect("spawn net"));
    spawner.spawn(dhcp_task(stack).expect("spawn dhcp"));
    spawner.spawn(dns_task(stack).expect("spawn dns"));
    spawner.spawn(portal_task(stack, DeviceSetup { name: identity.instance, networks }).expect("spawn portal"));
    info!("setup: join Wi-Fi {:?} and open {PORTAL_URL}", identity.ap_ssid);

    if show_screen {
        TEXT.signal(setup_screen(identity));
    }

    if device::current().has_wifi() {
        Timer::after(SETUP_TIMEOUT).await;
        info!("setup: no changes, trying the saved network again");
        esp_hal::system::software_reset();
    }
    // The controller must stay alive while the access point runs.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
        let _ = &controller;
    }
}

/// Scans this often; one pass can miss an access point that answers slowly.
const SCAN_PASSES: usize = 2;

/// Visible networks, strongest first, without duplicates or hidden ones.
async fn scan(controller: &mut WifiController<'static>) -> Vec<String> {
    if let Err(e) = controller.set_config(&WifiConfig::Station(StationConfig::default())) {
        warn!("setup: scan config failed: {e:?}");
        return Vec::new();
    }
    // esp-radio's default listens only 10-20 ms per channel and misses most routers
    // (ESP-IDF waits up to 120 ms). 100-300 ms over 13 channels takes about 4 s.
    let config = ScanConfig::default()
        .with_scan_type(ScanTypeConfig::Active { min: HalDuration::from_millis(100), max: HalDuration::from_millis(300) })
        .with_max(30);
    let mut found: Vec<(String, i8)> = Vec::new();
    for pass in 1..=SCAN_PASSES {
        match controller.scan_async(&config).await {
            Ok(aps) => {
                info!("setup: scan {pass}: {} access points", aps.len());
                for ap in aps {
                    let name = String::from(ap.ssid.as_str());
                    match found.iter_mut().find(|(n, _)| *n == name) {
                        Some((_, rssi)) => *rssi = (*rssi).max(ap.signal_strength),
                        None if !name.is_empty() => found.push((name, ap.signal_strength)),
                        None => {}
                    }
                }
            }
            Err(e) => warn!("setup: scan {pass} failed: {e:?}"),
        }
    }
    found.sort_by_key(|(_, rssi)| core::cmp::Reverse(*rssi));
    found.into_iter().map(|(name, _)| name).collect()
}

fn setup_screen(identity: &Identity) -> Vec<(String, u32)> {
    alloc::vec![
        ("Inkprint".into(), 3),
        ("Wi-Fi setup".into(), 2),
        (String::new(), 1),
        ("1. On your phone, join".into(), 2),
        (format!("   {}", identity.ap_ssid), 2),
        ("2. The setup page opens.".into(), 2),
        ("   If not, browse to".into(), 2),
        ("   192.168.4.1/setup".into(), 2),
        ("3. Pick your network,".into(), 2),
        ("   enter its password".into(), 2),
        ("   and the location.".into(), 2),
    ]
}

/// Hands out addresses on the access point, pointing DNS and the captive-portal URL
/// (DHCP option 114) at us.
#[embassy_executor::task]
async fn dhcp_task(stack: Stack<'static>) {
    static BUFFERS: StaticCell<UdpBuffers<1, 1024, 1024, 2>> = StaticCell::new();
    let udp = Udp::new(stack, BUFFERS.init(UdpBuffers::new()));
    let mut gateway = [AP_IP];
    let dns = [AP_IP];
    let mut options = ServerOptions::new(AP_IP, Some(&mut gateway));
    options.dns = &dns;
    options.captive_url = Some(PORTAL_URL);
    let mut server = DhcpServer::<_, 8>::new(|| Instant::now().as_secs(), AP_IP);
    let mut buf = [0u8; 1024];
    loop {
        match udp.bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 67)).await {
            Ok(mut socket) => {
                if let Err(e) = edge_dhcp::io::server::run(&mut server, &options, &mut socket, &mut buf).await {
                    warn!("setup: DHCP server stopped: {e:?}");
                }
            }
            Err(e) => warn!("setup: DHCP bind failed: {e:?}"),
        }
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// Answers every DNS query with our address, so any page a phone tries leads here.
#[embassy_executor::task]
async fn dns_task(stack: Stack<'static>) {
    static BUFFERS: StaticCell<UdpBuffers<1, 512, 512, 2>> = StaticCell::new();
    let udp = Udp::new(stack, BUFFERS.init(UdpBuffers::new()));
    let mut tx = [0u8; 512];
    let mut rx = [0u8; 512];
    loop {
        let local = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 53);
        if let Err(e) = edge_captive::io::run(&udp, local, &mut tx, &mut rx, AP_IP, core::time::Duration::from_secs(60)).await {
            warn!("setup: DNS server stopped: {e:?}");
        }
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// The setup form on port 80, answering every path (captive portal).
#[embassy_executor::task]
async fn portal_task(stack: Stack<'static>, backend: DeviceSetup) {
    static TCP_BUFFERS: StaticCell<TcpBuffers<HTTP_TASKS, 1536, 2048>> = StaticCell::new();
    static SERVER: StaticCell<Server<HTTP_TASKS, 2048, 32>> = StaticCell::new();
    let tcp = Tcp::new(stack, TCP_BUFFERS.init(TcpBuffers::new()));
    let server = SERVER.init(Server::new());
    let handler = SetupHandler::captive(backend);
    loop {
        match tcp.bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 80)).await {
            Ok(acceptor) => {
                if let Err(e) = server.run(Some(30_000), acceptor, &handler).await {
                    warn!("setup: portal stopped: {e:?}");
                }
            }
            Err(e) => warn!("setup: portal bind failed: {e:?}"),
        }
        Timer::after(Duration::from_secs(1)).await;
    }
}
