//! IPP over HTTP (RFC 8010 §4): an [`edge_http`] request handler in front of
//! [`ipp_printer::Printer`].
//!
//! Runs unchanged on the device (embassy-net sockets) and on the host (std sockets), so the
//! same code is exercised by `ipptool` in tests.
//!
//! - `POST /ipp/print`: buffers only the IPP attribute section, answers it, and hands any
//!   document data to a [`DocumentSink`] as a stream.
//! - `GET /`: a plain-text status page (`printer-more-info`).
//! - `Expect: 100-continue` is answered before the body is read; edge-http does not do it,
//!   and CUPS otherwise waits a second before sending.

#![no_std]
// The futures run on single-threaded executors (embassy, async-io); `Send` is not needed.
#![allow(async_fn_in_trait)]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Display;

use board::Board;
use edge_http::Method;
use edge_http::io::Error;
use edge_http::io::server::{Connection, Handler};
use edge_nal::TcpSplit;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_io_async::{Read, Write};
use ipp_codec::Message;
use ipp_printer::{JobError, Outcome, PendingJob, Printer};
use raster::{Decoder, Format};
use render::{GrayImage, PageCapture};

/// Resource path of the printer, as in `printer-uri` and the mDNS `rp` key.
pub const PRINTER_PATH: &str = "/ipp/print";

/// Largest IPP attribute section accepted before the document data.
pub const MAX_IPP_HEADER: usize = 16 * 1024;

/// Receives the document data of an accepted job.
pub trait DocumentSink {
    /// Consumes the document: `first` holds bytes already read past the IPP header, `rest`
    /// streams the remainder of the body. Returns the number of pages printed.
    async fn print<R: Read>(&self, job: &PendingJob, first: &[u8], rest: &mut R) -> Result<u32, JobError>;
}

/// A sink for builds without a display pipeline: reads and discards the document, then
/// reports the job as aborted.
pub struct DiscardDocuments;

impl DocumentSink for DiscardDocuments {
    async fn print<R: Read>(&self, job: &PendingJob, _first: &[u8], rest: &mut R) -> Result<u32, JobError> {
        log::warn!("job {}: no display pipeline, discarding document", job.job_id);
        drain(rest).await.map_err(|_| JobError::Aborted)?;
        Err(JobError::Aborted)
    }
}

/// Decodes PWG Raster / URF documents and passes the last page to `show`.
///
/// Pages are captured box-downsampled to at most `max_width` pixels wide; only the page
/// being decoded and the last completed one are in memory.
pub struct ShowLastPage<F> {
    pub max_width: u32,
    pub show: F,
}

impl<F: Fn(GrayImage)> DocumentSink for ShowLastPage<F> {
    async fn print<R: Read>(&self, job: &PendingJob, first: &[u8], rest: &mut R) -> Result<u32, JobError> {
        let mut capture = PageCapture::new(self.max_width);
        let mut buf = [0u8; 1024];
        let read = async |rest: &mut R, buf: &mut [u8]| rest.read(buf).await.map_err(|_| JobError::Aborted);

        // application/octet-stream: sniff the format from the first bytes.
        let mut head = Vec::from(first);
        let format = match job.format {
            Some(f) => f,
            None => loop {
                if let Some(f) = Format::sniff(&head) {
                    break f;
                }
                if head.len() >= 8 {
                    return Err(JobError::UnsupportedFormat);
                }
                let n = read(rest, &mut buf).await?;
                if n == 0 {
                    return Err(JobError::UnsupportedFormat);
                }
                head.extend_from_slice(&buf[..n]);
            },
        };

        let mut decoder = Decoder::new(format);
        let bad = |e| {
            log::warn!("job {}: raster error {e:?}", job.job_id);
            JobError::Malformed
        };
        decoder.push(&head, &mut capture).map_err(bad)?;
        drop(head);
        loop {
            let n = read(rest, &mut buf).await?;
            if n == 0 {
                break;
            }
            decoder.push(&buf[..n], &mut capture).map_err(bad)?;
        }
        decoder.finish().map_err(bad)?;

        let pages = capture.pages_seen;
        let page = capture.take_last_page().ok_or(JobError::Malformed)?;
        log::info!("job {}: {pages} page(s), last {}x{}", job.job_id, page.width, page.height);
        (self.show)(page);
        Ok(pages)
    }
}

/// The edge-http handler. `printer` is shared between the server's handler tasks.
pub struct IppHandler<'a, B: Board, M: RawMutex, S> {
    printer: &'a Mutex<M, RefCell<Printer<B>>>,
    sink: S,
    /// Seconds since boot, for `printer-up-time` and job timestamps.
    uptime: fn() -> u32,
}

impl<'a, B: Board, M: RawMutex, S: DocumentSink> IppHandler<'a, B, M, S> {
    pub fn new(printer: &'a Mutex<M, RefCell<Printer<B>>>, sink: S, uptime: fn() -> u32) -> Self {
        Self { printer, sink, uptime }
    }

    async fn ipp<T, const N: usize>(&self, connection: &mut Connection<'_, T, N>) -> Result<(), Error<T::Error>>
    where
        T: Read + Write,
    {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 512];
        let (request, used) = loop {
            match Message::parse(&buf) {
                Ok(Some(parsed)) => break parsed,
                Ok(None) if buf.len() > MAX_IPP_HEADER => {
                    return plain(connection, 413, "IPP request too large").await;
                }
                Ok(None) => {}
                Err(_) => return plain(connection, 400, "malformed IPP request").await,
            }
            let n = connection.read(&mut chunk).await?;
            if n == 0 {
                return plain(connection, 400, "truncated IPP request").await;
            }
            buf.extend_from_slice(&chunk[..n]);
        };

        let outcome = self.printer.lock(|p| p.borrow_mut().handle(&request, (self.uptime)()));
        let response = match outcome {
            Outcome::Respond(response) => response,
            Outcome::ReceiveDocument(job) => {
                let result = self.sink.print(&job, &buf[used..], connection).await;
                log::info!("job {}: {:?}", job.job_id, result);
                self.printer.lock(|p| p.borrow_mut().finish_job(job, result))
            }
        };
        // Whatever the sink or a rejected request left unread must go before the response.
        drain(connection).await?;

        let mut body = Vec::new();
        response.encode(&mut body);
        let len = format!("{}", body.len());
        connection
            .initiate_response(200, Some("OK"), &[("Content-Type", "application/ipp"), ("Content-Length", &len)])
            .await?;
        connection.write_all(&body).await
    }

    fn status_page(&self) -> alloc::string::String {
        self.printer.lock(|p| {
            let p = p.borrow();
            let jobs = p.jobs().iter().count();
            format!(
                "{}\n{} on {}\nIPP: {}\nRecent jobs: {}\n",
                p.config.name,
                B::MODEL,
                p.config.location,
                p.config.uri,
                jobs
            )
        })
    }
}

impl<B: Board, M: RawMutex, S: DocumentSink> Handler for IppHandler<'_, B, M, S> {
    type Error<E: core::fmt::Debug> = Error<E>;

    async fn handle<T, const N: usize>(
        &self,
        _task_id: impl Display + Copy,
        connection: &mut Connection<'_, T, N>,
    ) -> Result<(), Self::Error<T::Error>>
    where
        T: Read + Write + TcpSplit,
    {
        let headers = connection.headers()?;
        let method = headers.method;
        let path = headers.path.split('?').next().unwrap_or("");
        let expect_continue = headers.headers.get("Expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue"));

        match (method, path.trim_end_matches('/')) {
            (Method::Post, PRINTER_PATH) => {
                if expect_continue {
                    let (_, body) = connection.split();
                    body.as_raw_reader().write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await.map_err(Error::Io)?;
                }
                self.ipp(connection).await
            }
            (Method::Get, "") => {
                let page = self.status_page();
                let len = format!("{}", page.len());
                connection
                    .initiate_response(200, Some("OK"), &[("Content-Type", "text/plain; charset=utf-8"), ("Content-Length", &len)])
                    .await?;
                connection.write_all(page.as_bytes()).await
            }
            _ => plain(connection, 404, "not found").await,
        }
    }
}

async fn plain<T: Read + Write, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    status: u16,
    text: &str,
) -> Result<(), Error<T::Error>> {
    let len = format!("{}", text.len());
    connection
        .initiate_response(status, None, &[("Content-Type", "text/plain"), ("Content-Length", &len), ("Connection", "close")])
        .await?;
    connection.write_all(text.as_bytes()).await
}

/// Reads and discards the rest of a body.
async fn drain<R: Read>(r: &mut R) -> Result<(), R::Error> {
    let mut sink = [0u8; 512];
    while r.read(&mut sink).await? > 0 {}
    Ok(())
}
