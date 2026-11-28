//! `crash.log`, beside the settings file: what ended the app, and what was reported just before.
//!
//! Three kinds of line, so that an app that ends without being asked to leaves a line here
//! wherever that can be done:
//! - a panic, on any thread: its message and backtrace, from the panic hook installed first thing
//!   in `main`, before any other thread starts;
//! - an error or a warning the window library or the UI framework reports through the `log`
//!   crate (the window library's own reports reach it too): the UI framework ends the app on an
//!   error from drawing a frame with no panic at all, and says why only there. At most
//!   [`MAX_LINES`] lines a run, a line repeated being counted, not written again;
//! - a close nobody asked for: `on_exit` runs on the UI framework's way out, Quit or not, and
//!   says so when no Quit or update closed the app.
//!
//! What stays out of reach: an abort (a panic inside a panic, a stack overflow), a crash in
//! native code (a graphics driver), and the process being ended from outside.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Lines from the `log` crate one run writes at most.
pub const MAX_LINES: u32 = 100;

pub fn path() -> PathBuf {
    crate::settings::Settings::path().with_file_name("crash.log")
}

/// Append `text` to the log at `path`. A log that cannot be written is skipped.
pub fn append_at(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Write panics to `crash.log`; release builds have no console to show them.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        append_at(&path(), &format!("{}\n{info}\n{backtrace}\n\n", now()));
        default(info);
    }));
}

/// Hand the `log` crate's errors and warnings to `crash.log`, from now on.
pub fn install_logger() {
    static LOGGER: Logger = Logger { limiter: Mutex::new(Limiter::new()) };
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

/// What the `log` crate reports, as written: when, how bad, from where.
pub fn log_line(time: &str, level: log::Level, target: &str, message: &str) -> String {
    let what = if level == log::Level::Error { "error" } else { "warning" };
    format!("{time} {what} from {target}: {message}\n")
}

/// The line `on_exit` writes when nobody asked the app to close: `closing` is what asked (Quit,
/// an update), None when nothing did.
pub fn unexpected_exit_line(time: &str, closing: Option<&str>) -> Option<String> {
    closing.is_none().then(|| format!("{time} closed without Quit or an update: the UI framework ended the app (an error it reported, if any, is the line before)\n\n"))
}

/// Which lines from the `log` crate are written: a line the same as the one before is counted and
/// said once the next different line comes; after [`MAX_LINES`] nothing more is written but one
/// line saying so.
#[derive(Debug)]
pub struct Limiter {
    last: Option<String>,
    repeats: u32,
    written: u32,
}

impl Limiter {
    pub const fn new() -> Self {
        Self { last: None, repeats: 0, written: 0 }
    }

    /// The text to write for `message` (`line` being how it is written), or None.
    pub fn admit(&mut self, message: &str, line: String) -> Option<String> {
        if self.last.as_deref() == Some(message) {
            self.repeats += 1;
            return None;
        }
        if self.written > MAX_LINES {
            return None;
        }
        self.written += 1;
        if self.written > MAX_LINES {
            return Some(format!("({MAX_LINES} lines written this run; the rest are left out)\n"));
        }
        let mut text = String::new();
        if self.repeats > 0 {
            text.push_str(&format!("(the line before came {} more times)\n", self.repeats));
        }
        text.push_str(&line);
        self.last = Some(message.to_string());
        self.repeats = 0;
        Some(text)
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Logger {
    limiter: Mutex<Limiter>,
}

impl Logger {
    /// Write `record` to the log at `path`, as the limiter allows.
    pub fn write_to(&self, path: &Path, record: &log::Record) {
        if !log::Log::enabled(self, record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        let line = log_line(&now(), record.level(), record.target(), &message);
        let text = match self.limiter.lock() {
            Ok(mut l) => l.admit(&format!("{} {message}", record.target()), line),
            Err(_) => None,
        };
        if let Some(text) = text {
            append_at(path, &text);
        }
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        self.write_to(&path(), record);
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_line_is_counted_and_the_lines_of_a_run_are_bounded() {
        let mut l = Limiter::new();
        assert_eq!(l.admit("a", "A\n".into()).as_deref(), Some("A\n"));
        assert_eq!(l.admit("a", "A\n".into()), None, "the same line again: counted");
        assert_eq!(l.admit("a", "A\n".into()), None);
        assert_eq!(l.admit("b", "B\n".into()).as_deref(), Some("(the line before came 2 more times)\nB\n"));
        for i in 2..MAX_LINES {
            assert!(l.admit(&i.to_string(), format!("{i}\n")).is_some(), "line {i}");
        }
        assert_eq!(l.admit("x", "X\n".into()).as_deref(), Some("(100 lines written this run; the rest are left out)\n"));
        assert_eq!(l.admit("y", "Y\n".into()), None, "nothing after that");
    }

    #[test]
    fn the_log_crate_reports_errors_and_warnings_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("crash.log");
        let logger = Logger { limiter: Mutex::new(Limiter::new()) };
        for (level, text) in [(log::Level::Error, "Exiting because of error: the frame could not be shown"), (log::Level::Warn, "a warning"), (log::Level::Info, "news"), (log::Level::Debug, "detail")] {
            logger.write_to(&file, &log::Record::builder().level(level).target("ui::native::run").args(format_args!("{text}")).build());
        }
        let written = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2, "{written}");
        assert!(lines[0].ends_with(" error from ui::native::run: Exiting because of error: the frame could not be shown"), "{}", lines[0]);
        assert!(lines[1].ends_with(" warning from ui::native::run: a warning"), "{}", lines[1]);
    }

    /// The window library reports through `tracing`, which hands its events to the `log` crate
    /// when nothing else takes them: its warning for a monitor that had gone reaches `crash.log`.
    #[test]
    fn a_warning_of_the_window_library_reaches_the_crash_log() {
        install_logger();
        let marker = format!("monitor handle no longer valid (test {})", std::process::id());
        tracing::warn!(target: "window_library::monitor", "{marker}; the primary monitor is used instead");
        let written = std::fs::read_to_string(path()).unwrap_or_default();
        assert!(written.lines().any(|l| l.ends_with(&format!("warning from window_library::monitor: {marker}; the primary monitor is used instead"))), "{written}");
    }

    #[test]
    fn a_close_nobody_asked_for_leaves_a_line_and_quit_leaves_none() {
        assert_eq!(unexpected_exit_line("t", Some("quit")), None);
        assert_eq!(unexpected_exit_line("t", Some("update")), None);
        assert_eq!(
            unexpected_exit_line("t", None).as_deref(),
            Some("t closed without Quit or an update: the UI framework ended the app (an error it reported, if any, is the line before)\n\n")
        );
    }
}
