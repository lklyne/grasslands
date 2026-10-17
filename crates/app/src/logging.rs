//! Logging: `tracing` to stderr and to a daily file under
//! `~/Library/Logs/grasslands/`, filtered by `RUST_LOG`.
//!
//! `log` records (herdr-client, GPUI, `detach_and_log_err`) are bridged in,
//! so everything lands in the same two places.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Used when `RUST_LOG` is unset.
const DEFAULT_FILTER: &str = "warn,app=info,herdr_client=info";
const KEEP_FILES: usize = 7;

/// Installs the subscriber and a panic hook that logs panics. Keep the
/// returned guard alive for the whole run: dropping it flushes the file.
pub fn init() -> Option<WorkerGuard> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_file(true)
        .with_line_number(true);
    let (file, guard, file_error) = match file_writer() {
        Ok((writer, guard)) => {
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false);
            (Some(layer), Some(guard), None)
        }
        Err(error) => (None, None, Some(error)),
    };
    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file)
        .try_init();
    if let Err(error) = installed {
        eprintln!("logging: not installed: {error}");
        return None;
    }
    if let Some(error) = file_error {
        tracing::warn!("logging to stderr only: {error}");
    }
    install_panic_hook();
    guard
}

pub fn log_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Logs/grasslands"))
}

fn file_writer() -> anyhow::Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    let dir = log_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    std::fs::create_dir_all(&dir)?;
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("grasslands")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(&dir)?;
    Ok(tracing_appender::non_blocking(appender))
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default(info);
    }));
}
