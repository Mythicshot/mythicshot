//! `capture.log`, beside `crash.log`: a few lines per region capture saying where the monitors
//! are, where the selector's window was asked to go, where it really was in its first frames,
//! and what was cropped. It settles from a user's machine whether the window, the scale and
//! the picture agreed. It never holds picture contents, window titles or paths, and is kept
//! under [`CAP`] bytes: a full file becomes `capture.old.log` and a new one starts.

use std::path::{Path, PathBuf};

use eframe::egui::Rect;
use mythicshot_capture::{MonitorInfo, PxRect};

use crate::overlay::FrameNote;

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
    /// Its rectangles could be read (the window exists).
    pub readable: bool,
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

pub fn shown_line(frame: u32, anyway: bool, dpi_pending: bool, cloaked: bool) -> String {
    let why = match (anyway, dpi_pending) {
        (true, _) => "NOT settled, shown anyway",
        (false, true) => "in place, scale not changed yet while hidden: shown cloaked for it to change on show",
        (false, false) => "placed and settled",
    };
    format!("shown after frame {frame}: {why}{}", if cloaked { " · cloaked until a frame is painted" } else { " · not cloaked" })
}

/// `correction`: 0 when the window is uncloaked after it was first shown, n after the nth
/// correction.
pub fn uncloak_line(frame: u32, ok: bool, anyway: bool, dpi_pending: bool, correction: u8, dpi: Option<u32>) -> String {
    let result = if ok { "uncloaked" } else { "UNCLOAK FAILED: window hidden, capture cancelled" };
    let why = if anyway { "NOT settled in time" } else if dpi_pending { "settled, scale changed on show" } else { "settled" };
    let after = if correction > 0 { format!(" after correction {correction}") } else { String::new() };
    format!("{result} after frame {frame}: {why}{after} · dpi {}", opt(dpi, |d| d.to_string()))
}

pub fn uncloak_at_end_line(frame: u32, ok: bool) -> String {
    format!("{} after frame {frame}: the selection ended while it was still cloaked", if ok { "uncloaked" } else { "UNCLOAK FAILED" })
}

/// Placement gave up. `corrections`: Some(n) when the window was on screen and n corrections
/// were used; None when it was not shown yet and its moves were used up.
pub fn limit_line(frame: u32, moves: u8, corrections: Option<u8>, client: Option<PxRect>) -> String {
    let why = match corrections {
        Some(n) => format!("on screen, {n} corrections used"),
        None => format!("{moves} moves used"),
    };
    format!("frame {frame}: placement stopped ({why}); not on target, mapping follows client {}", opt(client, px))
}

fn dpi(w: Option<WinState>) -> String {
    opt(w.and_then(|w| w.dpi), |d| d.to_string())
}

/// The window was put back on its target after it had been on screen.
#[allow(clippy::too_many_arguments)]
pub fn correction_line(frame: u32, n: u8, max: u8, before: Option<WinState>, target: PxRect, moves: u8, after: Option<WinState>, recloaked: bool) -> String {
    format!(
        "frame {frame}: moved after show, correction {n} of {max}: was {} · put back on {} with {moves} moves · now {} · dpi {} -> {} · re-cloaked {}",
        win(before),
        px(target),
        win(after),
        dpi(before),
        dpi(after),
        yes(recloaked)
    )
}

/// Before it was first shown, the window went by another monitor so its scale was worked out again.
pub fn settle_line(frame: u32, before: Option<WinState>, away: PxRect, after: Option<WinState>) -> String {
    format!("frame {frame}: re-parked before showing, by way of {}: was {} · now {} · dpi {} -> {}", px(away), win(before), win(after), dpi(before), dpi(after))
}

pub fn displays_changed_line(frame: u32, was: &str, now: &str) -> String {
    format!("frame {frame}: displays changed during the capture (was {was}; now {now}): capture cancelled")
}

/// A check of the parked window found a monitor listed that could not be read (its handle no
/// longer valid: it went while it was read), and compared nothing.
pub fn unread_line(time: &str, code: u32, read: &str) -> String {
    let why = if code == crate::platform::INVALID_MONITOR_HANDLE { ", monitor handle no longer valid" } else { "" };
    format!("-- {time} parked window not checked: a monitor could not be read (error {code}{why}), read {read}; the displays are read again at the next check")
}

/// At the start of a capture, something in which the parked window disagrees with the displays.
pub fn capture_note_line(problem: &str, action: NoteAction) -> String {
    let what = match action {
        NoteAction::Settle => "re-parked by way of another monitor before showing",
        NoteAction::AlreadySettled => "already re-parked for these displays: not again, corrected after show if it moves",
        NoteAction::Place => "corrected while the window is placed",
    };
    format!("note: {problem} · {what}")
}

/// What is done at the start of a capture about a parked window that disagrees with the displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteAction {
    /// Moved by way of another monitor before it is shown.
    Settle,
    /// That was done already for these displays and is not repeated.
    AlreadySettled,
    /// Only placed on its target, as every capture does.
    Place,
}

/// At the start of a capture, the full monitor listing (taken for the picture) and the light
/// reading the checks use did not agree.
pub fn listings_line(full: &str, light: &str) -> String {
    format!("note: the monitor listing for the picture ({full}) is not the one the checks read ({light})")
}

/// A check of the parked window found it disagreeing with the displays.
pub fn parked_check_line(time: &str, problems: &[String], settle: bool) -> String {
    format!("-- {time} parked window checked: {} · parking again{}", problems.join("; "), if settle { " by way of another monitor" } else { "" })
}

pub fn focus_line(plain: bool) -> String {
    format!("focus: {}", if plain { "taken" } else { "refused, asked through winit (synthetic Alt)" })
}

pub fn finish_line(outcome: &str, sel_pt: Option<Rect>, sel_px: Option<PxRect>, crop: Option<PxRect>) -> String {
    format!("finish: {outcome} · selection {} pt = {} px · crop {}", opt(sel_pt, pt), opt(sel_px, px), opt(crop, px))
}

/// One line per small window placed (toast, picker, live draw, bar).
pub fn placed_line(time: &str, title: &str, target: PxRect, now: Option<WinState>, moves: u8, looks: u8, why: &str) -> String {
    format!("-- {time} \"{title}\" asked for {} · {} · {moves} moves, {looks} looks · {why}", px(target), win(now))
}

pub fn park_line(time: &str, why: &str, target: PxRect, away: Option<PxRect>, now: Option<WinState>, tries: u32) -> String {
    let by = away.map(|a| format!(" by way of {}", px(a))).unwrap_or_default();
    format!("-- {time} hidden window parked ({why}) over {}{by} with {tries} moves: {}", px(target), win(now))
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
        let note = FrameNote { content: (0.0, 0.0, 4480.0, 1080.0), ppp: 2.0, client: Some(PxRect::new(-3840, 0, 8960, 2160)), hover_px: Some((10, 12)), texture_new: true };
        let now = WinState { outer: PxRect::new(-3840, 0, 8960, 2160), client: PxRect::new(-3840, 0, 8960, 2160), dpi: Some(192), visible: false };
        assert_eq!(
            frame_line(1, 4, Some(now), Placed { moved: true, tries: 2, on_target: true, readable: true }, &note, Some((10, 11)), false),
            "frame 1 +4ms: window -3840,0 8960x2160 client -3840,0 8960x2160 dpi 192 hidden · moved (2 tries), on target yes · ppp 2.00 · content 0.0,0.0 4480.0x1080.0 · settled no · cursor 10,11 mapped 10,12 (off by 0,1) · texture new"
        );
        assert_eq!(
            finish_line("capture", Some(Rect::from_min_max(pos2(5.0, 6.0), pos2(55.0, 26.0))), Some(PxRect::new(-3830, 12, 100, 40)), Some(PxRect::new(-3830, 12, 100, 40))),
            "finish: capture · selection 5.0,6.0 50.0x20.0 pt = -3830,12 100x40 px · crop -3830,12 100x40"
        );
        assert_eq!(
            placed_line("t", "Mythicshot notification", PxRect::new(1, 2, 760, 192), Some(now), 1, 3, "placed"),
            "-- t \"Mythicshot notification\" asked for 1,2 760x192 · window -3840,0 8960x2160 client -3840,0 8960x2160 dpi 192 hidden · 1 moves, 3 looks · placed"
        );
        assert_eq!(finish_line("cancel", None, None, None), "finish: cancel · selection - pt = - px · crop -");
        assert_eq!(shown_line(2, true, false, false), "shown after frame 2: NOT settled, shown anyway · not cloaked");
        assert_eq!(uncloak_line(4, false, false, false, 0, Some(192)), "UNCLOAK FAILED: window hidden, capture cancelled after frame 4: settled · dpi 192");
        assert_eq!(limit_line(9, 6, None, Some(PxRect::new(0, 0, 10, 10))), "frame 9: placement stopped (6 moves used); not on target, mapping follows client 0,0 10x10");
        assert_eq!(header("2026-11-21 10:00:00.000", "hotkey", false), "== 2026-11-21 10:00:00.000 region capture · trigger hotkey · screenshot");
    }

    #[test]
    fn corrections_settling_and_display_changes_are_logged() {
        let shrunk = WinState { outer: PxRect::new(0, 0, 2987, 720), client: PxRect::new(0, 1, 2987, 720), dpi: Some(96), visible: true };
        let back = WinState { outer: PxRect::new(0, -1, 8960, 2160), client: PxRect::new(0, 0, 8960, 2160), dpi: Some(192), visible: true };
        assert_eq!(
            correction_line(4, 1, 3, Some(shrunk), PxRect::new(0, 0, 8960, 2160), 2, Some(back), true),
            "frame 4: moved after show, correction 1 of 3: was window 0,0 2987x720 client 0,1 2987x720 dpi 96 visible · put back on 0,0 8960x2160 with 2 moves · now window 0,-1 8960x2160 client 0,0 8960x2160 dpi 192 visible · dpi 96 -> 192 · re-cloaked yes"
        );
        assert_eq!(uncloak_line(6, true, false, false, 1, Some(192)), "uncloaked after frame 6: settled after correction 1 · dpi 192");
        let hidden = |dpi| WinState { outer: PxRect::new(0, -1, 8960, 2160), client: PxRect::new(0, 0, 8960, 2160), dpi: Some(dpi), visible: false };
        assert_eq!(
            settle_line(1, Some(hidden(288)), PxRect::new(0, 0, 2560, 1440), Some(hidden(192))),
            "frame 1: re-parked before showing, by way of 0,0 2560x1440: was window 0,-1 8960x2160 client 0,0 8960x2160 dpi 288 hidden · now window 0,-1 8960x2160 client 0,0 8960x2160 dpi 192 hidden · dpi 288 -> 192"
        );
        assert_eq!(
            displays_changed_line(7, "0,0 2560x1440 @96", "0,0 2560x1440 @96, 2560,0 2560x1440 @96"),
            "frame 7: displays changed during the capture (was 0,0 2560x1440 @96; now 0,0 2560x1440 @96, 2560,0 2560x1440 @96): capture cancelled"
        );
        assert_eq!(capture_note_line("window dpi 288 matches no monitor (expected 192 where it sits)", NoteAction::Settle), "note: window dpi 288 matches no monitor (expected 192 where it sits) · re-parked by way of another monitor before showing");
        assert_eq!(
            park_line("t", "scale not the displays'", PxRect::new(0, 0, 8960, 2160), Some(PxRect::new(0, 0, 2560, 1440)), Some(hidden(192)), 4),
            "-- t hidden window parked (scale not the displays') over 0,0 8960x2160 by way of 0,0 2560x1440 with 4 moves: window 0,-1 8960x2160 client 0,0 8960x2160 dpi 192 hidden"
        );
        assert_eq!(
            park_line("t", "start", PxRect::new(0, 0, 8960, 2160), None, Some(hidden(192)), 2),
            "-- t hidden window parked (start) over 0,0 8960x2160 with 2 moves: window 0,-1 8960x2160 client 0,0 8960x2160 dpi 192 hidden",
            "unchanged without a detour"
        );
        assert_eq!(parked_check_line("t", &["a".into(), "b".into()], false), "-- t parked window checked: a; b · parking again");
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
