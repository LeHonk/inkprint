//! Serves the IPP handler on the host, for poking at it with CUPS tools:
//!
//! ```sh
//! cargo run -p ipp-http --example serve -- 6311
//! ipptool -tv ipp://localhost:6311/ipp/print get-printer-attributes.test
//! ```

use std::cell::RefCell;
use std::net::SocketAddr;
use std::time::Instant;

use board::Inkplate6;
use edge_http::io::server::Server;
use edge_nal::TcpBind;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use ipp_http::{IppHandler, ShowLastPage};
use ipp_printer::{Config, Printer};

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(6311);
    START.get_or_init(Instant::now);
    let config = Config {
        name: "Inkprint example".into(),
        uuid: "urn:uuid:6c1f3f5e-1d9a-4c1e-9a52-000000000043".into(),
        uri: format!("ipp://localhost:{port}/ipp/print"),
        location: "Host".into(),
        more_info: format!("http://localhost:{port}/"),
        icon: Some(format!("http://localhost:{port}{}", ipp_http::ICON_PATH)),
        firmware_version: env!("CARGO_PKG_VERSION").into(),
    };
    let printer = Mutex::<NoopRawMutex, _>::new(RefCell::new(Printer::<Inkplate6>::new(config)));
    let show = |page: render::GrayImage| println!("last page: {}x{}", page.width, page.height);
    let sink = ShowLastPage { max_width: 900, show };
    let handler = IppHandler::new(&printer, sink, || START.get().unwrap().elapsed().as_secs() as u32);
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    println!("serving {} on {addr}", ipp_http::PRINTER_PATH);
    async_io::block_on(async {
        let acceptor = edge_nal_std::Stack::new().bind(addr).await.unwrap();
        let mut server = Box::new(Server::<2, 4096, 32>::new());
        server.run(Some(5_000), acceptor, handler).await.unwrap();
    });
}
