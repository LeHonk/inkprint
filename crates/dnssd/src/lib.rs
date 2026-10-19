//! DNS-SD records for the printer (RFC 6762/6763), as an `edge-mdns` answer source.
//!
//! edge-mdns ships a generic `Service`, but it publishes subtypes as a two-level PTR chain
//! (`_print._sub._ipp._tcp` → `_print.<instance>._ipp._tcp` → instance), which browsers
//! resolve to an instance without SRV/TXT. IPP Everywhere (`_print`) and AirPrint
//! (`_universal`) clients browse by subtype, so [`PrinterService`] publishes them the
//! RFC 6763 §7.1 way: the subtype name points straight at the service instance.

#![no_std]

use edge_mdns::domain::base::iana::Class;
use edge_mdns::domain::base::{Record, Ttl};
use edge_mdns::domain::rdata::{A, AllRecordData, Ptr, Srv};
use edge_mdns::{DNS_SD_OWNER, HostAnswer, HostAnswers, MdnsError, NameSlice, RecordDataChain, Txt};

pub use edge_mdns::HostAnswersMdnsHandler;

/// The IPP service type.
pub const SERVICE: &str = "_ipp";
pub const PROTOCOL: &str = "_tcp";
/// IPP Everywhere and AirPrint subtypes.
pub const PRINTER_SUBTYPES: &[&str] = &["_print", "_universal"];
/// RFC 6762 §10 recommends 120 s for records containing a host name.
pub const TTL: Ttl = Ttl::from_secs(120);

/// One IPP printer on one host.
#[derive(Debug, Clone)]
pub struct PrinterService<'a> {
    /// Service instance name shown to users, e.g. "Inkplate 6 (a1b2)". Spaces are fine.
    pub instance: &'a str,
    /// Host label without `.local`, e.g. "inkplate-a1b2".
    pub hostname: &'a str,
    pub ipv4: [u8; 4],
    pub port: u16,
    pub subtypes: &'a [&'a str],
    /// TXT key/value pairs, see `ipp_printer::mdns_txt_records`.
    pub txt: &'a [(&'a str, &'a str)],
}

impl HostAnswers for PrinterService<'_> {
    fn visit<F, E>(&self, mut f: F) -> Result<(), E>
    where
        F: FnMut(HostAnswer) -> Result<(), E>,
        E: From<MdnsError>,
    {
        let host = &[self.hostname, "local"];
        let instance = &[self.instance, SERVICE, PROTOCOL, "local"];
        let service = &[SERVICE, PROTOCOL, "local"];
        fn ptr<'n>(owner: NameSlice<'n>, target: NameSlice<'n>) -> HostAnswer<'n> {
            Record::new(owner, Class::IN, TTL, RecordDataChain::Next(AllRecordData::Ptr(Ptr::new(target))))
        }

        let [a, b, c, d] = self.ipv4;
        f(Record::new(
            NameSlice::new(host),
            Class::IN,
            TTL,
            RecordDataChain::Next(AllRecordData::A(A::from_octets(a, b, c, d))),
        ))?;
        f(Record::new(
            NameSlice::new(instance),
            Class::IN,
            TTL,
            RecordDataChain::Next(AllRecordData::Srv(Srv::new(0, 0, self.port, NameSlice::new(host)))),
        ))?;
        f(Record::new(NameSlice::new(instance), Class::IN, TTL, RecordDataChain::This(Txt::new(self.txt))))?;
        f(ptr(DNS_SD_OWNER, NameSlice::new(service)))?;
        f(ptr(NameSlice::new(service), NameSlice::new(instance)))?;
        for subtype in self.subtypes {
            let sub = &[subtype, "_sub", SERVICE, PROTOCOL, "local"];
            f(ptr(NameSlice::new(sub), NameSlice::new(instance)))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::format;
    use std::string::String;
    use std::vec::Vec;

    use edge_mdns::domain::base::{Message, MessageBuilder, ParsedName, Question, Rtype};
    use edge_mdns::{Buf, MdnsHandler, MdnsRequest, MdnsResponse};

    use super::*;

    /// The instance name as `domain` prints it (spaces escaped, no trailing dot).
    const INSTANCE: &str = "Inkplate\\ 6\\ (a1b2)._ipp._tcp.local";
    const TXT: &[(&str, &str)] = &[("txtvers", "1"), ("rp", "ipp/print"), ("pdl", "image/pwg-raster,image/urf")];

    fn service() -> PrinterService<'static> {
        PrinterService {
            instance: "Inkplate 6 (a1b2)",
            hostname: "inkplate-a1b2",
            ipv4: [192, 168, 1, 42],
            port: 631,
            subtypes: PRINTER_SUBTYPES,
            txt: TXT,
        }
    }

    /// Sends a PTR/SRV/... query for `name` through the edge-mdns handler and returns the
    /// answer and additional sections as "owner TYPE data" lines.
    fn ask(labels: &[&str], rtype: Rtype) -> (Vec<String>, Vec<String>) {
        let mut qbuf = [0u8; 512];
        let mut qb = MessageBuilder::from_target(Buf::new(&mut qbuf)).unwrap().question();
        qb.push(Question::new_in(NameSlice::new(labels), rtype)).unwrap();
        let Buf(qbuf, qlen) = qb.finish();

        let svc = service();
        let mut handler = HostAnswersMdnsHandler::new(&svc);
        let mut rbuf = [0u8; 1500];
        let request = MdnsRequest::Request { legacy: false, multicast: true, data: &qbuf[..qlen] };
        let data = match handler.handle(request, &mut rbuf).unwrap() {
            MdnsResponse::Reply { data, .. } => data,
            MdnsResponse::None => return (Vec::new(), Vec::new()),
        };

        let msg = Message::from_octets(data).unwrap();
        let lines = |section: edge_mdns::domain::base::message::RecordSection<'_, &[u8]>| {
            section
                .map(|rr| {
                    let rec = rr.unwrap().into_record::<AllRecordData<&[u8], ParsedName<&[u8]>>>().unwrap().unwrap();
                    format!("{} {} {}", rec.owner(), rec.rtype(), rec.data())
                })
                .collect::<Vec<_>>()
        };
        let answers = lines(msg.answer().unwrap());
        let additional = lines(msg.additional().unwrap());
        (answers, additional)
    }

    #[test]
    fn subtype_browse_points_straight_at_the_instance() {
        for subtype in PRINTER_SUBTYPES {
            let (answers, additional) = ask(&[subtype, "_sub", "_ipp", "_tcp", "local"], Rtype::PTR);
            assert_eq!(answers, [format!("{subtype}._sub._ipp._tcp.local PTR {INSTANCE}.")]);
            // The browser can resolve the instance from the additional section alone.
            assert!(additional.iter().any(|l| l == &format!("{INSTANCE} SRV 0 0 631 inkplate-a1b2.local")), "{additional:?}");
            assert!(additional.iter().any(|l| l.contains(" TXT ") && l.contains("rp=ipp/print")), "{additional:?}");
            assert!(additional.iter().any(|l| l == "inkplate-a1b2.local A 192.168.1.42"), "{additional:?}");
        }
    }

    #[test]
    fn plain_service_browse_and_enumeration() {
        let (answers, _) = ask(&["_ipp", "_tcp", "local"], Rtype::PTR);
        assert_eq!(answers, [format!("_ipp._tcp.local PTR {INSTANCE}.")]);
        let (answers, _) = ask(&["_services", "_dns-sd", "_udp", "local"], Rtype::PTR);
        assert_eq!(answers, ["_services._dns-sd._udp.local PTR _ipp._tcp.local."]);
    }

    #[test]
    fn host_name_resolves() {
        let (answers, _) = ask(&["inkplate-a1b2", "local"], Rtype::A);
        assert_eq!(answers, ["inkplate-a1b2.local A 192.168.1.42"]);
    }

    #[test]
    fn ignores_other_services() {
        let (answers, _) = ask(&["_http", "_tcp", "local"], Rtype::PTR);
        assert!(answers.is_empty());
    }
}
