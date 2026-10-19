//! The printer-attributes catalog and the matching DNS-SD TXT records.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use board::{Board, Media};
use ipp_codec::{Attribute, DelimiterTag, Group, StrTag, Value};

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

pub fn printer_attributes<B: Board>(config: &Config, now: u32, busy: bool, queued: usize) -> Group {
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
    g.push("printer-state", [Value::Enum(if busy { PROCESSING } else { IDLE })]);
    g.push("printer-state-reasons", keywords(&["none"]));
    g.push("printer-is-accepting-jobs", [Value::Boolean(true)]);
    g.push("printer-up-time", [Value::Integer(now.max(1) as i32)]);
    g.push("printer-config-change-time", [Value::Integer(1)]);
    g.push("queued-job-count", [Value::Integer(queued as i32)]);
    g.push("printer-kind", keywords(&["document"]));
    // TODO: printer-icons (serve a PNG over HTTP; macOS wants it), printer-device-id.

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
    g.push("job-creation-attributes-supported", keywords(&["copies", "media", "media-col", "print-color-mode", "print-quality", "printer-resolution", "sides"]));

    // Document formats.
    g.push("document-format-default", [Value::mime("application/octet-stream")]);
    g.push("document-format-supported", DOCUMENT_FORMATS.iter().map(|m| Value::mime(m)).collect::<Vec<_>>());
    let res: Vec<Value> = B::RESOLUTIONS.iter().map(|&r| Value::dpi(r as i32, r as i32)).collect();
    g.push("pwg-raster-document-resolution-supported", res.clone());
    g.push("pwg-raster-document-type-supported", keywords(if color { &["sgray_8", "srgb_8"] } else { &["sgray_8"] }));
    g.push("pwg-raster-document-sheet-back", keywords(&["normal"]));
    g.push("urf-supported", urf_supported::<B>().iter().map(|s| Value::keyword(s)).collect::<Vec<_>>());
    g.push("printer-resolution-supported", res);
    g.push("printer-resolution-default", [Value::dpi(B::RESOLUTIONS[0] as i32, B::RESOLUTIONS[0] as i32)]);

    // Media.
    let media = B::MEDIA;
    g.push("media-default", [Value::keyword(media[0].keyword)]);
    g.push("media-ready", [Value::keyword(media[0].keyword)]);
    g.push("media-supported", media.iter().map(|m| Value::keyword(m.keyword)).collect::<Vec<_>>());
    g.push("media-col-default", [media_col(&media[0])]);
    g.push("media-col-ready", [media_col(&media[0])]);
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

    // Job template.
    g.push("copies-default", [Value::Integer(1)]);
    g.push("copies-supported", [Value::Range { lower: 1, upper: 1 }]);
    g.push("sides-default", keywords(&["one-sided"]));
    g.push("sides-supported", keywords(&["one-sided"]));
    g.push("print-color-mode-default", keywords(&[if color { "color" } else { "monochrome" }]));
    g.push("print-color-mode-supported", keywords(if color { &["auto", "monochrome", "color"] } else { &["auto", "monochrome"] }));
    g.push("print-quality-default", [Value::Enum(4)]);
    g.push("print-quality-supported", [Value::Enum(4)]);
    g.push("print-scaling-default", keywords(&["fit"]));
    g.push("print-scaling-supported", keywords(&["auto", "fit", "fill", "none"]));
    g.push("orientation-requested-default", [Value::Enum(3)]);
    g.push("orientation-requested-supported", [Value::Enum(3), Value::Enum(4)]);
    g.push("finishings-default", [Value::Enum(3)]);
    g.push("finishings-supported", [Value::Enum(3)]);
    g.push("output-bin-default", keywords(&["face-up"]));
    g.push("output-bin-supported", keywords(&["face-up"]));
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
        ("pdl", String::from("image/pwg-raster,image/urf")),
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
