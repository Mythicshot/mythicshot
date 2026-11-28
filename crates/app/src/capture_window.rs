//! Placing and showing the app's hidden root window: as the region selector and as the
//! recording bar.
//!
//! The window is placed in physical pixels with `SetWindowPos` and checked with
//! `GetWindowInfo`, never through a size in points: moving a window across monitors with
//! different scale factors changes its scale half-way, and winit then resizes it to keep its
//! size in points. The selector is shown only once it sits on its target and a frame has been
//! painted at that size; cloaked for that frame where Windows allows it, so nothing half-drawn
//! reaches the screen. Once it is on screen it is watched every frame: a resize or move it was
//! not asked for (a scale change Windows delivers after the window is shown, which winit
//! answers by keeping the window's size in points) is put back on the target, cloaked again
//! until a frame at the right size is painted, at most [`MAX_CORRECTIONS`] times per capture.
//! Only when those are used up does the selector map from wherever the window is.
//!
//! The hidden window is also parked over the desktop at start-up and after a recording (the
//! recording bar moves it onto one monitor), so its scale has settled before a capture starts
//! and a capture normally does not move it at all. While parked it is checked against the
//! displays as Windows reports them now: a changed set of monitors, or a DPI that is not the one
//! Windows would give it where it sits (a DPI no monitor has is the plainest case: a display that
//! was listed but off at start-up can leave one), re-parks it by way of another monitor, so
//! Windows works its scale out afresh before the next capture shows it.

use std::time::{Duration, Instant};

use eframe::egui::{self, ViewportCommand};
use mythicshot_capture::{MonitorInfo, PxRect};

use crate::capture_log::{self, Placed, WinState};
use crate::overlay::FrameNote;
use crate::overlay::geometry::{self, MAX_HIDDEN_FRAMES};
use crate::platform::{self, WindowId};

/// Times one placement asks Windows at most to put the window on its target: a scale change
/// during the first move is answered by winit with a resize of its own, which the second undoes.
pub const TRIES: u8 = 3;

/// `SetWindowPos` calls one capture may make in all, before and while it is cloaked.
pub const MAX_MOVES: u8 = 6;

/// Frames the selector may stay cloaked waiting for a settled frame.
pub const MAX_CLOAKED_FRAMES: u32 = 8;

/// Frames the parking of the hidden window is tried before it waits for the next capture.
const PARK_FRAMES: u32 = 20;

/// Frame lines one capture may log at most.
const MAX_FRAME_LINES: u32 = 20;

/// Times one capture puts its window back on its target after it was on screen. After that the
/// selector maps from wherever the window is, and the log says so.
pub const MAX_CORRECTIONS: u8 = 3;

/// How often the displays are read again while the selector is on screen.
const DISPLAY_CHECK_CAPTURE: Duration = Duration::from_millis(250);

/// Light readings tried at the start of a capture before its full listing is taken instead.
pub const BASELINE_READS: u32 = 3;

/// How often the parked window is checked against the displays.
const DISPLAY_CHECK_PARKED: Duration = Duration::from_secs(1);

/// The monitors as Windows reports them: each one's rectangle in pixels and its DPI. Two sets are
/// equal when they hold the same rectangles at the same DPIs, in any order.
#[derive(Debug, Clone)]
pub struct DisplaySet {
    mons: Vec<MonitorInfo>,
    /// A monitor listed went before it could be read, twice (see [`platform::read_layout`]):
    /// Windows' error. Such a reading is incomplete while the monitors change, and is compared
    /// with nothing.
    unread: Option<u32>,
    /// A monitor that cannot be read is left out of this reading: Windows' error, for the log.
    left_out: Option<u32>,
}

impl PartialEq for DisplaySet {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl DisplaySet {
    pub fn from_monitors(monitors: &[MonitorInfo]) -> Self {
        let mut mons = monitors.to_vec();
        mons.sort_by_key(|m| (m.rect.x, m.rect.y, m.rect.w, m.rect.h));
        Self { mons, unread: None, left_out: None }
    }

    /// From the light reading: rectangles and effective DPIs.
    pub fn from_layout(layout: &[(PxRect, u32)]) -> Self {
        let mons: Vec<MonitorInfo> = layout.iter().map(|&(rect, dpi)| MonitorInfo { name: String::new(), primary: false, rect, scale: dpi as f32 / 96.0 }).collect();
        Self::from_monitors(&mons)
    }

    /// The monitors there are now, from the light reading ([`platform::monitor_layout`]): cheap
    /// enough for every check while parked and the poll during a capture. Every comparison is
    /// between two light readings; the full listing is only compared with it once per capture
    /// (see [`listings_disagree`]).
    pub fn now() -> Self {
        Self::from_reading(&platform::monitor_layout())
    }

    /// From a reading that may be incomplete: a monitor that went while it was read.
    pub fn from_reading(layout: &platform::Layout) -> Self {
        Self { unread: layout.unread, left_out: layout.left_out, ..Self::from_layout(&layout.monitors) }
    }

    /// The displays a capture is compared with while it is on screen: the light reading `read`
    /// gives, read again (at most [`BASELINE_READS`] times in all) while a monitor goes as it is
    /// read; still incomplete, the full listing `full` the picture was taken with. Never an
    /// incomplete set, which would switch the check for changed displays off for the capture.
    pub fn capture_baseline(mut read: impl FnMut() -> Self, full: &[MonitorInfo]) -> Self {
        for _ in 0..BASELINE_READS {
            let now = read();
            if now.readable() {
                return now;
            }
        }
        Self::from_monitors(full)
    }

    /// Every monitor listed could be read: the reading can be compared with another.
    pub fn readable(&self) -> bool {
        self.unread.is_none()
    }

    fn key(&self) -> Vec<(PxRect, u32)> {
        self.mons.iter().map(|m| (m.rect, dpi_of(m.scale))).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.mons.is_empty()
    }

    /// The selector's client rectangle over these monitors: their bounds.
    pub fn target(&self) -> Option<PxRect> {
        geometry::union(self.mons.iter().map(|m| m.rect)).map(|b| geometry::window_rect(b, 0))
    }

    /// The DPI Windows gives a top-level window on `rect`: its monitor's (the one it overlaps
    /// most, else the nearest).
    pub fn dpi_for(&self, rect: PxRect) -> Option<u32> {
        crate::placement::monitor_for(rect, &self.mons).map(|m| dpi_of(m.scale))
    }

    pub fn has_dpi(&self, dpi: u32) -> bool {
        self.mons.iter().any(|m| dpi_of(m.scale) == dpi)
    }

    /// Where to move the window first so Windows works out its scale again on the way back to
    /// `target`: a monitor other than the one `target` belongs to, one with another scale if
    /// there is one. None with a single monitor.
    pub fn away_rect(&self, target: PxRect) -> Option<PxRect> {
        let home = crate::placement::monitor_for(target, &self.mons)?;
        let others = || self.mons.iter().filter(|m| m.rect != home.rect);
        others().find(|m| dpi_of(m.scale) != dpi_of(home.scale)).or_else(|| others().next()).map(|m| m.rect)
    }

    /// For the log: "0,0 2560x1440 @96, ...".
    pub fn describe(&self) -> String {
        if self.mons.is_empty() {
            return "none".into();
        }
        self.key().iter().map(|(r, d)| format!("{} @{d}", capture_log::px(*r))).collect::<Vec<_>>().join(", ")
    }
}

/// A scale factor as a DPI (96 is 100%).
pub fn dpi_of(scale: f32) -> u32 {
    (scale * 96.0).round() as u32
}

/// Something about the hidden window that does not agree with the displays as they are now.
#[derive(Debug, Clone, PartialEq)]
pub enum Mismatch {
    /// The monitors are not the ones it was parked for (added, removed, moved or rescaled).
    Displays { parked_for: String, now: String },
    /// Its DPI is no monitor's at all.
    DpiOfNoMonitor { dpi: u32, expected: u32 },
    /// Its DPI is some monitor's, but not the one Windows would give it where it sits.
    DpiNotExpected { dpi: u32, expected: u32 },
    /// It is not over the desktop's bounds.
    OffTarget { client: PxRect, target: PxRect },
}

impl Mismatch {
    /// Whether this calls for re-parking by way of another monitor, so the scale is worked out
    /// afresh (a plain move onto the target does not change a window's monitor).
    pub fn wants_settle(&self) -> bool {
        !matches!(self, Mismatch::OffTarget { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Mismatch::Displays { parked_for, now } => format!("display set-up changed since parking (parked for {parked_for}; now {now})"),
            Mismatch::DpiOfNoMonitor { dpi, expected } => format!("window dpi {dpi} matches no monitor (expected {expected} where it sits)"),
            Mismatch::DpiNotExpected { dpi, expected } => format!("window dpi {dpi} is not the {expected} Windows gives it where it sits"),
            Mismatch::OffTarget { client, target } => format!("window client {} is not on {}", capture_log::px(*client), capture_log::px(*target)),
        }
    }
}

/// What does not agree between the hidden window `win`, the displays `now`, and the displays it
/// was parked for (`parked_for`, None when it was never parked).
pub fn check_parked(parked_for: Option<&DisplaySet>, now: &DisplaySet, win: Option<WinState>) -> Vec<Mismatch> {
    let mut out = Vec::new();
    // No reading, or one a monitor went from while it was read: nothing to compare.
    if now.is_empty() || !now.readable() {
        return out;
    }
    if let Some(p) = parked_for
        && p != now
    {
        out.push(Mismatch::Displays { parked_for: p.describe(), now: now.describe() });
    }
    let Some(w) = win else { return out };
    if let (Some(dpi), Some(expected)) = (w.dpi, now.dpi_for(w.client))
        && dpi != expected
    {
        out.push(if now.has_dpi(dpi) { Mismatch::DpiNotExpected { dpi, expected } } else { Mismatch::DpiOfNoMonitor { dpi, expected } });
    }
    if let Some(target) = now.target()
        && w.client != target
    {
        out.push(Mismatch::OffTarget { client: w.client, target });
    }
    out
}

pub fn state(hwnd: WindowId) -> Option<WinState> {
    let (outer, client) = platform::window_rects(hwnd)?;
    Some(WinState { outer, client, dpi: platform::window_dpi(hwnd), visible: platform::is_window_visible(hwnd) })
}

/// Put the window's client area on `target`, in pixels, with at most `budget` moves.
pub fn place(hwnd: WindowId, target: PxRect, budget: u8) -> Placed {
    let mut placed = Placed::default();
    while let Some((outer, client)) = platform::window_rects(hwnd) {
        placed.readable = true;
        if client == target {
            placed.on_target = true;
            break;
        }
        if placed.tries >= budget.min(TRIES) {
            break;
        }
        placed.tries += 1;
        placed.moved = true;
        platform::place_window(hwnd, geometry::outer_for(target, outer, client));
    }
    placed
}

/// What a check of the parked window calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repark {
    pub why: &'static str,
    /// By way of another monitor, so Windows works out its scale again.
    pub settle: bool,
}

/// The decision after a check of the parked window. `settled_for_now`: it was already re-parked
/// by way of another monitor for these very displays (doing it again would only repeat it; a
/// scale Windows still gets wrong is then corrected after the selector is shown).
/// `last_reached`: the last parking got it onto its target (else a window that cannot get there
/// would be moved again every check).
pub fn recheck(mismatches: &[Mismatch], settled_for_now: bool, last_reached: bool) -> Option<Repark> {
    if mismatches.iter().any(|m| matches!(m, Mismatch::Displays { .. })) {
        return Some(Repark { why: "display set-up changed", settle: true });
    }
    if !settled_for_now && mismatches.iter().any(Mismatch::wants_settle) {
        return Some(Repark { why: "scale not the displays'", settle: true });
    }
    if last_reached && mismatches.iter().any(|m| matches!(m, Mismatch::OffTarget { .. })) {
        return Some(Repark { why: "moved", settle: false });
    }
    None
}

/// Parking the hidden window over the desktop, until it is there; then checking it against the
/// displays and parking it again when they disagree.
#[derive(Debug, Default)]
pub struct Parking {
    /// Why it is due ("start", "after recording"); None when it is parked.
    due: Option<&'static str>,
    frames: u32,
    tries: u32,
    /// Frames to let pass first: eframe shows the root window after its first frame, and the
    /// app hides it again only then; a window parked before that would flash over the desktop.
    skip: u8,
    /// Move it onto another monitor first, then onto the desktop.
    settle: bool,
    /// The monitor it went by on the way, for the log.
    away: Option<PxRect>,
    /// The displays it was last parked for.
    parked_for: Option<DisplaySet>,
    /// The displays it was last re-parked by way of another monitor for.
    settled_for: Option<DisplaySet>,
    /// The last parking got it onto its target.
    reached: bool,
    checked: Option<Instant>,
    /// The last check found a monitor that could not be read, and said so in the log.
    unread_logged: bool,
    /// A monitor is left out of the readings, and the log said so.
    left_out_logged: bool,
}

impl Parking {
    pub fn start() -> Self {
        Self { due: Some("start"), skip: 2, ..Self::default() }
    }

    pub fn request(&mut self, why: &'static str) {
        self.restart(why, false, 1);
    }

    fn restart(&mut self, why: &'static str, settle: bool, skip: u8) {
        self.due = Some(why);
        self.frames = 0;
        self.tries = 0;
        self.skip = skip;
        self.settle = settle;
        self.away = None;
    }

    /// Each frame with no capture or recording on screen: park the window if it is hidden, or
    /// check that it still agrees with the displays.
    pub fn tick(&mut self, hwnd: Option<WindowId>) {
        let Some(hwnd) = hwnd else { return };
        let Some(why) = self.due else {
            self.watch(hwnd);
            return;
        };
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        // Never moved while it shows anything (the recording bar's last frame).
        if platform::is_window_visible(hwnd) {
            return;
        }
        let displays = DisplaySet::now();
        // A monitor went while it was read: parked on the next frame's reading instead.
        if !displays.readable() {
            return;
        }
        let Some(target) = displays.target() else { return };
        self.frames += 1;
        if std::mem::take(&mut self.settle) {
            self.settled_for = Some(displays.clone());
            if let Some(away) = displays.away_rect(target) {
                // Onto another monitor this frame; onto the desktop the next, once winit has
                // handled whatever scale Windows gives it there.
                let placed = place(hwnd, away, TRIES);
                self.tries += u32::from(placed.tries);
                self.away = Some(away);
                return;
            }
        }
        let placed = place(hwnd, target, TRIES);
        self.tries += u32::from(placed.tries);
        if placed.on_target || self.frames >= PARK_FRAMES {
            self.due = None;
            self.reached = placed.on_target;
            self.parked_for = Some(displays);
            self.checked = Some(Instant::now());
            capture_log::append(&[capture_log::park_line(&capture_log::now(), why, target, self.away, state(hwnd), self.tries)]);
        }
    }

    /// Parked: about once a second, the window against the displays as Windows reports them now.
    fn watch(&mut self, hwnd: WindowId) {
        if self.checked.is_some_and(|t| t.elapsed() < DISPLAY_CHECK_PARKED) {
            return;
        }
        self.checked = Some(Instant::now());
        if platform::is_window_visible(hwnd) {
            return;
        }
        let lines = self.observe(&DisplaySet::now(), state(hwnd), &capture_log::now());
        capture_log::append(&lines);
    }

    /// One check of the parked window `win` against the displays `now`: the lines for the log,
    /// and a parking again when they disagree. A reading a monitor went from while it was read is
    /// said once in the log and compared with nothing; the next check reads the displays afresh.
    pub fn observe(&mut self, now: &DisplaySet, win: Option<WinState>, time: &str) -> Vec<String> {
        if let Some(code) = now.unread {
            return if std::mem::replace(&mut self.unread_logged, true) { Vec::new() } else { vec![capture_log::unread_line(time, code, &now.describe())] };
        }
        self.unread_logged = false;
        let mut lines = Vec::new();
        match now.left_out {
            Some(code) if !self.left_out_logged => {
                self.left_out_logged = true;
                lines.push(capture_log::left_out_line(time, code, &now.describe()));
            }
            Some(_) => {}
            None => self.left_out_logged = false,
        }
        let mismatches = check_parked(self.parked_for.as_ref(), now, win);
        let Some(r) = recheck(&mismatches, self.settled_for.as_ref() == Some(now), self.reached) else { return lines };
        let problems: Vec<String> = mismatches.iter().map(Mismatch::describe).collect();
        self.restart(r.why, r.settle, 0);
        lines.push(capture_log::parked_check_line(time, &problems, r.settle));
        lines
    }

    /// At the start of a capture: lines for the log about anything in which the parked window
    /// disagrees with the displays `now`, and whether it should be settled again (by way of
    /// another monitor) before the selector is shown. As with the checks while parked, that is
    /// done once per set of displays: a scale Windows still gets wrong after it is left to the
    /// correction after show, instead of costing every capture two moves and two scale changes.
    pub fn capture_notes(&mut self, now: &DisplaySet, win: Option<WinState>) -> (Vec<String>, bool) {
        let mismatches = check_parked(self.parked_for.as_ref(), now, win);
        let wanted = mismatches.iter().any(Mismatch::wants_settle) && now.target().is_some_and(|t| now.away_rect(t).is_some());
        let done_before = self.settled_for.as_ref() == Some(now);
        let settle = wanted && !done_before;
        if settle {
            self.settled_for = Some(now.clone());
        }
        let action = if settle {
            capture_log::NoteAction::Settle
        } else if wanted {
            capture_log::NoteAction::AlreadySettled
        } else {
            capture_log::NoteAction::Place
        };
        (mismatches.iter().map(|m| capture_log::capture_note_line(&m.describe(), action)).collect(), settle)
    }
}

/// What was true of the selector's window after a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    /// It was moved before this frame was laid out.
    pub moved: bool,
    pub on_target: bool,
    /// egui's scale is the DPI Windows reports for the window.
    pub dpi_matches: bool,
    /// egui's content area times its scale is the client area.
    pub content_matches: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Hidden,
    /// Shown but cloaked. `settled`: the frame painted last was right, so it can be uncloaked.
    /// `dpi_pending`: it was shown before Windows told it its new scale. `correction`: 0 when
    /// it is being shown, n when it was cloaked again for the nth correction after show.
    Cloaked { frames: u32, settled: bool, dpi_pending: bool, correction: u8 },
    /// On screen, after `corrections` corrections.
    Shown { corrections: u8 },
}

/// What to do about a window on screen before a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drift {
    /// Where it should be, or not on screen yet.
    InPlace,
    /// Not on its target: put it back, the nth correction of this capture.
    Correct { n: u8 },
    /// Not on its target and the corrections are used up: map from where it is.
    GiveUp,
}

/// Whether a window in `stage`, its client area on its target or not, needs putting back.
/// `placed_when_shown`: it was on its target when it came on screen. Corrections are for a
/// window that was in place and then moved; one shown off its target because placement gave
/// up is mapped from where it is (as the log already says), not cloaked and uncloaked again.
pub fn drift(stage: Stage, on_target: bool, placed_when_shown: bool) -> Drift {
    match stage {
        Stage::Shown { .. } if !placed_when_shown => Drift::InPlace,
        Stage::Shown { corrections } if !on_target => {
            if corrections < MAX_CORRECTIONS {
                Drift::Correct { n: corrections + 1 }
            } else {
                Drift::GiveUp
            }
        }
        _ => Drift::InPlace,
    }
}

/// Whether the window was on its target when it came on screen, after a frame that took it from
/// `before` to `after` (`on_target` that frame); `so_far` until then.
pub fn placed_when_shown(before: Stage, after: Stage, on_target: bool, so_far: bool) -> bool {
    let came_on_screen = !matches!(before, Stage::Shown { .. }) && matches!(after, Stage::Shown { .. });
    if came_on_screen { on_target } else { so_far }
}

/// Whether listings of the same monitors disagree: the full one taken for the picture and the
/// light one every check compares with. Both read rectangles in desktop pixels and the effective
/// DPI; a scale is turned back into a DPI by rounding scale × 96.
pub fn listings_disagree(full: &[MonitorInfo], light: &DisplaySet) -> bool {
    !full.is_empty() && !light.is_empty() && light.readable() && DisplaySet::from_monitors(full) != *light
}

/// The stage for correction `n`: cloaked again, waiting for a right frame, when Windows cloaked
/// it; else on screen (it is moved where it can be seen, as a window that cannot be cloaked is
/// shown).
pub fn correcting(n: u8, cloaked: bool) -> Stage {
    if cloaked { Stage::Cloaked { frames: 0, settled: false, dpi_pending: false, correction: n } } else { Stage::Shown { corrections: n } }
}

/// What to do after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wait,
    /// Show it, cloaked. `dpi_pending`: in place and painted at its size, but egui's scale is
    /// not the window's yet: Windows may not tell a hidden window its new scale, so it is
    /// shown cloaked for the change to arrive. `anyway`: it did not settle in time.
    Show { dpi_pending: bool, anyway: bool },
    /// The last frame painted while cloaked was right (or time is up): uncloak.
    Uncloak { anyway: bool },
}

/// The decision after frame `frame` (1 for the first) of a capture.
pub fn step(stage: Stage, frame: u32, f: Facts) -> Step {
    match stage {
        Stage::Hidden => {
            let in_place = !f.moved && f.on_target && f.content_matches;
            if in_place {
                Step::Show { dpi_pending: !f.dpi_matches, anyway: false }
            } else if frame >= MAX_HIDDEN_FRAMES {
                Step::Show { dpi_pending: false, anyway: true }
            } else {
                Step::Wait
            }
        }
        Stage::Cloaked { settled: true, .. } => Step::Uncloak { anyway: false },
        Stage::Cloaked { frames, .. } if frames >= MAX_CLOAKED_FRAMES => Step::Uncloak { anyway: true },
        Stage::Cloaked { .. } | Stage::Shown { .. } => Step::Wait,
    }
}

/// The stage after `step`, `cloaked` saying whether Windows cloaked the window when it was shown.
pub fn advance(stage: Stage, step: Step, f: Facts, cloaked: bool) -> Stage {
    match (stage, step) {
        (Stage::Hidden, Step::Show { dpi_pending, .. }) if cloaked => Stage::Cloaked { frames: 0, settled: false, dpi_pending, correction: 0 },
        (Stage::Hidden, Step::Show { .. }) => Stage::Shown { corrections: 0 },
        (Stage::Cloaked { correction, .. }, Step::Uncloak { .. }) => Stage::Shown { corrections: correction },
        (Stage::Cloaked { frames, dpi_pending, correction, .. }, _) => {
            // A scale change comes, if at all, as the window is shown: two frames after that a
            // window still on Windows' old word keeps it, and the mapping follows egui's scale.
            let dpi_ok = f.dpi_matches || frames >= 2;
            Stage::Cloaked { frames: frames + 1, settled: !f.moved && f.on_target && f.content_matches && dpi_ok, dpi_pending, correction }
        }
        (s, _) => s,
    }
}

/// How many moves the window may get before a frame from the ordinary placement: none once it
/// is on screen (a window on screen is moved only by a correction, see [`drift`], which cloaks
/// it again first and starts its own count of moves).
pub fn move_budget(stage: Stage, used: u8) -> u8 {
    match stage {
        Stage::Shown { .. } => 0,
        _ => MAX_MOVES.saturating_sub(used).min(TRIES),
    }
}

/// What the app must do after a frame of the selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    Continue,
    /// The window could not be uncloaked: it was hidden, and the capture ends as cancelled,
    /// so no invisible window is left over the desktop.
    Abort,
}

/// After an uncloak that worked or did not: a window Windows will not uncloak is shown but never
/// seen, a topmost window over every monitor that takes every click; it is hidden at once.
pub fn after_uncloak(ok: bool) -> After {
    if ok { After::Continue } else { After::Abort }
}

/// One capture's window, from the frozen desktop to the end of the selection.
pub struct Presenting {
    target: PxRect,
    hwnd: Option<WindowId>,
    stage: Stage,
    frame: u32,
    started: Instant,
    focus_due: bool,
    placed: Placed,
    moves: u8,
    limit_logged: bool,
    lines: Vec<String>,
    frame_lines: u32,
    /// The displays at the start of the capture: the frozen picture is of them.
    displays: DisplaySet,
    checked: Instant,
    /// Before it is first shown, move the window by way of this monitor so Windows works out
    /// its scale afresh (the parked window disagreed with the displays).
    settle_by: Option<PxRect>,
    /// The capture must end: the displays changed under it. The line says how.
    abort: Option<String>,
    /// Lines were added outside the logged frames: write them after this frame.
    flush_due: bool,
    /// It was on its target when it came on screen: only then is it corrected if it moves.
    placed_when_shown: bool,
}

/// Whether the displays changed during a capture, so its frozen picture is no longer of the
/// screens there are. A reading with no monitors at all (it failed), or one a monitor went from
/// while it was read, changes nothing: the next reading, a quarter of a second later, decides.
pub fn displays_changed(start: &DisplaySet, now: &DisplaySet) -> bool {
    !now.is_empty() && !start.is_empty() && now.readable() && start.readable() && start != now
}

impl Presenting {
    /// `displays`: the monitors at the start of the capture. `settle`: re-park the window by way
    /// of another monitor before showing it (see [`Parking::capture_notes`]).
    pub fn new(target: PxRect, started: Instant, lines: Vec<String>, displays: DisplaySet, settle: bool) -> Self {
        let settle_by = if settle { displays.away_rect(target) } else { None };
        Self {
            target,
            hwnd: None,
            stage: Stage::Hidden,
            frame: 0,
            started,
            focus_due: false,
            placed: Placed::default(),
            moves: 0,
            limit_logged: false,
            lines,
            frame_lines: 0,
            displays,
            checked: started,
            settle_by,
            abort: None,
            flush_due: false,
            placed_when_shown: false,
        }
    }

    /// Before each frame of the selector: put the window on its target while it is not on
    /// screen yet, put it back if it was moved after it was shown, and say where its client
    /// area is now, for the mapping. None without a native window.
    pub fn before_frame(&mut self, ctx: &egui::Context, hwnd: Option<WindowId>) -> Option<PxRect> {
        self.hwnd = hwnd;
        let Some(hwnd) = hwnd else {
            // No native window to place in pixels: ask in points, as before, and show it.
            if self.stage == Stage::Hidden {
                let ppp = ctx.pixels_per_point();
                let t = self.target;
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(egui::pos2(t.x as f32 / ppp, t.y as f32 / ppp)));
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(t.w as f32 / ppp, t.h as f32 / ppp)));
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
                self.stage = Stage::Shown { corrections: 0 };
            }
            return None;
        };
        let frame = self.frame + 1;
        // A monitor added, removed, moved or rescaled during the capture: the picture is no longer
        // of the screens, and the window's target no longer the desktop. The capture ends.
        self.poll_displays(frame, Instant::now(), DisplaySet::now);
        // Before it is first shown, by way of another monitor when the parked window disagreed
        // with the displays: Windows works out its scale on the way back.
        if self.stage == Stage::Hidden
            && let Some(away) = self.settle_by.take()
        {
            let before = state(hwnd);
            let there = place(hwnd, away, TRIES);
            let back = place(hwnd, self.target, TRIES);
            self.placed = Placed { moved: true, tries: there.tries + back.tries, ..back };
            self.lines.push(capture_log::settle_line(frame, before, away, state(hwnd)));
            return platform::window_rects(hwnd).map(|(_, client)| client);
        }
        // On screen and not on its target: something it did not ask for moved or sized it
        // (a scale change Windows delivered after show, which winit answers by keeping its
        // size in points). Cloaked again and put back, a bounded number of times.
        if let Stage::Shown { .. } = self.stage
            && let Some((_, client)) = platform::window_rects(hwnd)
            && let Drift::Correct { n } = drift(self.stage, client == self.target, self.placed_when_shown)
        {
            let before = state(hwnd);
            let cloaked = platform::set_cloaked(hwnd, true);
            self.moves = 0;
            let placed = place(hwnd, self.target, TRIES);
            self.moves = placed.tries;
            self.placed = Placed { moved: true, ..placed };
            self.stage = correcting(n, cloaked);
            self.lines.push(capture_log::correction_line(frame, n, MAX_CORRECTIONS, before, self.target, placed.tries, state(hwnd), cloaked));
            self.flush_due = true;
            return platform::window_rects(hwnd).map(|(_, client)| client);
        }
        let budget = move_budget(self.stage, self.moves);
        self.placed = place(hwnd, self.target, budget);
        self.moves += self.placed.tries;
        if !self.placed.on_target && move_budget(self.stage, self.moves) == 0 && !self.limit_logged {
            self.limit_logged = true;
            let client = platform::window_rects(hwnd).map(|(_, c)| c);
            let corrections = if let Stage::Shown { corrections } = self.stage { Some(corrections) } else { None };
            self.lines.push(capture_log::limit_line(frame, self.moves, corrections, client));
            self.flush_due = true;
        }
        platform::window_rects(hwnd).map(|(_, client)| client)
    }

    /// At `at`, before frame `frame`: every [`DISPLAY_CHECK_CAPTURE`], the displays `read` gives
    /// against the ones the capture started with; changed, the capture is to end.
    pub fn poll_displays(&mut self, frame: u32, at: Instant, read: impl FnOnce() -> DisplaySet) {
        if self.abort.is_some() || at.saturating_duration_since(self.checked) < DISPLAY_CHECK_CAPTURE {
            return;
        }
        self.checked = at;
        let now = read();
        if displays_changed(&self.displays, &now) {
            self.abort = Some(capture_log::displays_changed_line(frame, &self.displays.describe(), &now.describe()));
        }
    }

    /// Whether the capture is to end because the displays changed.
    pub fn aborting(&self) -> bool {
        self.abort.is_some()
    }

    /// Whether [`Self::before_frame`] moved the window (placed it, settled it or put it back) just
    /// now: the frame's input was taken before, so its pointer positions are of the window as it was.
    pub fn moved(&self) -> bool {
        self.placed.moved
    }

    /// After each frame of the selector: show the window once the frame was right, uncloak it
    /// once a frame after a correction was right.
    pub fn after_frame(&mut self, ctx: &egui::Context, note: &FrameNote) -> After {
        self.frame += 1;
        if let Some(line) = self.abort.take() {
            self.lines.push(line);
            return After::Abort;
        }
        let Some(hwnd) = self.hwnd else { return After::Continue };
        let now = state(hwnd);
        let client = now.map(|s| s.client).unwrap_or(self.target);
        let facts = Facts {
            moved: self.placed.moved,
            on_target: self.placed.on_target,
            dpi_matches: geometry::dpi_matches(note.ppp, ctx.zoom_factor(), now.and_then(|s| s.dpi)),
            content_matches: geometry::content_matches(note.ppp, (note.content.2, note.content.3), client),
        };
        let settled = facts.dpi_matches && facts.content_matches;
        let mut log_this = self.frame <= capture_log::FRAMES_LOGGED || self.placed.moved;
        let mut after = Vec::new();
        let mut result = After::Continue;
        let decided = step(self.stage, self.frame, facts);
        let mut cloaked = false;
        match decided {
            Step::Wait => {}
            Step::Show { dpi_pending, anyway } => {
                // Cloaked, the window is shown and painted once at this size before it is composited.
                cloaked = platform::set_cloaked(hwnd, true);
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                self.focus_due = !cloaked;
                after.push(capture_log::shown_line(self.frame, anyway, dpi_pending, cloaked));
                log_this = true;
            }
            Step::Uncloak { anyway } => {
                let ok = platform::set_cloaked(hwnd, false);
                let Stage::Cloaked { dpi_pending, correction, .. } = self.stage else { unreachable!("uncloak only follows cloaked") };
                after.push(capture_log::uncloak_line(self.frame, ok, anyway, dpi_pending, correction, now.and_then(|s| s.dpi)));
                result = after_uncloak(ok);
                if result == After::Continue {
                    // Cloaking again for a correction keeps the focus it already has.
                    self.focus_due = correction == 0;
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                }
                log_this = true;
            }
        }
        let was_shown = matches!(self.stage, Stage::Shown { .. });
        let before = self.stage;
        self.stage = advance(self.stage, decided, facts, cloaked);
        self.placed_when_shown = placed_when_shown(before, self.stage, facts.on_target, self.placed_when_shown);
        if was_shown && self.focus_due {
            self.focus_due = false;
            // A plain request first; winit's, with its synthetic Alt key, only when refused.
            let plain = platform::take_foreground(hwnd);
            if !plain {
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            after.push(capture_log::focus_line(plain));
        }
        if log_this && self.frame_lines < MAX_FRAME_LINES {
            self.frame_lines += 1;
            let line = capture_log::frame_line(self.frame, self.started.elapsed().as_millis(), now, self.placed, note, platform::cursor_pos(), settled);
            self.lines.push(line);
        }
        let flush = !after.is_empty() || self.frame == capture_log::FRAMES_LOGGED + 1 || std::mem::take(&mut self.flush_due);
        self.lines.extend(after);
        if flush && self.frame > capture_log::FRAMES_LOGGED {
            self.flush();
        }
        result
    }

    /// The selection ended: uncloak (it may end before the window was ever composited) and log.
    pub fn end(mut self, finish: String) {
        if let Some(hwnd) = self.hwnd
            && matches!(self.stage, Stage::Cloaked { .. })
        {
            let ok = platform::set_cloaked(hwnd, false);
            self.lines.push(capture_log::uncloak_at_end_line(self.frame, ok));
        }
        self.lines.push(finish);
        self.flush();
    }

    fn flush(&mut self) {
        capture_log::append(&std::mem::take(&mut self.lines));
    }
}

/// Showing the root window as the recording bar, the selector's way: placed in pixels on the
/// bar's monitor while hidden, shown cloaked once drawn at that size, placed again while
/// cloaked if a scale change on show resized it, uncloaked after a right frame.
#[derive(Debug)]
pub struct BarShowing {
    target: PxRect,
    stage: Stage,
    frames: u32,
    moves: u8,
    /// Frames spent waiting for the selector before it to be hidden.
    waits: u32,
    cloaked: bool,
}

/// Where showing the bar is after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarState {
    Pending,
    Shown,
    /// It could not be uncloaked: it was hidden again, and the recording must not start.
    Failed,
}

impl BarShowing {
    pub fn new(target: PxRect) -> Self {
        Self { target, stage: Stage::Hidden, frames: 0, moves: 0, waits: 0, cloaked: false }
    }

    /// Each frame until it is shown or has failed.
    pub fn tick(&mut self, ctx: &egui::Context, hwnd: Option<WindowId>) -> BarState {
        let Some(hwnd) = hwnd else {
            // No native window: in points, as before.
            let ppp = ctx.pixels_per_point();
            let (pos, size) = crate::placement::guess_points(self.target, ppp);
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            return BarState::Shown;
        };
        // The selector it may follow is hidden at the end of its last frame: wait for that.
        if self.stage == Stage::Hidden && platform::is_window_visible(hwnd) && self.waits < MAX_HIDDEN_FRAMES {
            self.waits += 1;
            return BarState::Pending;
        }
        if self.stage == Stage::Hidden && self.frames == 0 {
            // A selector that ended still cloaked must not leave the bar unseen.
            platform::set_cloaked(hwnd, false);
        }
        self.frames += 1;
        let placed = place(hwnd, self.target, move_budget(self.stage, self.moves));
        self.moves += placed.tries;
        let now = state(hwnd);
        let client = now.map(|s| s.client).unwrap_or(self.target);
        let content = ctx.content_rect();
        let ppp = ctx.pixels_per_point();
        let f = Facts {
            moved: placed.moved,
            on_target: placed.on_target,
            dpi_matches: geometry::dpi_matches(ppp, ctx.zoom_factor(), now.and_then(|s| s.dpi)),
            content_matches: geometry::content_matches(ppp, (content.width(), content.height()), client),
        };
        let decided = step(self.stage, self.frames, f);
        let mut result = BarState::Pending;
        let mut why = None;
        match decided {
            Step::Wait => {}
            Step::Show { anyway, .. } => {
                self.cloaked = platform::set_cloaked(hwnd, true);
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                if !self.cloaked {
                    result = BarState::Shown;
                    why = Some(if anyway { "NOT settled, shown anyway, not cloaked" } else { "placed, not cloaked" }.to_string());
                }
            }
            Step::Uncloak { anyway } => {
                let ok = platform::set_cloaked(hwnd, false);
                let Stage::Cloaked { dpi_pending, .. } = self.stage else { unreachable!("uncloak only follows cloaked") };
                let how = if anyway { "NOT settled in time" } else if dpi_pending { "placed; scale changed on show" } else { "placed" };
                if ok {
                    result = BarState::Shown;
                    why = Some(format!("{how}, uncloaked"));
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                    result = BarState::Failed;
                    why = Some(format!("UNCLOAK FAILED, hidden ({how})"));
                }
            }
        }
        self.stage = advance(self.stage, decided, f, self.cloaked);
        if let Some(why) = why {
            capture_log::append(&[capture_log::placed_line(&capture_log::now(), "recording bar", self.target, state(hwnd), self.moves, self.frames.min(255) as u8, &why)]);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT: Facts = Facts { moved: false, on_target: true, dpi_matches: true, content_matches: true };

    /// Runs the state machine over frames of facts, as `after_frame` does, with cloaking
    /// available; returns the steps.
    fn run(frames: &[Facts]) -> Vec<Step> {
        let mut stage = Stage::Hidden;
        frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let s = step(stage, i as u32 + 1, *f);
                stage = advance(stage, s, *f, true);
                s
            })
            .collect()
    }

    #[test]
    fn a_parked_window_is_shown_at_once_and_uncloaked_after_one_painted_frame() {
        let steps = run(&[RIGHT, RIGHT, RIGHT, RIGHT]);
        assert_eq!(steps, [Step::Show { dpi_pending: false, anyway: false }, Step::Wait, Step::Uncloak { anyway: false }, Step::Wait]);
    }

    #[test]
    fn a_window_moved_this_frame_waits_for_a_frame_laid_out_at_its_size() {
        let moved = Facts { moved: true, ..RIGHT };
        let steps = run(&[moved, RIGHT, RIGHT, RIGHT]);
        assert_eq!(steps[0], Step::Wait);
        assert_eq!(steps[1], Step::Show { dpi_pending: false, anyway: false });
        assert_eq!(steps[3], Step::Uncloak { anyway: false });
    }

    #[test]
    fn a_scale_windows_reports_only_at_show_time_does_not_cost_the_full_wait() {
        // Hidden, Windows reports 200% but egui still draws at 100%: shown cloaked at once.
        let pending = Facts { dpi_matches: false, ..RIGHT };
        let steps = run(&[pending]);
        assert_eq!(steps, [Step::Show { dpi_pending: true, anyway: false }]);
        // The change arrives on show: winit resizes the window, it is placed again while
        // cloaked, then a right frame is painted and it is uncloaked.
        let resized = Facts { moved: true, on_target: true, dpi_matches: true, content_matches: false };
        let steps = run(&[pending, resized, RIGHT, RIGHT]);
        assert_eq!(steps[1], Step::Wait, "not right yet: placed again");
        assert_eq!(steps[2], Step::Wait, "the right frame is painted");
        assert_eq!(steps[3], Step::Uncloak { anyway: false });
        // The change never arrives: two frames later the window is taken as it is.
        let steps = run(&[pending, pending, pending, pending, pending]);
        assert_eq!(steps[1..4], [Step::Wait, Step::Wait, Step::Wait]);
        assert_eq!(steps[4], Step::Uncloak { anyway: false }, "uncloaked after frame 5, not after 8 frames");
    }

    #[test]
    fn a_window_that_never_settles_is_shown_after_eight_frames_and_uncloaked_after_eight_more() {
        let wrong = Facts { on_target: false, ..RIGHT };
        let mut stage = Stage::Hidden;
        for frame in 1..MAX_HIDDEN_FRAMES {
            assert_eq!(step(stage, frame, wrong), Step::Wait);
            stage = advance(stage, Step::Wait, wrong, true);
        }
        let s = step(stage, MAX_HIDDEN_FRAMES, wrong);
        assert_eq!(s, Step::Show { dpi_pending: false, anyway: true });
        stage = advance(stage, s, wrong, true);
        for frame in 0..MAX_CLOAKED_FRAMES {
            let s = step(stage, MAX_HIDDEN_FRAMES + 1 + frame, wrong);
            assert_eq!(s, Step::Wait);
            stage = advance(stage, s, wrong, true);
        }
        assert_eq!(step(stage, 99, wrong), Step::Uncloak { anyway: true });
    }

    #[test]
    fn without_cloaking_the_window_is_shown_directly() {
        let s = step(Stage::Hidden, 1, RIGHT);
        assert_eq!(advance(Stage::Hidden, s, RIGHT, false), Stage::Shown { corrections: 0 });
    }

    // Before 0.2.28 this test was `a_window_on_screen_is_never_moved_again_and_moves_are_limited`
    // and asserted that a shown window off its target "stays shown and asks for nothing". That
    // decision was the defect (the selector stayed a third of the desktop): a window on screen
    // is still never moved by the ordinary placement, but `drift` now asks for a correction.
    #[test]
    fn the_ordinary_placement_never_moves_a_window_on_screen_and_moves_are_limited() {
        assert_eq!(move_budget(Stage::Hidden, 0), TRIES);
        assert_eq!(move_budget(Stage::Hidden, 5), 1);
        assert_eq!(move_budget(Stage::Hidden, MAX_MOVES), 0, "the capture's moves are used up");
        assert_eq!(move_budget(Stage::Cloaked { frames: 0, settled: false, dpi_pending: true, correction: 0 }, 3), TRIES, "placed again once while cloaked");
        assert_eq!(move_budget(Stage::Shown { corrections: 0 }, 0), 0, "on screen: moved only through a correction");
        // A shown window that is not on its target is corrected, not left where it is.
        let off = Facts { on_target: false, content_matches: false, ..RIGHT };
        assert_eq!(step(Stage::Shown { corrections: 0 }, 30, off), Step::Wait);
        assert_eq!(drift(Stage::Shown { corrections: 0 }, false, true), Drift::Correct { n: 1 });
        assert_eq!(drift(Stage::Shown { corrections: 0 }, true, true), Drift::InPlace);
    }

    /// What `Presenting` decides over frames, without a window: before each frame a window on
    /// screen off its target is corrected (cloaked again, Windows allowing it) or given up on;
    /// then `step` and `advance` as after a frame. Each entry: whether the client area was on
    /// the target before the frame, and the facts after it. Returns the steps and the drifts.
    fn presenting(frames: &[(bool, Facts)], cloaking: bool) -> (Vec<Step>, Vec<Drift>, Stage) {
        let mut stage = Stage::Hidden;
        let mut placed = false;
        let (mut steps, mut drifts) = (Vec::new(), Vec::new());
        for (i, (on_target, f)) in frames.iter().enumerate() {
            let mut f = *f;
            let d = drift(stage, *on_target, placed);
            drifts.push(d);
            if let Drift::Correct { n } = d {
                stage = correcting(n, cloaking);
                // Put back before the frame: this frame was laid out at the old size.
                f = Facts { moved: true, on_target: true, ..f };
            }
            let s = step(stage, i as u32 + 1, f);
            let before = stage;
            stage = advance(stage, s, f, cloaking);
            placed = placed_when_shown(before, stage, f.on_target, placed);
            steps.push(s);
        }
        (steps, drifts, stage)
    }

    #[test]
    fn a_scale_change_after_the_uncloak_is_put_back_cloaked_and_uncloaked_after_a_right_frame() {
        // The owner's capture: shown, painted, uncloaked after frame 3; then a scale change
        // resizes the window to its size in points (2987x720) before frame 4.
        let stale = Facts { content_matches: false, dpi_matches: false, ..RIGHT };
        let frames = [(true, RIGHT), (true, RIGHT), (true, RIGHT), (false, stale), (true, RIGHT), (true, RIGHT), (true, RIGHT)];
        let (steps, drifts, stage) = presenting(&frames, true);
        assert_eq!(steps[..3], [Step::Show { dpi_pending: false, anyway: false }, Step::Wait, Step::Uncloak { anyway: false }]);
        assert_eq!(drifts[3], Drift::Correct { n: 1 }, "frame 4: put back, not mapped from 2987x720");
        assert_eq!(steps[3], Step::Wait, "cloaked again: the frame laid out at the old size is not seen");
        assert_eq!(steps[4], Step::Wait, "the right frame is painted, still cloaked");
        assert_eq!(steps[5], Step::Uncloak { anyway: false }, "uncloaked once a frame at the right size was painted");
        assert_eq!(steps[6], Step::Wait);
        assert_eq!(stage, Stage::Shown { corrections: 1 });
        assert!(drifts[4..].iter().all(|d| *d == Drift::InPlace));
    }

    #[test]
    fn corrections_are_bounded_and_then_the_selector_maps_from_where_the_window_is() {
        // Every time it is uncloaked a scale change moves it again.
        let mut frames = vec![(true, RIGHT), (true, RIGHT), (true, RIGHT)];
        for _ in 0..10 {
            frames.extend([(false, RIGHT), (true, RIGHT), (true, RIGHT)]);
        }
        let (steps, drifts, stage) = presenting(&frames, true);
        let corrections: Vec<_> = drifts.iter().filter_map(|d| if let Drift::Correct { n } = d { Some(*n) } else { None }).collect();
        assert_eq!(corrections, (1..=MAX_CORRECTIONS).collect::<Vec<_>>(), "at most {MAX_CORRECTIONS} corrections");
        assert!(drifts.contains(&Drift::GiveUp), "then it gives up");
        assert_eq!(stage, Stage::Shown { corrections: MAX_CORRECTIONS }, "on screen, never cloaked again");
        assert_eq!(steps.iter().filter(|s| matches!(s, Step::Uncloak { .. })).count(), 1 + MAX_CORRECTIONS as usize);
        assert_eq!(
            capture_log::limit_line(13, 1, Some(MAX_CORRECTIONS), Some(PxRect::new(0, 1, 2987, 720))),
            "frame 13: placement stopped (on screen, 3 corrections used); not on target, mapping follows client 0,1 2987x720"
        );
        // A window that cannot be cloaked is put back where it can be seen, still bounded.
        let (_, drifts, stage) = presenting(&frames, false);
        assert_eq!(drifts.iter().filter(|d| matches!(d, Drift::Correct { .. })).count(), MAX_CORRECTIONS as usize);
        assert_eq!(stage, Stage::Shown { corrections: MAX_CORRECTIONS });
    }

    #[test]
    fn a_correction_that_never_settles_is_uncloaked_in_time_and_counts() {
        let wrong = Facts { on_target: false, ..RIGHT };
        let mut frames = vec![(true, RIGHT), (true, RIGHT), (true, RIGHT), (false, RIGHT)];
        frames.extend(std::iter::repeat_n((false, wrong), MAX_CLOAKED_FRAMES as usize));
        let (steps, drifts, stage) = presenting(&frames, true);
        assert!(drifts[4..].iter().all(|d| *d == Drift::InPlace), "while cloaked the ordinary placement tries, no new correction");
        assert_eq!(*steps.last().unwrap(), Step::Uncloak { anyway: true });
        assert_eq!(stage, Stage::Shown { corrections: 1 });
    }

    #[test]
    fn a_scale_change_while_cloaked_is_still_handled_by_the_ordinary_placement() {
        let cloaked = Stage::Cloaked { frames: 0, settled: false, dpi_pending: true, correction: 0 };
        assert_eq!(drift(cloaked, false, true), Drift::InPlace, "no correction while it is cloaked: it is placed as before");
        assert_eq!(drift(Stage::Hidden, false, true), Drift::InPlace);
        assert!(move_budget(cloaked, 0) > 0);
    }

    fn mon(x: i32, y: i32, w: u32, h: u32, scale: f32, primary: bool) -> MonitorInfo {
        MonitorInfo { name: String::new(), primary, rect: PxRect::new(x, y, w, h), scale }
    }

    /// The owner's displays: two 2560x1440 at 100% and a 3840x2160 TV at 200%.
    fn owners() -> Vec<MonitorInfo> {
        vec![mon(2560, 0, 2560, 1440, 1.0, false), mon(5120, 0, 3840, 2160, 2.0, false), mon(0, 0, 2560, 1440, 1.0, true)]
    }

    fn parked(dpi: u32) -> WinState {
        WinState { outer: PxRect::new(0, -1, 8960, 2160), client: PxRect::new(0, 0, 8960, 2160), dpi: Some(dpi), visible: false }
    }

    #[test]
    fn displays_compare_by_rectangle_and_dpi_in_any_order() {
        let a = DisplaySet::from_monitors(&owners());
        let mut reordered = owners();
        reordered.rotate_left(1);
        assert_eq!(a, DisplaySet::from_monitors(&reordered), "enumerated in another order: the same displays");
        assert_eq!(a.target(), Some(PxRect::new(0, 0, 8960, 2160)));
        assert_eq!(a.dpi_for(PxRect::new(0, 0, 8960, 2160)), Some(192), "the window over the desktop belongs to the TV");
        assert_eq!(a.away_rect(PxRect::new(0, 0, 8960, 2160)), Some(PxRect::new(0, 0, 2560, 1440)), "a monitor at another scale");
        assert_eq!(DisplaySet::from_monitors(&[mon(0, 0, 1920, 1080, 1.0, true)]).away_rect(PxRect::new(0, 0, 1920, 1080)), None, "one monitor: nowhere to go");
        let mut tv_300 = owners();
        tv_300[1].scale = 3.0;
        assert_ne!(a, DisplaySet::from_monitors(&tv_300), "a rescaled monitor");
        assert_eq!(a.describe(), "0,0 2560x1440 @96, 2560,0 2560x1440 @96, 5120,0 3840x2160 @192");
    }

    #[test]
    fn a_window_dpi_that_matches_no_monitor_at_capture_start_is_logged_and_settled_before_showing() {
        let now = DisplaySet::from_monitors(&owners());
        let fresh = || Parking { parked_for: Some(now.clone()), ..Parking::default() };
        let mut p = fresh();
        let (notes, settle) = p.capture_notes(&now, Some(parked(288)));
        assert!(settle, "re-parked by way of another monitor before the selector is shown");
        assert_eq!(notes, ["note: window dpi 288 matches no monitor (expected 192 where it sits) · re-parked by way of another monitor before showing"]);
        // The window the selector then gets is told to settle first.
        let pres = Presenting::new(PxRect::new(0, 0, 8960, 2160), Instant::now(), Vec::new(), now.clone(), settle);
        assert_eq!(pres.settle_by, Some(PxRect::new(0, 0, 2560, 1440)));
        // Some monitor's DPI, but not the TV's: also settled.
        let (notes, settle) = fresh().capture_notes(&now, Some(parked(96)));
        assert!(settle);
        assert_eq!(notes, ["note: window dpi 96 is not the 192 Windows gives it where it sits · re-parked by way of another monitor before showing"]);
        // Right: nothing said, nothing done.
        assert_eq!(fresh().capture_notes(&now, Some(parked(192))), (Vec::new(), false));
        let pres = Presenting::new(PxRect::new(0, 0, 8960, 2160), Instant::now(), Vec::new(), now, false);
        assert_eq!(pres.settle_by, None);
    }

    #[test]
    fn a_monitor_set_changed_since_parking_reparks_the_window() {
        // Parked at start-up after a reboot with the TV off: Windows listed it at 300%.
        let mut at_start = owners();
        at_start[1].scale = 3.0;
        let at_start = DisplaySet::from_monitors(&at_start);
        let now = DisplaySet::from_monitors(&owners());
        let m = check_parked(Some(&at_start), &now, Some(parked(288)));
        assert_eq!(m.len(), 2);
        assert!(matches!(m[0], Mismatch::Displays { .. }));
        assert_eq!(m[1], Mismatch::DpiOfNoMonitor { dpi: 288, expected: 192 });
        assert_eq!(recheck(&m, false, true), Some(Repark { why: "display set-up changed", settle: true }));
        assert_eq!(
            m[0].describe(),
            "display set-up changed since parking (parked for 0,0 2560x1440 @96, 2560,0 2560x1440 @96, 5120,0 3840x2160 @288; now 0,0 2560x1440 @96, 2560,0 2560x1440 @96, 5120,0 3840x2160 @192)"
        );
        let line = capture_log::parked_check_line("t", &m.iter().map(Mismatch::describe).collect::<Vec<_>>(), true);
        assert!(line.starts_with("-- t parked window checked: display set-up changed since parking ("), "{line}");
        assert!(line.ends_with("; window dpi 288 matches no monitor (expected 192 where it sits) · parking again by way of another monitor"), "{line}");
        // A monitor added or removed is a change too.
        let two = DisplaySet::from_monitors(&owners()[..2]);
        assert!(matches!(check_parked(Some(&two), &now, Some(parked(192)))[0], Mismatch::Displays { .. }));
        // The same displays, the window right: nothing to do.
        assert_eq!(check_parked(Some(&now), &now, Some(parked(192))), Vec::new());
        assert_eq!(recheck(&[], false, true), None);
    }

    #[test]
    fn a_stale_scale_is_settled_once_per_set_of_displays_and_a_window_that_cannot_reach_its_target_is_not_moved_every_second() {
        let now = DisplaySet::from_monitors(&owners());
        let m = check_parked(Some(&now), &now, Some(parked(288)));
        assert_eq!(recheck(&m, false, true), Some(Repark { why: "scale not the displays'", settle: true }));
        assert_eq!(recheck(&m, true, true), None, "already settled for these displays: left to the correction after show");
        let off = WinState { client: PxRect::new(0, 1, 2987, 720), ..parked(96) };
        let m = check_parked(Some(&now), &now, Some(off));
        assert!(m.contains(&Mismatch::OffTarget { client: PxRect::new(0, 1, 2987, 720), target: PxRect::new(0, 0, 8960, 2160) }));
        assert_eq!(recheck(&m, true, true), Some(Repark { why: "moved", settle: false }));
        assert_eq!(recheck(&m, true, false), None, "the last parking could not reach the target");
        // A failed reading of the displays (no monitors) never re-parks.
        assert_eq!(check_parked(Some(&now), &DisplaySet::from_monitors(&[]), Some(parked(288))), Vec::new());
    }

    /// The crash record's evening: the parked window is checked once a second while three
    /// monitors become one (a game going full screen) and come back. A reading taken while a
    /// monitor goes is incomplete: it is said once, compares with nothing and moves nothing; the
    /// next complete readings re-park the window for one monitor.
    #[test]
    fn a_monitor_that_goes_while_the_parked_window_is_checked_is_logged_once_and_moves_nothing() {
        use crate::platform::{INVALID_MONITOR_HANDLE, Layout};
        let three = DisplaySet::from_monitors(&owners());
        let mut p = Parking { parked_for: Some(three.clone()), reached: true, ..Parking::default() };
        assert_eq!(p.observe(&three, Some(parked(192)), "t0"), Vec::<String>::new(), "nothing changed");
        // Two monitors listed but gone by the time they are read.
        let torn = DisplaySet::from_reading(&Layout { monitors: vec![(PxRect::new(0, 0, 2560, 1440), 96)], unread: Some(INVALID_MONITOR_HANDLE), left_out: None });
        assert!(!torn.readable());
        assert_eq!(
            p.observe(&torn, Some(parked(192)), "t1"),
            ["-- t1 parked window not checked: a monitor could not be read (error 1461, monitor handle no longer valid), read 0,0 2560x1440 @96; the displays are read again at the next check"]
        );
        assert_eq!(p.due, None, "not parked again on a reading that is incomplete");
        assert_eq!(p.observe(&torn, Some(parked(192)), "t2"), Vec::<String>::new(), "said once while it lasts");
        assert!(check_parked(Some(&three), &torn, Some(parked(192))).is_empty(), "compared with nothing");
        // One monitor, read completely: parked again for it.
        let one = DisplaySet::from_reading(&Layout { monitors: vec![(PxRect::new(0, 0, 3840, 2160), 192)], unread: None, left_out: None });
        let lines = p.observe(&one, Some(parked(192)), "t3");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("-- t3 parked window checked: display set-up changed since parking (parked for 0,0 2560x1440 @96, 2560,0 2560x1440 @96, 5120,0 3840x2160 @192; now 0,0 3840x2160 @192)"), "{}", lines[0]);
        assert_eq!(p.due, Some("display set-up changed"));
        // A capture started meanwhile is not cancelled by an incomplete reading either.
        assert!(!displays_changed(&three, &torn) && !displays_changed(&torn, &three));
        assert!(displays_changed(&three, &one));
        assert!(!listings_disagree(&owners(), &torn), "an incomplete light reading says nothing about the full one");
        // A later incomplete reading is said again: the last one was complete.
        p.due = None;
        assert_eq!(p.observe(&torn, Some(parked(192)), "t4").len(), 1);
    }

    /// A capture starts while a monitor goes: its light readings stay incomplete, so it is
    /// compared with the full listing its picture was taken with, and monitors that change 100 ms
    /// later still end it at the next check, as they do after a complete start.
    #[test]
    fn a_capture_whose_first_readings_were_incomplete_is_still_ended_when_the_displays_change() {
        use crate::platform::{INVALID_MONITOR_HANDLE, Layout};
        let torn = || DisplaySet::from_reading(&Layout { monitors: vec![(PxRect::new(0, 0, 2560, 1440), 96)], unread: Some(INVALID_MONITOR_HANDLE), left_out: None });
        let mut reads = 0;
        let start = DisplaySet::capture_baseline(
            || {
                reads += 1;
                torn()
            },
            &owners(),
        );
        assert_eq!(reads, BASELINE_READS);
        assert!(start.readable());
        assert_eq!(start, DisplaySet::from_monitors(&owners()), "the listing the picture was taken with");
        // Read again until complete when that comes first.
        let mut n = 0;
        let complete = DisplaySet::capture_baseline(
            || {
                n += 1;
                if n == 1 { torn() } else { DisplaySet::from_monitors(&owners()[..2]) }
            },
            &owners(),
        );
        assert_eq!((n, complete), (2, DisplaySet::from_monitors(&owners()[..2])));
        let t0 = Instant::now();
        let one = DisplaySet::from_reading(&Layout { monitors: vec![(PxRect::new(0, 0, 3840, 2160), 192)], unread: None, left_out: None });
        let mut pres = Presenting::new(PxRect::new(0, 0, 8960, 2160), t0, Vec::new(), start, false);
        pres.poll_displays(1, t0 + Duration::from_millis(100), || one.clone());
        assert!(!pres.aborting(), "not read before the next check is due");
        pres.poll_displays(2, t0 + DISPLAY_CHECK_CAPTURE, || one.clone());
        assert!(pres.aborting(), "the capture ends");
        assert_eq!(pres.abort.as_deref(), Some("frame 2: displays changed during the capture (was 0,0 2560x1440 @96, 2560,0 2560x1440 @96, 5120,0 3840x2160 @192; now 0,0 3840x2160 @192): capture cancelled"));
        // The same with a start that was complete.
        let mut pres = Presenting::new(PxRect::new(0, 0, 8960, 2160), t0, Vec::new(), DisplaySet::from_monitors(&owners()), false);
        pres.poll_displays(2, t0 + DISPLAY_CHECK_CAPTURE, || one.clone());
        assert!(pres.aborting());
    }

    /// A monitor that cannot ever be read (a DPI query failing on a virtual display) is left out,
    /// said once, and the checks go on: a real change re-parks the window as before.
    #[test]
    fn a_monitor_that_always_fails_is_left_out_said_once_and_the_checks_go_on() {
        use crate::platform::Layout;
        const E_INVALIDARG: u32 = 0x8007_0057;
        let reading = |mons: &[MonitorInfo]| DisplaySet::from_reading(&Layout { monitors: mons.iter().map(|m| (m.rect, dpi_of(m.scale))).collect(), unread: None, left_out: Some(E_INVALIDARG) });
        let three = reading(&owners());
        assert!(three.readable());
        let mut p = Parking { parked_for: Some(three.clone()), reached: true, ..Parking::default() };
        assert_eq!(p.observe(&three, Some(parked(192)), "t0"), ["-- t0 a monitor could not be read (error 2147942487) and is left out of the displays: 0,0 2560x1440 @96, 2560,0 2560x1440 @96, 5120,0 3840x2160 @192"]);
        assert_eq!(p.due, None);
        assert_eq!(p.observe(&three, Some(parked(192)), "t1"), Vec::<String>::new(), "said once");
        let two = reading(&owners()[..2]);
        let lines = p.observe(&two, Some(parked(192)), "t2");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("-- t2 parked window checked: display set-up changed since parking"), "{}", lines[0]);
        assert_eq!(p.due, Some("display set-up changed"), "parked again");
        assert!(displays_changed(&three, &two), "a capture still ends on a real change");
    }

    #[test]
    fn a_wrong_scale_that_survives_the_detour_costs_only_the_first_capture_a_detour() {
        let now = DisplaySet::from_monitors(&owners());
        let mut p = Parking { parked_for: Some(now.clone()), ..Parking::default() };
        let (_, first) = p.capture_notes(&now, Some(parked(288)));
        assert!(first, "the first capture goes by another monitor");
        // Windows still says 288 afterwards: later captures on these displays do not detour,
        // but the log still says what was wrong.
        for _ in 0..3 {
            let (notes, settle) = p.capture_notes(&now, Some(parked(288)));
            assert!(!settle);
            assert_eq!(notes, ["note: window dpi 288 matches no monitor (expected 192 where it sits) · already re-parked for these displays: not again, corrected after show if it moves"]);
            assert_eq!(Presenting::new(PxRect::new(0, 0, 8960, 2160), Instant::now(), Vec::new(), now.clone(), settle).settle_by, None);
        }
        // A detour by the idle check counts as well.
        let mut q = Parking { parked_for: Some(now.clone()), settled_for: Some(now.clone()), ..Parking::default() };
        assert!(!q.capture_notes(&now, Some(parked(288))).1);
        // Other displays: one detour again.
        let mut tv_off = owners();
        tv_off.truncate(2);
        let other = DisplaySet::from_monitors(&tv_off);
        assert!(p.capture_notes(&other, Some(WinState { client: PxRect::new(2560, 0, 2560, 1440), ..parked(288) })).1);
    }

    #[test]
    fn a_window_shown_off_target_because_placement_gave_up_is_not_corrected() {
        // Never on target while hidden: shown anyway after 8 frames, uncloaked anyway after 8
        // more, then off target every frame.
        let wrong = Facts { on_target: false, ..RIGHT };
        let frames = vec![(false, wrong); (MAX_HIDDEN_FRAMES + MAX_CLOAKED_FRAMES + 10) as usize];
        let (steps, drifts, stage) = presenting(&frames, true);
        assert!(steps.contains(&Step::Show { dpi_pending: false, anyway: true }));
        assert!(steps.contains(&Step::Uncloak { anyway: true }));
        assert!(drifts.iter().all(|d| *d == Drift::InPlace), "no re-cloak and uncloak cycles: {drifts:?}");
        assert_eq!(stage, Stage::Shown { corrections: 0 });
        assert_eq!(drift(Stage::Shown { corrections: 0 }, false, false), Drift::InPlace);
        // On target when it came on screen: corrected when it moves.
        assert!(placed_when_shown(Stage::Cloaked { frames: 1, settled: true, dpi_pending: false, correction: 0 }, Stage::Shown { corrections: 0 }, true, false));
        assert!(!placed_when_shown(Stage::Cloaked { frames: 8, settled: false, dpi_pending: false, correction: 0 }, Stage::Shown { corrections: 0 }, false, false));
        assert!(placed_when_shown(Stage::Shown { corrections: 0 }, Stage::Shown { corrections: 0 }, false, true), "a later move does not undo it");
        // A correction uncloaked anyway off target ends the corrections.
        let mut frames = vec![(true, RIGHT), (true, RIGHT), (true, RIGHT), (false, RIGHT)];
        frames.extend(std::iter::repeat_n((false, wrong), MAX_CLOAKED_FRAMES as usize + 5));
        let (_, drifts, _) = presenting(&frames, true);
        assert_eq!(drifts.iter().filter(|d| matches!(d, Drift::Correct { .. })).count(), 1);
    }

    #[test]
    fn the_light_reading_and_the_full_listing_compare_equal_for_the_same_monitors() {
        // The full listing gives a scale (effective DPI / 96 as f32), the light one the DPI.
        for dpi in (96..=480).step_by(24).chain([100, 110, 125, 144, 168, 216, 264, 288, 336]) {
            let full = [mon(0, 0, 2560, 1440, dpi as f32 / 96.0, true), mon(2560, -200, 3840, 2160, 2.0, false)];
            let light = DisplaySet::from_layout(&[(PxRect::new(2560, -200, 3840, 2160), 192), (PxRect::new(0, 0, 2560, 1440), dpi)]);
            assert!(!listings_disagree(&full, &light), "dpi {dpi}");
            assert_eq!(DisplaySet::from_monitors(&full), light, "dpi {dpi}");
        }
        let light = DisplaySet::from_layout(&[(PxRect::new(0, 0, 2560, 1440), 96)]);
        assert!(listings_disagree(&[mon(0, 0, 2560, 1440, 1.25, true)], &light), "another scale");
        assert!(listings_disagree(&[mon(0, 0, 2560, 1400, 1.0, true)], &light), "another size");
        assert!(!listings_disagree(&[], &light) && !listings_disagree(&[mon(0, 0, 1, 1, 1.0, true)], &DisplaySet::from_layout(&[])), "a failed reading says nothing");
        assert_eq!(
            capture_log::listings_line("0,0 2560x1400 @96", "0,0 2560x1440 @96"),
            "note: the monitor listing for the picture (0,0 2560x1400 @96) is not the one the checks read (0,0 2560x1440 @96)"
        );
    }

    #[test]
    fn a_monitor_set_changing_while_the_selector_is_on_screen_ends_the_capture() {
        let start = DisplaySet::from_monitors(&owners());
        let mut reordered = owners();
        reordered.reverse();
        assert!(!displays_changed(&start, &DisplaySet::from_monitors(&reordered)));
        assert!(displays_changed(&start, &DisplaySet::from_monitors(&owners()[..2])), "the TV vanished");
        let mut more = owners();
        more.push(mon(-1920, 0, 1920, 1080, 1.0, false));
        assert!(displays_changed(&start, &DisplaySet::from_monitors(&more)), "a monitor arrived");
        let mut rescaled = owners();
        rescaled[1].scale = 1.5;
        assert!(displays_changed(&start, &DisplaySet::from_monitors(&rescaled)));
        assert!(!displays_changed(&start, &DisplaySet::from_monitors(&[])), "a failed reading changes nothing");
        // The capture ends through the same path as a window that cannot be uncloaked.
        let mut pres = Presenting::new(PxRect::new(0, 0, 8960, 2160), Instant::now(), Vec::new(), start, false);
        pres.abort = Some(capture_log::displays_changed_line(5, "a", "b"));
        let ctx = egui::Context::default();
        assert_eq!(pres.after_frame(&ctx, &FrameNote::default()), After::Abort);
        assert_eq!(pres.lines, ["frame 5: displays changed during the capture (was a; now b): capture cancelled"]);
    }

    /// The overlay is told the window moved before a frame exactly when `before_frame` placed,
    /// settled or put it back that frame (each sets `placed.moved`), and not otherwise.
    #[test]
    fn the_selector_is_told_when_the_window_moved_before_the_frame() {
        let mut pres = Presenting::new(PxRect::new(0, 0, 8960, 2160), Instant::now(), Vec::new(), DisplaySet::from_monitors(&owners()), false);
        assert!(!pres.moved(), "nothing moved yet");
        pres.placed = Placed { moved: true, on_target: true, readable: true, tries: 1 };
        assert!(pres.moved(), "put back before this frame");
        pres.placed = Placed { moved: false, on_target: true, readable: true, tries: 0 };
        assert!(!pres.moved());
    }

    #[test]
    fn a_window_that_cannot_be_uncloaked_ends_the_capture() {
        assert_eq!(after_uncloak(true), After::Continue);
        assert_eq!(after_uncloak(false), After::Abort);
    }

    #[test]
    fn the_bar_follows_the_selectors_steps_with_a_scale_change_on_show_and_without() {
        // The bar uses `step` and `advance` too. Parked at the desktop's size: its content is
        // still laid out for that, so it waits; then it is drawn at the bar's size and shown.
        let parked = Facts { content_matches: false, ..RIGHT };
        assert_eq!(run(&[parked, RIGHT, RIGHT, RIGHT])[1..], [Step::Show { dpi_pending: false, anyway: false }, Step::Wait, Step::Uncloak { anyway: false }]);
        // Windows tells it its new scale only on show: resized, placed again, then uncloaked.
        let pending = Facts { dpi_matches: false, ..RIGHT };
        let resized = Facts { moved: true, content_matches: false, ..RIGHT };
        assert_eq!(run(&[pending, resized, RIGHT, RIGHT]), [Step::Show { dpi_pending: true, anyway: false }, Step::Wait, Step::Wait, Step::Uncloak { anyway: false }]);
    }
}
