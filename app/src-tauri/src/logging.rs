//! File logging for the UI process (`%LOCALAPPDATA%\softwelop\DeskBridge\logs\ui.<date>.log`, 7 days).

use crate::brand;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

pub fn init() -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let dir = directories::BaseDirs::new().map(|d| {
        d.data_local_dir()
            .join(brand::COMPANY_DIR)
            .join(brand::APP_DIR)
            .join("logs")
    });

    let file = dir.and_then(|dir| {
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("ui")
            .filename_suffix("log")
            .max_log_files(7)
            .build(dir)
            .ok()
    });

    match file {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_ansi(false)
                .try_init();
            Some(guard)
        }
        None => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
            None
        }
    }
}
