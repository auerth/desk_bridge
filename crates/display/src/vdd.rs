//! Virtual Display Driver (VirtualDrivers "MttVDD", ADR 0004) configuration logic that needs no
//! OS access: patching `vdd_settings.xml`, building pipe messages and choosing the render GPU.
//!
//! The XML is edited as text (the file is tiny and its schema flat) so that unknown options and
//! the user's other settings survive a rewrite. Comments are skipped while searching.

use deskbridge_platform::display::Mode;

/// Template embedded from the driver package config (`drivers/display/config`).
pub const DEFAULT_SETTINGS_TEMPLATE: &str =
    include_str!("../../../drivers/display/config/vdd_settings.deskbridge.xml");
pub const SETTINGS_FILE_NAME: &str = "vdd_settings.xml";
pub const DEFAULT_SETTINGS_DIR: &str = r"C:\VirtualDisplayDriver";
pub const PIPE_NAME: &str = r"\\.\pipe\MTTVirtualDisplayPipe";
/// Hardware ids the driver INF declares (ADR 0004).
pub const HARDWARE_IDS: [&str; 2] = [r"Root\MttVDD", "MttVDD"];
/// Upper bound of modes we write (the driver fails to initialise at ~100+ combinations).
pub const MAX_MODES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("settings XML has no <vdd_settings> root element")]
    NoRoot,
    #[error("no valid mode in the requested list")]
    NoModes,
}

// ---------------------------------------------------------------------------------------------
// XML text helpers

/// Copy of `xml` with every comment replaced by spaces (same byte offsets, still valid UTF-8).
fn mask_comments(xml: &str) -> String {
    let b = xml.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i + 3 < b.len() {
        if &b[i..i + 4] == b"<!--" {
            let end = xml[i + 4..]
                .find("-->")
                .map_or(b.len(), |e| i + 4 + e + 3);
            for c in &mut out[i..end] {
                *c = b' ';
            }
            i = end;
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("masking with ASCII spaces keeps UTF-8 valid")
}

pub fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c => o.push(c),
        }
    }
    o
}

pub fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Byte range of the inner text of `<tag>…</tag>` inside `masked[from..to]`.
fn element_inner(masked: &str, tag: &str, from: usize, to: usize) -> Option<(usize, usize)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let s = masked[from..to].find(&open)? + from + open.len();
    let e = masked[s..to].find(&close)? + s;
    Some((s, e))
}

/// Byte range of a self-closing `<tag/>` / `<tag />` inside `masked[from..to]`.
fn element_selfclosing(masked: &str, tag: &str, from: usize, to: usize) -> Option<(usize, usize)> {
    for pat in [format!("<{tag}/>"), format!("<{tag} />")] {
        if let Some(p) = masked[from..to].find(&pat) {
            return Some((from + p, from + p + pat.len()));
        }
    }
    None
}

/// Replaces `<tag>…</tag>` content in `xml`, optionally restricted to a parent `<section>`.
/// Inserts the element (and the section) when missing.
fn set_element(xml: &str, section: Option<&str>, tag: &str, value_xml: &str) -> String {
    let masked = mask_comments(xml);
    let (lo, hi) = match section {
        Some(sec) => match element_inner(&masked, sec, 0, masked.len()) {
            Some(r) => r,
            None => {
                // Section missing: append it before the root's closing tag.
                let add = format!("    <{sec}>\n        <{tag}>{value_xml}</{tag}>\n    </{sec}>\n");
                return insert_before_root_end(xml, &masked, &add);
            }
        },
        None => (0, masked.len()),
    };
    if let Some((s, e)) = element_inner(&masked, tag, lo, hi) {
        return format!("{}{}{}", &xml[..s], value_xml, &xml[e..]);
    }
    if let Some((s, e)) = element_selfclosing(&masked, tag, lo, hi) {
        return format!("{}<{tag}>{value_xml}</{tag}>{}", &xml[..s], &xml[e..]);
    }
    match section {
        Some(_) => {
            let add = format!("    <{tag}>{value_xml}</{tag}>\n");
            format!("{}{}{}", &xml[..hi], add, &xml[hi..])
        }
        None => insert_before_root_end(xml, &masked, &format!("    <{tag}>{value_xml}</{tag}>\n")),
    }
}

fn insert_before_root_end(xml: &str, masked: &str, add: &str) -> String {
    match masked.rfind("</vdd_settings>") {
        Some(p) => format!("{}{}{}", &xml[..p], add, &xml[p..]),
        None => format!("{xml}{add}"),
    }
}

fn resolutions_xml(modes: &[Mode]) -> String {
    let mut s = String::from("\n");
    for m in modes {
        s.push_str(&format!(
            "        <resolution><width>{}</width><height>{}</height><refresh_rate>{}</refresh_rate></resolution>\n",
            m.width, m.height, m.hz
        ));
    }
    s.push_str("    ");
    s
}

fn is_sane(m: &Mode) -> bool {
    (64..=16_384).contains(&m.width) && (64..=16_384).contains(&m.height) && (1..=1_000).contains(&m.hz)
}

/// De-duplicated, sanity-filtered mode list, capped to [`MAX_MODES`] (order preserved).
#[must_use]
pub fn clean_modes(modes: &[Mode]) -> Vec<Mode> {
    let mut out: Vec<Mode> = Vec::new();
    for m in modes {
        if is_sane(m) && !out.contains(m) && out.len() < MAX_MODES {
            out.push(*m);
        }
    }
    out
}

/// Produces a new settings document from `base` (an existing file or the template):
/// monitor count, render GPU (`None` keeps the current value) and the complete mode list.
///
/// Everything else in `base` (options, comments outside the replaced elements) is preserved.
pub fn render_settings(
    base: &str,
    modes: &[Mode],
    gpu: Option<&str>,
    count: u32,
) -> Result<String, SettingsError> {
    let masked = mask_comments(base);
    if !masked.contains("<vdd_settings>") {
        return Err(SettingsError::NoRoot);
    }
    let modes = clean_modes(modes);
    if modes.is_empty() {
        return Err(SettingsError::NoModes);
    }
    let mut x = set_element(base, Some("monitors"), "count", &count.max(1).to_string());
    if let Some(name) = gpu {
        x = set_element(&x, Some("gpu"), "friendlyname", &xml_escape(name));
    }
    x = set_element(&x, None, "resolutions", &resolutions_xml(&modes));
    Ok(x)
}

/// Modes listed in the settings (fractional rates are rounded).
#[must_use]
pub fn parse_modes(xml: &str) -> Vec<Mode> {
    let masked = mask_comments(xml);
    let Some((lo, hi)) = element_inner(&masked, "resolutions", 0, masked.len()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut at = lo;
    while let Some(p) = masked[at..hi].find("<resolution>") {
        let s = at + p;
        let Some(e) = masked[s..hi].find("</resolution>").map(|e| e + s) else {
            break;
        };
        let get = |tag: &str| -> Option<f64> {
            let (a, b) = element_inner(&masked, tag, s, e)?;
            masked[a..b].trim().parse().ok()
        };
        if let (Some(w), Some(h), Some(r)) = (get("width"), get("height"), get("refresh_rate")) {
            out.push(Mode {
                width: w.round() as u32,
                height: h.round() as u32,
                hz: r.round() as u32,
            });
        }
        at = e + "</resolution>".len();
    }
    out
}

fn text_of(xml: &str, section: &str, tag: &str) -> Option<String> {
    let masked = mask_comments(xml);
    let (lo, hi) = element_inner(&masked, section, 0, masked.len())?;
    let (a, b) = element_inner(&masked, tag, lo, hi)?;
    Some(xml_unescape(xml[a..b].trim()))
}

/// `<gpu><friendlyname>` of the settings.
#[must_use]
pub fn parse_gpu(xml: &str) -> Option<String> {
    text_of(xml, "gpu", "friendlyname")
}

/// `<monitors><count>` of the settings.
#[must_use]
pub fn parse_count(xml: &str) -> Option<u32> {
    text_of(xml, "monitors", "count")?.parse().ok()
}

/// Mode list for enabling `wanted`: keep what is configured, make sure `wanted` is first, add
/// the 60 Hz twin and the safe mode as fallbacks.
#[must_use]
pub fn modes_for_enable(existing: &[Mode], wanted: Mode) -> Vec<Mode> {
    let mut v = vec![wanted];
    if wanted.hz != 60 {
        v.push(Mode { hz: 60, ..wanted });
    }
    v.extend_from_slice(existing);
    v.push(crate::plan::SAFE_MODE);
    clean_modes(&v)
}

// ---------------------------------------------------------------------------------------------
// Pipe protocol

/// Wire format of one pipe command: UTF-16LE text without terminator.
#[must_use]
pub fn pipe_message(cmd: &str) -> Vec<u8> {
    cmd.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

pub const CMD_RELOAD: &str = "RELOAD_DRIVER";

#[must_use]
pub fn cmd_setgpu(name: &str) -> String {
    // The driver splits on the quotes; a quote inside the name would break the command.
    format!("SETGPU \"{}\"", name.replace('"', ""))
}

// ---------------------------------------------------------------------------------------------
// Render GPU selection

/// A DXGI adapter as far as GPU choice is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    /// `DXGI_ADAPTER_DESC.Description` – what the driver matches `<friendlyname>` against.
    pub name: String,
    pub vendor_id: u32,
    pub dedicated_vram: u64,
    /// `DXGI_ADAPTER_FLAG_SOFTWARE` (WARP, Basic Render Driver).
    pub software: bool,
}

pub const VENDOR_NVIDIA: u32 = 0x10DE;
pub const VENDOR_AMD: u32 = 0x1002;
pub const VENDOR_INTEL: u32 = 0x8086;
pub const VENDOR_MICROSOFT: u32 = 0x1414;

impl AdapterInfo {
    /// Microsoft Basic Render Driver / WARP / other software adapters.
    #[must_use]
    pub fn is_software(&self) -> bool {
        self.software
            || self.vendor_id == VENDOR_MICROSOFT
            || self.name.to_ascii_lowercase().contains("basic render")
    }

    /// NVENC (NVIDIA), AMF (AMD) or Quick Sync (Intel) – plan 8.7 pipeline.
    #[must_use]
    pub fn has_hw_encoder(&self) -> bool {
        !self.is_software() && matches!(self.vendor_id, VENDOR_NVIDIA | VENDOR_AMD | VENDOR_INTEL)
    }
}

/// Picks the render adapter: the `hint` (exact name, then substring, case-insensitive) when it
/// matches; otherwise the hardware-encoder adapter with the most dedicated VRAM (the dGPU on an
/// Optimus laptop); otherwise any non-software adapter with the most VRAM. Never a software
/// adapter.
#[must_use]
pub fn choose_render_adapter<'a>(
    adapters: &'a [AdapterInfo],
    hint: Option<&str>,
) -> Option<&'a AdapterInfo> {
    let usable = || adapters.iter().filter(|a| !a.is_software());
    if let Some(h) = hint.map(str::trim).filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("default")) {
        let hl = h.to_ascii_lowercase();
        let exact = usable().find(|a| a.name.to_ascii_lowercase() == hl);
        let sub = || usable().find(|a| a.name.to_ascii_lowercase().contains(&hl));
        if let Some(a) = exact.or_else(sub) {
            return Some(a);
        }
        tracing::warn!(hint = h, "render adapter hint matches no adapter; choosing automatically");
    }
    usable()
        .filter(|a| a.has_hw_encoder())
        .max_by_key(|a| a.dedicated_vram)
        .or_else(|| usable().max_by_key(|a| a.dedicated_vram))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(w: u32, h: u32, hz: u32) -> Mode {
        Mode {
            width: w,
            height: h,
            hz,
        }
    }

    #[test]
    fn template_roundtrip_parses() {
        let modes = parse_modes(DEFAULT_SETTINGS_TEMPLATE);
        assert_eq!(modes.len(), 7, "{modes:?}");
        assert!(modes.contains(&m(5120, 1440, 120)));
        assert_eq!(parse_count(DEFAULT_SETTINGS_TEMPLATE), Some(1));
        assert_eq!(parse_gpu(DEFAULT_SETTINGS_TEMPLATE).as_deref(), Some("default"));
    }

    #[test]
    fn render_replaces_modes_count_and_gpu_but_keeps_the_rest() {
        let want = [m(2560, 1440, 120), m(1920, 1080, 60)];
        let x = render_settings(
            DEFAULT_SETTINGS_TEMPLATE,
            &want,
            Some("NVIDIA GeForce RTX 5060 Laptop GPU"),
            3,
        )
        .unwrap();
        assert_eq!(parse_modes(&x), want);
        assert_eq!(parse_count(&x), Some(3));
        assert_eq!(parse_gpu(&x).as_deref(), Some("NVIDIA GeForce RTX 5060 Laptop GPU"));
        // untouched parts survive
        assert!(x.contains("<HardwareCursor>true</HardwareCursor>"));
        assert!(x.contains("Desk Bridge default configuration"));
        // the header comment mentions <resolution> and <count>: it must not have been edited
        assert!(x.contains("<count> is the number of monitors"));
        // idempotent
        let y = render_settings(&x, &want, Some("NVIDIA GeForce RTX 5060 Laptop GPU"), 3).unwrap();
        assert_eq!(parse_modes(&y), want);
        assert_eq!(x.matches("<resolution>").count(), y.matches("<resolution>").count());
    }

    #[test]
    fn gpu_name_is_escaped_and_restored() {
        let x = render_settings(DEFAULT_SETTINGS_TEMPLATE, &[m(1920, 1080, 60)], Some("A&B <x>"), 1)
            .unwrap();
        assert!(x.contains("A&amp;B &lt;x&gt;"));
        assert_eq!(parse_gpu(&x).as_deref(), Some("A&B <x>"));
    }

    #[test]
    fn gpu_none_keeps_current_value() {
        let x = render_settings(DEFAULT_SETTINGS_TEMPLATE, &[m(1920, 1080, 60)], Some("GPU1"), 1).unwrap();
        let y = render_settings(&x, &[m(1920, 1080, 60)], None, 1).unwrap();
        assert_eq!(parse_gpu(&y).as_deref(), Some("GPU1"));
    }

    #[test]
    fn missing_elements_are_inserted() {
        let bare = "<vdd_settings>\n</vdd_settings>";
        let x = render_settings(bare, &[m(1920, 1080, 60)], Some("G"), 1).unwrap();
        assert_eq!(parse_modes(&x), [m(1920, 1080, 60)]);
        assert_eq!(parse_count(&x), Some(1));
        assert_eq!(parse_gpu(&x).as_deref(), Some("G"));
        // self-closing friendlyname
        let sc = "<vdd_settings><gpu><friendlyname/></gpu></vdd_settings>";
        let y = render_settings(sc, &[m(1920, 1080, 60)], Some("G2"), 1).unwrap();
        assert_eq!(parse_gpu(&y).as_deref(), Some("G2"));
        assert_eq!(y.matches("friendlyname").count(), 2);
    }

    #[test]
    fn commented_out_modes_are_ignored() {
        let xml = "<vdd_settings><resolutions><!-- <resolution><width>1</width><height>1</height><refresh_rate>1</refresh_rate></resolution> -->\
                   <resolution><width>800</width><height>600</height><refresh_rate>59.94</refresh_rate></resolution></resolutions></vdd_settings>";
        assert_eq!(parse_modes(xml), [m(800, 600, 60)]);
    }

    #[test]
    fn errors() {
        assert_eq!(
            render_settings("<foo/>", &[m(1920, 1080, 60)], None, 1),
            Err(SettingsError::NoRoot)
        );
        assert_eq!(
            render_settings(DEFAULT_SETTINGS_TEMPLATE, &[m(0, 0, 0)], None, 1),
            Err(SettingsError::NoModes)
        );
    }

    #[test]
    fn mode_list_for_enable() {
        let existing = [m(5120, 1440, 120), m(2560, 1440, 120)];
        let v = modes_for_enable(&existing, m(5120, 1440, 120));
        assert_eq!(v[0], m(5120, 1440, 120));
        assert!(v.contains(&m(5120, 1440, 60)));
        assert!(v.contains(&m(2560, 1440, 120)));
        assert!(v.contains(&crate::plan::SAFE_MODE));
        let mut d = v.clone();
        d.dedup();
        assert_eq!(d, v);
        // unknown wanted mode is added first
        let w = modes_for_enable(&existing, m(3840, 1600, 144));
        assert_eq!(w[0], m(3840, 1600, 144));
        assert_eq!(w[1], m(3840, 1600, 60));
    }

    #[test]
    fn mode_cap() {
        let many: Vec<Mode> = (0..200).map(|i| m(640 + i, 480, 60)).collect();
        assert_eq!(clean_modes(&many).len(), MAX_MODES);
    }

    #[test]
    fn pipe_encoding() {
        assert_eq!(pipe_message("AB"), [b'A', 0, b'B', 0]);
        assert_eq!(cmd_setgpu("X\"Y"), "SETGPU \"XY\"");
    }

    fn ad(name: &str, vendor: u32, vram_mb: u64, sw: bool) -> AdapterInfo {
        AdapterInfo {
            name: name.into(),
            vendor_id: vendor,
            dedicated_vram: vram_mb << 20,
            software: sw,
        }
    }

    #[test]
    fn adapter_choice_prefers_dgpu_with_encoder() {
        let list = [
            ad("Intel(R) Iris(R) Xe Graphics", VENDOR_INTEL, 128, false),
            ad("NVIDIA GeForce RTX 5060 Laptop GPU", VENDOR_NVIDIA, 8151, false),
            ad("Microsoft Basic Render Driver", VENDOR_MICROSOFT, 0, true),
        ];
        assert_eq!(
            choose_render_adapter(&list, None).unwrap().name,
            "NVIDIA GeForce RTX 5060 Laptop GPU"
        );
        // hint wins (substring, case-insensitive)
        assert_eq!(choose_render_adapter(&list, Some("iris")).unwrap().vendor_id, VENDOR_INTEL);
        // unknown hint falls back to automatic
        assert_eq!(choose_render_adapter(&list, Some("Radeon")).unwrap().vendor_id, VENDOR_NVIDIA);
        // "default" is no hint
        assert_eq!(choose_render_adapter(&list, Some("default")).unwrap().vendor_id, VENDOR_NVIDIA);
        // software adapters are never chosen, even by hint
        assert_eq!(
            choose_render_adapter(&list, Some("Basic Render")).unwrap().vendor_id,
            VENDOR_NVIDIA
        );
    }

    #[test]
    fn adapter_choice_edge_cases() {
        let only_sw = [ad("Microsoft Basic Render Driver", VENDOR_MICROSOFT, 0, true)];
        assert!(choose_render_adapter(&only_sw, None).is_none());
        assert!(choose_render_adapter(&[], None).is_none());
        // an unknown vendor with a lot of VRAM loses against an encoder-capable GPU
        let list = [
            ad("Mystery GPU", 0x1234, 16000, false),
            ad("AMD Radeon 780M", VENDOR_AMD, 512, false),
        ];
        assert_eq!(choose_render_adapter(&list, None).unwrap().name, "AMD Radeon 780M");
        // but is used when it is all there is
        assert_eq!(choose_render_adapter(&list[..1], None).unwrap().name, "Mystery GPU");
    }
}
