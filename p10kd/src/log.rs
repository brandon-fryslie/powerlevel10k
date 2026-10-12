//! One line per event on stderr, which the daemon points at its log file. Timestamps are
//! unix seconds: cheap, sortable, and `date -r` turns them back into clock time.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn line(msg: std::fmt::Arguments<'_>) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    eprintln!("{secs} [{}] {msg}", std::process::id());
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::line(format_args!($($arg)*)) };
}
