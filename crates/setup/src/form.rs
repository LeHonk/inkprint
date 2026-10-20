//! The setup form: `application/x-www-form-urlencoded` parsing and the HTML pages.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::{BRIGHTNESS_MAX, BRIGHTNESS_MIN, Settings};

/// Labels for brightness steps -2 … +2.
const BRIGHTNESS_LABELS: [&str; 5] = ["Much darker", "Darker", "Normal", "Lighter", "Much lighter"];

/// Path of the form, for both GET (show) and POST (save).
pub const SETUP_PATH: &str = "/setup";

/// Decodes `a=1&b=x+y%21` into pairs. Invalid escapes are kept literally.
pub fn parse_urlencoded(body: &str) -> Vec<(String, String)> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' => {
                    let hex = bytes.get(i + 1..i + 3).and_then(|h| core::str::from_utf8(h).ok());
                    match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        Some(b) => {
                            out.push(b);
                            i += 2;
                        }
                        None => out.push(b'%'),
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    body.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (decode(k), decode(v))
        })
        .collect()
}

/// Builds settings from a submitted form. `ssid_other` (a typed network name) wins over
/// the `ssid` picked from the list. An empty password keeps the current one when the
/// network is unchanged, so the location can be changed without retyping it.
pub fn settings_from_form(body: &str, current: &Settings) -> Result<Settings, &'static str> {
    let fields = parse_urlencoded(body);
    let get = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("");
    let ssid = match get("ssid_other").trim() {
        "" => get("ssid"),
        typed => typed,
    };
    let mut password = String::from(get("password"));
    if password.is_empty() && ssid == current.ssid && get("open") != "on" {
        password = current.password.clone();
    }
    let brightness = match get("brightness") {
        "" => current.brightness,
        v => v.parse().map_err(|_| "Choose a brightness from the list.")?,
    };
    let settings = Settings { ssid: ssid.into(), password, location: get("location").trim().into(), brightness };
    settings.validate()?;
    Ok(settings)
}

/// Escapes text for HTML content and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

const STYLE: &str = "body{font-family:system-ui,sans-serif;max-width:28rem;margin:1.5rem auto;padding:0 1rem;\
color:#222}h1{font-size:1.4rem}label{display:block;margin-top:1rem;font-weight:600}\
input,select{width:100%;box-sizing:border-box;padding:.6rem;font-size:1rem;margin-top:.3rem}\
.hint{color:#666;font-size:.85rem;font-weight:400}.error{background:#fde8e8;padding:.6rem;border-radius:4px}\
button{margin-top:1.5rem;width:100%;padding:.8rem;font-size:1rem}";

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width,initial-scale=1\"><title>{t}</title><style>{STYLE}</style></head>\
         <body><h1>{t}</h1>{body}</body></html>",
        t = escape(title)
    )
}

/// The form. `name` is the printer's name; `networks` the scanned SSIDs (strongest first);
/// `error` a message from a rejected submission.
pub fn render_form(name: &str, current: &Settings, networks: &[String], error: Option<&str>) -> String {
    let mut body = String::new();
    if let Some(e) = error {
        body += &format!("<p class=\"error\">{}</p>", escape(e));
    }
    body += &format!("<form method=\"post\" action=\"{SETUP_PATH}\">");

    body += "<label>Wi-Fi network<select name=\"ssid\">";
    let mut listed = networks.to_vec();
    if current.has_wifi() && !listed.contains(&current.ssid) {
        listed.insert(0, current.ssid.clone());
    }
    for ssid in &listed {
        let selected = if *ssid == current.ssid { " selected" } else { "" };
        body += &format!("<option value=\"{v}\"{selected}>{v}</option>", v = escape(ssid));
    }
    body += "</select></label>";
    body += "<label>Other network <span class=\"hint\">(hidden networks; overrides the list)</span>\
             <input name=\"ssid_other\" maxlength=\"32\" autocapitalize=\"none\"></label>";

    let hint = if current.has_wifi() { "leave empty to keep the current password" } else { "empty for an open network" };
    body += &format!(
        "<label>Password <span class=\"hint\">({hint})</span>\
         <input name=\"password\" type=\"password\" maxlength=\"63\" autocomplete=\"off\"></label>"
    );
    if current.has_wifi() {
        body += "<label><input name=\"open\" type=\"checkbox\" style=\"width:auto\"> \
                 <span class=\"hint\">The new network is open (no password)</span></label>";
    }
    body += &format!(
        "<label>Printer location <span class=\"hint\">(shown in print dialogs)</span>\
         <input name=\"location\" maxlength=\"63\" value=\"{}\"></label>",
        escape(&current.location)
    );
    body += "<label>Brightness <span class=\"hint\">(photos and gray areas; black text \
             stays black)</span><select name=\"brightness\">";
    for (level, label) in (BRIGHTNESS_MIN..=BRIGHTNESS_MAX).zip(BRIGHTNESS_LABELS) {
        let selected = if level == current.brightness { " selected" } else { "" };
        body += &format!("<option value=\"{level}\"{selected}>{label}</option>");
    }
    body += "</select></label>";
    body += "<button type=\"submit\">Save</button></form>\
             <p class=\"hint\">Changing the network or location restarts the printer; \
             brightness is applied to the page on display right away.</p>";
    page(&format!("{name} setup"), &body)
}

/// Shown after saving. `restarting`: the printer restarts to apply the settings (network or
/// location changed); otherwise they were applied in place.
pub fn render_saved(name: &str, settings: &Settings, restarting: bool) -> String {
    let body = if restarting {
        format!(
            "<p>Saved. The printer restarts now and joins <b>{}</b>.</p>\
             <p>If this device was connected to the printer's own setup network, reconnect it \
             to your usual network. The printer then appears in print dialogs, located in \
             <b>{}</b>.</p>",
            escape(&settings.ssid),
            escape(&settings.location)
        )
    } else {
        let label = BRIGHTNESS_LABELS[(settings.brightness - BRIGHTNESS_MIN) as usize];
        format!("<p>Saved. Brightness <b>{label}</b> is applied to the page on display.</p><p><a href=\"{SETUP_PATH}\">Back</a></p>")
    };
    page(&format!("{name} setup"), &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> Settings {
        Settings { ssid: "Home".into(), password: "secret123".into(), location: "Kitchen".into(), brightness: 0 }
    }

    #[test]
    fn urlencoded() {
        let pairs = parse_urlencoded("ssid=My+Net%21&password=a%26b%3Dc&bad=%zz&empty=&k");
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("ssid"), Some("My Net!"));
        assert_eq!(get("password"), Some("a&b=c"));
        assert_eq!(get("bad"), Some("%zz"));
        assert_eq!(get("empty"), Some(""));
        assert_eq!(get("k"), Some(""));
        assert_eq!(parse_urlencoded("x=%C3%B6")[0].1, "ö");
        assert_eq!(parse_urlencoded("x=50%")[0].1, "50%");
    }

    #[test]
    fn form_to_settings() {
        // Changing only the location keeps network and password.
        let s = settings_from_form("ssid=Home&ssid_other=&password=&location=Hall", &current()).unwrap();
        assert_eq!(s, Settings { location: "Hall".into(), ..current() });
        // A typed network wins; a new network needs its own password (or none).
        let s = settings_from_form("ssid=Home&ssid_other=Cabin&password=&location=Hall", &current()).unwrap();
        assert_eq!((s.ssid.as_str(), s.password.as_str()), ("Cabin", ""));
        // Explicitly open network on the same SSID.
        let s = settings_from_form("ssid=Home&password=&open=on&location=Hall", &current()).unwrap();
        assert_eq!(s.password, "");
        // Brightness: from the form, kept when absent, rejected when invalid.
        let s = settings_from_form("ssid=Home&location=Kitchen&brightness=2", &current()).unwrap();
        assert_eq!(s.brightness, 2);
        let s = settings_from_form("ssid=Home&location=Kitchen", &Settings { brightness: -1, ..current() }).unwrap();
        assert_eq!(s.brightness, -1);
        assert!(settings_from_form("ssid=Home&location=Kitchen&brightness=7", &current()).is_err());
        assert!(settings_from_form("ssid=Home&location=Kitchen&brightness=x", &current()).is_err());
        // Validation errors come back as messages.
        assert!(settings_from_form("ssid=Home&password=short&location=Hall", &current()).is_err());
        assert!(settings_from_form("ssid=&location=Hall", &Settings::default()).is_err());
    }

    #[test]
    fn form_escapes_and_lists_networks() {
        let mut s = current();
        s.location = "<Kök & \"Hall\">".into();
        let html = render_form("Inkplate 6 (a1b2)", &s, &["Neighbour".into(), "Home".into()], Some("Oops <b>"));
        assert!(html.contains("&lt;Kök &amp; &quot;Hall&quot;&gt;"));
        assert!(html.contains("Oops &lt;b&gt;"));
        assert!(html.contains("<option value=\"Home\" selected>Home</option>"));
        assert!(html.contains("<option value=\"Neighbour\">"));
        assert!(html.contains("<option value=\"0\" selected>Normal</option>"));
        assert!(html.contains("<option value=\"2\">Much lighter</option>"));
        assert!(!html.contains("secret123"));
    }
}
