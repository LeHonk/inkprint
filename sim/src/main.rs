//! Host simulator: serves the same IPP printer logic as the firmware on
//! `ipp://<host>:6310/ipp/print` and writes the last printed page to `last-page.png`
//! (as the panel would show it, in native orientation and gray levels).
//!
//! Usage: `cargo run -p sim [-- <port>]`, then advertise it with the printed
//! `avahi-publish-service` command so phones and CUPS can find it.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use board::{Board, PixelKind, Selected};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ipp_codec::Message;
use ipp_http::{DocumentSink, ShowLastPage};
use ipp_printer::{Config, Outcome, Printer};
use tokio::net::TcpListener;

/// Largest IPP attribute section we buffer before the document data.
const MAX_IPP_HEADER: usize = 64 * 1024;
/// Captured page width bound; ~2× the panel keeps cropping precise.
const CAPTURE_MAX_WIDTH: u32 = 1200;
const OUTPUT: &str = "last-page.png";

struct State {
    printer: Printer<Selected>,
    started: Instant,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(6310);
    let host = std::fs::read_to_string("/etc/hostname")
        .map(|h| h.trim().to_owned())
        .unwrap_or_else(|_| "localhost".into());
    let config = Config {
        name: format!("Inkprint sim ({})", Selected::MODEL),
        uuid: "urn:uuid:6c1f3f5e-1d9a-4c1e-9a52-1b2d3c4e5f60".into(),
        uri: format!("ipp://{host}.local:{port}/ipp/print"),
        location: "Simulator".into(),
        more_info: format!("http://{host}.local:{port}/"),
        icon: None,
    };
    print_avahi_command::<Selected>(&config, port);

    let state = Arc::new(Mutex::new(State { printer: Printer::new(config), started: Instant::now() }));
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    println!("listening on :{port}, writing pages to {OUTPUT}");

    loop {
        let (stream, peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req| handle(req, state.clone()));
            if let Err(e) = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await {
                eprintln!("{peer}: {e}");
            }
        });
    }
}

fn print_avahi_command<B: Board>(config: &Config, port: u16) {
    let txt: Vec<String> = ipp_printer::mdns_txt_records::<B>(config)
        .into_iter()
        .map(|(k, v)| format!("'{k}={v}'"))
        .collect();
    println!("advertise with:");
    println!(
        "  avahi-publish-service --subtype _print._sub._ipp._tcp --subtype _universal._sub._ipp._tcp \\\n    '{}' _ipp._tcp {port} {}",
        config.name,
        txt.join(" ")
    );
}

fn reply(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn ipp_reply(msg: &Message) -> Response<Full<Bytes>> {
    let mut out = Vec::new();
    msg.encode(&mut out);
    reply(StatusCode::OK, "application/ipp", out)
}

async fn handle(req: Request<Incoming>, state: Arc<Mutex<State>>) -> Result<Response<Full<Bytes>>, Infallible> {
    if req.method() == Method::GET && req.uri().path() == "/" {
        return Ok(reply(StatusCode::OK, "text/plain", b"inkprint simulator\n".to_vec()));
    }
    if req.method() != Method::POST || req.uri().path() != "/ipp/print" {
        return Ok(reply(StatusCode::NOT_FOUND, "text/plain", Vec::new()));
    }
    let bad = |msg: &str| Ok(reply(StatusCode::BAD_REQUEST, "text/plain", msg.as_bytes().to_vec()));

    // Buffer only the IPP attribute section; document data is streamed afterwards.
    let mut body = req.into_body();
    let mut buf = Vec::new();
    let (request, rest) = loop {
        match Message::parse(&buf) {
            Ok(Some((msg, used))) => break (msg, buf.split_off(used)),
            Ok(None) if buf.len() > MAX_IPP_HEADER => return bad("IPP header too large"),
            Ok(None) => {}
            Err(_) => return bad("malformed IPP request"),
        }
        match next_data(&mut body).await {
            Some(chunk) => buf.extend_from_slice(&chunk),
            None => return bad("truncated IPP request"),
        }
    };

    let outcome = {
        let mut s = state.lock().unwrap();
        let now = s.started.elapsed().as_secs() as u32;
        s.printer.handle(&request, now)
    };
    let pending = match outcome {
        Outcome::Respond(resp) => return Ok(ipp_reply(&resp)),
        Outcome::Identify(resp) => {
            println!("identify requested");
            return Ok(ipp_reply(&resp));
        }
        Outcome::ReceiveDocument(p) => p,
    };

    // The device's decoding path (ipp-http's ShowLastPage), on the whole body: buffering
    // is fine on a PC.
    let mut document = rest;
    while let Some(chunk) = next_data(&mut body).await {
        document.extend_from_slice(&chunk);
    }
    let last_page = std::sync::Mutex::new(None);
    let sink = ShowLastPage { max_width: CAPTURE_MAX_WIDTH, show: |page| *last_page.lock().unwrap() = Some(page) };
    let result = sink.print(&pending, &document, &mut &[][..]).await;
    if let Some(page) = last_page.into_inner().unwrap()
        && let Err(e) = save_panel_preview::<Selected>(&page)
    {
        eprintln!("writing {OUTPUT}: {e}");
    }
    println!("job {}: {result:?}", pending.job_id);
    let resp = state.lock().unwrap().printer.finish_job(pending, result);
    Ok(ipp_reply(&resp))
}

async fn next_data(body: &mut Incoming) -> Option<Bytes> {
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.ok()?.into_data() {
            return Some(data);
        }
    }
    None
}

/// Writes what the panel would show: cropped, fitted, rotated and quantized.
fn save_panel_preview<B: Board>(page: &render::GrayImage) -> image::ImageResult<()> {
    let panel = render::prepare_for_panel::<B>(page, true);
    let pixels: Vec<u8> = match B::PIXEL {
        PixelKind::Mono1 => render::dither_mono(&panel).iter().map(|&b| b * 255).collect(),
        // TODO: 7-color palette preview for Color7.
        PixelKind::Gray3 | PixelKind::Color7 => {
            render::quantize_gray3(&panel).iter().map(|&l| (l as u16 * 255 / 7) as u8).collect()
        }
    };
    image::GrayImage::from_raw(panel.width, panel.height, pixels)
        .expect("buffer matches dimensions")
        .save(OUTPUT)
}
