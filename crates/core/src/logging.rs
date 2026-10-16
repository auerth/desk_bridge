//! `tracing` setup: rolling daily log files (7 days kept) plus stderr.

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Keep this alive for the lifetime of the process (flushes on drop).
pub struct LogGuard {
    _guard: WorkerGuard,
}

/// Initialises logging. `level` is one of error/warn/info/debug/trace; `RUST_LOG` overrides it.
pub fn init(logs_dir: &Path, file_prefix: &str, level: &str) -> LogGuard {
    let _ = std::fs::create_dir_all(logs_dir);
    prune_old_logs(logs_dir, file_prefix, 7);
    let appender = tracing_appender::rolling::daily(logs_dir, format!("{file_prefix}.log"));
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("{level},quinn=warn,rustls=warn,mdns_sd=warn")));
    let file_layer = fmt::layer().with_ansi(false).with_writer(non_blocking).with_target(true);
    let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_target(false);
    let _ = tracing_subscriber::registry().with(filter).with(file_layer).with(stderr_layer).try_init();
    LogGuard { _guard: guard }
}

/// Deletes rolled log files older than `days`.
pub fn prune_old_logs(dir: &Path, prefix: &str, days: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with(prefix) {
            continue;
        }
        if let Ok(modified) = e.metadata().and_then(|m| m.modified()) {
            if modified < cutoff {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Installs a panic hook that writes a crash report next to the logs (never uploaded).
pub fn install_panic_hook(logs_dir: &Path) {
    let dir = logs_dir.to_path_buf();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let bt = std::backtrace::Backtrace::force_capture();
        let report = format!("Desk Bridge crash report\nversion: {}\ntime(unix): {ts}\n{info}\n\n{bt}\n", crate::brand::VERSION);
        let _ = std::fs::write(dir.join(format!("crash-{ts}.txt")), report);
        prev(info);
    }));
}
