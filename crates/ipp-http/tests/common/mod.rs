//! Runs the real edge-http server with [`IppHandler`] on a localhost port, on std sockets.

use std::cell::RefCell;
use std::net::{SocketAddr, TcpListener};
use std::time::Instant;

use board::Inkplate6;
use edge_http::io::server::Server;
use edge_nal::TcpBind;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use ipp_http::{DiscardDocuments, IppHandler};
use ipp_printer::{Config, Printer};

pub fn config(port: u16) -> Config {
    Config {
        name: "Test Inkplate".into(),
        uuid: "urn:uuid:6c1f3f5e-1d9a-4c1e-9a52-000000000042".into(),
        uri: format!("ipp://localhost:{port}/ipp/print"),
        location: "Test kitchen".into(),
        more_info: format!("http://localhost:{port}/"),
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn uptime() -> u32 {
    START.get_or_init(Instant::now).elapsed().as_secs() as u32
}

/// Starts a server thread and returns its address once it accepts connections.
pub fn start_server() -> SocketAddr {
    let port = free_port();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    std::thread::spawn(move || {
        let printer = Box::leak(Box::new(Mutex::<NoopRawMutex, _>::new(RefCell::new(Printer::<Inkplate6>::new(config(port))))));
        let handler = IppHandler::new(printer, DiscardDocuments, uptime);
        async_io::block_on(async {
            let stack = edge_nal_std::Stack::new();
            let acceptor = stack.bind(addr).await.unwrap();
            let mut server = Box::new(Server::<2, 4096, 32>::new());
            server.run(Some(5_000), acceptor, handler).await.unwrap();
        });
    });
    for _ in 0..200 {
        if std::net::TcpStream::connect(addr).is_ok() {
            return addr;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("server did not start on {addr}");
}
