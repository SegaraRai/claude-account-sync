//! A minimal [`log`] backend that writes timestamped lines to standard error or to a file.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;

/// A log file larger than this many bytes is renamed to `<name>.old` when logging starts.
const MAX_LOG_LEN: u64 = 5 << 20;

struct Logger {
    output: Mutex<Box<dyn Write + Send>>,
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        if let Ok(mut output) = self.output.lock() {
            let _ = writeln!(output, "{now} {:5} {}", record.level(), record.args());
            let _ = output.flush();
        }
    }

    fn flush(&self) {}
}

/// Sends log lines to standard error.
pub fn init_stderr() {
    install(Box::new(io::stderr()));
}

/// Sends log lines to the file at `path`, and keeps one older file when the current one grows too large.
pub fn init_file(path: &Path) -> io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_LEN) {
        std::fs::rename(path, path.with_extension("log.old"))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file: File = OpenOptions::new().create(true).append(true).open(path)?;
    install(Box::new(file));
    Ok(())
}

fn install(output: Box<dyn Write + Send>) {
    let logger = Box::leak(Box::new(Logger {
        output: Mutex::new(output),
    }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
