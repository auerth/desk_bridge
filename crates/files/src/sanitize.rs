//! Path hardening for received relative paths.
//!
//! The peer is authenticated, but a compromised or buggy peer must never be able to write outside
//! the target folder or create names Windows treats specially. Validation is purely lexical.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("leerer Pfad")]
    Empty,
    #[error("absoluter Pfad")]
    Absolute,
    #[error("Pfad enthält '..' oder '.'")]
    Traversal,
    #[error("leere Pfadkomponente")]
    EmptyComponent,
    #[error("NUL-Zeichen im Pfad")]
    Nul,
    #[error("unzulässiges Zeichen {0:?}")]
    InvalidChar(char),
    #[error("Name endet auf Punkt oder Leerzeichen")]
    TrailingDotOrSpace,
    #[error("reservierter Windows-Name: {0}")]
    Reserved(String),
    #[error("Pfadkomponente zu lang")]
    TooLong,
}

const MAX_COMPONENT_UTF16: usize = 255;

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn is_forbidden_char(c: char) -> bool {
    (c as u32) < 0x20
        || c == '\u{7f}'
        || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        || is_bidi_control(c)
}

/// True for DOS device names (also with extension or trailing spaces: `CON.txt`, `nul .x`).
fn is_reserved_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let chars: Vec<char> = stem.chars().collect();
    chars.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(chars[3], '0'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}')
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn validate_component(c: &str) -> Result<(), PathError> {
    if c.is_empty() {
        return Err(PathError::EmptyComponent);
    }
    if c == "." || c == ".." {
        return Err(PathError::Traversal);
    }
    if c.contains('\0') {
        return Err(PathError::Nul);
    }
    if let Some(bad) = c.chars().find(|&ch| is_forbidden_char(ch)) {
        return Err(PathError::InvalidChar(bad));
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Err(PathError::TrailingDotOrSpace);
    }
    if is_reserved_name(c) {
        return Err(PathError::Reserved(c.to_string()));
    }
    if utf16_len(c) > MAX_COMPONENT_UTF16 {
        return Err(PathError::TooLong);
    }
    Ok(())
}

/// Joins a '/'-separated relative path onto `root`, rejecting everything that could escape it.
///
/// Rejected: empty paths, absolute paths (`/x`, `\x`, `C:\x`, `\\server\share`), `.`/`..`,
/// empty components (`a//b`, trailing `/`), NUL and control characters, backslashes, alternate
/// data streams and drive letters (`:`), trailing dots/spaces, reserved device names and
/// over-long components.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, PathError> {
    if rel.is_empty() {
        return Err(PathError::Empty);
    }
    if rel.contains('\0') {
        return Err(PathError::Nul);
    }
    if rel.starts_with('/') || rel.starts_with('\\') {
        return Err(PathError::Absolute);
    }
    // "C:..." or "C:\..." style drive prefix
    let b = rel.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Err(PathError::Absolute);
    }
    let mut out = root.to_path_buf();
    for comp in rel.split('/') {
        validate_component(comp)?;
        out.push(comp);
    }
    Ok(out)
}

/// Maps a single file name to something Windows accepts: illegal characters become `_`,
/// trailing dots/spaces are replaced, reserved names get a `_` prefix. Valid names are returned
/// unchanged (idempotent).
#[must_use]
pub fn sanitize_component(s: &str) -> String {
    if s == "." || s == ".." {
        return "_".to_string();
    }
    let mut out: String = s
        .chars()
        .map(|c| if is_forbidden_char(c) { '_' } else { c })
        .collect();
    // trailing dots and spaces
    let trimmed_len = out.trim_end_matches(['.', ' ']).len();
    let pad = out.len() - trimmed_len;
    if pad > 0 {
        out.truncate(trimmed_len);
        out.push_str(&"_".repeat(pad));
    }
    if out.is_empty() || out == "." || out == ".." {
        return "_".to_string();
    }
    if is_reserved_name(&out) {
        out.insert(0, '_');
    }
    while utf16_len(&out) > MAX_COMPONENT_UTF16 {
        out.pop();
    }
    if out.ends_with('.') || out.ends_with(' ') {
        out.push('_');
    }
    out
}

/// Returns `path` itself if it does not exist, else `stem (1).ext`, `stem (2).ext`, ...
#[must_use]
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    unique_candidates(path)
        .find(|p| !p.exists())
        .expect("unbounded iterator")
}

/// Infinite iterator over `stem (n).ext` candidates, n = 1, 2, ...
pub(crate) fn unique_candidates(path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned());
    (1u32..).map(move |n| {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        path.with_file_name(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(r"C:\dest")
    }

    #[test]
    fn accepts_normal_paths() {
        for ok in [
            "a.txt",
            "dir/a.txt",
            "dir/sub dir/file.tar.gz",
            "Überraschung/日本語.txt",
            "emoji 😀/ok.txt",
            ".hidden",
            "a b",
            "COM10.txt",
            "CONSOLE",
            "nulled",
            "LPT",
            "x/auxiliary.txt",
        ] {
            let p = safe_join(&root(), ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert!(p.starts_with(root()));
        }
    }

    #[test]
    fn rejects_attack_corpus() {
        let bad = [
            "",
            "/",
            "/etc/passwd",
            "\\windows\\system32",
            "C:\\Windows\\x",
            "C:/Windows/x",
            "c:x",
            "\\\\server\\share\\x",
            "//server/share/x",
            "..",
            "../x",
            "a/../../x",
            "a/..",
            "a/./b",
            ".",
            "a//b",
            "a/",
            "a/b/",
            "..\\x",
            "a\\..\\x",
            "a\\b",
            "a\0b",
            "a/\0",
            "file.txt:stream",
            "file.txt::$DATA",
            "dir/file:ads:$DATA",
            "a/b:c",
            "trailing.",
            "trailing ",
            "dir./x",
            "dir /x",
            "CON",
            "con",
            "PRN",
            "AUX",
            "NUL",
            "NUL.txt",
            "dir/nul.tar.gz",
            "COM1",
            "com9.txt",
            "LPT1",
            "lpt9.log",
            "COM0",
            "CON .txt",
            "CONIN$",
            "CONOUT$",
            "COM\u{00B9}",
            "a<b",
            "a>b",
            "a|b",
            "a?b",
            "a*b",
            "a\"b",
            "a\tb",
            "a\nb",
            "a\u{202E}txt.exe",
        ];
        for b in bad {
            assert!(safe_join(&root(), b).is_err(), "should reject {b:?}");
        }
        let long = "x".repeat(300);
        assert_eq!(safe_join(&root(), &long), Err(PathError::TooLong));
    }

    #[test]
    fn result_never_escapes_root() {
        for ok in ["a", "a/b/c", "ü/ö"] {
            let p = safe_join(&root(), ok).unwrap();
            assert!(p.starts_with(root()));
            assert!(
                !p.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            );
        }
    }

    #[test]
    fn sanitize_maps_illegal_characters() {
        assert_eq!(sanitize_component("a:b"), "a_b");
        assert_eq!(sanitize_component("a<b>c|d?e*f\"g"), "a_b_c_d_e_f_g");
        assert_eq!(sanitize_component("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_component("name."), "name_");
        assert_eq!(sanitize_component("name  "), "name__");
        assert_eq!(sanitize_component(""), "_");
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_component("a\0b"), "a_b");
        assert_eq!(sanitize_component("ok name.txt"), "ok name.txt");
        assert_eq!(sanitize_component("日本語.txt"), "日本語.txt");
    }

    #[test]
    fn sanitize_output_always_validates() {
        for s in [
            "a:b",
            "CON",
            "nul.txt",
            "x.",
            "  ",
            "..",
            ".",
            "a\u{202E}b",
            "COM1.x",
            "😀:😀",
        ] {
            let c = sanitize_component(s);
            assert!(validate_component(&c).is_ok(), "{s:?} -> {c:?}");
        }
        let long = "é".repeat(400);
        assert!(validate_component(&sanitize_component(&long)).is_ok());
    }

    #[test]
    fn unique_candidates_format() {
        let p = PathBuf::from(r"C:\x\file.tar.gz");
        let v: Vec<_> = unique_candidates(&p).take(2).collect();
        assert_eq!(v[0], PathBuf::from(r"C:\x\file.tar (1).gz"));
        assert_eq!(v[1], PathBuf::from(r"C:\x\file.tar (2).gz"));
        let p = PathBuf::from(r"C:\x\noext");
        assert_eq!(
            unique_candidates(&p).next().unwrap(),
            PathBuf::from(r"C:\x\noext (1)")
        );
    }
}
