//! Transfer planning: turns a list of files/folders into a flat, ordered list of entries.

use crate::sanitize::sanitize_component;
use deskbridge_proto::pb;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// One file or directory to transfer.
#[derive(Debug, Clone)]
pub struct PlannedFile {
    /// Absolute (or at least openable) path on the sending machine.
    pub abs: PathBuf,
    /// Wire description; `rel_path` uses '/' separators and starts with the top-level name.
    pub entry: pb::FileEntry,
}

/// Result of [`plan_with_warnings`].
#[derive(Debug, Default)]
pub struct Plan {
    pub files: Vec<PlannedFile>,
    /// Human readable (German) notes about skipped items.
    pub warnings: Vec<String>,
}

impl Plan {
    /// Sum of all file sizes.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.entry.size).sum()
    }
}

/// Plans the given files and folders; unreadable items are skipped (and logged).
pub fn plan(paths: &[PathBuf]) -> io::Result<Vec<PlannedFile>> {
    let p = plan_with_warnings(paths)?;
    for w in &p.warnings {
        tracing::warn!("{w}");
    }
    Ok(p.files)
}

/// Like [`plan`] but returns the warnings. Fails only if a top-level path does not exist.
///
/// * Folders are walked recursively in name order; directories get their own `is_dir` entry
///   (parents before children) so empty folders survive.
/// * Symbolic links are not followed (loop safety) and reported as skipped.
/// * Top-level names that collide (`a/x.txt` and `b/x.txt`) get a ` (n)` suffix.
pub fn plan_with_warnings(paths: &[PathBuf]) -> io::Result<Plan> {
    let mut plan = Plan::default();
    let mut used: HashSet<String> = HashSet::new();
    for path in paths {
        let meta = std::fs::symlink_metadata(path)?;
        let raw_name = path.file_name().map_or_else(
            || path.to_string_lossy().replace([':', '\\', '/'], "_"),
            |n| n.to_string_lossy().into_owned(),
        );
        let name = unique_name(&mut used, &sanitize_component(&raw_name));
        if meta.file_type().is_symlink() {
            plan.warnings
                .push(format!("Verknüpfung übersprungen: {}", path.display()));
            continue;
        }
        walk(path, &name, &meta, &mut plan);
    }
    Ok(plan)
}

fn unique_name(used: &mut HashSet<String>, name: &str) -> String {
    let key = |s: &str| s.to_lowercase();
    if used.insert(key(name)) {
        return name.to_string();
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p.extension().map(|s| s.to_string_lossy().into_owned());
    for n in 1u32.. {
        let cand = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        if used.insert(key(&cand)) {
            return cand;
        }
    }
    unreachable!()
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| match t.duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).ok(),
            Err(e) => i64::try_from(e.duration().as_millis()).ok().map(|v| -v),
        })
        .unwrap_or(0)
}

fn walk(abs: &Path, rel: &str, meta: &std::fs::Metadata, plan: &mut Plan) {
    if meta.is_dir() {
        plan.files.push(PlannedFile {
            abs: abs.to_path_buf(),
            entry: pb::FileEntry {
                rel_path: rel.to_string(),
                size: 0,
                mtime_unix_ms: mtime_ms(meta),
                is_dir: true,
            },
        });
        let rd = match std::fs::read_dir(abs) {
            Ok(rd) => rd,
            Err(e) => {
                plan.warnings
                    .push(format!("Ordner nicht lesbar: {} ({e})", abs.display()));
                return;
            }
        };
        let mut children: Vec<(String, PathBuf)> = Vec::new();
        for item in rd {
            match item {
                Ok(de) => children.push((de.file_name().to_string_lossy().into_owned(), de.path())),
                Err(e) => plan
                    .warnings
                    .push(format!("Eintrag nicht lesbar in {} ({e})", abs.display())),
            }
        }
        children.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, child) in children {
            let cm = match std::fs::symlink_metadata(&child) {
                Ok(m) => m,
                Err(e) => {
                    plan.warnings
                        .push(format!("Nicht lesbar: {} ({e})", child.display()));
                    continue;
                }
            };
            if cm.file_type().is_symlink() {
                plan.warnings
                    .push(format!("Verknüpfung übersprungen: {}", child.display()));
                continue;
            }
            let crel = format!("{rel}/{}", sanitize_component(&name));
            walk(&child, &crel, &cm, plan);
        }
    } else if meta.is_file() {
        // Probe readability now so a locked file is reported before the transfer starts.
        if let Err(e) = std::fs::File::open(abs) {
            plan.warnings
                .push(format!("Datei nicht lesbar: {} ({e})", abs.display()));
            return;
        }
        plan.files.push(PlannedFile {
            abs: abs.to_path_buf(),
            entry: pb::FileEntry {
                rel_path: rel.to_string(),
                size: meta.len(),
                mtime_unix_ms: mtime_ms(meta),
                is_dir: false,
            },
        });
    } else {
        plan.warnings.push(format!(
            "Kein reguläres Objekt übersprungen: {}",
            abs.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_files_dirs_and_empty_dirs_in_stable_order() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("proj");
        std::fs::create_dir_all(root.join("b/empty")).unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/z.txt"), b"zz").unwrap();
        std::fs::write(root.join("a/y.txt"), b"y").unwrap();
        std::fs::write(root.join("top.bin"), b"").unwrap();
        let single = t.path().join("single.txt");
        std::fs::write(&single, b"hello").unwrap();

        let p = plan(&[root.clone(), single]).unwrap();
        let rels: Vec<_> = p
            .iter()
            .map(|f| (f.entry.rel_path.as_str(), f.entry.is_dir, f.entry.size))
            .collect();
        assert_eq!(
            rels,
            vec![
                ("proj", true, 0),
                ("proj/a", true, 0),
                ("proj/a/y.txt", false, 1),
                ("proj/a/z.txt", false, 2),
                ("proj/b", true, 0),
                ("proj/b/empty", true, 0),
                ("proj/top.bin", false, 0),
                ("single.txt", false, 5),
            ]
        );
        assert!(p.iter().all(|f| f.entry.mtime_unix_ms > 0));
    }

    #[test]
    fn colliding_top_level_names_get_suffix() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("x")).unwrap();
        std::fs::create_dir_all(t.path().join("y")).unwrap();
        std::fs::write(t.path().join("x/f.txt"), b"1").unwrap();
        std::fs::write(t.path().join("y/f.txt"), b"2").unwrap();
        let p = plan(&[t.path().join("x/f.txt"), t.path().join("y/f.txt")]).unwrap();
        assert_eq!(p[0].entry.rel_path, "f.txt");
        assert_eq!(p[1].entry.rel_path, "f (1).txt");
    }

    #[test]
    fn missing_top_level_path_is_an_error() {
        assert!(plan(&[PathBuf::from(r"C:\definitely\not\here\x.txt")]).is_err());
    }
}
