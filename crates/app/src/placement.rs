//! Where the app's small windows go, in desktop pixels, and putting them there.
//!
//! A window's size in points becomes pixels through the scale of the monitor it ends up on,
//! never through another window's scale: the hidden root window spans the whole desktop and
//! carries the scale of the monitor it overlaps most (200% on a machine with one such monitor),
//! while a new window is created on whatever monitor Windows picks. So every rectangle here is
//! worked out in pixels from the target monitor's own scale, and [`Placer`] puts the window on
//! it with `SetWindowPos` while it is still hidden, then shows it.

use eframe::egui;
use mythicshot_capture::{MonitorInfo, PxRect};

use crate::capture_log::Placed;
use crate::capture_window;
use crate::platform::{self, WindowId};

/// The toast's size in points.
pub const TOAST_SIZE: [f32; 2] = [380.0, 96.0];
/// The destination picker's size in points.
pub const PICKER_SIZE: [f32; 2] = [270.0, 262.0];
/// The recording control bar's size in points.
pub const BAR_SIZE: [f32; 2] = [680.0, 46.0];
/// The red frame's width outside the recorded area, in points.
pub const FRAME_BORDER: f32 = 4.0;

fn px(points: f32, scale: f32) -> i32 {
    (points * scale).round() as i32
}

fn area(a: PxRect, b: PxRect) -> u64 {
    a.intersect(&b).map_or(0, |r| r.w as u64 * r.h as u64)
}

/// The monitor a window on `rect` belongs to, as Windows decides it: the one it overlaps most,
/// else the nearest one.
pub fn monitor_for<'a>(rect: PxRect, monitors: &'a [MonitorInfo]) -> Option<&'a MonitorInfo> {
    let best = monitors.iter().max_by_key(|m| area(rect, m.rect))?;
    if area(rect, best.rect) > 0 {
        return Some(best);
    }
    let (cx, cy) = (rect.x as i64 + rect.w as i64 / 2, rect.y as i64 + rect.h as i64 / 2);
    monitors.iter().min_by_key(|m| {
        let dx = (m.rect.x as i64 - cx).max(cx - m.rect.right() as i64).max(0);
        let dy = (m.rect.y as i64 - cy).max(cy - m.rect.bottom() as i64).max(0);
        dx * dx + dy * dy
    })
}

/// The scale a window on `rect` gets.
pub fn scale_for(rect: PxRect, monitors: &[MonitorInfo]) -> f32 {
    monitor_for(rect, monitors).map_or(1.0, |m| m.scale)
}

/// The toast: bottom-right of the primary monitor, its size and margins in that monitor's scale.
pub fn toast_rect(monitors: &[MonitorInfo]) -> Option<PxRect> {
    let m = monitors.iter().find(|m| m.primary).or(monitors.first())?;
    let s = m.scale;
    let (w, h) = (px(TOAST_SIZE[0], s), px(TOAST_SIZE[1], s));
    Some(PxRect::new(m.rect.right() - w - px(16.0, s), m.rect.bottom() - h - px(64.0, s), w as u32, h as u32))
}

/// The destination picker: beside the pointer, on the pointer's monitor, in its scale, kept on it.
pub fn picker_rect(cursor: (i32, i32), monitors: &[MonitorInfo]) -> PxRect {
    let m = monitors.iter().find(|m| m.rect.contains(cursor.0, cursor.1)).or_else(|| monitor_for(PxRect::new(cursor.0, cursor.1, 1, 1), monitors));
    let s = m.map_or(1.0, |m| m.scale);
    let (w, h) = (px(PICKER_SIZE[0], s), px(PICKER_SIZE[1], s));
    let (mut x, mut y) = (cursor.0 + px(12.0, s), cursor.1 + px(12.0, s));
    if let Some(m) = m {
        x = x.min(m.rect.right() - w - px(8.0, s)).max(m.rect.x);
        y = y.min(m.rect.bottom() - h - px(48.0, s)).max(m.rect.y);
    }
    PxRect::new(x, y, w as u32, h as u32)
}

/// The control bar for a recording of `rect` on `monitor` (scale `scale`): above the area, else
/// below it, else inside its top edge, centred on it and kept on the monitor.
pub fn bar_rect(rect: PxRect, monitor: PxRect, scale: f32) -> PxRect {
    let (bw, bh) = (px(BAR_SIZE[0], scale), px(BAR_SIZE[1], scale));
    let gap = px(10.0, scale);
    let x = (rect.x + rect.w as i32 / 2 - bw / 2).clamp(monitor.x, (monitor.right() - bw).max(monitor.x));
    let y = if rect.y - bh - gap >= monitor.y {
        rect.y - bh - gap
    } else if rect.bottom() + gap + bh <= monitor.bottom() {
        rect.bottom() + gap
    } else {
        rect.y + gap
    };
    PxRect::new(x, y, bw as u32, bh as u32)
}

/// The red frame's window around a recorded area: the area plus the border, in the scale of
/// the monitor the window belongs to (the area's own monitor, since the area is on one).
pub fn frame_rect(rect: PxRect, monitors: &[MonitorInfo]) -> PxRect {
    let b = px(FRAME_BORDER, scale_for(rect, monitors));
    PxRect::new(rect.x - b, rect.y - b, rect.w + 2 * b as u32, rect.h + 2 * b as u32)
}

/// A rectangle in points for a viewport builder, from pixels at `scale`. Only a first guess:
/// the window is moved onto its pixels by a [`Placer`] before it is shown.
pub fn guess_points(r: PxRect, scale: f32) -> (egui::Pos2, egui::Vec2) {
    (egui::pos2(r.x as f32 / scale, r.y as f32 / scale), egui::vec2(r.w as f32 / scale, r.h as f32 / scale))
}

/// Most `SetWindowPos` calls one window gets before it is shown wherever it is.
const MAX_MOVES: u8 = 6;

/// Puts one of our windows, created hidden by egui, on its rectangle in pixels and then shows
/// it. It never moves a window once shown.
#[derive(Debug)]
pub struct Placer {
    title: &'static str,
    target: PxRect,
    hwnd: Option<WindowId>,
    moves: u8,
    /// Looks that found no window to place.
    misses: u8,
    shown: bool,
    /// Take the keyboard when shown.
    focus: Focus,
}

/// Whether and how a window takes the keyboard once shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    None,
    /// A plain request only: never a synthetic key press (a window over a recording).
    Plain,
    /// A plain request, then winit's (with its synthetic Alt key) when Windows refuses.
    PlainThenWinit,
}

/// Looks without finding the window before it is shown wherever egui put it: a window that
/// cannot be found must still appear.
const MAX_MISSES: u8 = 10;

impl Placer {
    pub fn new(title: &'static str, target: PxRect, focus: Focus) -> Self {
        Self { title, target, hwnd: None, moves: 0, misses: 0, shown: false, focus }
    }

    /// Each frame after the viewport `id` was asked for. Its window appears at the end of the
    /// frame it was first asked for, so the first look may find nothing.
    pub fn tick(&mut self, ctx: &egui::Context, id: egui::ViewportId) {
        if self.shown {
            // The frame after it was shown: take the keyboard, with a plain request first and
            // winit's (with its synthetic Alt key) only when Windows refuses.
            let focus = std::mem::replace(&mut self.focus, Focus::None);
            if focus != Focus::None
                && let Some(hwnd) = self.hwnd
                && !platform::take_foreground(hwnd)
                && focus == Focus::PlainThenWinit
            {
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
            }
            return;
        }
        // Look again soon: the window is new and waits for this to appear.
        ctx.request_repaint();
        let Some(hwnd) = self.hwnd.or_else(|| platform::find_own_hidden_window(self.title)) else {
            self.misses += 1;
            if !cfg!(windows) || self.misses >= MAX_MISSES {
                // No native window to place: shown where egui put it.
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
                self.shown = true;
            }
            return;
        };
        self.hwnd = Some(hwnd);
        let budget = MAX_MOVES.saturating_sub(self.moves).min(capture_window::TRIES);
        let placed = capture_window::place(hwnd, self.target, budget);
        self.moves += placed.tries;
        if show_child(placed, self.moves >= MAX_MOVES) {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
            self.shown = true;
        }
    }
}

/// Whether a hidden window is shown after a placement: once it sits on its target and was not
/// moved in this look (so its content has been laid out for that size), or out of moves.
pub fn show_child(placed: Placed, out_of_moves: bool) -> bool {
    (placed.on_target && !placed.moved) || out_of_moves
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, y: i32, w: u32, h: u32, scale: f32, primary: bool) -> MonitorInfo {
        MonitorInfo { name: String::new(), primary, rect: PxRect::new(x, y, w, h), scale }
    }

    /// The owner's machine: two monitors at 100% and one at 200%, either a 3840×2160 or a
    /// 1920×1080 panel, primary or not, left or right of the pair.
    fn layouts() -> Vec<(String, Vec<MonitorInfo>)> {
        let mut out = Vec::new();
        for (w, h) in [(3840u32, 2160u32), (1920, 1080)] {
            for big_primary in [true, false] {
                // The 200% monitor right of the pair.
                let (pair_x, big_x) = if big_primary { (-5120, 0) } else { (0, 5120) };
                out.push((format!("200% {w}x{h} right, primary {big_primary}"), vec![mon(pair_x, 0, 2560, 1440, 1.0, !big_primary), mon(pair_x + 2560, 0, 2560, 1440, 1.0, false), mon(big_x, 0, w, h, 2.0, big_primary)]));
                // Left of the pair.
                let (pair_x, big_x) = if big_primary { (w as i32, 0) } else { (0, -(w as i32)) };
                out.push((format!("200% {w}x{h} left, primary {big_primary}"), vec![mon(pair_x, 0, 2560, 1440, 1.0, !big_primary), mon(pair_x + 2560, 0, 2560, 1440, 1.0, false), mon(big_x, 0, w, h, 2.0, big_primary)]));
            }
        }
        out
    }

    fn inside(r: PxRect, m: PxRect) -> bool {
        r.x >= m.x && r.y >= m.y && r.right() <= m.right() && r.bottom() <= m.bottom()
    }

    #[test]
    fn the_toast_sits_in_the_bottom_right_corner_of_the_primary_monitor_in_its_scale() {
        for (name, mons) in layouts() {
            let p = mons.iter().find(|m| m.primary).unwrap();
            let t = toast_rect(&mons).unwrap();
            let s = p.scale;
            assert_eq!((t.w, t.h), ((380.0 * s) as u32, (96.0 * s) as u32), "{name}: the toast's size at its monitor's scale");
            assert_eq!((t.right(), t.bottom()), (p.rect.right() - (16.0 * s) as i32, p.rect.bottom() - (64.0 * s) as i32), "{name}: its corner");
            assert!(inside(t, p.rect), "{name}");
            assert_eq!(scale_for(t, &mons), s, "{name}: the window gets the scale it was sized for");
        }
    }

    #[test]
    fn the_picker_opens_beside_the_pointer_on_the_pointers_monitor() {
        for (name, mons) in layouts() {
            for m in &mons {
                let s = m.scale;
                // With room around it: just below and right of the pointer.
                let c = (m.rect.x + m.rect.w as i32 / 4, m.rect.y + m.rect.h as i32 / 4);
                let r = picker_rect(c, &mons);
                assert_eq!((r.x, r.y), (c.0 + (12.0 * s) as i32, c.1 + (12.0 * s) as i32), "{name}");
                assert_eq!((r.w, r.h), ((270.0 * s) as u32, (262.0 * s) as u32), "{name}");
                assert!(inside(r, m.rect) && scale_for(r, &mons) == s, "{name}");
                // In the bottom-right corner: pushed back onto the same monitor.
                let r = picker_rect((m.rect.right() - 1, m.rect.bottom() - 1), &mons);
                assert!(inside(r, m.rect) && scale_for(r, &mons) == s, "{name}: corner {r:?}");
                // On the left edge next to another monitor: never across it.
                let r = picker_rect((m.rect.x, m.rect.y), &mons);
                assert!(inside(r, m.rect), "{name}: edge {r:?}");
            }
        }
    }

    #[test]
    fn the_bar_and_the_frame_go_around_a_region_in_its_monitors_scale() {
        for (name, mons) in layouts() {
            for m in &mons {
                let s = m.scale;
                let region = PxRect::new(m.rect.x + 300, m.rect.y + 400, 800, 500);
                let bar = bar_rect(region, m.rect, s);
                assert_eq!((bar.w, bar.h), ((680.0 * s) as u32, (46.0 * s) as u32), "{name}: the bar's size in its monitor's scale");
                assert_eq!(bar.bottom(), region.y - (10.0 * s) as i32, "{name}: above the area");
                assert!(inside(bar, m.rect) && scale_for(bar, &mons) == s, "{name}: on the area's monitor {bar:?}");
                let f = frame_rect(region, &mons);
                let b = (4.0 * s) as i32;
                assert_eq!(f, PxRect::new(region.x - b, region.y - b, region.w + 2 * b as u32, region.h + 2 * b as u32), "{name}");
                assert_eq!(scale_for(f, &mons), s);
                // A whole monitor: the bar goes inside its top edge, the frame just outside it.
                let bar = bar_rect(m.rect, m.rect, s);
                assert_eq!(bar.y, m.rect.y + (10.0 * s) as i32, "{name}");
                assert!(inside(bar, m.rect));
                assert_eq!(scale_for(frame_rect(m.rect, &mons), &mons), s, "{name}: the frame over a whole monitor keeps its scale");
            }
            // A region dragged across two monitors is clipped to the one holding its centre
            // (the recorder's rule); its frame and bar follow that monitor's scale.
            let (a, b) = (&mons[1], &mons[2]);
            if a.rect.right() == b.rect.x {
                let across = PxRect::new(a.rect.right() - 300, a.rect.y + 100, 400, 300);
                let centre = (across.x + 200, across.y + 150);
                let on = mons.iter().find(|m| m.rect.contains(centre.0, centre.1)).unwrap();
                let clipped = across.intersect(&on.rect).unwrap();
                assert_eq!(scale_for(frame_rect(clipped, &mons), &mons), on.scale, "{name}");
                assert!(inside(bar_rect(clipped, on.rect, on.scale), on.rect), "{name}");
            }
        }
    }

    #[test]
    fn the_live_draw_window_is_its_region_and_keeps_that_regions_scale() {
        for (name, mons) in layouts() {
            for m in &mons {
                let region = PxRect::new(m.rect.x + 10, m.rect.y + 10, 640, 360);
                let (pos, size) = guess_points(region, m.scale);
                assert_eq!((pos.x * m.scale, size.x * m.scale), (region.x as f32, region.w as f32), "{name}");
                assert_eq!(scale_for(region, &mons), m.scale, "{name}");
            }
        }
    }

    #[test]
    fn a_window_belongs_to_the_monitor_it_overlaps_most_or_the_nearest() {
        let mons = vec![mon(0, 0, 2560, 1440, 1.0, true), mon(2560, 0, 3840, 2160, 2.0, false)];
        assert_eq!(scale_for(PxRect::new(2400, 0, 400, 100), &mons), 2.0, "240 px on the 200% one, 160 on the other");
        assert_eq!(scale_for(PxRect::new(2400, 0, 300, 100), &mons), 1.0);
        assert_eq!(scale_for(PxRect::new(-500, -500, 10, 10), &mons), 1.0, "off every monitor: the nearest");
        assert_eq!(scale_for(PxRect::new(9000, 100, 10, 10), &mons), 2.0);
    }

    #[test]
    fn a_hidden_window_is_shown_once_in_place_or_out_of_moves() {
        let p = |moved, on_target| Placed { moved, tries: u8::from(moved), on_target };
        assert!(show_child(p(false, true), false));
        assert!(!show_child(p(true, true), false), "moved in this look: its content is still the old size");
        assert!(!show_child(p(false, false), false));
        assert!(show_child(p(true, false), true), "out of moves: shown where it is");
    }
}
