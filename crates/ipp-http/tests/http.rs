//! End-to-end tests over real localhost TCP: a hand-written HTTP client talks to the
//! edge-http server running `IppHandler`.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use ipp_codec::{DelimiterTag, Group, Message, Operation, StatusCode, StrTag, Value};

fn request(op: Operation, id: u32, extra: &[(&str, Value)]) -> Vec<u8> {
    let mut ops = Group::new(DelimiterTag::Operation);
    ops.push("attributes-charset", [Value::str(StrTag::Charset, "utf-8")]);
    ops.push("attributes-natural-language", [Value::str(StrTag::NaturalLanguage, "en")]);
    ops.push("printer-uri", [Value::uri("ipp://localhost/ipp/print")]);
    for (name, v) in extra {
        ops.push(name, [v.clone()]);
    }
    let mut out = Vec::new();
    Message { version: (2, 0), code: op as u16, request_id: id, groups: vec![ops] }.encode(&mut out);
    out
}

struct Client {
    reader: BufReader<TcpStream>,
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Client {
    fn connect(addr: std::net::SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Self { reader: BufReader::new(stream) }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.reader.get_mut().write_all(bytes).unwrap();
    }

    fn status_line(&mut self) -> (u16, Vec<(String, String)>) {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let status = line.split(' ').nth(1).unwrap_or_else(|| panic!("bad status line {line:?}")).parse().unwrap();
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            self.reader.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').unwrap();
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
        (status, headers)
    }

    fn response(&mut self) -> Response {
        let (status, headers) = self.status_line();
        let len: usize = headers.iter().find(|(k, _)| k == "content-length").map(|(_, v)| v.parse().unwrap()).unwrap();
        let mut body = vec![0; len];
        self.reader.read_exact(&mut body).unwrap();
        Response { status, headers, body }
    }

    fn post_ipp(&mut self, body: &[u8]) -> Response {
        self.send(format!("POST /ipp/print HTTP/1.1\r\nHost: x\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
        self.send(body);
        self.response()
    }
}

fn ipp(resp: &Response) -> Message {
    assert_eq!(resp.status, 200);
    assert!(resp.headers.iter().any(|(k, v)| k == "content-type" && v == "application/ipp"));
    Message::parse(&resp.body).unwrap().unwrap().0
}

fn chunked(data: &[u8], chunk: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for c in data.chunks(chunk) {
        out.extend(format!("{:x}\r\n", c.len()).as_bytes());
        out.extend(c);
        out.extend(b"\r\n");
    }
    out.extend(b"0\r\n\r\n");
    out
}

#[test]
fn get_printer_attributes_with_expect_continue() {
    let addr = common::start_server();
    let mut c = Client::connect(addr);
    let body = request(Operation::GetPrinterAttributes, 7, &[("requested-attributes", Value::keyword("all"))]);
    c.send(
        format!("POST /ipp/print HTTP/1.1\r\nHost: x\r\nContent-Type: application/ipp\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n", body.len())
            .as_bytes(),
    );
    // The interim response must arrive before we send the body.
    let (status, _) = c.status_line();
    assert_eq!(status, 100);
    c.send(&body);

    let msg = ipp(&c.response());
    assert_eq!(msg.code, StatusCode::SuccessfulOk as u16);
    assert_eq!(msg.request_id, 7);
    let printer = msg.group(DelimiterTag::Printer).unwrap();
    assert_eq!(printer.get("printer-name").unwrap().values[0].as_str(), Some("Test Inkplate"));
    assert!(printer.get("media-col-default").is_some());
}

#[test]
fn chunked_request_body() {
    let addr = common::start_server();
    let mut c = Client::connect(addr);
    let body = request(Operation::GetPrinterAttributes, 8, &[("requested-attributes", Value::keyword("printer-state"))]);
    c.send(b"POST /ipp/print HTTP/1.1\r\nHost: x\r\nContent-Type: application/ipp\r\nTransfer-Encoding: chunked\r\n\r\n");
    c.send(&chunked(&body, 7));
    let msg = ipp(&c.response());
    let printer = msg.group(DelimiterTag::Printer).unwrap();
    assert_eq!(printer.attributes.len(), 1);
    assert_eq!(printer.attributes[0].name, "printer-state");
}

#[test]
fn discarded_print_job_keeps_connection_usable() {
    let addr = common::start_server();
    let mut c = Client::connect(addr);
    let mut body = request(Operation::PrintJob, 9, &[("document-format", Value::mime("image/pwg-raster"))]);
    body.extend(std::iter::repeat_n(0xAB, 20_000)); // document data
    c.send(b"POST /ipp/print HTTP/1.1\r\nHost: x\r\nContent-Type: application/ipp\r\nTransfer-Encoding: chunked\r\n\r\n");
    c.send(&chunked(&body, 1024));
    let msg = ipp(&c.response());
    assert_eq!(msg.code, StatusCode::ServerErrorInternalError as u16);
    let job = msg.group(DelimiterTag::Job).unwrap();
    assert_eq!(job.get("job-state").unwrap().values[0], Value::Enum(8)); // aborted

    // Same connection: the whole document was consumed, so the next request parses.
    let msg = ipp(&c.post_ipp(&request(Operation::GetJobs, 10, &[("which-jobs", Value::keyword("completed"))])));
    assert_eq!(msg.request_id, 10);
    assert_eq!(msg.groups.iter().filter(|g| g.tag == DelimiterTag::Job).count(), 1);
}

#[test]
fn malformed_ipp_is_bad_request() {
    let addr = common::start_server();
    let mut c = Client::connect(addr);
    c.send(b"POST /ipp/print HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\n\r\n\x02\x00\x00\x0b\x00\x00\x00\x01\x44\x00\x01a");
    assert_eq!(c.response().status, 400);
}

#[test]
fn status_page_and_not_found() {
    let addr = common::start_server();
    let mut c = Client::connect(addr);
    c.send(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    let resp = c.response();
    assert_eq!(resp.status, 200);
    let text = String::from_utf8(resp.body).unwrap();
    assert!(text.contains("Test Inkplate") && text.contains("Test kitchen"), "{text}");

    let mut c = Client::connect(addr);
    c.send(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(c.response().status, 404);
}

/// A sgray_8 PWG page `width`×`height` filled with `shade`, one literal run per line.
fn pwg_page(width: u32, height: u32, shade: u8) -> Vec<u8> {
    let mut h = vec![0u8; 1796];
    let mut put = |off: usize, v: u32| h[off..off + 4].copy_from_slice(&v.to_be_bytes());
    put(276, 150);
    put(280, 150);
    put(372, width);
    put(376, height);
    put(384, 8);
    put(388, 8);
    put(392, width);
    put(400, 18);
    let mut line = vec![0]; // line repeat
    let mut left = width;
    while left > 0 {
        let run = left.min(128);
        line.extend([(run - 1) as u8, shade]);
        left -= run;
    }
    for _ in 0..height {
        h.extend(&line);
    }
    h
}

type Shown = std::sync::Arc<std::sync::Mutex<Vec<render::GrayImage>>>;

fn showing_server() -> (std::net::SocketAddr, Shown) {
    let shown = Shown::default();
    let sink = ipp_http::ShowLastPage { max_width: 200, show: { let shown = shown.clone(); move |p| shown.lock().unwrap().push(p) } };
    (common::start_server_with(sink), shown)
}

#[test]
fn print_job_shows_the_last_page() {
    let (addr, shown) = showing_server();
    for format in ["image/pwg-raster", "application/octet-stream"] {
        let mut body = request(Operation::PrintJob, 11, &[("document-format", Value::mime(format))]);
        body.extend(b"RaS2");
        body.extend(pwg_page(400, 300, 0x20));
        body.extend(pwg_page(400, 300, 0xC0));
        let mut c = Client::connect(addr);
        c.send(b"POST /ipp/print HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n");
        c.send(&chunked(&body, 333));
        let msg = ipp(&c.response());
        assert_eq!(msg.code, StatusCode::SuccessfulOk as u16, "{format}");
        let job = msg.group(DelimiterTag::Job).unwrap();
        assert_eq!(job.get("job-state").unwrap().values[0], Value::Enum(9)); // completed
        assert_eq!(job.get("job-impressions-completed").unwrap().values[0], Value::Integer(2));
    }
    let shown = shown.lock().unwrap();
    assert_eq!(shown.len(), 2);
    // Downsampled 2× to fit max_width 200; second page's shade.
    assert_eq!((shown[0].width, shown[0].height), (200, 150));
    assert!(shown[0].pixels.iter().all(|&p| p == 0xC0));
}

#[test]
fn bad_documents_are_rejected() {
    let (addr, shown) = showing_server();
    let cases = [
        ("application/octet-stream", b"%PDF-1.7 not a raster".to_vec(), StatusCode::ClientErrorDocumentFormatNotSupported),
        ("image/pwg-raster", b"RaS2 truncated header".to_vec(), StatusCode::ClientErrorBadRequest),
    ];
    for (format, doc, status) in cases {
        let mut body = request(Operation::PrintJob, 12, &[("document-format", Value::mime(format))]);
        body.extend(doc);
        let mut c = Client::connect(addr);
        let msg = ipp(&c.post_ipp(&body));
        assert_eq!(msg.code, status as u16, "{format}");
    }
    assert!(shown.lock().unwrap().is_empty());
}
