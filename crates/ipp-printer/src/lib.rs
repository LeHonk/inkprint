//! Sans-IO IPP printer.
//!
//! The transport (HTTP over embassy-net on the device, hyper in the simulator) parses the
//! IPP header with [`ipp_codec::Message::parse`] and calls [`Printer::handle`]. That
//! either yields a response, or [`Outcome::ReceiveDocument`]: the transport then streams
//! the rest of the body through a [`raster::Decoder`] and reports back with
//! [`Printer::finish_job`]. [`Outcome::Identify`] asks the transport to make the printer
//! identify itself before sending the response.
//!
//! Jobs either carry their document (Print-Job) or are created empty (Create-Job) and get
//! documents with Send-Document until one has `last-document` set, or Close-Job.

#![no_std]

extern crate alloc;

mod attributes;
mod jobs;

pub use attributes::{PrinterStatus, mdns_txt_records, printer_attributes};
pub use jobs::{JobRecord, JobState, JobTable};

use alloc::format;
use alloc::string::String;
use core::marker::PhantomData;

use board::Board;
use ipp_codec::{DelimiterTag, Message, Operation, StatusCode, Value};
use raster::Format;

/// A document format the printer accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFormat {
    /// PWG Raster or Apple URF.
    Raster(Format),
    Jpeg,
}

impl DocumentFormat {
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/jpeg" => Some(DocumentFormat::Jpeg),
            m => Format::from_mime(m).map(DocumentFormat::Raster),
        }
    }

    /// Detects the format from the first bytes, for `application/octet-stream`.
    pub fn sniff(prefix: &[u8]) -> Option<Self> {
        if jpeg::sniff(prefix) {
            Some(DocumentFormat::Jpeg)
        } else {
            Format::sniff(prefix).map(DocumentFormat::Raster)
        }
    }
}

/// Static identity of the printer. `uri` is what clients use, e.g.
/// `ipp://inkplate.local:631/ipp/print`.
#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub uuid: String,
    pub uri: String,
    /// Free-form location, e.g. "Kitchen" (`printer-location`, mDNS `note`).
    pub location: String,
    /// Status page served over HTTP (`printer-more-info`, mDNS `adminurl`).
    pub more_info: String,
    /// URL of a PNG icon (`printer-icons`), shown by macOS and iOS.
    pub icon: Option<String>,
}

/// Operations answered by [`Printer::handle`] (the IPP Everywhere required set).
/// Advertised as `operations-supported`.
pub const IMPLEMENTED_OPERATIONS: &[Operation] = Operation::ALL;

/// Seconds a client may take between Create-Job and Send-Document
/// (`multiple-operation-time-out`); after that the job is aborted.
pub const MULTIPLE_OPERATION_TIMEOUT: u32 = 120;

pub const DOCUMENT_FORMATS: &[&str] = &["application/octet-stream", "image/jpeg", "image/pwg-raster", "image/urf"];

/// Document data still to be received for an accepted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingJob {
    pub job_id: u32,
    /// `None` for application/octet-stream: sniff the first bytes with
    /// [`DocumentFormat::sniff`].
    pub format: Option<DocumentFormat>,
    /// More documents may follow (Send-Document with `last-document` false).
    last_document: bool,
    version: (u8, u8),
    request_id: u32,
}

#[derive(Debug)]
pub enum Outcome {
    Respond(Message),
    ReceiveDocument(PendingJob),
    /// Identify-Printer: identify the printer (e.g. show a status screen), then respond.
    Identify(Message),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    UnsupportedFormat,
    Malformed,
    Aborted,
}

pub struct Printer<B: Board> {
    pub config: Config,
    jobs: JobTable,
    next_job_id: u32,
    busy: bool,
    /// Uptime seen by the last request, and of the last printer-state change.
    clock: u32,
    state_changed: u32,
    /// Unix time at uptime 0, once known.
    boot_unix: Option<u64>,
    _board: PhantomData<B>,
}

impl<B: Board> Printer<B> {
    pub fn new(config: Config) -> Self {
        Self { config, jobs: JobTable::new(), next_job_id: 1, busy: false, clock: 0, state_changed: 0, boot_unix: None, _board: PhantomData }
    }

    /// `now` is seconds since boot (for `printer-up-time` and job timestamps).
    pub fn handle(&mut self, req: &Message, now: u32) -> Outcome {
        if !(1..=2).contains(&req.version.0) {
            return Outcome::Respond(Message::response(req, StatusCode::ServerErrorVersionNotSupported));
        }
        // RFC 8011 §4.1.1: request-id must be 1 or more. §4.1.4: the operation attributes
        // start with attributes-charset and attributes-natural-language, in that order.
        // §4.2: the target is printer-uri, or job-uri for job operations.
        let leading = |i: usize| req.group(DelimiterTag::Operation).and_then(|g| g.attributes.get(i)).map(|a| a.name.as_str());
        if req.request_id == 0
            || leading(0) != Some("attributes-charset")
            || leading(1) != Some("attributes-natural-language")
            || (req.op_attr("printer-uri").is_none() && req.op_attr("job-uri").is_none())
        {
            return Outcome::Respond(Message::response(req, StatusCode::ClientErrorBadRequest));
        }
        self.clock = now;
        self.expire_pending_jobs(now);
        let op = req.operation().filter(|op| IMPLEMENTED_OPERATIONS.contains(op));
        let resp = match op {
            Some(Operation::GetPrinterAttributes) => self.get_printer_attributes(req, now),
            Some(Operation::ValidateJob) => match self.document_format(req) {
                Ok(_) => Message::response(req, StatusCode::SuccessfulOk),
                Err(status) => Message::response(req, status),
            },
            Some(Operation::PrintJob) => return self.print_job(req, now),
            Some(Operation::CreateJob) => self.create_job(req, now),
            Some(Operation::SendDocument) => return self.send_document(req),
            Some(Operation::CloseJob) => self.close_job(req),
            Some(Operation::CancelJob) => self.cancel_job(req),
            Some(Operation::CancelMyJobs) => self.cancel_my_jobs(req),
            Some(Operation::GetJobAttributes) => self.get_job_attributes(req),
            Some(Operation::GetJobs) => self.get_jobs(req),
            Some(Operation::IdentifyPrinter) => {
                return Outcome::Identify(Message::response(req, StatusCode::SuccessfulOk));
            }
            None => Message::response(req, StatusCode::ServerErrorOperationNotSupported),
        };
        Outcome::Respond(resp)
    }

    /// Completes a job after its document data was consumed. Returns the response to send.
    pub fn finish_job(&mut self, pending: PendingJob, result: Result<u32, JobError>) -> Message {
        self.set_busy(false);
        let state = match result {
            Ok(pages) => {
                self.jobs.update(pending.job_id, |j| j.pages += pages);
                if pending.last_document { JobState::Completed } else { JobState::Pending }
            }
            Err(_) => JobState::Aborted,
        };
        let now = self.clock;
        self.jobs.update(pending.job_id, |j| j.set_state(state, now));
        let status = match result {
            Ok(_) => StatusCode::SuccessfulOk,
            Err(JobError::UnsupportedFormat) => StatusCode::ClientErrorDocumentFormatNotSupported,
            Err(JobError::Malformed) => StatusCode::ClientErrorBadRequest,
            Err(JobError::Aborted) => StatusCode::ServerErrorInternalError,
        };
        let req = Message { version: pending.version, code: 0, request_id: pending.request_id, groups: alloc::vec![] };
        let mut resp = Message::response(&req, status);
        if let Some(job) = self.jobs.get(pending.job_id) {
            self.job_attributes(job, resp.group_mut(DelimiterTag::Job));
        }
        resp
    }

    pub fn jobs(&self) -> &JobTable {
        &self.jobs
    }

    /// Sets the wall clock: `now_unix` is the current Unix time, `uptime` seconds since boot.
    /// Date-time attributes report real time from then on.
    pub fn set_wall_clock(&mut self, now_unix: u64, uptime: u32) {
        self.boot_unix = Some(now_unix.saturating_sub(u64::from(uptime)));
    }

    fn set_busy(&mut self, busy: bool) {
        if self.busy != busy {
            self.busy = busy;
            self.state_changed = self.clock;
        }
    }

    /// `multiple-operation-time-out-action` = abort-job.
    fn expire_pending_jobs(&mut self, now: u32) {
        self.jobs.update_all(|j| {
            if j.state == JobState::Pending && now.saturating_sub(j.created) > MULTIPLE_OPERATION_TIMEOUT {
                j.set_state(JobState::Aborted, now);
            }
        });
    }

    fn document_format(&self, req: &Message) -> Result<Option<DocumentFormat>, StatusCode> {
        let mime = req
            .op_attr("document-format")
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        match mime {
            "application/octet-stream" => Ok(None),
            m => DocumentFormat::from_mime(m).map(Some).ok_or(StatusCode::ClientErrorDocumentFormatNotSupported),
        }
    }

    fn new_job(&mut self, req: &Message, state: JobState, now: u32) -> u32 {
        let op_str = |name: &str| req.op_attr(name).and_then(|a| a.first()).and_then(Value::as_str);
        let job_id = self.next_job_id;
        self.next_job_id += 1;
        let mut job = JobRecord {
            id: job_id,
            name: String::from(op_str("job-name").unwrap_or("untitled")),
            user: String::from(op_str("requesting-user-name").unwrap_or("anonymous")),
            state: JobState::Pending,
            pages: 0,
            created: now,
            processing: None,
            completed: None,
        };
        job.set_state(state, now);
        self.jobs.push(job);
        job_id
    }

    fn print_job(&mut self, req: &Message, now: u32) -> Outcome {
        if self.busy {
            return Outcome::Respond(Message::response(req, StatusCode::ServerErrorBusy));
        }
        let format = match self.document_format(req) {
            Ok(f) => f,
            Err(status) => return Outcome::Respond(Message::response(req, status)),
        };
        let job_id = self.new_job(req, JobState::Processing, now);
        self.set_busy(true);
        Outcome::ReceiveDocument(PendingJob {
            job_id,
            format,
            last_document: true,
            version: req.version,
            request_id: req.request_id,
        })
    }

    fn create_job(&mut self, req: &Message, now: u32) -> Message {
        let job_id = self.new_job(req, JobState::Pending, now);
        self.job_response(req, job_id)
    }

    fn send_document(&mut self, req: &Message) -> Outcome {
        let job_id = match self.requested_job(req) {
            Ok(job) if job.state == JobState::Pending => job.id,
            Ok(_) => return Outcome::Respond(Message::response(req, StatusCode::ClientErrorNotPossible)),
            Err(status) => return Outcome::Respond(Message::response(req, status)),
        };
        if self.busy {
            return Outcome::Respond(Message::response(req, StatusCode::ServerErrorBusy));
        }
        let format = match self.document_format(req) {
            Ok(f) => f,
            Err(status) => return Outcome::Respond(Message::response(req, status)),
        };
        // Required by RFC 8011 §4.3.1.
        let last_document = match req.op_attr("last-document").and_then(|a| a.first()) {
            Some(Value::Boolean(last)) => *last,
            _ => return Outcome::Respond(Message::response(req, StatusCode::ClientErrorBadRequest)),
        };
        let now = self.clock;
        self.jobs.update(job_id, |j| j.set_state(JobState::Processing, now));
        self.set_busy(true);
        Outcome::ReceiveDocument(PendingJob { job_id, format, last_document, version: req.version, request_id: req.request_id })
    }

    /// Moves a pending job to `to` (Close-Job, Cancel-Job); anything else is not possible.
    fn end_pending_job(&mut self, req: &Message, to: JobState) -> Message {
        match self.requested_job(req) {
            Ok(job) if job.state == JobState::Pending => {
                let id = job.id;
                let now = self.clock;
                self.jobs.update(id, |j| j.set_state(to, now));
                self.job_response(req, id)
            }
            Ok(_) => Message::response(req, StatusCode::ClientErrorNotPossible),
            Err(status) => Message::response(req, status),
        }
    }

    fn close_job(&mut self, req: &Message) -> Message {
        self.end_pending_job(req, JobState::Completed)
    }

    /// Only pending jobs can be canceled: a document being received is already streaming
    /// to the display.
    fn cancel_job(&mut self, req: &Message) -> Message {
        self.end_pending_job(req, JobState::Canceled)
    }

    /// Without authentication every job is "mine".
    fn cancel_my_jobs(&mut self, req: &Message) -> Message {
        let now = self.clock;
        self.jobs.update_all(|j| {
            if j.state == JobState::Pending {
                j.set_state(JobState::Canceled, now);
            }
        });
        Message::response(req, StatusCode::SuccessfulOk)
    }

    fn job_response(&self, req: &Message, job_id: u32) -> Message {
        let mut resp = Message::response(req, StatusCode::SuccessfulOk);
        if let Some(job) = self.jobs.get(job_id) {
            self.job_attributes(job, resp.group_mut(DelimiterTag::Job));
        }
        resp
    }

    /// The job named by `job-id`, or by the last path segment of `job-uri`.
    fn requested_job(&self, req: &Message) -> Result<&JobRecord, StatusCode> {
        let from_id = || req.op_attr("job-id").and_then(|a| a.first()).and_then(Value::as_int).map(|id| id as u32);
        let from_uri = || {
            let uri = req.op_attr("job-uri")?.first()?.as_str()?;
            uri.rsplit('/').next()?.parse().ok()
        };
        let id = from_id().or_else(from_uri).ok_or(StatusCode::ClientErrorBadRequest)?;
        self.jobs.get(id).ok_or(StatusCode::ClientErrorNotFound)
    }

    fn get_job_attributes(&self, req: &Message) -> Message {
        match self.requested_job(req) {
            Ok(job) => {
                let mut resp = self.job_response(req, job.id);
                filter_job_attributes(resp.group_mut(DelimiterTag::Job), req, None);
                resp
            }
            Err(status) => Message::response(req, status),
        }
    }

    fn get_jobs(&self, req: &Message) -> Message {
        let completed = req
            .op_attr("which-jobs")
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            == Some("completed");
        // job-ids selects jobs regardless of which-jobs.
        let ids: Option<alloc::vec::Vec<i32>> =
            req.op_attr("job-ids").map(|a| a.values.iter().filter_map(Value::as_int).collect());
        let wanted = |j: &&JobRecord| match &ids {
            Some(ids) => ids.contains(&(j.id as i32)),
            None => j.state.is_terminal() == completed,
        };
        let mut resp = Message::response(req, StatusCode::SuccessfulOk);
        for job in self.jobs.iter().filter(wanted) {
            // Each job is its own job-attributes group.
            let mut group = ipp_codec::Group::new(DelimiterTag::Job);
            self.job_attributes(job, &mut group);
            // RFC 8011 §4.2.6.1: by default only job-uri and job-id.
            filter_job_attributes(&mut group, req, Some(&["job-uri", "job-id"]));
            resp.groups.push(group);
        }
        resp
    }

    fn job_attributes(&self, job: &JobRecord, group: &mut ipp_codec::Group) {
        group.push("job-id", [Value::Integer(job.id as i32)]);
        group.push("job-uri", [Value::uri(&format!("{}/{}", self.config.uri, job.id))]);
        group.push("job-printer-uri", [Value::uri(&self.config.uri)]);
        group.push("job-name", [Value::name(&job.name)]);
        group.push("job-state", [Value::Enum(job.state as i32)]);
        group.push("job-state-reasons", [Value::keyword(job.state.reason())]);
        group.push("job-impressions-completed", [Value::Integer(job.pages as i32)]);
        group.push("job-originating-user-name", [Value::name(&job.user)]);
        let time = |t: Option<u32>| t.map_or(Value::OutOfBand(ipp_codec::ValueTag::NoValue as u8), |t| Value::Integer(t as i32));
        group.push("time-at-creation", [Value::Integer(job.created as i32)]);
        group.push("time-at-processing", [time(job.processing)]);
        group.push("time-at-completed", [time(job.completed)]);
        group.push("job-printer-up-time", [Value::Integer(self.clock.max(1) as i32)]);
    }

    fn get_printer_attributes(&self, req: &Message, now: u32) -> Message {
        let requested: Option<alloc::vec::Vec<&str>> = req
            .op_attr("requested-attributes")
            .map(|a| a.values.iter().filter_map(Value::as_str).collect());
        let status = PrinterStatus {
            now,
            busy: self.busy,
            queued: self.jobs.active_count(),
            state_changed: self.state_changed,
            boot_unix: self.boot_unix,
        };
        let mut all = printer_attributes::<B>(&self.config, status);
        let names = requested.unwrap_or_default();
        let everything =
            names.is_empty() || names.iter().any(|n| matches!(*n, "all" | "printer-description" | "job-template"));
        all.attributes.retain(|a| {
            let name = a.name.as_str();
            let named = names.contains(&name);
            // PWG 5100.7: media-col-database is large and only returned when named.
            if name == "media-col-database" { named } else { everything || named }
        });
        let mut resp = Message::response(req, StatusCode::SuccessfulOk);
        resp.groups.push(all);
        resp
    }
}

/// Keeps the job attributes named in `requested-attributes`; without it, keeps `default`
/// (or everything when `None`).
fn filter_job_attributes(group: &mut ipp_codec::Group, req: &Message, default: Option<&[&str]>) {
    let requested: Option<alloc::vec::Vec<&str>> =
        req.op_attr("requested-attributes").map(|a| a.values.iter().filter_map(Value::as_str).collect());
    let names: alloc::vec::Vec<&str> = match (requested, default) {
        (Some(names), _) => names,
        (None, Some(default)) => default.to_vec(),
        (None, None) => return,
    };
    if names.iter().any(|n| matches!(*n, "all" | "job-description" | "job-template")) {
        return;
    }
    group.attributes.retain(|a| names.contains(&a.name.as_str()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use board::Inkplate6;
    use ipp_codec::{Group, StrTag};

    fn printer() -> Printer<Inkplate6> {
        Printer::new(Config {
            name: String::from("Inkplate"),
            uuid: String::from("urn:uuid:00000000-0000-0000-0000-000000000001"),
            uri: String::from("ipp://inkplate.local:631/ipp/print"),
            location: String::from("Kitchen"),
            more_info: String::from("http://inkplate.local/"),
            icon: Some(String::from("http://inkplate.local/icon.png")),
        })
    }

    fn request(op: Operation, extra: &[(&str, Value)]) -> Message {
        let mut ops = Group::new(DelimiterTag::Operation);
        ops.push("attributes-charset", [Value::str(StrTag::Charset, "utf-8")]);
        ops.push("attributes-natural-language", [Value::str(StrTag::NaturalLanguage, "en")]);
        ops.push("printer-uri", [Value::uri("ipp://inkplate.local:631/ipp/print")]);
        for (name, v) in extra {
            ops.push(name, [v.clone()]);
        }
        Message { version: (2, 0), code: op as u16, request_id: 42, groups: alloc::vec![ops] }
    }

    fn respond(outcome: Outcome) -> Message {
        match outcome {
            Outcome::Respond(m) => m,
            other => panic!("expected response, got {other:?}"),
        }
    }

    #[test]
    fn get_printer_attributes_filters() {
        let mut p = printer();
        let req = request(Operation::GetPrinterAttributes, &[("requested-attributes", Value::keyword("media-default"))]);
        let resp = respond(p.handle(&req, 10));
        assert_eq!(resp.code, StatusCode::SuccessfulOk as u16);
        assert_eq!(resp.request_id, 42);
        let attrs = &resp.group(DelimiterTag::Printer).unwrap().attributes;
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].values[0].as_str(), Some("iso_a6_105x148mm"));
    }

    #[test]
    fn ready_media_includes_a_size_android_knows() {
        let mut p = printer();
        let resp = respond(p.handle(&request(Operation::GetPrinterAttributes, &[]), 0));
        let attrs = resp.group(DelimiterTag::Printer).unwrap();
        let keywords = |name: &str| -> alloc::vec::Vec<&str> {
            attrs.get(name).unwrap().values.iter().filter_map(Value::as_str).collect()
        };
        assert_eq!(keywords("media-default"), ["iso_a6_105x148mm"]);
        assert_eq!(keywords("media-ready"), ["iso_a6_105x148mm", "na_index-4x6_4x6in"]);
        assert_eq!(attrs.get("media-col-ready").unwrap().values.len(), 2);
    }

    #[test]
    fn output_bin_is_face_down_so_pages_arrive_in_order() {
        let mut p = printer();
        let resp = respond(p.handle(&request(Operation::GetPrinterAttributes, &[]), 0));
        let attrs = resp.group(DelimiterTag::Printer).unwrap();
        for name in ["output-bin-default", "output-bin-supported"] {
            let values: alloc::vec::Vec<&str> = attrs.get(name).unwrap().values.iter().filter_map(Value::as_str).collect();
            assert_eq!(values, ["face-down"], "{name}");
        }
    }

    #[test]
    fn media_col_database_only_when_named() {
        let mut p = printer();
        let has_db = |resp: &Message| resp.group(DelimiterTag::Printer).unwrap().get("media-col-database").is_some();
        for extra in [alloc::vec![], alloc::vec![("requested-attributes", Value::keyword("all"))]] {
            let resp = respond(p.handle(&request(Operation::GetPrinterAttributes, &extra), 0));
            assert!(!has_db(&resp));
            assert!(resp.group(DelimiterTag::Printer).unwrap().get("media-col-default").is_some());
        }
        let req = request(Operation::GetPrinterAttributes, &[("requested-attributes", Value::keyword("media-col-database"))]);
        assert!(has_db(&respond(p.handle(&req, 0))));
    }

    #[test]
    fn print_job_lifecycle() {
        let mut p = printer();
        let req = request(Operation::PrintJob, &[("document-format", Value::mime("image/pwg-raster"))]);
        let Outcome::ReceiveDocument(pending) = p.handle(&req, 5) else { panic!() };
        assert_eq!(pending.format, Some(DocumentFormat::Raster(Format::Pwg)));

        let busy = respond(p.handle(&req, 6));
        assert_eq!(busy.code, StatusCode::ServerErrorBusy as u16);

        let done = p.finish_job(pending, Ok(2));
        assert_eq!(done.code, StatusCode::SuccessfulOk as u16);
        let job = done.group(DelimiterTag::Job).unwrap();
        assert_eq!(job.get("job-state").unwrap().values[0], Value::Enum(JobState::Completed as i32));

        let req = request(Operation::GetJobs, &[("which-jobs", Value::keyword("completed"))]);
        let jobs = respond(p.handle(&req, 7));
        assert_eq!(jobs.groups.iter().filter(|g| g.tag == DelimiterTag::Job).count(), 1);
    }

    #[test]
    fn rejects_pdf_and_unknown_ops() {
        let mut p = printer();
        let req = request(Operation::ValidateJob, &[("document-format", Value::mime("application/pdf"))]);
        assert_eq!(respond(p.handle(&req, 0)).code, StatusCode::ClientErrorDocumentFormatNotSupported as u16);
        let mut req = request(Operation::GetJobs, &[]);
        req.code = 0x0010; // Pause-Printer
        assert_eq!(respond(p.handle(&req, 0)).code, StatusCode::ServerErrorOperationNotSupported as u16);
    }

    fn job_id(resp: &Message) -> Value {
        resp.group(DelimiterTag::Job).unwrap().get("job-id").unwrap().values[0].clone()
    }

    fn job_state(p: &mut Printer<Inkplate6>, id: &Value) -> Value {
        let resp = respond(p.handle(&request(Operation::GetJobAttributes, &[("job-id", id.clone())]), 0));
        resp.group(DelimiterTag::Job).unwrap().get("job-state").unwrap().values[0].clone()
    }

    fn send_document(p: &mut Printer<Inkplate6>, id: &Value, last: bool) -> Outcome {
        let req = request(
            Operation::SendDocument,
            &[("job-id", id.clone()), ("last-document", Value::Boolean(last)), ("document-format", Value::mime("image/urf"))],
        );
        p.handle(&req, 0)
    }

    #[test]
    fn create_job_then_send_documents() {
        let mut p = printer();
        let created = respond(p.handle(&request(Operation::CreateJob, &[("job-name", Value::name("recipe"))]), 1));
        assert_eq!(created.code, StatusCode::SuccessfulOk as u16);
        let id = job_id(&created);
        assert_eq!(job_state(&mut p, &id), Value::Enum(JobState::Pending as i32));

        let Outcome::ReceiveDocument(doc) = send_document(&mut p, &id, false) else { panic!() };
        assert_eq!(doc.format, Some(DocumentFormat::Raster(Format::Urf)));
        assert_eq!(job_state(&mut p, &id), Value::Enum(JobState::Processing as i32));
        p.finish_job(doc, Ok(2));
        assert_eq!(job_state(&mut p, &id), Value::Enum(JobState::Pending as i32));

        let Outcome::ReceiveDocument(doc) = send_document(&mut p, &id, true) else { panic!() };
        let done = p.finish_job(doc, Ok(1));
        let job = done.group(DelimiterTag::Job).unwrap();
        assert_eq!(job.get("job-state").unwrap().values[0], Value::Enum(JobState::Completed as i32));
        assert_eq!(job.get("job-impressions-completed").unwrap().values[0], Value::Integer(3));

        // A completed job takes no more documents.
        assert_eq!(respond(send_document(&mut p, &id, true)).code, StatusCode::ClientErrorNotPossible as u16);
        // Unknown job.
        assert_eq!(respond(send_document(&mut p, &Value::Integer(99), true)).code, StatusCode::ClientErrorNotFound as u16);
    }

    #[test]
    fn send_document_requires_last_document() {
        let mut p = printer();
        let id = job_id(&respond(p.handle(&request(Operation::CreateJob, &[]), 0)));
        let req = request(Operation::SendDocument, &[("job-id", id.clone())]);
        assert_eq!(respond(p.handle(&req, 0)).code, StatusCode::ClientErrorBadRequest as u16);
        assert_eq!(job_state(&mut p, &id), Value::Enum(JobState::Pending as i32));
    }

    #[test]
    fn send_document_while_busy() {
        let mut p = printer();
        let id = job_id(&respond(p.handle(&request(Operation::CreateJob, &[]), 0)));
        let print = request(Operation::PrintJob, &[]);
        let Outcome::ReceiveDocument(_) = p.handle(&print, 0) else { panic!() };
        assert_eq!(respond(send_document(&mut p, &id, true)).code, StatusCode::ServerErrorBusy as u16);
    }

    #[test]
    fn close_and_cancel_pending_jobs() {
        let mut p = printer();
        let create = request(Operation::CreateJob, &[]);
        let a = job_id(&respond(p.handle(&create, 0)));
        let b = job_id(&respond(p.handle(&create, 0)));
        let c = job_id(&respond(p.handle(&create, 0)));

        let cancel = |id: &Value| request(Operation::CancelJob, &[("job-id", id.clone())]);
        assert_eq!(respond(p.handle(&cancel(&a), 0)).code, StatusCode::SuccessfulOk as u16);
        assert_eq!(job_state(&mut p, &a), Value::Enum(JobState::Canceled as i32));
        assert_eq!(respond(p.handle(&cancel(&a), 0)).code, StatusCode::ClientErrorNotPossible as u16);

        let close = request(Operation::CloseJob, &[("job-id", b.clone())]);
        assert_eq!(respond(p.handle(&close, 0)).code, StatusCode::SuccessfulOk as u16);
        assert_eq!(job_state(&mut p, &b), Value::Enum(JobState::Completed as i32));

        assert_eq!(respond(p.handle(&request(Operation::CancelMyJobs, &[]), 0)).code, StatusCode::SuccessfulOk as u16);
        assert_eq!(job_state(&mut p, &c), Value::Enum(JobState::Canceled as i32));
        assert_eq!(job_state(&mut p, &b), Value::Enum(JobState::Completed as i32));
    }

    #[test]
    fn charset_and_language_must_lead() {
        let mut p = printer();
        let mut req = request(Operation::GetPrinterAttributes, &[]);
        req.groups[0].attributes.swap(0, 1);
        assert_eq!(respond(p.handle(&req, 0)).code, StatusCode::ClientErrorBadRequest as u16);
        req.groups[0].attributes.remove(0);
        assert_eq!(respond(p.handle(&req, 0)).code, StatusCode::ClientErrorBadRequest as u16);
    }

    #[test]
    fn target_uri_is_required_and_job_uri_selects_the_job() {
        let mut p = printer();
        let mut req = request(Operation::GetPrinterAttributes, &[]);
        req.groups[0].attributes.retain(|a| a.name != "printer-uri");
        assert_eq!(respond(p.handle(&req, 0)).code, StatusCode::ClientErrorBadRequest as u16);

        let id = job_id(&respond(p.handle(&request(Operation::CreateJob, &[]), 0)));
        let mut req = request(Operation::GetJobAttributes, &[]);
        req.groups[0].attributes.retain(|a| a.name != "printer-uri");
        let uri = alloc::format!("ipp://inkplate.local:631/ipp/print/{}", id.as_int().unwrap());
        req.groups[0].push("job-uri", [Value::uri(&uri)]);
        let resp = respond(p.handle(&req, 0));
        assert_eq!(resp.group(DelimiterTag::Job).unwrap().get("job-id").unwrap().values[0], id);
    }

    #[test]
    fn request_id_zero_is_bad_request() {
        let mut p = printer();
        let mut req = request(Operation::GetPrinterAttributes, &[]);
        req.request_id = 0;
        let resp = respond(p.handle(&req, 0));
        assert_eq!(resp.code, StatusCode::ClientErrorBadRequest as u16);
        assert!(resp.group(DelimiterTag::Printer).is_none());
    }

    #[test]
    fn pending_jobs_abort_after_timeout() {
        let mut p = printer();
        let id = job_id(&respond(p.handle(&request(Operation::CreateJob, &[]), 10)));
        let state_at = |p: &mut Printer<Inkplate6>, t: u32| {
            let resp = respond(p.handle(&request(Operation::GetJobAttributes, &[("job-id", id.clone())]), t));
            resp.group(DelimiterTag::Job).unwrap().get("job-state").unwrap().values[0].clone()
        };
        assert_eq!(state_at(&mut p, 10 + MULTIPLE_OPERATION_TIMEOUT), Value::Enum(JobState::Pending as i32));
        assert_eq!(state_at(&mut p, 11 + MULTIPLE_OPERATION_TIMEOUT), Value::Enum(JobState::Aborted as i32));
    }

    #[test]
    fn get_jobs_returns_uri_and_id_unless_asked() {
        let mut p = printer();
        p.handle(&request(Operation::CreateJob, &[]), 0);
        let names = |resp: &Message| -> alloc::vec::Vec<String> {
            resp.group(DelimiterTag::Job).unwrap().attributes.iter().map(|a| a.name.clone()).collect()
        };
        let resp = respond(p.handle(&request(Operation::GetJobs, &[]), 0));
        assert_eq!(names(&resp), ["job-id", "job-uri"]);
        let resp = respond(p.handle(&request(Operation::GetJobs, &[("requested-attributes", Value::keyword("job-state"))]), 0));
        assert_eq!(names(&resp), ["job-state"]);
        let resp = respond(p.handle(&request(Operation::GetJobs, &[("requested-attributes", Value::keyword("all"))]), 0));
        assert!(names(&resp).len() > 5);
    }

    #[test]
    fn get_jobs_by_job_ids() {
        let mut p = printer();
        let a = job_id(&respond(p.handle(&request(Operation::CreateJob, &[]), 0)));
        let _b = job_id(&respond(p.handle(&request(Operation::CreateJob, &[]), 0)));
        let resp = respond(p.handle(&request(Operation::GetJobs, &[("job-ids", a.clone())]), 0));
        let jobs: alloc::vec::Vec<_> = resp.groups.iter().filter(|g| g.tag == DelimiterTag::Job).collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].get("job-id").unwrap().values[0], a);
    }

    #[test]
    fn identify_printer_asks_transport_to_identify() {
        let mut p = printer();
        let req = request(Operation::IdentifyPrinter, &[("identify-actions", Value::keyword("display"))]);
        let Outcome::Identify(resp) = p.handle(&req, 0) else { panic!("expected Identify") };
        assert_eq!(resp.code, StatusCode::SuccessfulOk as u16);
    }

    #[test]
    fn advertises_all_operations_and_icon() {
        let mut p = printer();
        let resp = respond(p.handle(&request(Operation::GetPrinterAttributes, &[]), 0));
        let attrs = resp.group(DelimiterTag::Printer).unwrap();
        let ops: alloc::vec::Vec<i32> = attrs.get("operations-supported").unwrap().values.iter().filter_map(Value::as_int).collect();
        for op in Operation::ALL {
            assert!(ops.contains(&(*op as i32)), "{op:?}");
        }
        assert_eq!(attrs.get("printer-icons").unwrap().values[0].as_str(), Some("http://inkplate.local/icon.png"));
        assert_eq!(attrs.get("identify-actions-supported").unwrap().values[0].as_str(), Some("display"));
    }

    #[test]
    fn txt_records_advertise_formats() {
        let txt = mdns_txt_records::<Inkplate6>(&printer().config);
        let get = |k: &str| txt.iter().find(|(key, _)| *key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("pdl"), Some("image/jpeg,image/pwg-raster,image/urf"));
        assert_eq!(get("Color"), Some("F"));
        assert!(get("URF").unwrap().contains("RS150-300"));
    }
}
