//! Placing and showing the app's hidden root window: as the region selector and as the
//! recording bar.
//!
//! The window is placed in physical pixels with `SetWindowPos` and checked with
//! `GetWindowInfo`, never through a size in points: moving a window across monitors with
//! different scale factors changes its scale half-way, and winit then resizes it to keep its
//! size in points. The selector is shown only once it sits on its target and a frame has been
//! painted at that size; cloaked for that frame where Windows allows it, so nothing half-drawn
//! reaches the screen. Once it is on screen it is never moved again: if it is not where it was
//! asked to go, the selector maps from where it is.
//!
//! The hidden window is also parked over the desktop at start-up and after a recording (the
//! recording bar moves it onto one monitor), so its scale has settled before a capture starts
//! and a capture normally does not move it at all.

use std::time::Instant;

use eframe::egui::{self, ViewportCommand};
use mythicshot_capture::PxRect;

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
const MAX_FRAME_LINES: u32 = 12;

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

/// The client rectangle the selector's window gets over the monitors there are now.
pub fn desktop_target() -> Option<PxRect> {
    geometry::union(mythicshot_capture::list_monitors().iter().map(|m| m.rect)).map(|b| geometry::window_rect(b, 0))
}

/// Parking the hidden window over the desktop, until it is there.
#[derive(Debug, Default)]
pub struct Parking {
    /// Why it is due ("start", "after recording"); None when it is parked.
    due: Option<&'static str>,
    frames: u32,
    tries: u32,
    /// Frames to let pass first: eframe shows the root window after its first frame, and the
    /// app hides it again only then; a window parked before that would flash over the desktop.
    skip: u8,
}

impl Parking {
    pub fn start() -> Self {
        Self { due: Some("start"), frames: 0, tries: 0, skip: 2 }
    }

    pub fn request(&mut self, why: &'static str) {
        *self = Self { due: Some(why), frames: 0, tries: 0, skip: 1 };
    }

    /// Each frame with no capture or recording on screen: park the window if it is hidden.
    pub fn tick(&mut self, hwnd: Option<WindowId>) {
        let (Some(why), Some(hwnd)) = (self.due, hwnd) else { return };
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        // Never moved while it shows anything (the recording bar's last frame).
        if platform::is_window_visible(hwnd) {
            return;
        }
        let Some(target) = desktop_target() else { return };
        self.frames += 1;
        let placed = place(hwnd, target, TRIES);
        self.tries += u32::from(placed.tries);
        if placed.on_target || self.frames >= PARK_FRAMES {
            self.due = None;
            capture_log::append(&[capture_log::park_line(&capture_log::now(), why, target, state(hwnd), self.tries)]);
        }
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
    /// `dpi_pending`: it was shown before Windows told it its new scale.
    Cloaked { frames: u32, settled: bool, dpi_pending: bool },
    Shown,
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
        Stage::Cloaked { .. } | Stage::Shown => Step::Wait,
    }
}

/// The stage after `step`, `cloaked` saying whether Windows cloaked the window when it was shown.
pub fn advance(stage: Stage, step: Step, f: Facts, cloaked: bool) -> Stage {
    match (stage, step) {
        (Stage::Hidden, Step::Show { dpi_pending, .. }) if cloaked => Stage::Cloaked { frames: 0, settled: false, dpi_pending },
        (Stage::Hidden, Step::Show { .. }) => Stage::Shown,
        (Stage::Cloaked { .. }, Step::Uncloak { .. }) => Stage::Shown,
        (Stage::Cloaked { frames, dpi_pending, .. }, _) => {
            // A scale change comes, if at all, as the window is shown: two frames after that a
            // window still on Windows' old word keeps it, and the mapping follows egui's scale.
            let dpi_ok = f.dpi_matches || frames >= 2;
            Stage::Cloaked { frames: frames + 1, settled: !f.moved && f.on_target && f.content_matches && dpi_ok, dpi_pending }
        }
        (s, _) => s,
    }
}

/// How many moves the window may get before a frame: none once it is on screen.
pub fn move_budget(stage: Stage, used: u8) -> u8 {
    match stage {
        Stage::Shown => 0,
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
}

impl Presenting {
    pub fn new(target: PxRect, started: Instant, lines: Vec<String>) -> Self {
        Self { target, hwnd: None, stage: Stage::Hidden, frame: 0, started, focus_due: false, placed: Placed::default(), moves: 0, limit_logged: false, lines, frame_lines: 0 }
    }

    /// Before each frame of the selector: put the window on its target while it is not on
    /// screen yet, and say where its client area is now, for the mapping. None without a
    /// native window.
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
                self.stage = Stage::Shown;
            }
            return None;
        };
        let budget = move_budget(self.stage, self.moves);
        self.placed = place(hwnd, self.target, budget);
        self.moves += self.placed.tries;
        if !self.placed.on_target && move_budget(self.stage, self.moves) == 0 && !self.limit_logged {
            self.limit_logged = true;
            let client = platform::window_rects(hwnd).map(|(_, c)| c);
            self.lines.push(capture_log::limit_line(self.frame + 1, self.moves, self.stage == Stage::Shown, client));
        }
        platform::window_rects(hwnd).map(|(_, client)| client)
    }

    /// After each frame of the selector: show the window once the frame was right.
    pub fn after_frame(&mut self, ctx: &egui::Context, note: &FrameNote) -> After {
        self.frame += 1;
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
                let Stage::Cloaked { dpi_pending, .. } = self.stage else { unreachable!("uncloak only follows cloaked") };
                after.push(capture_log::uncloak_line(self.frame, ok, anyway, dpi_pending, now.and_then(|s| s.dpi)));
                result = after_uncloak(ok);
                if result == After::Continue {
                    self.focus_due = true;
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                }
                log_this = true;
            }
        }
        let was_shown = self.stage == Stage::Shown;
        self.stage = advance(self.stage, decided, facts, cloaked);
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
        let flush = !after.is_empty() || self.frame == capture_log::FRAMES_LOGGED + 1;
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
        assert_eq!(advance(Stage::Hidden, s, RIGHT, false), Stage::Shown);
    }

    #[test]
    fn a_window_on_screen_is_never_moved_again_and_moves_are_limited() {
        assert_eq!(move_budget(Stage::Hidden, 0), TRIES);
        assert_eq!(move_budget(Stage::Hidden, 5), 1);
        assert_eq!(move_budget(Stage::Hidden, MAX_MOVES), 0, "the capture's moves are used up");
        assert_eq!(move_budget(Stage::Cloaked { frames: 0, settled: false, dpi_pending: true }, 3), TRIES, "placed again once while cloaked");
        assert_eq!(move_budget(Stage::Shown, 0), 0, "on screen: never moved, mapped from where it is");
        // A shown window that is not on its target stays shown and asks for nothing.
        let off = Facts { on_target: false, content_matches: false, ..RIGHT };
        assert_eq!(step(Stage::Shown, 30, off), Step::Wait);
        assert_eq!(advance(Stage::Shown, Step::Wait, off, true), Stage::Shown);
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
