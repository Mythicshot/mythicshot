//! `capture.log`, beside `crash.log`: a few lines per region capture saying where the monitors
//! are, where the selector's window was asked to go, where it really was in its first frames,
//! and what was cropped. It settles from a user's machine whether the window, the scale and
//! the picture agreed. It never holds picture contents, window titles or paths, and is kept
//! under [`CAP`] bytes: a full file becomes `capture.old.log` and a new one starts.

use std::path::{Path, PathBuf};

use eframe::egui::Rect;
use mythicshot_capture::{MonitorInfo, PxRect};

use crate::overlay::FrameNote;
use crate::overlay::geometry::Show;

/// The most the log holds before it starts again (the previous one is kept as `capture.old.log`).
pub const CAP: u64 = 256 * 1024;

/// Frames of each capture that are logged, besides the frame the window is shown in.
pub const FRAMES_LOGGED: u32 = 3;

pub fn path() -> PathBuf {
    crate::settings::Settings::path().with_file_name("capture.log")
}

/// A native window as Windows reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinState {
    pub outer: PxRect,
    pub client: PxRect,
    pub dpi: Option<u32>,
    pub visible: bool,
}

/// What the placement of the window did before a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placed {
    /// It was moved or sized this frame.
    pub moved: bool,
    /// How many times it was asked to go to its target this frame.
    pub tries: u8,
    /// Its client area is on the target now.
    pub on_target: bool,
}

pub fn px(r: PxRect) -> String {
    format!("{},{} {}x{}", r.x, r.y, r.w, r.h)
}

fn pt(r: Rect) -> String {
    format!("{:.1},{:.1} {:.1}x{:.1}", r.min.x, r.min.y, r.width(), r.height())
}

fn opt<T>(v: Option<T>, f: impl FnOnce(T) -> String) -> String {
    v.map(f).unwrap_or_else(|| "-".into())
}

fn win(w: Option<WinState>) -> String {
    opt(w, |w| format!("window {} client {} dpi {} {}", px(w.outer), px(w.client), opt(w.dpi, |d| d.to_string()), if w.visible { "visible" } else { "hidden" }))
}

pub fn header(time: &str, trigger: &str, record: bool) -> String {
    format!("== {time} region capture · trigger {trigger} · {}", if record { "record" } else { "screenshot" })
}

pub fn monitor_line(i: usize, m: &MonitorInfo) -> String {
    format!("monitor {}: {} scale {:.2}{}", i + 1, px(m.rect), m.scale, if m.primary { " primary" } else { "" })
}

pub fn request_line(bounds: PxRect, ppp_before: f32, target: PxRect, before: Option<WinState>) -> String {
    format!("bounds {} · ppp before {ppp_before:.2} · window asked for {} (pixels) · before: {}", px(bounds), px(target), win(before))
}

pub fn frame_line(n: u32, ms: u128, now: Option<WinState>, placed: Placed, note: &FrameNote, cursor: Option<(i32, i32)>, settled: bool) -> String {
    let (cx, cy, cw, ch) = note.content;
    let place = if placed.moved { format!("moved ({} tries)", placed.tries) } else { "in place".into() };
    let delta = match (cursor, note.hover_px) {
        (Some(c), Some(h)) => format!(" (off by {},{})", h.0 - c.0, h.1 - c.1),
        _ => String::new(),
    };
    format!(
        "frame {n} +{ms}ms: {} · {place}, on target {} · ppp {:.2} · content {cx:.1},{cy:.1} {cw:.1}x{ch:.1} · settled {} · cursor {} mapped {}{delta} · texture {}",
        win(now),
        yes(placed.on_target),
        note.ppp,
        yes(settled),
        opt(cursor, |(x, y)| format!("{x},{y}")),
        opt(note.hover_px, |(x, y)| format!("{x},{y}")),
        if note.texture_new { "new" } else { "kept" },
    )
}

pub fn shown_line(frame: u32, show: Show, cloaked: bool) -> String {
    let why = match show {
        Show::Now => "placed and settled",
        Show::Anyway => "NOT settled, shown anyway",
        Show::Wait => "waiting",
    };
    format!("shown after frame {frame}: {why}{}", if cloaked { " · cloaked until a frame is painted" } else { "" })
}

pub fn focus_line(plain: bool) -> String {
    format!("focus: {}", if plain { "taken" } else { "refused, asked through winit (synthetic Alt)" })
}

pub fn finish_line(outcome: &str, sel_pt: Option<Rect>, sel_px: Option<PxRect>, crop: Option<PxRect>) -> String {
    format!("finish: {outcome} · selection {} pt = {} px · crop {}", opt(sel_pt, pt), opt(sel_px, px), opt(crop, px))
}

pub fn park_line(time: &str, why: &str, target: PxRect, now: Option<WinState>, tries: u32) -> String {
    format!("-- {time} hidden window parked ({why}) over {} after {tries} tries: {}", px(target), win(now))
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// Append to the log at `path`, starting a new one (the old kept beside it) past `cap` bytes.
pub fn append_at(path: &Path, text: &str, cap: u64) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > 0 && size + text.len() as u64 > cap {
        let _ = std::fs::rename(path, path.with_file_name("capture.old.log"));
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(text.as_bytes())
}

/// Append lines to `capture.log`. A log that cannot be written is skipped: it never stops a capture.
pub fn append(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    let _ = append_at(&path(), &text, CAP);
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    #[test]
    fn lines_carry_physical_rects_scales_and_the_mapping() {
        let m = MonitorInfo { name: "\\\\.\\DISPLAY3".into(), primary: true, rect: PxRect::new(-3840, 0, 3840, 2160), scale: 2.0 };
        let line = monitor_line(2, &m);
        assert_eq!(line, "monitor 3: -3840,0 3840x2160 scale 2.00 primary");
        assert!(!line.contains("DISPLAY"), "no monitor names");
        let before = WinState { outer: PxRect::new(-32000, -32000, 1, 1), client: PxRect::new(-32000, -32000, 1, 1), dpi: Some(96), visible: false };
        assert_eq!(
            request_line(PxRect::new(-3840, 0, 8960, 2160), 1.0, PxRect::new(-3840, 0, 8960, 2160), Some(before)),
            "bounds -3840,0 8960x2160 · ppp before 1.00 · window asked for -3840,0 8960x2160 (pixels) · before: window -32000,-32000 1x1 client -32000,-32000 1x1 dpi 96 hidden"
        );
        let note = FrameNote { content: (0.0, 0.0, 4480.0, 1080.0), ppp: 2.0, client: Some(PxRect::new(-3840, 0, 8960, 2160)), hover_px: Some((10, 12)), texture_new: true, texture_ready: true };
        let now = WinState { outer: PxRect::new(-3840, 0, 8960, 2160), client: PxRect::new(-3840, 0, 8960, 2160), dpi: Some(192), visible: false };
        assert_eq!(
            frame_line(1, 4, Some(now), Placed { moved: true, tries: 2, on_target: true }, &note, Some((10, 11)), false),
            "frame 1 +4ms: window -3840,0 8960x2160 client -3840,0 8960x2160 dpi 192 hidden · moved (2 tries), on target yes · ppp 2.00 · content 0.0,0.0 4480.0x1080.0 · settled no · cursor 10,11 mapped 10,12 (off by 0,1) · texture new"
        );
        assert_eq!(
            finish_line("capture", Some(Rect::from_min_max(pos2(5.0, 6.0), pos2(55.0, 26.0))), Some(PxRect::new(-3830, 12, 100, 40)), Some(PxRect::new(-3830, 12, 100, 40))),
            "finish: capture · selection 5.0,6.0 50.0x20.0 pt = -3830,12 100x40 px · crop -3830,12 100x40"
        );
        assert_eq!(finish_line("cancel", None, None, None), "finish: cancel · selection - pt = - px · crop -");
        assert_eq!(shown_line(2, Show::Anyway, false), "shown after frame 2: NOT settled, shown anyway");
        assert_eq!(header("2026-11-21 10:00:00.000", "hotkey", false), "== 2026-11-21 10:00:00.000 region capture · trigger hotkey · screenshot");
    }

    #[test]
    fn a_full_log_starts_again_and_keeps_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.log");
        append_at(&path, "aaaa\n", 12).unwrap();
        append_at(&path, "bbbb\n", 12).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "aaaa\nbbbb\n");
        append_at(&path, "cccc\n", 12).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "cccc\n");
        assert_eq!(std::fs::read_to_string(dir.path().join("capture.old.log")).unwrap(), "aaaa\nbbbb\n");
        append_at(&path, "dddd\neeee\n", 12).unwrap();
        append_at(&path, "ffff\n", 12).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("capture.old.log")).unwrap(), "dddd\neeee\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ffff\n");
    }
}
