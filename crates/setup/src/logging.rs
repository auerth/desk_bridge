//! Logging to stderr and to `%LOCALAPPDATA%\softwelop\DeskBridge\logs\setup.log`.

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

    let file = crate::paths::setup_log_file().and_then(|path| {
        std::fs::create_dir_all(path.parent()?).ok()?;
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        Some((path, f))
    });

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
