//! `crash.log`, beside the settings file: what ended the app, and what was reported just before.
//!
//! Three kinds of line, so that an app that ends without being asked to leaves a line here
//! wherever that can be done:
//! - a panic, on any thread: its message and backtrace, from the panic hook installed first thing
//!   in `main`, before any other thread starts. A panic inside a keyboard or mouse hook callback
//!   is one short line instead (no backtrace), once per hook a run: Windows removes a hook that
//!   answers too slowly, and a callback that panics on every event must not write for each;
//! - an error any library reports through the `log` crate, and a warning from the UI framework or
//!   the window library (whose own reports reach `log` too): the UI framework ends the app on an
//!   error from drawing a frame with no panic at all, and says why only there. At most
//!   [`MAX_LINES`] lines a run; a line that comes again is counted, not written again, and the
//!   counts are written when the app closes;
//! - a close nobody asked for: `on_exit` runs on the UI framework's way out, Quit or not, and
//!   when no Quit or update closed the app says so, with the age of the last close request to its
//!   window and the last error reported, if there were any; it names no cause.
//!
//! What stays out of reach: an abort (a panic inside a panic, a stack overflow), a crash in
//! native code (a graphics driver), and the process being ended from outside.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Lines from the `log` crate one run writes at most.
pub const MAX_LINES: u32 = 100;

/// How many different lines are remembered for counting repeats.
const REMEMBERED: usize = 8;

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

pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// A low-level hook callback, for a panic inside one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    Keyboard = 0,
    Mouse = 1,
}

impl HookKind {
    fn name(self) -> &'static str {
        match self {
            HookKind::Keyboard => "keyboard",
            HookKind::Mouse => "mouse",
        }
    }
}

thread_local! {
    /// Set while this thread runs a hook callback (see [`in_hook`]).
    static IN_HOOK: Cell<Option<HookKind>> = const { Cell::new(None) };
}

/// A panic inside each kind of hook callback was written this run.
static HOOK_PANIC_WRITTEN: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

/// Run a hook callback of `kind`, marked so that a panic in it is cheap (see [`panic_text`]). The
/// mark goes when the callback ends, by a panic too.
pub fn in_hook<R>(kind: HookKind, f: impl FnOnce() -> R) -> R {
    struct Unmark(Option<HookKind>);
    impl Drop for Unmark {
        fn drop(&mut self) {
            IN_HOOK.with(|c| c.set(self.0));
        }
    }
    let _unmark = Unmark(IN_HOOK.with(|c| c.replace(Some(kind))));
    f()
}

/// The hook callback this thread is in, if any.
pub fn current_hook() -> Option<HookKind> {
    IN_HOOK.with(Cell::get)
}

/// What a panic writes: in a hook callback (`in_hook`), one short line with the place and the
/// message, only for the first panic of that kind this run (`first`), the backtrace never taken;
/// elsewhere the panic (`info`) and its backtrace.
pub fn panic_text(in_hook: Option<HookKind>, first: bool, time: &str, info: &str, location: &str, message: &str, backtrace: impl FnOnce() -> String) -> Option<String> {
    match in_hook {
        Some(kind) => first.then(|| format!("{time} panic in the {} hook at {location}: {message} (the event was passed on; later panics in this hook are not written)\n\n", kind.name())),
        None => Some(format!("{time}\n{info}\n{}\n\n", backtrace())),
    }
}

fn panic_message(info: &std::panic::PanicHookInfo) -> String {
    let payload = info.payload();
    payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "(no message)".into())
}

/// Write panics to `crash.log`; release builds have no console to show them.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let hook = current_hook();
        let first = hook.is_some_and(|k| !HOOK_PANIC_WRITTEN[k as usize].swap(true, Ordering::Relaxed));
        // In a hook callback nothing else is done: the hook returns at once and lets the event pass.
        if hook.is_some() && !first {
            return;
        }
        let location = info.location().map(|l| l.to_string()).unwrap_or_default();
        let text = panic_text(hook, first, &now(), &info.to_string(), &location, &panic_message(info), || std::backtrace::Backtrace::force_capture().to_string());
        if let Some(text) = text {
            append_at(&path(), &text);
        }
        if hook.is_none() {
            default(info);
        }
    }));
}

static LOGGER: Logger = Logger::new();

/// Hand the `log` crate's errors and warnings to `crash.log`, from now on.
pub fn install_logger() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

/// When the app closes: the counts of lines that came again and were not written.
pub fn flush_repeats() {
    let text = LOGGER.limiter.lock().map(|mut l| l.pending()).unwrap_or_default();
    if !text.is_empty() {
        append_at(&path(), &text);
    }
}

/// Whose warnings are written: the UI framework's and the window library's. Errors are written
/// from every library; other warnings (a capture library's routine ones, on every display mode
/// switch) are not.
pub fn warning_wanted(target: &str) -> bool {
    ["eframe", "egui", "winit"].iter().any(|t| target == *t || target.starts_with(&format!("{t}::")) || target.starts_with(&format!("{t}_")))
}

/// What a line from the `log` crate is written as: when, how bad, from where.
pub fn log_line(time: &str, level: log::Level, target: &str, message: &str) -> String {
    let what = if level == log::Level::Error { "error" } else { "warning" };
    format!("{time} {what} from {target}: {message}\n")
}

/// The line `on_exit` writes when no Quit or update closed the app (`asked`), None when one did.
/// It asserts no cause: it gives how long before the last close request to the app's window came
/// (`close_request_secs`, if one came this run), and the last error reported through the `log`
/// crate (`last_error`, if any).
pub fn unexpected_exit_line(time: &str, asked: bool, close_request_secs: Option<u64>, last_error: Option<&str>) -> Option<String> {
    if asked {
        return None;
    }
    let mut line = format!("{time} closed without Quit or an update");
    if let Some(s) = close_request_secs {
        line.push_str(&format!("; the last close request to its window came {s} s before"));
    }
    if let Some(e) = last_error {
        line.push_str(&format!("; the last error reported: {e}"));
    }
    line.push_str("\n\n");
    Some(line)
}

/// The last error reported through the `log` crate this run, as written (without its time).
pub fn last_error() -> Option<String> {
    LOGGER.last_error()
}

/// Which lines from the `log` crate are written: a line that is one of the last [`REMEMBERED`]
/// different ones (two that alternate included) is counted instead, its count written when it is
/// forgotten or the app closes; after [`MAX_LINES`] nothing more is written but one line saying so.
#[derive(Debug)]
pub struct Limiter {
    recent: Vec<(String, u32)>,
    written: u32,
}

impl Limiter {
    pub const fn new() -> Self {
        Self { recent: Vec::new(), written: 0 }
    }

    fn repeats(key: &str, n: u32) -> String {
        format!("(\"{key}\" came {n} more times)\n")
    }

    /// The text to write for `key` (`line` being how it is written), or None.
    pub fn admit(&mut self, key: &str, line: String) -> Option<String> {
        if let Some((_, n)) = self.recent.iter_mut().find(|(k, _)| k == key) {
            *n += 1;
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
        if self.recent.len() >= REMEMBERED {
            let (old, n) = self.recent.remove(0);
            if n > 0 {
                text.push_str(&Self::repeats(&old, n));
            }
        }
        text.push_str(&line);
        self.recent.push((key.to_string(), 0));
        Some(text)
    }

    /// The counts not written yet, each once.
    pub fn pending(&mut self) -> String {
        let mut text = String::new();
        for (key, n) in &mut self.recent {
            if *n > 0 {
                text.push_str(&Self::repeats(key, *n));
                *n = 0;
            }
        }
        text
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Logger {
    limiter: Mutex<Limiter>,
    /// The last error it was given, for the exit line.
    last_error: Mutex<Option<String>>,
}

impl Logger {
    pub const fn new() -> Self {
        Self { limiter: Mutex::new(Limiter::new()), last_error: Mutex::new(None) }
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|e| e.clone())
    }

    /// Write `record` to the log at `path`, as the limiter allows.
    pub fn write_to(&self, path: &Path, record: &log::Record) {
        if !log::Log::enabled(self, record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        if record.level() == log::Level::Error
            && let Ok(mut last) = self.last_error.lock()
        {
            *last = Some(format!("{}: {message}", record.target()));
        }
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
        metadata.level() <= log::Level::Error || (metadata.level() == log::Level::Warn && warning_wanted(metadata.target()))
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
        assert_eq!(l.admit("b", "B\n".into()).as_deref(), Some("B\n"));
        // Two lines that alternate are counted too, and use up nothing.
        for _ in 0..50 {
            assert_eq!(l.admit("a", "A\n".into()), None);
            assert_eq!(l.admit("b", "B\n".into()), None);
        }
        assert_eq!(l.pending(), "(\"a\" came 51 more times)\n(\"b\" came 50 more times)\n", "written when the app closes");
        assert_eq!(l.pending(), "", "once");
        for i in 2..MAX_LINES {
            assert!(l.admit(&i.to_string(), format!("{i}\n")).is_some(), "line {i}");
        }
        assert_eq!(l.admit("x", "X\n".into()).as_deref(), Some("(100 lines written this run; the rest are left out)\n"));
        assert_eq!(l.admit("y", "Y\n".into()), None, "nothing after that");
    }

    #[test]
    fn a_forgotten_line_has_its_count_written_first() {
        let mut l = Limiter::new();
        l.admit("a", "A\n".into());
        l.admit("a", "A\n".into());
        for i in 1..REMEMBERED {
            l.admit(&i.to_string(), format!("{i}\n"));
        }
        assert_eq!(l.admit("new", "N\n".into()).as_deref(), Some("(\"a\" came 1 more times)\nN\n"));
    }

    #[test]
    fn warnings_are_kept_from_the_ui_framework_and_the_window_library_only() {
        for t in ["eframe::native::run", "egui_winit", "egui", "winit::platform_impl::windows::monitor", "egui_glow::painter"] {
            assert!(warning_wanted(t), "{t}");
        }
        for t in ["capture_library::duplication", "eframelike", "ureq", "rustls::conn"] {
            assert!(!warning_wanted(t), "{t}");
        }
    }

    #[test]
    fn the_log_crate_reports_errors_from_all_and_warnings_from_two() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("crash.log");
        let logger = Logger::new();
        for (level, target, text) in [
            (log::Level::Error, "eframe::native::run", "Exiting because of error: the frame could not be shown"),
            (log::Level::Warn, "eframe::native::run", "a warning"),
            (log::Level::Warn, "capture_library::dxgi", "duplication access lost; recreating"),
            (log::Level::Error, "capture_library::dxgi", "device lost"),
            (log::Level::Info, "eframe::native::run", "news"),
            (log::Level::Debug, "eframe::native::run", "detail"),
        ] {
            logger.write_to(&file, &log::Record::builder().level(level).target(target).args(format_args!("{text}")).build());
        }
        let written = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 3, "{written}");
        assert!(lines[0].ends_with(" error from eframe::native::run: Exiting because of error: the frame could not be shown"), "{}", lines[0]);
        assert!(lines[1].ends_with(" warning from eframe::native::run: a warning"), "{}", lines[1]);
        assert!(lines[2].ends_with(" error from capture_library::dxgi: device lost"), "errors from every library: {}", lines[2]);
    }

    /// The window library reports through `tracing`, which hands its events to the `log` crate
    /// when nothing else takes them: its warning for a monitor that had gone reaches `crash.log`.
    #[test]
    fn a_warning_of_the_window_library_reaches_the_crash_log() {
        install_logger();
        let marker = format!("monitor handle no longer valid (test {})", std::process::id());
        tracing::warn!(target: "winit::platform_impl::windows::monitor", "{marker}; the primary monitor is used instead");
        let written = std::fs::read_to_string(path()).unwrap_or_default();
        assert!(written.lines().any(|l| l.ends_with(&format!("warning from winit::platform_impl::windows::monitor: {marker}; the primary monitor is used instead"))), "{written}");
    }

    /// A panic in a hook callback: one short line with the place and the message, the first of
    /// its kind only, and no backtrace is ever taken; elsewhere the whole panic and its backtrace.
    #[test]
    fn a_panic_in_a_hook_callback_is_one_short_line_without_a_backtrace() {
        let never = || -> String { panic!("no backtrace may be taken in a hook callback") };
        assert_eq!(
            panic_text(Some(HookKind::Keyboard), true, "t", "info", "src\\input.rs:12:5", "boom", never).as_deref(),
            Some("t panic in the keyboard hook at src\\input.rs:12:5: boom (the event was passed on; later panics in this hook are not written)\n\n")
        );
        assert_eq!(panic_text(Some(HookKind::Mouse), false, "t", "info", "here", "boom", never), None, "once per hook a run");
        assert_eq!(panic_text(None, false, "t", "panicked at here:\nboom", "here", "boom", || "BACKTRACE".into()).as_deref(), Some("t\npanicked at here:\nboom\nBACKTRACE\n\n"));
    }

    #[test]
    fn the_hook_mark_is_set_while_a_callback_runs_and_cleared_after_a_panic() {
        assert_eq!(current_hook(), None);
        assert_eq!(in_hook(HookKind::Mouse, current_hook), Some(HookKind::Mouse));
        let caught = std::panic::catch_unwind(|| in_hook(HookKind::Keyboard, || panic!("a hook callback of this test panics")));
        assert!(caught.is_err());
        assert_eq!(current_hook(), None, "cleared after the panic");
    }

    #[test]
    fn the_exit_line_gives_the_last_close_request_and_error_without_a_cause() {
        assert_eq!(unexpected_exit_line("t", true, Some(0), Some("x")), None, "Quit or an update");
        assert_eq!(unexpected_exit_line("t", false, None, None).as_deref(), Some("t closed without Quit or an update\n\n"));
        assert_eq!(
            unexpected_exit_line("t", false, Some(42), Some("eframe::native::run: Exiting because of error: swap failed")).as_deref(),
            Some("t closed without Quit or an update; the last close request to its window came 42 s before; the last error reported: eframe::native::run: Exiting because of error: swap failed\n\n")
        );
    }

    #[test]
    fn the_last_error_is_kept_for_the_exit_line() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new();
        logger.write_to(&dir.path().join("crash.log"), &log::Record::builder().level(log::Level::Error).target("test_target").args(format_args!("kept for the exit line")).build());
        assert_eq!(logger.last_error().as_deref(), Some("test_target: kept for the exit line"));
    }
}
