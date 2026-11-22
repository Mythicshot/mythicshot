//! The region selector's window, which is the app's hidden root window, as Windows reports
//! it during a capture: written to `capture.log` without changing anything on screen.

use std::time::Instant;

use mythicshot_capture::PxRect;

use crate::capture_log::{self, Placed, WinState};
use crate::overlay::FrameNote;
use crate::platform::{self, WindowId};

pub fn state(hwnd: WindowId) -> Option<WinState> {
    let (outer, client) = platform::window_rects(hwnd)?;
    Some(WinState { outer, client, dpi: platform::window_dpi(hwnd), visible: platform::is_window_visible(hwnd) })
}

/// One capture's lines, from the frozen desktop to the end of the selection.
pub struct Trace {
    target: PxRect,
    frame: u32,
    started: Instant,
    lines: Vec<String>,
}

impl Trace {
    pub fn new(target: PxRect, started: Instant, lines: Vec<String>) -> Self {
        Self { target, frame: 0, started, lines }
    }

    /// After each frame of the selector: where the window really is, for the first frames.
    pub fn after_frame(&mut self, hwnd: Option<WindowId>, note: &FrameNote) {
        self.frame += 1;
        if self.frame > capture_log::FRAMES_LOGGED {
            return;
        }
        let now = hwnd.and_then(state);
        let placed = Placed { moved: false, tries: 0, on_target: now.is_some_and(|s| s.client == self.target) };
        let settled = now.is_some_and(|s| (note.content.2 * note.ppp - s.client.w as f32).abs() <= 0.5 && (note.content.3 * note.ppp - s.client.h as f32).abs() <= 0.5);
        self.lines.push(capture_log::frame_line(self.frame, self.started.elapsed().as_millis(), now, placed, note, platform::cursor_pos(), settled));
        if self.frame == capture_log::FRAMES_LOGGED {
            capture_log::append(&std::mem::take(&mut self.lines));
        }
    }

    pub fn end(mut self, finish: String) {
        self.lines.push(finish);
        capture_log::append(&self.lines);
    }
}
