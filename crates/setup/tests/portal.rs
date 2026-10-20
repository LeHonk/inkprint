//! The portal over real localhost TCP, with a recording backend.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use edge_http::io::server::{Connection, Handler, Server};
use edge_nal::{TcpBind, TcpSplit};
use setup::Settings;
use setup::portal::{Restart, SetupBackend, SetupHandler, WithSetup};

#[derive(Clone, Default)]
struct Backend {
    saved: Arc<Mutex<Vec<Settings>>>,
}

impl SetupBackend for Backend {
    fn name(&self) -> String {
        "Inkplate 6 (a1b2)".into()
    }
    fn current(&self) -> Settings {
        Settings { ssid: "Home".into(), password: "secret123".into(), location: "Kitchen".into(), brightness: 0 }
    }
    fn networks(&self) -> Vec<String> {
        vec!["Home".into(), "Neighbour".into()]
    }
    /// Like the device: only a brightness change applies without a restart.
    fn save(&self, settings: &Settings) -> Result<Restart, &'static str> {
        self.saved.lock().unwrap().push(settings.clone());
        let current = self.current();
        let same = settings.ssid == current.ssid && settings.password == current.password && settings.location == current.location;
        Ok(if same { Restart::No } else { Restart::Yes })
    }
}

/// Stands in for the IPP handler behind the router.
struct Other;

impl Handler for Other {
    type Error<E: std::fmt::Debug> = edge_http::io::Error<E>;

    async fn handle<T, const N: usize>(&self, _: impl std::fmt::Display + Copy, c: &mut Connection<'_, T, N>) -> Result<(), Self::Error<T::Error>>
    where
        T: embedded_io_async::Read + embedded_io_async::Write + TcpSplit,
    {
        c.initiate_response(200, None, &[("Content-Length", "5")]).await?;
        embedded_io_async::Write::write_all(c, b"other").await
    }
}

fn serve<H: Handler + Send + 'static>(handler: H) -> SocketAddr {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    std::thread::spawn(move || {
        async_io::block_on(async {
            let acceptor = edge_nal_std::Stack::new().bind(addr).await.unwrap();
            let mut server = Box::new(Server::<2, 4096, 32>::new());
            server.run(Some(5_000), acceptor, handler).await.unwrap();
        })
    });
    for _ in 0..200 {
        if TcpStream::connect(addr).is_ok() {
            return addr;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("server did not start");
}

/// Sends one request and returns (status, body).
fn request(addr: SocketAddr, req: &str) -> (u16, String) {
    let stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut r = BufReader::new(stream);
    r.get_mut().write_all(req.as_bytes()).unwrap();
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut len = 0;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    (status, String::from_utf8(body).unwrap())
}

fn post(path: &str, form: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{form}",
        form.len()
    )
}

#[test]
fn captive_portal_answers_probes_and_saves() {
    let backend = Backend::default();
    let addr = serve(SetupHandler::captive(backend.clone()));

    // Android/Apple/Windows connectivity probes all get the form.
    for path in ["/generate_204", "/hotspot-detect.html", "/connecttest.txt", "/"] {
        let (status, body) = request(addr, &format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n"));
        assert_eq!(status, 200, "{path}");
        assert!(body.contains("Inkplate 6 (a1b2) setup") && body.contains("Neighbour"), "{path}");
    }

    let (status, body) = request(addr, &post("/setup", "ssid=Neighbour&ssid_other=&password=hunter2hunter2&location=Hall"));
    assert_eq!(status, 200);
    assert!(body.contains("joins <b>Neighbour</b>") && body.contains("<b>Hall</b>"), "{body}");
    let saved = backend.saved.lock().unwrap().clone();
    assert_eq!(saved, [Settings { ssid: "Neighbour".into(), password: "hunter2hunter2".into(), location: "Hall".into(), brightness: 0 }]);
}

#[test]
fn invalid_submission_shows_the_error() {
    let backend = Backend::default();
    let addr = serve(SetupHandler::captive(backend.clone()));
    let (status, body) = request(addr, &post("/setup", "ssid=Neighbour&password=short&location=Hall"));
    assert_eq!(status, 400);
    assert!(body.contains("class=\"error\"") && body.contains("8 to 63 characters"), "{body}");
    assert!(backend.saved.lock().unwrap().is_empty());
}

#[test]
fn lan_router_serves_setup_and_passes_the_rest_on() {
    let backend = Backend::default();
    let addr = serve(WithSetup { setup: SetupHandler::lan(backend.clone()), other: Other });

    let (status, body) = request(addr, "GET /setup HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(status, 200);
    assert!(body.contains("value=\"Kitchen\""));
    assert_eq!(request(addr, "GET / HTTP/1.1\r\nHost: x\r\n\r\n"), (200, "other".into()));
    assert_eq!(request(addr, "POST /ipp/print HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n").1, "other");

    // Location-only change keeps the saved network and password.
    let (status, _) = request(addr, &post("/setup", "ssid=Home&ssid_other=&password=&location=Pantry"));
    assert_eq!(status, 200);
    let saved = backend.saved.lock().unwrap().clone();
    assert_eq!(saved, [Settings { ssid: "Home".into(), password: "secret123".into(), location: "Pantry".into(), brightness: 0 }]);
}

#[test]
fn brightness_only_change_applies_without_restart() {
    let backend = Backend::default();
    let addr = serve(WithSetup { setup: SetupHandler::lan(backend.clone()), other: Other });
    let (status, body) = request(addr, &post("/setup", "ssid=Home&ssid_other=&password=&location=Kitchen&brightness=2"));
    assert_eq!(status, 200);
    assert!(body.contains("Brightness <b>Much lighter</b> is applied"), "{body}");
    assert!(!body.contains("restarts"), "{body}");
    assert_eq!(backend.saved.lock().unwrap()[0].brightness, 2);
}
