//! The printer-attributes catalog and the matching DNS-SD TXT records.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use board::{Board, Media};
use ipp_codec::{Attribute, DelimiterTag, Group, StrTag, Value, ValueTag};

use crate::{Config, DOCUMENT_FORMATS, IMPLEMENTED_OPERATIONS};

/// `printer-state` enum values.
const IDLE: i32 = 3;
const PROCESSING: i32 = 4;

fn media_col(m: &Media) -> Value {
    let size = Value::Collection(alloc::vec![
        Attribute::new("x-dimension", [Value::Integer(m.width_hmm)]),
        Attribute::new("y-dimension", [Value::Integer(m.height_hmm)]),
    ]);
    // Borderless: the whole sheet maps to the panel, so margins are all zero.
    Value::Collection(alloc::vec![
        Attribute::new("media-size", [size]),
        Attribute::new("media-size-name", [Value::keyword(m.keyword)]),
        Attribute::new("media-top-margin", [Value::Integer(0)]),
        Attribute::new("media-bottom-margin", [Value::Integer(0)]),
        Attribute::new("media-left-margin", [Value::Integer(0)]),
        Attribute::new("media-right-margin", [Value::Integer(0)]),
    ])
}

fn keywords(list: &[&str]) -> Vec<Value> {
    list.iter().map(|k| Value::keyword(k)).collect()
}

/// `urf-supported` values; also joined with "," for the mDNS `URF` key.
fn urf_supported<B: Board>() -> Vec<String> {
    let mut v: Vec<String> = ["V1.4", "CP1", "W8"].iter().map(|s| String::from(*s)).collect();
    if B::is_color() {
        v.push(String::from("SRGB24"));
    }
    let res = B::RESOLUTIONS;
    v.push(match res {
        [one] => format!("RS{one}"),
        _ => format!("RS{}-{}", res[0], res[res.len() - 1]),
    });
    v
}

/// Printer state that changes at run time.
#[derive(Debug, Clone, Copy)]
pub struct PrinterStatus {
    /// Seconds since boot.
    pub now: u32,
    pub busy: bool,
    pub queued: usize,
    /// Uptime of the last printer-state change.
    pub state_changed: u32,
    /// Unix time at uptime 0, once known (RTC or NTP).
    pub boot_unix: Option<u64>,
}

impl PrinterStatus {
    /// `dateTime` (RFC 2579, UTC) for an uptime. Without a wall clock, times count from
    /// 1970-01-01T00:00:00Z at boot.
    fn date_time(&self, uptime: u32) -> Value {
        let t = clock::DateTime::from_unix(self.boot_unix.unwrap_or(0) + u64::from(uptime));
        let [y0, y1] = t.year.to_be_bytes();
        Value::DateTime([y0, y1, t.month, t.day, t.hour, t.minute, t.second, 0, b'+', 0, 0])
    }
}

pub fn printer_attributes<B: Board>(config: &Config, status: PrinterStatus) -> Group {
    let PrinterStatus { now, busy, queued, state_changed, .. } = status;
    let mut g = Group::new(DelimiterTag::Printer);
    let color = B::is_color();

    // Identity and state.
    g.push("printer-name", [Value::name(&config.name)]);
    g.push("printer-make-and-model", [Value::text(&format!("Soldered {}", B::MODEL))]);
    g.push("printer-info", [Value::text(&config.name)]);
    g.push("printer-uuid", [Value::uri(&config.uuid)]);
    g.push("printer-uri-supported", [Value::uri(&config.uri)]);
    g.push("uri-security-supported", keywords(&["none"]));
    g.push("uri-authentication-supported", keywords(&["none"]));
    g.push("printer-location", [Value::text(&config.location)]);
    g.push("printer-more-info", [Value::uri(&config.more_info)]);
    if let Some(icon) = &config.icon {
        g.push("printer-icons", [Value::uri(icon)]);
    }
    g.push("identify-actions-default", keywords(&["display"]));
    g.push("identify-actions-supported", keywords(&["display"]));
    g.push("printer-state", [Value::Enum(if busy { PROCESSING } else { IDLE })]);
    g.push("printer-state-reasons", keywords(&["none"]));
    g.push("printer-is-accepting-jobs", [Value::Boolean(true)]);
    g.push("printer-up-time", [Value::Integer(now.max(1) as i32)]);
    g.push("printer-current-time", [status.date_time(now)]);
    g.push("printer-config-change-time", [Value::Integer(0)]);
    g.push("printer-config-change-date-time", [status.date_time(0)]);
    g.push("printer-state-change-time", [Value::Integer(state_changed as i32)]);
    g.push("printer-state-change-date-time", [status.date_time(state_changed)]);
    g.push("queued-job-count", [Value::Integer(queued as i32)]);
    g.push("printer-kind", keywords(&["document"]));
    g.push("printer-device-id", [Value::text(&format!("MFG:Soldered;MDL:{};CMD:PWGRaster,URF;", B::MODEL))]);
    g.push("printer-geo-location", [Value::OutOfBand(ValueTag::Unknown as u8)]);
    g.push("printer-organization", [Value::text("")]);
    g.push("printer-organizational-unit", [Value::text("")]);
    // No consumables; one "supply" keeps clients that expect the attribute happy.
    g.push("printer-supply", [Value::Other(ValueTag::OctetString as u8, b"index=1;class=supplyThatIsConsumed;type=other;unit=percent;maxcapacity=100;level=100;".to_vec())]);
    g.push("printer-supply-description", [Value::text("E-paper panel")]);
    g.push("printer-supply-info-uri", [Value::uri(&config.more_info)]);
    // A full refresh takes a few seconds.
    g.push("pages-per-minute", [Value::Integer(10)]);

    // Protocol.
    g.push("ipp-versions-supported", keywords(&["1.1", "2.0"]));
    g.push("ipp-features-supported", keywords(&["ipp-everywhere"]));
    g.push(
        "operations-supported",
        IMPLEMENTED_OPERATIONS.iter().map(|op| Value::Enum(*op as i32)).collect::<Vec<_>>(),
    );
    g.push("charset-configured", [Value::str(StrTag::Charset, "utf-8")]);
    g.push("charset-supported", [Value::str(StrTag::Charset, "utf-8")]);
    g.push("natural-language-configured", [Value::str(StrTag::NaturalLanguage, "en")]);
    g.push("generated-natural-language-supported", [Value::str(StrTag::NaturalLanguage, "en")]);
    g.push("compression-supported", keywords(&["none"]));
    g.push("pdl-override-supported", keywords(&["attempted"]));
    g.push("multiple-document-jobs-supported", [Value::Boolean(false)]);
    g.push("multiple-operation-time-out", [Value::Integer(crate::MULTIPLE_OPERATION_TIMEOUT as i32)]);
    g.push("multiple-operation-time-out-action", keywords(&["abort-job"]));
    g.push("which-jobs-supported", keywords(&["completed", "not-completed"]));
    g.push("job-ids-supported", [Value::Boolean(true)]);
    g.push("preferred-attributes-supported", [Value::Boolean(false)]);
    // Every format gets the same attributes, so the filter is accepted and ignored.
    g.push("printer-get-attributes-supported", keywords(&["document-format"]));
    // Overrides are accepted and ignored: there is nothing to vary per page on a display.
    g.push("overrides-supported", keywords(&["document-number", "pages"]));
    g.push("job-creation-attributes-supported", keywords(&["copies", "media", "media-col", "print-color-mode", "print-quality", "printer-resolution", "sides"]));

    // Document formats.
    g.push("document-format-default", [Value::mime("application/octet-stream")]);
    g.push("document-format-supported", DOCUMENT_FORMATS.iter().map(|m| Value::mime(m)).collect::<Vec<_>>());
    // JPEG is decoded as it streams in, so file size is not limited; progressive,
    // arithmetic-coded and CMYK files are not supported.
    g.push("jpeg-k-octets-supported", [Value::Range { lower: 0, upper: i32::MAX }]);
    g.push("jpeg-x-dimension-supported", [Value::Range { lower: 1, upper: i32::from(jpeg::MAX_DIMENSION) }]);
    g.push("jpeg-y-dimension-supported", [Value::Range { lower: 1, upper: i32::from(jpeg::MAX_DIMENSION) }]);
    g.push("jpeg-features-supported", keywords(&["none"]));
    let res: Vec<Value> = B::RESOLUTIONS.iter().map(|&r| Value::dpi(r as i32, r as i32)).collect();
    g.push("pwg-raster-document-resolution-supported", res.clone());
    g.push("pwg-raster-document-type-supported", keywords(if color { &["sgray_8", "srgb_8"] } else { &["sgray_8"] }));
    g.push("pwg-raster-document-sheet-back", keywords(&["normal"]));
    g.push("urf-supported", urf_supported::<B>().iter().map(|s| Value::keyword(s)).collect::<Vec<_>>());
    g.push("printer-resolution-supported", res);
    g.push("printer-resolution-default", [Value::dpi(B::RESOLUTIONS[0] as i32, B::RESOLUTIONS[0] as i32)]);

    // Media.
    let media = B::MEDIA;
    // The panel holds no paper, so "ready" is a hint for client defaults; see READY_MEDIA.
    let ready = &media[..B::READY_MEDIA.clamp(1, media.len())];
    g.push("media-default", [Value::keyword(media[0].keyword)]);
    g.push("media-ready", ready.iter().map(|m| Value::keyword(m.keyword)).collect::<Vec<_>>());
    g.push("media-supported", media.iter().map(|m| Value::keyword(m.keyword)).collect::<Vec<_>>());
    g.push("media-col-default", [media_col(&media[0])]);
    g.push("media-col-ready", ready.iter().map(media_col).collect::<Vec<_>>());
    g.push("media-col-database", media.iter().map(media_col).collect::<Vec<_>>());
    g.push("media-col-supported", keywords(&["media-size", "media-size-name", "media-top-margin", "media-bottom-margin", "media-left-margin", "media-right-margin"]));
    let size = |m: &Media| {
        Value::Collection(alloc::vec![
            Attribute::new("x-dimension", [Value::Integer(m.width_hmm)]),
            Attribute::new("y-dimension", [Value::Integer(m.height_hmm)]),
        ])
    };
    g.push("media-size-supported", media.iter().map(size).collect::<Vec<_>>());
    for margin in ["media-top-margin-supported", "media-bottom-margin-supported", "media-left-margin-supported", "media-right-margin-supported"] {
        g.push(margin, [Value::Integer(0)]);
    }
    g.push("media-type-supported", keywords(&["stationery"]));
    g.push("media-source-supported", keywords(&["main"]));

    // Job template.
    g.push("copies-default", [Value::Integer(1)]);
    g.push("copies-supported", [Value::Range { lower: 1, upper: 1 }]);
    g.push("sides-default", keywords(&["one-sided"]));
    g.push("sides-supported", keywords(&["one-sided"]));
    g.push("print-color-mode-default", keywords(&[if color { "color" } else { "monochrome" }]));
    g.push("print-color-mode-supported", keywords(if color { &["auto", "monochrome", "color"] } else { &["auto", "monochrome"] }));
    g.push("print-content-optimize-default", keywords(&["auto"]));
    g.push("print-content-optimize-supported", keywords(&["auto"]));
    g.push("print-rendering-intent-default", keywords(&["auto"]));
    g.push("print-rendering-intent-supported", keywords(&["auto"]));
    g.push("print-quality-default", [Value::Enum(4)]);
    g.push("print-quality-supported", [Value::Enum(4)]);
    g.push("print-scaling-default", keywords(&["fit"]));
    g.push("print-scaling-supported", keywords(&["auto", "fit", "fill", "none"]));
    g.push("orientation-requested-default", [Value::Enum(3)]);
    g.push("orientation-requested-supported", [Value::Enum(3), Value::Enum(4)]);
    g.push("finishings-default", [Value::Enum(3)]);
    g.push("finishings-supported", [Value::Enum(3)]);
    // Face-down: clients send pages first to last. For face-up bins they reverse the order
    // (Android's print service, CUPS) so the stack comes out right, and the page left on
    // the panel would be the first one instead of the last.
    g.push("output-bin-default", keywords(&["face-down"]));
    g.push("output-bin-supported", keywords(&["face-down"]));
    g.push("multiple-document-handling-supported", keywords(&["separate-documents-uncollated-copies"]));
    g.push("color-supported", [Value::Boolean(color)]);
    g
}

/// TXT records for `_ipp._tcp` (subtypes `_print` and `_universal`), per the
/// IPP Everywhere and AirPrint Bonjour specs.
pub fn mdns_txt_records<B: Board>(config: &Config) -> Vec<(&'static str, String)> {
    let rp = config.uri.splitn(4, '/').nth(3).unwrap_or("ipp/print");
    let yes_no = |b: bool| String::from(if b { "T" } else { "F" });
    alloc::vec![
        ("txtvers", String::from("1")),
        ("qtotal", String::from("1")),
        ("rp", String::from(rp)),
        ("ty", format!("Soldered {}", B::MODEL)),
        ("product", format!("({})", B::MODEL)),
        ("pdl", String::from("image/jpeg,image/pwg-raster,image/urf")),
        ("URF", urf_supported::<B>().join(",")),
        ("Color", yes_no(B::is_color())),
        ("Duplex", yes_no(false)),
        ("UUID", String::from(config.uuid.trim_start_matches("urn:uuid:"))),
        ("kind", String::from("document")),
        ("PaperMax", String::from("legal-A4")),
        ("note", config.location.clone()),
        ("adminurl", config.more_info.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_time_uses_wall_clock_when_known() {
        let mut status = PrinterStatus { now: 0, busy: false, queued: 0, state_changed: 0, boot_unix: None };
        assert_eq!(status.date_time(0), Value::DateTime([0x07, 0xB2, 1, 1, 0, 0, 0, 0, b'+', 0, 0]));
        // Booted 2026-10-19T19:41:01Z; one hour, one minute and one second later.
        status.boot_unix = Some(1_792_438_861);
        assert_eq!(status.date_time(3_661), Value::DateTime([0x07, 0xEA, 10, 19, 20, 42, 2, 0, b'+', 0, 0]));
    }
}
