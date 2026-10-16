//! Pure (Win32-free) format helpers: MIME mapping, the CF_HTML wrapper, sensitive-flag logic,
//! UTF-16 helpers and the `text/uri-list` conversion.
//!
//! Everything in here is unit-testable without touching the real clipboard.

use deskbridge_platform::clipboard::mime;
use std::path::{Path, PathBuf};

/// Predefined clipboard format ids (winuser.h).
pub const CF_DIB: u32 = 8;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_HDROP: u32 = 15;
pub const CF_DIBV5: u32 = 17;
/// First id of the registered (private) clipboard format range.
pub const CF_REGISTERED_FIRST: u32 = 0xC000;

/// Registered clipboard format names.
pub const NAME_HTML: &str = "HTML Format";
pub const NAME_RTF: &str = "Rich Text Format";
pub const NAME_PNG: &str = "PNG";
/// Alias used by Chromium/Firefox-style producers.
pub const NAME_PNG_ALIAS: &str = "image/png";
pub const NAME_ORIGIN: &str = "DeskBridge.Origin";
pub const NAME_EXCLUDE_MONITOR: &str = "ExcludeClipboardContentFromMonitorProcessing";
pub const NAME_CAN_INCLUDE_HISTORY: &str = "CanIncludeInClipboardHistory";
pub const NAME_CAN_UPLOAD_CLOUD: &str = "CanUploadToCloudClipboard";
pub const NAME_PREFERRED_DROP_EFFECT: &str = "Preferred DropEffect";
pub const NAME_PERFORMED_DROP_EFFECT: &str = "Performed DropEffect";
pub const NAME_LOGICAL_PERFORMED_DROP_EFFECT: &str = "Logical Performed DropEffect";
pub const NAME_PASTE_SUCCEEDED: &str = "Paste Succeeded";
pub const NAME_FILE_DESCRIPTOR: &str = "FileGroupDescriptorW";
pub const NAME_FILE_CONTENTS: &str = "FileContents";

/// Eager-text limit (plan 8.5): text below this many UTF-8 bytes is sent immediately.
pub const EAGER_TEXT_LIMIT: usize = 1024 * 1024;

/// The clipboard formats this backend understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Html,
    Rtf,
    /// Registered "PNG" format.
    Png,
    DibV5,
    Dib,
    HDrop,
}

impl Kind {
    /// The internal MIME type this format maps to.
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Kind::Text => mime::TEXT,
            Kind::Html => mime::HTML,
            Kind::Rtf => mime::RTF,
            Kind::Png | Kind::DibV5 | Kind::Dib => mime::PNG,
            Kind::HDrop => mime::URI_LIST,
        }
    }
}

/// Formats to register (in priority order) when offering `mime` with delayed rendering.
/// `text/uri-list` has no delayed representation (remote paths are meaningless locally).
#[must_use]
pub fn kinds_for_mime(m: &str) -> &'static [Kind] {
    match m {
        mime::TEXT => &[Kind::Text],
        mime::HTML => &[Kind::Html],
        mime::RTF => &[Kind::Rtf],
        // PNG first so modern apps (Teams, Paint) pick it; DIBV5 for everything else.
        mime::PNG => &[Kind::Png, Kind::DibV5],
        _ => &[],
    }
}

/// Ids of the registered formats (filled in by the Win32 layer, plain data here so the mapping
/// is testable).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RegisteredIds {
    pub html: u32,
    pub rtf: u32,
    pub png: u32,
    pub png_alias: u32,
}

impl RegisteredIds {
    /// Maps a clipboard format id to the format kind we understand.
    #[must_use]
    pub fn kind_of(&self, id: u32) -> Option<Kind> {
        match id {
            CF_UNICODETEXT => Some(Kind::Text),
            CF_HDROP => Some(Kind::HDrop),
            CF_DIBV5 => Some(Kind::DibV5),
            CF_DIB => Some(Kind::Dib),
            0 => None,
            _ if id == self.html => Some(Kind::Html),
            _ if id == self.rtf => Some(Kind::Rtf),
            _ if id == self.png || id == self.png_alias => Some(Kind::Png),
            _ => None,
        }
    }

    /// Clipboard id to register for a kind (predefined or registered).
    #[must_use]
    pub fn id_of(&self, k: Kind) -> u32 {
        match k {
            Kind::Text => CF_UNICODETEXT,
            Kind::Html => self.html,
            Kind::Rtf => self.rtf,
            Kind::Png => self.png,
            Kind::DibV5 => CF_DIBV5,
            Kind::Dib => CF_DIB,
            Kind::HDrop => CF_HDROP,
        }
    }
}

/// Distinct MIME types (in clipboard priority order) for a list of format ids.
#[must_use]
pub fn mimes_of_formats(ids: &RegisteredIds, formats: &[u32]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for &f in formats {
        if let Some(k) = ids.kind_of(f) {
            let m = k.mime();
            if !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out
}

/// Decides whether the clipboard content is sensitive (password managers).
///
/// `names` are the registered format names currently on the clipboard; `dword` reads the first
/// DWORD of the named format's data. Sensitive if:
/// * `ExcludeClipboardContentFromMonitorProcessing` is present (any value), or
/// * `CanIncludeInClipboardHistory` / `CanUploadToCloudClipboard` is present and 0
///   (or present but unreadable: we err on the private side).
#[must_use]
pub fn is_sensitive<S: AsRef<str>>(names: &[S], dword: impl Fn(&str) -> Option<u32>) -> bool {
    let has = |n: &str| names.iter().any(|x| x.as_ref().eq_ignore_ascii_case(n));
    if has(NAME_EXCLUDE_MONITOR) {
        return true;
    }
    [NAME_CAN_INCLUDE_HISTORY, NAME_CAN_UPLOAD_CLOUD]
        .into_iter()
        .any(|n| has(n) && dword(n).is_none_or(|v| v == 0))
}

// ---------------------------------------------------------------- text

/// UTF-16 (up to the first NUL) to `String`.
#[must_use]
pub fn utf16_to_string(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// `String` to NUL-terminated UTF-16 little-endian bytes.
#[must_use]
pub fn string_to_utf16_bytes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity((s.len() + 1) * 2);
    for u in s.encode_utf16().chain(std::iter::once(0)) {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// Little-endian UTF-16 bytes (up to the first NUL unit) to `String`.
#[must_use]
pub fn utf16_bytes_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    utf16_to_string(&units)
}

/// Drops trailing NUL bytes (clipboard byte formats are usually NUL terminated).
#[must_use]
pub fn trim_trailing_nuls(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    &data[..end]
}

// ---------------------------------------------------------------- CF_HTML

const START_MARK: &str = "<!--StartFragment-->";
const END_MARK: &str = "<!--EndFragment-->";
const CF_HTML_HEADER_LEN: usize = 105;

/// Makes sure an HTML document carries `<!--StartFragment-->` / `<!--EndFragment-->` markers.
fn html_with_markers(html: &str) -> String {
    if html.contains(START_MARK) && html.contains(END_MARK) {
        return html.to_owned();
    }
    // ASCII lowercase keeps byte offsets intact.
    let lower = html.to_ascii_lowercase();
    if let Some(b) = lower.find("<body")
        && let Some(gt) = lower[b..].find('>')
    {
        let start = b + gt + 1;
        let end = lower
            .rfind("</body")
            .filter(|&e| e >= start)
            .unwrap_or(html.len());
        return format!(
            "{}{START_MARK}{}{END_MARK}{}",
            &html[..start],
            &html[start..end],
            &html[end..]
        );
    }
    format!("<html><body>\r\n{START_MARK}{html}{END_MARK}\r\n</body></html>")
}

/// Builds a `CF_HTML` ("HTML Format") payload from the internal representation (UTF-8 HTML
/// document or fragment). All offsets are UTF-8 byte offsets, as the format requires.
#[must_use]
pub fn build_cf_html(html: &str) -> Vec<u8> {
    let doc = html_with_markers(html);
    // Both markers exist by construction.
    let sf = doc.find(START_MARK).map_or(0, |p| p + START_MARK.len());
    let ef = doc[sf..].find(END_MARK).map_or(doc.len(), |p| p + sf);
    let header = format!(
        "Version:0.9\r\nStartHTML:{:010}\r\nEndHTML:{:010}\r\nStartFragment:{:010}\r\nEndFragment:{:010}\r\n",
        CF_HTML_HEADER_LEN,
        CF_HTML_HEADER_LEN + doc.len(),
        CF_HTML_HEADER_LEN + sf,
        CF_HTML_HEADER_LEN + ef,
    );
    debug_assert_eq!(header.len(), CF_HTML_HEADER_LEN);
    let mut out = header.into_bytes();
    out.extend_from_slice(doc.as_bytes());
    out
}

/// Parses a `CF_HTML` payload into the internal representation: the HTML document between
/// `StartHTML` and `EndHTML`, with fragment markers (re-inserted from `StartFragment` /
/// `EndFragment` when the producer left them out). Tolerates bogus offsets (-1, past the end).
#[must_use]
pub fn parse_cf_html(data: &[u8]) -> Option<String> {
    let data = trim_trailing_nuls(data);
    let len = data.len();
    let (mut sh, mut eh, mut sf, mut ef) = (None, None, None, None);
    for line in data.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line[0] == b'<' {
            break;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            break;
        };
        let key = std::str::from_utf8(&line[..colon]).ok()?.trim();
        let val = std::str::from_utf8(&line[colon + 1..])
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok());
        match key {
            "StartHTML" => sh = val,
            "EndHTML" => eh = val,
            "StartFragment" => sf = val,
            "EndFragment" => ef = val,
            _ => {}
        }
    }
    let valid = |o: Option<i64>| {
        o.filter(|&v| v >= 0 && (v as usize) <= len)
            .map(|v| v as usize)
    };
    let (sh, eh, sf, ef) = (valid(sh), valid(eh), valid(sf), valid(ef));
    let frag = match (sf, ef) {
        (Some(a), Some(b)) if a <= b => Some((a, b)),
        _ => None,
    };
    let Some(start) = sh else {
        // No usable document range: fall back to the bare fragment.
        let (a, b) = frag?;
        return Some(String::from_utf8_lossy(&data[a..b]).into_owned());
    };
    let end = eh.filter(|&e| e >= start).unwrap_or(len);
    let bytes = &data[start..end];
    let mut text = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => return Some(String::from_utf8_lossy(bytes).into_owned()),
    };
    if !(text.contains(START_MARK) && text.contains(END_MARK))
        && let Some((a, b)) = frag
        && a >= start
        && b <= end
    {
        let (ra, rb) = (a - start, b - start);
        if text.is_char_boundary(ra) && text.is_char_boundary(rb) {
            text.insert_str(rb, END_MARK);
            text.insert_str(ra, START_MARK);
        }
    }
    Some(text)
}

// ---------------------------------------------------------------- DROPFILES / URI list

/// Size of the `DROPFILES` header.
pub const DROPFILES_HEADER_LEN: usize = 20;

/// Builds a `CF_HDROP` payload (wide `DROPFILES`, double-NUL terminated list).
#[must_use]
pub fn build_dropfiles(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DROPFILES_HEADER_LEN + 2);
    out.extend_from_slice(&(DROPFILES_HEADER_LEN as u32).to_le_bytes()); // pFiles
    out.extend_from_slice(&[0u8; 8]); // pt
    out.extend_from_slice(&0u32.to_le_bytes()); // fNC
    out.extend_from_slice(&1u32.to_le_bytes()); // fWide
    for p in paths {
        let s = p.to_string_lossy().replace('/', "\\");
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Parses a `CF_HDROP` payload into paths. Returns an empty list for malformed data.
#[must_use]
pub fn parse_dropfiles(data: &[u8]) -> Vec<PathBuf> {
    if data.len() < DROPFILES_HEADER_LEN {
        return Vec::new();
    }
    let p_files = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let wide = u32::from_le_bytes([data[16], data[17], data[18], data[19]]) != 0;
    let Some(list) = data.get(p_files..) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if wide {
        let units: Vec<u16> = list
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        for part in units.split(|&u| u == 0) {
            if part.is_empty() {
                break; // double NUL
            }
            out.push(PathBuf::from(String::from_utf16_lossy(part)));
        }
    } else {
        for part in list.split(|&b| b == 0) {
            if part.is_empty() {
                break;
            }
            out.push(PathBuf::from(String::from_utf8_lossy(part).into_owned()));
        }
    }
    out
}

/// Converts paths to an RFC 2483 `text/uri-list` (CRLF separated `file:` URIs).
#[must_use]
pub fn paths_to_uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        out.push_str(&path_to_file_uri(p));
        out.push_str("\r\n");
    }
    out
}

fn path_to_file_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let (prefix, rest) = match s.strip_prefix("//") {
        Some(unc) => ("file://", unc.to_owned()),
        None => ("file:///", s),
    };
    let mut out = String::from(prefix);
    for b in rest.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CF_TEXT: u32 = 1;

    fn ids() -> RegisteredIds {
        RegisteredIds {
            html: 0xC001,
            rtf: 0xC002,
            png: 0xC003,
            png_alias: 0xC004,
        }
    }

    #[test]
    fn mime_mapping_dedupes_and_orders() {
        let i = ids();
        let got = mimes_of_formats(
            &i,
            &[
                CF_UNICODETEXT,
                CF_TEXT,
                0xC003,
                CF_DIBV5,
                CF_DIB,
                0xC001,
                0xC002,
                CF_HDROP,
                0xC999,
            ],
        );
        assert_eq!(
            got,
            vec![
                "text/plain",
                "image/png",
                "text/html",
                "text/rtf",
                "text/uri-list"
            ]
        );
        assert_eq!(mimes_of_formats(&i, &[]), Vec::<&str>::new());
        assert_eq!(i.kind_of(0xC004), Some(Kind::Png));
        assert_eq!(i.kind_of(0), None);
        assert_eq!(i.kind_of(CF_TEXT), None); // synthesised, never read directly
    }

    #[test]
    fn kinds_for_mime_table() {
        assert_eq!(kinds_for_mime("image/png"), &[Kind::Png, Kind::DibV5]);
        assert_eq!(kinds_for_mime("text/plain"), &[Kind::Text]);
        assert!(kinds_for_mime("text/uri-list").is_empty());
        assert!(kinds_for_mime("application/x-unknown").is_empty());
        let i = ids();
        assert_eq!(i.id_of(Kind::Html), 0xC001);
        assert_eq!(i.id_of(Kind::DibV5), CF_DIBV5);
    }

    #[test]
    fn sensitive_decision() {
        let none = |_: &str| None;
        assert!(!is_sensitive::<&str>(&[], none));
        assert!(!is_sensitive(&["HTML Format"], none));
        // Exclude flag alone is enough, whatever the value.
        assert!(is_sensitive(
            &["ExcludeClipboardContentFromMonitorProcessing"],
            none
        ));
        // Case-insensitive names.
        assert!(is_sensitive(
            &["excludeclipboardcontentfrommonitorprocessing"],
            none
        ));
        // DWORD flags.
        let zero = |_: &str| Some(0);
        let one = |_: &str| Some(1);
        assert!(is_sensitive(&["CanIncludeInClipboardHistory"], zero));
        assert!(!is_sensitive(&["CanIncludeInClipboardHistory"], one));
        assert!(is_sensitive(&["CanUploadToCloudClipboard"], zero));
        assert!(!is_sensitive(&["CanUploadToCloudClipboard"], one));
        // Present but unreadable: conservative.
        assert!(is_sensitive(&["CanUploadToCloudClipboard"], none));
        // Mixed: history ok, cloud denied.
        let mixed = |n: &str| Some(u32::from(n == NAME_CAN_INCLUDE_HISTORY));
        assert!(is_sensitive(
            &["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"],
            mixed
        ));
        // Flags absent -> not sensitive even if the reader would say 0.
        assert!(!is_sensitive(&["Rich Text Format"], zero));
    }

    #[test]
    fn utf16_helpers() {
        let b = string_to_utf16_bytes("aé€😀");
        assert_eq!(&b[b.len() - 2..], &[0, 0]);
        assert_eq!(utf16_bytes_to_string(&b), "aé€😀");
        assert_eq!(utf16_to_string(&[0x61, 0, 0x62]), "a");
        assert_eq!(trim_trailing_nuls(b"ab\0\0"), b"ab");
        assert_eq!(trim_trailing_nuls(b"\0\0"), b"");
    }

    #[test]
    fn cf_html_build_offsets_with_multibyte() {
        let frag = "<b>Grüße € 日本 😀</b>";
        let data = build_cf_html(frag);
        let text = std::str::from_utf8(&data).unwrap();
        assert!(text.starts_with("Version:0.9\r\nStartHTML:0000000105\r\n"));
        let num = |key: &str| -> usize {
            let p = text.find(key).unwrap() + key.len();
            text[p..p + 10].parse().unwrap()
        };
        let (sh, eh, sf, ef) = (
            num("StartHTML:"),
            num("EndHTML:"),
            num("StartFragment:"),
            num("EndFragment:"),
        );
        assert_eq!(sh, 105);
        assert_eq!(eh, data.len());
        assert_eq!(&data[sf..ef], frag.as_bytes());
        assert!(data[sh..].starts_with(b"<html>"));
        assert!(data[..sf].ends_with(START_MARK.as_bytes()));
        assert!(data[ef..].starts_with(END_MARK.as_bytes()));
    }

    #[test]
    fn cf_html_roundtrip_and_idempotence() {
        let frag = "<p>Zürich – 東京</p>";
        let doc = parse_cf_html(&build_cf_html(frag)).unwrap();
        assert!(doc.contains(frag));
        assert!(doc.contains(START_MARK) && doc.contains(END_MARK));
        // Re-wrapping the internal representation keeps the markers (no nesting).
        let again = parse_cf_html(&build_cf_html(&doc)).unwrap();
        assert_eq!(again, doc);
        // NUL terminated input (as delivered by GlobalLock).
        let mut nul = build_cf_html(frag);
        nul.push(0);
        assert_eq!(parse_cf_html(&nul).unwrap(), doc);
    }

    #[test]
    fn cf_html_full_document_gets_markers_inside_body() {
        let html = "<html><head><title>t</title></head><BODY class=x><p>hi</p></BODY></html>";
        let text = String::from_utf8(build_cf_html(html)).unwrap();
        assert!(
            text.contains("<BODY class=x><!--StartFragment--><p>hi</p><!--EndFragment--></BODY>")
        );
    }

    #[test]
    fn cf_html_parse_foreign_without_markers_and_bad_offsets() {
        // Producer without markers: fragment offsets only.
        let body = "<html><body>Hello</body></html>";
        let header_len = 97;
        let hdr = format!(
            "Version:1.0\r\nStartHTML:{:08}\r\nEndHTML:{:08}\r\nStartFragment:{:08}\r\nEndFragment:{:08}\r\n",
            header_len,
            header_len + body.len(),
            header_len + 12,
            header_len + 17
        );
        assert_eq!(hdr.len(), header_len);
        let data = format!("{hdr}{body}");
        let doc = parse_cf_html(data.as_bytes()).unwrap();
        assert_eq!(
            doc,
            "<html><body><!--StartFragment-->Hello<!--EndFragment--></body></html>"
        );
        // EndHTML = -1 and nonsense fragment offsets: take everything after StartHTML.
        let tpl = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:-1\r\nStartFragment:9999999999\r\nEndFragment:-1\r\n<i>x</i>";
        let start = tpl.find("<i>").unwrap();
        let fixed = tpl.replacen("0000000000", &format!("{start:010}"), 1);
        assert_eq!(parse_cf_html(fixed.as_bytes()).unwrap(), "<i>x</i>");
        // Garbage.
        assert_eq!(parse_cf_html(b"hello"), None);
        assert_eq!(parse_cf_html(b""), None);
    }

    #[test]
    fn dropfiles_roundtrip() {
        let paths = vec![
            PathBuf::from(r"C:\Users\ü\a b.txt"),
            PathBuf::from(r"\\srv\share\日本.bin"),
        ];
        let d = build_dropfiles(&paths);
        assert_eq!(u32::from_le_bytes(d[0..4].try_into().unwrap()), 20);
        assert_eq!(u32::from_le_bytes(d[16..20].try_into().unwrap()), 1); // fWide
        assert_eq!(&d[d.len() - 4..], &[0, 0, 0, 0]); // double NUL
        assert_eq!(parse_dropfiles(&d), paths);
        assert_eq!(
            parse_dropfiles(&build_dropfiles(&[])),
            Vec::<PathBuf>::new()
        );
        assert_eq!(build_dropfiles(&[]).len(), 22);
        assert!(parse_dropfiles(&[1, 2, 3]).is_empty());
        // Forward slashes are normalised.
        assert_eq!(
            parse_dropfiles(&build_dropfiles(&[PathBuf::from("C:/x/y.txt")])),
            vec![PathBuf::from(r"C:\x\y.txt")]
        );
    }

    #[test]
    fn dropfiles_ansi_variant() {
        let mut d = vec![0u8; 20];
        d[0] = 20;
        d.extend_from_slice(b"C:\\a.txt\0D:\\b.txt\0\0");
        assert_eq!(
            parse_dropfiles(&d),
            vec![PathBuf::from(r"C:\a.txt"), PathBuf::from(r"D:\b.txt")]
        );
    }

    #[test]
    fn uri_list_encoding() {
        let l = paths_to_uri_list(&[
            PathBuf::from(r"C:\Users\Thorben\My File ä.txt"),
            PathBuf::from(r"\\srv\share\x.txt"),
        ]);
        assert_eq!(
            l,
            "file:///C:/Users/Thorben/My%20File%20%C3%A4.txt\r\nfile://srv/share/x.txt\r\n"
        );
    }
}
