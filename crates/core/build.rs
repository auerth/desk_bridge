//! Build version (`brand::VERSION`): the package version with the git commit in front of the
//! pre-release part, so two devices can tell builds of the same version number apart:
//! `0.1.8-beta` → `0.1.8-1a2b3c4-beta`, `0.1.8` → `0.1.8-1a2b3c4`, and `-dirty` after the hash
//! when the working tree had uncommitted changes (`0.1.8-1a2b3c4-dirty-beta`).
//! `DESKBRIDGE_GIT_HASH` overrides the commit (builds without a git checkout).

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let package = std::env::var("CARGO_PKG_VERSION").unwrap();
    println!("cargo:rerun-if-env-changed=DESKBRIDGE_GIT_HASH");

    let build = match std::env::var("DESKBRIDGE_GIT_HASH") {
        Ok(h) if !h.trim().is_empty() => Some(h.trim().to_string()),
        _ => git(&manifest, &["rev-parse", "--short=7", "HEAD"]).map(|hash| {
            if let Some(git_dir) = git(&manifest, &["rev-parse", "--absolute-git-dir"]) {
                let git_dir = PathBuf::from(git_dir);
                for f in ["HEAD", "index", "packed-refs", "refs"] {
                    println!("cargo:rerun-if-changed={}", git_dir.join(f).display());
                }
            }
            // The dirty flag follows every edit of the sources, not only of this crate.
            let root = manifest.join("../..");
            for d in ["crates", "app/src-tauri/src", "Cargo.toml"] {
                println!("cargo:rerun-if-changed={}", root.join(d).display());
            }
            let dirty = git(&manifest, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            if dirty { format!("{hash}-dirty") } else { hash }
        }),
    };

    let version = match (build, package.split_once('-')) {
        (None, _) => package.clone(),
        (Some(b), Some((core, pre))) => format!("{core}-{b}-{pre}"),
        (Some(b), None) => format!("{package}-{b}"),
    };
    println!("cargo:rustc-env=DESKBRIDGE_BUILD_VERSION={version}");
}
