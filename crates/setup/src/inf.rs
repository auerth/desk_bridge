//! Minimal INF reader: class, class GUID and the hardware IDs a package declares.
//!
//! Only the parts needed to create a root-enumerated device are parsed. Everything here is pure
//! and unit-tested; the SetupAPI calls live in `driver.rs`.

use std::collections::HashSet;

/// What we need to know about a driver package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InfInfo {
    pub class: Option<String>,
    /// With braces, as written in the INF: `{4d36e968-...}`.
    pub class_guid: Option<String>,
    pub provider: Option<String>,
    pub driver_ver: Option<String>,
    /// Hardware and compatible IDs found in the model sections (original spelling, deduplicated).
    pub hwids: Vec<String>,
}

impl InfInfo {
    #[must_use]
    pub fn declares(&self, hwid: &str) -> bool {
        self.hwids.iter().any(|h| hwid_eq(h, hwid))
    }
}

/// Hardware IDs compare case-insensitively.
#[must_use]
pub fn hwid_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Decode INF bytes: UTF-16 LE/BE with BOM, UTF-8 (with or without BOM), else lossy.
#[must_use]
pub fn decode_inf(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Strip a `;` comment (not inside double quotes) and surrounding whitespace.
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ';' if !in_quotes => return line[..i].trim(),
            _ => {}
        }
    }
    line.trim()
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s)
        .trim()
}

/// Split `key = a, b, c` into the key and the comma separated values.
fn split_entry(line: &str) -> Option<(&str, Vec<&str>)> {
    let (k, v) = line.split_once('=')?;
    Some((unquote(k), v.split(',').map(unquote).collect()))
}

#[must_use]
pub fn parse_inf(text: &str) -> InfInfo {
    // Pass 1: collect sections as (lowercase name, lines).
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for raw in text.lines() {
        let line = strip_comment(raw.trim_start_matches('\u{feff}'));
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push((name.trim().to_ascii_lowercase(), Vec::new()));
        } else if let Some((_, lines)) = sections.last_mut() {
            lines.push(line.to_owned());
        }
    }
    let section = |name: &str| -> Option<&Vec<String>> {
        sections
            .iter()
            .find(|(n, _)| n == &name.to_ascii_lowercase())
            .map(|(_, l)| l)
    };

    let mut info = InfInfo::default();
    if let Some(version) = section("Version") {
        for line in version {
            if let Some((k, v)) = split_entry(line) {
                let first = v.first().map(|s| (*s).to_owned()).filter(|s| !s.is_empty());
                match k.to_ascii_lowercase().as_str() {
                    "class" => info.class = first,
                    "classguid" => info.class_guid = first,
                    "provider" => info.provider = first,
                    "driverver" => info.driver_ver = Some(v.join(",")),
                    _ => {}
                }
            }
        }
    }

    // Pass 2: model sections referenced by [Manufacturer].
    let mut model_sections: Vec<String> = Vec::new();
    if let Some(mfg) = section("Manufacturer") {
        for line in mfg {
            if let Some((_, vals)) = split_entry(line)
                && let Some(base) = vals.first().filter(|b| !b.is_empty())
            {
                model_sections.push(base.to_ascii_lowercase());
                for deco in &vals[1..] {
                    if !deco.is_empty() {
                        model_sections.push(format!("{base}.{deco}").to_ascii_lowercase());
                    }
                }
            }
        }
    }
    let mut seen = HashSet::new();
    for name in model_sections {
        let Some(lines) = section(&name) else {
            continue;
        };
        for line in lines {
            if let Some((_, vals)) = split_entry(line) {
                // vals[0] = install section; the rest are hardware / compatible IDs.
                for id in vals.iter().skip(1).filter(|id| !id.is_empty()) {
                    if seen.insert(id.to_ascii_lowercase()) {
                        info.hwids.push((*id).to_owned());
                    }
                }
            }
        }
    }
    info
}

/// Parse `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` (braces optional) to its binary fields.
#[must_use]
pub fn parse_guid(s: &str) -> Option<(u32, u16, u16, [u8; 8])> {
    let s = s.trim().trim_start_matches('{').trim_end_matches('}');
    if !crate::lid::is_guid(s) {
        return None;
    }
    let p: Vec<&str> = s.split('-').collect();
    let d1 = u32::from_str_radix(p[0], 16).ok()?;
    let d2 = u16::from_str_radix(p[1], 16).ok()?;
    let d3 = u16::from_str_radix(p[2], 16).ok()?;
    let tail = format!("{}{}", p[3], p[4]);
    let mut d4 = [0u8; 8];
    for (i, b) in d4.iter_mut().enumerate() {
        *b = u8::from_str_radix(&tail[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some((d1, d2, d3, d4))
}

/// `REG_MULTI_SZ` bytes (UTF-16 LE, double NUL terminated) for one hardware ID.
#[must_use]
pub fn multi_sz_bytes(hwid: &str) -> Vec<u8> {
    hwid.encode_utf16()
        .chain([0u16, 0u16])
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
; Virtual display sample
[Version]
Signature   = "$WINDOWS NT$"
Class       = Display   ; trailing comment
ClassGuid   = {4d36e968-e325-11ce-bfc1-08002be10318}
Provider    = %ManufacturerName%
DriverVer   = 06/21/2006,10.0.0.1

[Manufacturer]
%ManufacturerName% = Standard, NTamd64, NTamd64.10.0

[Standard.NTamd64]
%DeviceName% = MyDevice_Install, Root\MttVDD   ; the id
%Other%      = MyDevice_Install, "Root\Quoted;Id", ROOT\Compat

[Standard.NTamd64.10.0]
%DeviceName% = MyDevice_Install, root\mttvdd

[Standard]
%Legacy% = X, Root\Legacy

[Unrelated.NTamd64]
%Nope% = Y, Root\ShouldNotAppear

[Strings]
ManufacturerName = "Example"
"#;

    #[test]
    fn parses_version_section() {
        let i = parse_inf(SAMPLE);
        assert_eq!(i.class.as_deref(), Some("Display"));
        assert_eq!(
            i.class_guid.as_deref(),
            Some("{4d36e968-e325-11ce-bfc1-08002be10318}")
        );
        assert_eq!(i.provider.as_deref(), Some("%ManufacturerName%"));
    }

    #[test]
    fn collects_hwids_from_referenced_sections_only() {
        let i = parse_inf(SAMPLE);
        assert!(i.declares(r"Root\MttVDD"));
        assert!(i.declares(r"ROOT\MTTVDD"), "case-insensitive");
        assert!(
            i.declares(r"Root\Quoted;Id"),
            "semicolon inside quotes is not a comment"
        );
        assert!(i.declares(r"Root\Compat"));
        assert!(
            i.declares(r"Root\Legacy"),
            "undecorated section is included"
        );
        assert!(!i.declares(r"Root\ShouldNotAppear"));
        // duplicates (same id in two decorated sections) are reported once
        assert_eq!(
            i.hwids
                .iter()
                .filter(|h| h.eq_ignore_ascii_case(r"root\mttvdd"))
                .count(),
            1
        );
    }

    #[test]
    fn decodes_utf16_and_utf8() {
        let utf16: Vec<u8> = [0xFFu8, 0xFE]
            .into_iter()
            .chain(
                "[Version]\r\nClass=Media\r\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(
            parse_inf(&decode_inf(&utf16)).class.as_deref(),
            Some("Media")
        );
        let mut utf8 = vec![0xEF, 0xBB, 0xBF];
        utf8.extend_from_slice(b"[Version]\nClass=Media\n");
        assert_eq!(
            parse_inf(&decode_inf(&utf8)).class.as_deref(),
            Some("Media")
        );
    }

    #[test]
    fn empty_or_garbage_inf() {
        assert_eq!(parse_inf(""), InfInfo::default());
        assert!(
            parse_inf("garbage without sections\n= = =")
                .hwids
                .is_empty()
        );
    }

    #[test]
    fn guid_parsing() {
        let (a, b, c, d) = parse_guid("{4d36e968-e325-11ce-bfc1-08002be10318}").unwrap();
        assert_eq!((a, b, c), (0x4d36_e968, 0xe325, 0x11ce));
        assert_eq!(d, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18]);
        assert!(parse_guid("nope").is_none());
    }

    #[test]
    fn multi_sz_is_double_nul_terminated_utf16() {
        let b = multi_sz_bytes("Ab");
        assert_eq!(b, [b'A', 0, b'b', 0, 0, 0, 0, 0]);
    }
}
