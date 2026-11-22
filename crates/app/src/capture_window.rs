//! Placing and showing the region selector's window, which is the app's hidden root window.
//!
//! The window is placed over the desktop in physical pixels with `SetWindowPos` and checked
//! with `GetWindowInfo`, never through a size in points: moving a window across monitors with
//! different scale factors changes its scale half-way, and winit then resizes it to keep its
//! size in points. It is shown only once it sits on its target, egui's scale is the window's
//! and a frame has been painted at that size; cloaked for that frame where Windows allows it,
//! so nothing half-drawn reaches the screen.
//!
//! The hidden window is also parked over the desktop at start-up and after a recording (the
//! recording bar moves it onto one monitor), so its scale has settled before a capture starts
//! and a capture normally does not move it at all.

use std::time::Instant;

use eframe::egui::{self, ViewportCommand};
use mythicshot_capture::PxRect;

use crate::capture_log::{self, Placed, WinState};
use crate::overlay::FrameNote;
use crate::overlay::geometry::{self, Show};
use crate::platform::{self, WindowId};

/// Times one placement asks Windows to put the window on its target: a scale change during
/// the first move is answered by winit with a resize of its own, which the second undoes.
const TRIES: u8 = 3;

/// Frames the parking of the hidden window is tried before it waits for the next capture.
const PARK_FRAMES: u32 = 20;

/// Frame lines one capture may log at most: its first frames, the one it is shown in, and
/// frames it had to be moved in.
const MAX_FRAME_LINES: u32 = 12;

pub fn state(hwnd: WindowId) -> Option<WinState> {
    let (outer, client) = platform::window_rects(hwnd)?;
    Some(WinState { outer, client, dpi: platform::window_dpi(hwnd), visible: platform::is_window_visible(hwnd) })
}

/// Put the window's client area on `target`, in pixels.
pub fn place(hwnd: WindowId, target: PxRect) -> Placed {
    let mut placed = Placed::default();
    while let Some((outer, client)) = platform::window_rects(hwnd) {
        if client == target {
            placed.on_target = true;
            break;
        }
        if placed.tries >= TRIES {
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
    /// Frames to let pass first: eframe shows the root window after its first frame, and the
    /// app hides it again only then; a window parked before that would flash over the desktop.
    skip: u8,
}

impl Parking {
    pub fn start() -> Self {
        Self { due: Some("start"), frames: 0, skip: 2 }
    }

    pub fn request(&mut self, why: &'static str) {
        *self = Self { due: Some(why), frames: 0, skip: 1 };
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
        let placed = place(hwnd, target);
        if placed.on_target || self.frames >= PARK_FRAMES {
            self.due = None;
            capture_log::append(&[capture_log::park_line(&capture_log::now(), why, target, state(hwnd), self.frames)]);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Hidden,
    /// Shown but cloaked; the number of frames painted since.
    Cloaked(u8),
    Shown,
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
    lines: Vec<String>,
    frame_lines: u32,
}

impl Presenting {
    pub fn new(target: PxRect, started: Instant, lines: Vec<String>) -> Self {
        Self { target, hwnd: None, stage: Stage::Hidden, frame: 0, started, focus_due: false, placed: Placed::default(), lines, frame_lines: 0 }
    }

    /// Before each frame of the selector: put the window on its target and say where its
    /// client area is now, for the mapping. None without a native window.
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
        self.placed = place(hwnd, self.target);
        platform::window_rects(hwnd).map(|(_, client)| client)
    }

    /// After each frame of the selector: show the window once the frame was right.
    pub fn after_frame(&mut self, ctx: &egui::Context, note: &FrameNote) {
        self.frame += 1;
        let Some(hwnd) = self.hwnd else { return };
        let now = state(hwnd);
        let client = now.map(|s| s.client).unwrap_or(self.target);
        let settled = geometry::scale_settled(note.ppp, ctx.zoom_factor(), now.and_then(|s| s.dpi), (note.content.2, note.content.3), client);
        let mut log_this = self.frame <= capture_log::FRAMES_LOGGED || self.placed.moved;
        let mut after = None;
        match self.stage {
            Stage::Hidden => match geometry::decide_show(self.frame, self.placed.moved, self.placed.on_target, settled, note.texture_ready) {
                Show::Wait => {}
                show => {
                    // Cloaked, the window is shown and painted once at this size before it is composited.
                    let cloaked = platform::set_cloaked(hwnd, true);
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    self.stage = if cloaked { Stage::Cloaked(0) } else { Stage::Shown };
                    self.focus_due = !cloaked;
                    after = Some(capture_log::shown_line(self.frame, show, cloaked));
                    log_this = true;
                }
            },
            // The frame painted after the window was shown has been presented: uncloak it.
            Stage::Cloaked(n) if n >= 1 => {
                platform::set_cloaked(hwnd, false);
                self.stage = Stage::Shown;
                self.focus_due = true;
            }
            Stage::Cloaked(n) => self.stage = Stage::Cloaked(n + 1),
            Stage::Shown if self.focus_due => {
                self.focus_due = false;
                // A plain request first; winit's, with its synthetic Alt key, only when refused.
                let plain = platform::take_foreground(hwnd);
                if !plain {
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                }
                after = Some(capture_log::focus_line(plain));
            }
            Stage::Shown => {}
        }
        if log_this && self.frame_lines < MAX_FRAME_LINES {
            self.frame_lines += 1;
            let line = capture_log::frame_line(self.frame, self.started.elapsed().as_millis(), now, self.placed, note, platform::cursor_pos(), settled);
            self.lines.push(line);
        }
        let focused = after.is_some() && self.stage == Stage::Shown && !self.focus_due;
        self.lines.extend(after);
        if self.frame == capture_log::FRAMES_LOGGED + 1 || focused {
            self.flush();
        }
    }

    /// The selection ended: uncloak (it may end before the window was ever composited) and log.
    pub fn end(mut self, finish: String) {
        if let Some(hwnd) = self.hwnd
            && matches!(self.stage, Stage::Cloaked(_))
        {
            platform::set_cloaked(hwnd, false);
        }
        self.lines.push(finish);
        self.flush();
    }

    fn flush(&mut self) {
        capture_log::append(&std::mem::take(&mut self.lines));
    }
}
