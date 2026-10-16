//! Copies the FFmpeg DLLs (`vendor/ffmpeg/bin/*.dll`) next to the built executable so the spike
//! runs without touching PATH. The installer does the same for the real application.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let bin = manifest.join("../../vendor/ffmpeg/bin");
    println!("cargo:rerun-if-changed={}", bin.display());
    let Ok(out) = std::env::var("OUT_DIR") else { return };
    // OUT_DIR = target/<profile>/build/<pkg>-<hash>/out  ->  target/<profile>
    let profile_dir = PathBuf::from(out).ancestors().nth(3).map(PathBuf::from);
    let Some(profile_dir) = profile_dir else { return };
    let Ok(rd) = std::fs::read_dir(&bin) else {
        println!("cargo:warning=vendor/ffmpeg/bin not found; FFmpeg DLLs must be on PATH");
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")) {
            let dst = profile_dir.join(p.file_name().expect("file name"));
            let same = std::fs::metadata(&dst).ok().zip(std::fs::metadata(&p).ok()).is_some_and(|(a, b)| a.len() == b.len());
            if !same {
                let _ = std::fs::copy(&p, &dst);
            }
        }
    }
}
