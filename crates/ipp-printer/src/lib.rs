//! Sans-IO IPP printer.
//!
//! The transport (HTTP over embassy-net on the device, hyper in the simulator) parses the
//! IPP header with [`ipp_codec::Message::parse`] and calls [`Printer::handle`]. That
//! either yields a response, or [`Outcome::ReceiveDocument`]: the transport then streams
//! the rest of the body through a [`raster::Decoder`] and reports back with
//! [`Printer::finish_job`].

#![no_std]

extern crate alloc;

mod attributes;
mod jobs;

pub use attributes::{mdns_txt_records, printer_attributes};
pub use jobs::{JobRecord, JobState, JobTable};

use alloc::format;
use alloc::string::String;
use core::marker::PhantomData;

use board::Board;
use ipp_codec::{DelimiterTag, Message, Operation, StatusCode, Value};
use raster::Format;

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
}

/// Operations answered by [`Printer::handle`]. Advertised as `operations-supported`.
// TODO: Create-Job, Send-Document, Cancel-Job, Cancel-My-Jobs, Close-Job and
// Identify-Printer are required for IPP Everywhere (milestone 7).
pub const IMPLEMENTED_OPERATIONS: &[Operation] = &[
    Operation::PrintJob,
    Operation::ValidateJob,
    Operation::GetJobAttributes,
    Operation::GetJobs,
    Operation::GetPrinterAttributes,
];

pub const DOCUMENT_FORMATS: &[&str] = &["application/octet-stream", "image/pwg-raster", "image/urf"];

/// Document data still to be received for an accepted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingJob {
    pub job_id: u32,
    /// `None` for application/octet-stream: sniff the first bytes with [`Format::sniff`].
    pub format: Option<Format>,
    version: (u8, u8),
    request_id: u32,
}

#[derive(Debug)]
pub enum Outcome {
    Respond(Message),
    ReceiveDocument(PendingJob),
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
    _board: PhantomData<B>,
}

impl<B: Board> Printer<B> {
    pub fn new(config: Config) -> Self {
        Self { config, jobs: JobTable::new(), next_job_id: 1, busy: false, _board: PhantomData }
    }

    /// `now` is seconds since boot (for `printer-up-time` and job timestamps).
    pub fn handle(&mut self, req: &Message, now: u32) -> Outcome {
        if !(1..=2).contains(&req.version.0) {
            return Outcome::Respond(Message::response(req, StatusCode::ServerErrorVersionNotSupported));
        }
        if req.group(DelimiterTag::Operation).is_none() {
            return Outcome::Respond(Message::response(req, StatusCode::ClientErrorBadRequest));
        }
        let op = req.operation().filter(|op| IMPLEMENTED_OPERATIONS.contains(op));
        let resp = match op {
            Some(Operation::GetPrinterAttributes) => self.get_printer_attributes(req, now),
            Some(Operation::ValidateJob) => match self.document_format(req) {
                Ok(_) => Message::response(req, StatusCode::SuccessfulOk),
                Err(status) => Message::response(req, status),
            },
            Some(Operation::PrintJob) => return self.print_job(req, now),
            Some(Operation::GetJobAttributes) => self.get_job_attributes(req),
            Some(Operation::GetJobs) => self.get_jobs(req),
            _ => Message::response(req, StatusCode::ServerErrorOperationNotSupported),
        };
        Outcome::Respond(resp)
    }

    /// Completes a job after its document data was consumed. Returns the response to send.
    pub fn finish_job(&mut self, pending: PendingJob, result: Result<u32, JobError>) -> Message {
        self.busy = false;
        let state = match result {
            Ok(pages) => {
                self.jobs.update(pending.job_id, |j| j.pages = pages);
                JobState::Completed
            }
            Err(_) => JobState::Aborted,
        };
        self.jobs.update(pending.job_id, |j| j.state = state);
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

    fn document_format(&self, req: &Message) -> Result<Option<Format>, StatusCode> {
        let mime = req
            .op_attr("document-format")
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        match mime {
            "application/octet-stream" => Ok(None),
            m => Format::from_mime(m).map(Some).ok_or(StatusCode::ClientErrorDocumentFormatNotSupported),
        }
    }

    fn print_job(&mut self, req: &Message, now: u32) -> Outcome {
        if self.busy {
            return Outcome::Respond(Message::response(req, StatusCode::ServerErrorBusy));
        }
        let format = match self.document_format(req) {
            Ok(f) => f,
            Err(status) => return Outcome::Respond(Message::response(req, status)),
        };
        let name = req
            .op_attr("job-name")
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .unwrap_or("untitled");
        let job_id = self.next_job_id;
        self.next_job_id += 1;
        self.jobs.push(JobRecord { id: job_id, name: String::from(name), state: JobState::Processing, pages: 0, created: now });
        self.busy = true;
        Outcome::ReceiveDocument(PendingJob { job_id, format, version: req.version, request_id: req.request_id })
    }

    fn requested_job(&self, req: &Message) -> Result<&JobRecord, StatusCode> {
        let id = req.op_attr("job-id").and_then(|a| a.first()).and_then(Value::as_int);
        id.and_then(|id| self.jobs.get(id as u32)).ok_or(StatusCode::ClientErrorNotFound)
    }

    fn get_job_attributes(&self, req: &Message) -> Message {
        match self.requested_job(req) {
            Ok(job) => {
                let mut resp = Message::response(req, StatusCode::SuccessfulOk);
                self.job_attributes(job, resp.group_mut(DelimiterTag::Job));
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
        let mut resp = Message::response(req, StatusCode::SuccessfulOk);
        for job in self.jobs.iter().filter(|j| j.state.is_terminal() == completed) {
            // Each job is its own job-attributes group.
            let mut group = ipp_codec::Group::new(DelimiterTag::Job);
            self.job_attributes(job, &mut group);
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
        group.push("time-at-creation", [Value::Integer(job.created as i32)]);
    }

    fn get_printer_attributes(&self, req: &Message, now: u32) -> Message {
        let requested: Option<alloc::vec::Vec<&str>> = req
            .op_attr("requested-attributes")
            .map(|a| a.values.iter().filter_map(Value::as_str).collect());
        let mut all = printer_attributes::<B>(&self.config, now, self.busy, self.jobs.active_count());
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
        assert_eq!(pending.format, Some(Format::Pwg));

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
        let req = request(Operation::IdentifyPrinter, &[]);
        assert_eq!(respond(p.handle(&req, 0)).code, StatusCode::ServerErrorOperationNotSupported as u16);
    }

    #[test]
    fn txt_records_advertise_raster_only() {
        let txt = mdns_txt_records::<Inkplate6>(&printer().config);
        let get = |k: &str| txt.iter().find(|(key, _)| *key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("pdl"), Some("image/pwg-raster,image/urf"));
        assert_eq!(get("Color"), Some("F"));
        assert!(get("URF").unwrap().contains("RS150-300"));
    }
}
