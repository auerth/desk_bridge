//! Logging to stderr and to `setup.log` in `%ProgramData%` and `%LOCALAPPDATA%` (`softwelop\DeskBridge\logs`).

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;

use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Install the global subscriber. Returns the log file path if the file could be opened.
///
/// `quiet` raises the stderr threshold to warnings; the file always receives `info` (or whatever
/// `RUST_LOG` says).
pub fn init(quiet: bool) -> Option<PathBuf> {
    let stderr_level = if quiet {
        LevelFilter::WARN
    } else {
        LevelFilter::INFO
    };
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .with_filter(stderr_level);

    // The same log goes to the user's profile and to %ProgramData% (the installer may run under
    // an admin account whose profile is not the user's).
    let open = |path: Option<PathBuf>| {
        let path = path?;
        std::fs::create_dir_all(path.parent()?).ok()?;
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        Some((path, f))
    };
    let mut files: Vec<(PathBuf, std::fs::File)> = Vec::new();
    files.extend(open(crate::paths::machine_log_file()));
    files.extend(open(crate::paths::setup_log_file()));
    let file = if files.is_empty() {
        None
    } else {
        let primary = files[0].0.clone();
        Some((primary, Tee(files.into_iter().map(|(_, f)| f).collect())))
    };

    match file {
        Some((path, f)) => {
            let env = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
            let file_layer = fmt::layer()
                .with_ansi(false)
                .with_writer(Mutex::new(f))
                .with_filter(env);
            let _ = tracing_subscriber::registry()
                .with(stderr_layer)
                .with(file_layer)
                .try_init();
            Some(path)
        }
        None => {
            let _ = tracing_subscriber::registry().with(stderr_layer).try_init();
            None
        }
    }
}

/// Writes every line to all log files.
struct Tee(Vec<std::fs::File>);

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for f in &mut self.0 {
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        for f in &mut self.0 {
            let _ = f.flush();
        }
        Ok(())
    }
}
