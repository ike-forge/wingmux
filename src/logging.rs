//! File logging.
//!
//! The TUI owns stdout/stderr, so *all* diagnostics go to
//! `$XDG_STATE_HOME/wingmux/wingmux.log`. Nothing is ever written to
//! stdout; startup failures are reported on stderr *before* the TUI takes
//! over the terminal.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// A [`log::Log`] implementation that appends timestamped lines to a file.
struct FileLogger {
    file: Mutex<File>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} [{}] {}\n",
            chrono_now(),
            record.level().to_string(),
            record.target(),
            record.args()
        );
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

/// Timestamp without pulling in chrono: local RFC3339-ish `YYYY-MM-DD HH:MM:SS`.
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days since epoch → civil date (Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Initialise the file logger. Returns an error if the log file cannot be
/// created (caller reports it on stderr and continues without logging).
pub fn init() -> std::io::Result<()> {
    let dir = crate::util::state_dir();
    std::fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::util::log_path())?;
    let logger = FileLogger {
        file: Mutex::new(file),
    };
    log::set_boxed_logger(Box::new(logger))
        .map_err(|e| std::io::Error::other(format!("logger already set: {e}")))?;
    log::set_max_level(LevelFilter::Info);
    log::info!("=== wingmux log session started ===");
    let _ = Level::Info; // keep Level import used
    Ok(())
}
