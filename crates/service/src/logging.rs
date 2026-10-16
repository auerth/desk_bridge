//! Logging for the service processes: `%ProgramData%\softwelop\DeskBridge\logs\<prefix>.log`
//! (daily rolling, 14 days kept) plus stderr for console runs.

use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub const KEEP_DAYS: u64 = 14;

#[must_use]
pub fn logs_dir() -> PathBuf {
    crate::winutil::program_data_dir().join("logs")
}

/// Keep alive for the process lifetime (flushes on drop).
pub struct LogGuard(#[allow(dead_code)] Option<WorkerGuard>);

/// `to_file`: write `<prefix>.log.<date>` in [`logs_dir`]; `to_stderr`: mirror to stderr.
/// `RUST_LOG` overrides the default `info` filter. Failure to open the file is not fatal.
#[must_use]
pub fn init(prefix: &str, to_file: bool, to_stderr: bool) -> LogGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (file_layer, guard) = if to_file {
        let dir = logs_dir();
        match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                prune(&dir, prefix, KEEP_DAYS);
                let appender = tracing_appender::rolling::daily(&dir, format!("{prefix}.log"));
                let (nb, guard) = tracing_appender::non_blocking(appender);
                (
                    Some(fmt::layer().with_ansi(false).with_writer(nb)),
                    Some(guard),
                )
            }
            Err(e) => {
                eprintln!("cannot create log directory {}: {e}", dir.display());
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    let stderr_layer =
        to_stderr.then(|| fmt::layer().with_writer(std::io::stderr).with_target(false));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init();
    LogGuard(guard)
}

/// Deletes rolled files `<prefix>.log.*` older than `days`.
fn prune(dir: &Path, prefix: &str, days: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    let stem = format!("{prefix}.log.");
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().starts_with(&stem) {
            continue;
        }
        if e.metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|m| m < cutoff)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
