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

use crate::capture_log;
use crate::capture_window;
use crate::platform::{self, WindowId};

/// The toast's size in points.
pub const TOAST_SIZE: [f32; 2] = [380.0, 96.0];
/// The destination picker's size in points.
pub const PICKER_SIZE: [f32; 2] = [270.0, 262.0];
/// The recording control bar's size in points.
pub const BAR_SIZE: [f32; 2] = [680.0, 46.0];

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

/// A rectangle in points for a viewport builder, from pixels at `scale`. Only a first guess:
/// the window is moved onto its pixels by a [`Placer`] before it is shown.
pub fn guess_points(r: PxRect, scale: f32) -> (egui::Pos2, egui::Vec2) {
    (egui::pos2(r.x as f32 / scale, r.y as f32 / scale), egui::vec2(r.w as f32 / scale, r.h as f32 / scale))
}

/// Most `SetWindowPos` calls one window gets.
const MAX_MOVES: u8 = 6;

/// Looks one window gets in all, from the frame egui is first asked for it: then it is shown
/// (and uncloaked) wherever it is, and nothing about it is looked at or repainted for again.
pub const MAX_LOOKS: u8 = 12;

/// Whether and how a window takes the keyboard once shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    None,
    /// A plain request only: never a synthetic key press (a window over a recording).
    Plain,
    /// A plain request, then winit's (with its synthetic Alt key) when Windows refuses.
    PlainThenWinit,
}

/// Where a window is in being placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceStage {
    /// Hidden, being found and placed.
    Seeking,
    /// Shown, cloaked when Windows allowed it; put back on its pixels if a scale change on show
    /// resized it, uncloaked once a look finds it in place.
    Showing { moved_after_show: bool },
    /// Over: shown, or hidden after an uncloak that failed. Never looked at again.
    Done,
}

/// What one look found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Look {
    /// The window was found and its rectangles could be read.
    pub readable: bool,
    pub on_target: bool,
    /// It was moved in this look.
    pub moved: bool,
}

/// What to do after a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceStep {
    Wait,
    /// In place and not moved in this look: show it, cloaked.
    Show,
    /// Shown and found in place: uncloak. `put_back`: a scale change on show had resized it.
    Uncloak { put_back: bool },
    /// Out of looks: show it (or uncloak it) wherever it is, and stop.
    Anyway { shown: bool },
}

/// The decision after look number `looks` (1 for the first).
pub fn place_step(stage: PlaceStage, looks: u8, look: Look) -> PlaceStep {
    let in_place = look.readable && look.on_target && !look.moved;
    match stage {
        PlaceStage::Done => PlaceStep::Wait,
        PlaceStage::Seeking if in_place => PlaceStep::Show,
        PlaceStage::Showing { moved_after_show } if in_place => PlaceStep::Uncloak { put_back: moved_after_show },
        PlaceStage::Seeking if looks >= MAX_LOOKS => PlaceStep::Anyway { shown: false },
        PlaceStage::Showing { .. } if looks >= MAX_LOOKS => PlaceStep::Anyway { shown: true },
        _ => PlaceStep::Wait,
    }
}

/// The stage after a step.
/// Whether the window is asked to paint after this step: every step that shows or uncloaks it.
/// The repaints its own contents ask for are not relied on across those two moments.
pub fn paints_after(step: PlaceStep) -> bool {
    !matches!(step, PlaceStep::Wait)
}

pub fn place_advance(stage: PlaceStage, step: PlaceStep, look: Look) -> PlaceStage {
    match (stage, step) {
        (_, PlaceStep::Uncloak { .. } | PlaceStep::Anyway { .. }) => PlaceStage::Done,
        (PlaceStage::Seeking, PlaceStep::Show) => PlaceStage::Showing { moved_after_show: false },
        (PlaceStage::Showing { moved_after_show }, _) => PlaceStage::Showing { moved_after_show: moved_after_show || look.moved },
        (s, _) => s,
    }
}

/// Times a pinned window is put back on its pixels after it was shown.
pub const MAX_PIN_CORRECTIONS: u8 = 3;

/// What to do about a pinned window that is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinStep {
    Keep,
    /// Not on its pixels: put it back, the nth time.
    PutBack { n: u8 },
    /// Not on its pixels and the corrections are used up: left where it is (logged once).
    GiveUp,
}

/// `placed`: the first placement ended with the window on its pixels. One that was shown where it
/// was because placement gave up is left there: put-backs are for a window that was in place and
/// then moved.
pub fn pin_step(readable: bool, on_target: bool, corrections: u8, placed: bool) -> PinStep {
    if !readable || on_target || !placed {
        PinStep::Keep
    } else if corrections < MAX_PIN_CORRECTIONS {
        PinStep::PutBack { n: corrections + 1 }
    } else {
        PinStep::GiveUp
    }
}

/// Puts one of our windows, created hidden by egui, on its rectangle in pixels and shows it:
/// cloaked first, so a scale change Windows only delivers on show (winit then resizes the
/// window to keep its size in points) is undone before anyone sees it, and so its first frame
/// at the new size is never seen stale. Every look counts toward [`MAX_LOOKS`].
#[derive(Debug)]
pub struct Placer {
    title: &'static str,
    target: PxRect,
    hwnd: Option<WindowId>,
    stage: PlaceStage,
    looks: u8,
    moves: u8,
    cloaked: bool,
    /// Take the keyboard once uncloaked.
    focus: Focus,
    /// Kept on its pixels after it is shown (a window whose contents are laid out in pixels,
    /// not points), with the corrections made so far and whether giving up was logged.
    pinned: bool,
    corrections: u8,
    gave_up: bool,
    /// The placement ended with the window on its pixels (uncloaked in place, not shown anyway).
    placed: bool,
}

impl Placer {
    pub fn new(title: &'static str, target: PxRect, focus: Focus) -> Self {
        Self { title, target, hwnd: None, stage: PlaceStage::Seeking, looks: 0, moves: 0, cloaked: false, focus, pinned: false, corrections: 0, gave_up: false, placed: false }
    }

    /// Keep the window on its pixels once shown: a scale change afterwards, which winit answers
    /// by keeping its size in points, is put back (at most [`MAX_PIN_CORRECTIONS`] times).
    pub fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    /// Each frame after the viewport `id` was asked for. Its window appears at the end of the
    /// frame it was first asked for, so the first look may find nothing.
    pub fn tick(&mut self, ctx: &egui::Context, id: egui::ViewportId) {
        if self.stage == PlaceStage::Done {
            if self.pinned {
                self.keep_pinned(ctx, id);
            }
            return;
        }
        self.looks = self.looks.saturating_add(1);
        // A hidden window is found by its title; once shown, it is the handle found before.
        if self.hwnd.is_none() && self.stage == PlaceStage::Seeking {
            self.hwnd = platform::find_own_hidden_window(self.title);
        }
        let mut look = Look::default();
        if let Some(hwnd) = self.hwnd {
            let placed = capture_window::place(hwnd, self.target, MAX_MOVES.saturating_sub(self.moves).min(capture_window::TRIES));
            self.moves += placed.tries;
            look = Look { readable: placed.readable, on_target: placed.on_target, moved: placed.moved };
            if !placed.readable {
                // Gone or made again: looked up afresh, within the same looks.
                self.hwnd = None;
            }
        }
        let step = place_step(self.stage, self.looks, look);
        match step {
            PlaceStep::Wait => {}
            PlaceStep::Show => {
                self.cloaked = self.hwnd.is_some_and(|h| platform::set_cloaked(h, true));
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
            }
            PlaceStep::Uncloak { put_back } => {
                self.placed = true;
                self.finish(ctx, id, if put_back { "placed; scale changed on show, put back" } else { "placed" });
            }
            PlaceStep::Anyway { shown } => {
                if !shown {
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
                }
                let why = if self.hwnd.is_some() { "NOT in place in time, shown where it is" } else { "window not found or lost, shown where egui put it" };
                self.finish(ctx, id, why);
            }
        }
        if paints_after(step) {
            // Its contents ask for their own repaints, but a request made around the show or the
            // uncloak can go unanswered and end that chain: ask once more from here.
            ctx.request_repaint_of(id);
        }
        self.stage = place_advance(self.stage, step, look);
        if self.stage != PlaceStage::Done {
            // Look again soon: the window waits for this to appear.
            ctx.request_repaint();
        }
    }

    /// A pinned window on screen: put back on its pixels if something moved or sized it.
    fn keep_pinned(&mut self, ctx: &egui::Context, id: egui::ViewportId) {
        let Some(hwnd) = self.hwnd else { return };
        let Some((_, client)) = platform::window_rects(hwnd) else { return };
        match pin_step(true, client == self.target, self.corrections, self.placed) {
            PinStep::Keep => {}
            PinStep::PutBack { n } => {
                let before = capture_window::state(hwnd);
                self.corrections = n;
                let placed = capture_window::place(hwnd, self.target, capture_window::TRIES);
                ctx.request_repaint_of(id);
                let why = format!("moved after show, put back (correction {n} of {MAX_PIN_CORRECTIONS}); was {}", before.map_or("-".into(), |w| capture_log::px(w.client)));
                capture_log::append(&[capture_log::placed_line(&capture_log::now(), self.title, self.target, capture_window::state(hwnd), placed.tries, self.looks, &why)]);
            }
            PinStep::GiveUp => {
                if !std::mem::replace(&mut self.gave_up, true) {
                    capture_log::append(&[capture_log::placed_line(&capture_log::now(), self.title, self.target, capture_window::state(hwnd), 0, self.looks, "moved after show, corrections used up, left where it is")]);
                }
            }
        }
    }

    /// Uncloak, take the keyboard if asked, and log the one line for this window.
    fn finish(&mut self, ctx: &egui::Context, id: egui::ViewportId, why: &str) {
        let mut why = why.to_string();
        let cloaked = std::mem::take(&mut self.cloaked);
        if cloaked && let Some(hwnd) = self.hwnd && !platform::set_cloaked(hwnd, false) {
            // Never an invisible window left on top: hidden instead.
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(false));
            why = format!("UNCLOAK FAILED, hidden ({why})");
        } else if let Some(hwnd) = self.hwnd
            && self.focus != Focus::None
            && !platform::take_foreground(hwnd)
            && self.focus == Focus::PlainThenWinit
        {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        }
        if let Some(hwnd) = self.hwnd {
            why = format!("{why} · {}", platform::composition_note(hwnd));
        }
        let now = self.hwnd.and_then(capture_window::state);
        capture_log::append(&[capture_log::placed_line(&capture_log::now(), self.title, self.target, now, self.moves, self.looks, &why)]);
    }
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
        // Scales between 100% and 200%, where a size in points times the scale is not whole.
        out.push(("125% primary, 150% left, 100% right".into(), vec![mon(0, 0, 2560, 1440, 1.25, true), mon(-1920, 0, 1920, 1080, 1.5, false), mon(2560, 0, 2560, 1440, 1.0, false)]));
        out.push(("150% primary, 125% right".into(), vec![mon(0, 0, 1920, 1080, 1.5, true), mon(-2560, 0, 2560, 1440, 1.0, false), mon(1920, 0, 2560, 1440, 1.25, false)]));
        out
    }

    /// Points times a scale, rounded to whole pixels as the code does.
    fn rnd(v: f32) -> i32 {
        v.round() as i32
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
            assert_eq!((t.w, t.h), (rnd(380.0 * s) as u32, rnd(96.0 * s) as u32), "{name}: the toast's size at its monitor's scale");
            assert_eq!((t.right(), t.bottom()), (p.rect.right() - rnd(16.0 * s), p.rect.bottom() - rnd(64.0 * s)), "{name}: its corner");
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
                assert_eq!((r.x, r.y), (c.0 + rnd(12.0 * s), c.1 + rnd(12.0 * s)), "{name}");
                assert_eq!((r.w, r.h), (rnd(270.0 * s) as u32, rnd(262.0 * s) as u32), "{name}");
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
    fn the_bar_goes_around_a_region_in_its_monitors_scale() {
        for (name, mons) in layouts() {
            for m in &mons {
                let s = m.scale;
                let region = PxRect::new(m.rect.x + 300, m.rect.y + 400, 800, 500);
                let bar = bar_rect(region, m.rect, s);
                assert_eq!((bar.w, bar.h), (rnd(680.0 * s) as u32, rnd(46.0 * s) as u32), "{name}: the bar's size in its monitor's scale");
                assert_eq!(bar.bottom(), region.y - rnd(10.0 * s), "{name}: above the area");
                assert!(inside(bar, m.rect) && scale_for(bar, &mons) == s, "{name}: on the area's monitor {bar:?}");
                // A whole monitor: the bar goes inside its top edge.
                let bar = bar_rect(m.rect, m.rect, s);
                assert_eq!(bar.y, m.rect.y + rnd(10.0 * s), "{name}");
                assert!(inside(bar, m.rect));
            }
            // A region dragged across two monitors is clipped to the one holding its centre
            // (the recorder's rule); its bar follows that monitor's scale.
            let (a, b) = (&mons[1], &mons[2]);
            if a.rect.right() == b.rect.x {
                let across = PxRect::new(a.rect.right() - 300, a.rect.y + 100, 400, 300);
                let centre = (across.x + 200, across.y + 150);
                let on = mons.iter().find(|m| m.rect.contains(centre.0, centre.1)).unwrap();
                let clipped = across.intersect(&on.rect).unwrap();
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

    /// Runs the placement decisions over looks; returns the steps.
    fn looks(seq: &[Look]) -> Vec<PlaceStep> {
        let mut stage = PlaceStage::Seeking;
        seq.iter()
            .enumerate()
            .map(|(i, l)| {
                let s = place_step(stage, i as u8 + 1, *l);
                stage = place_advance(stage, s, *l);
                s
            })
            .collect()
    }

    const NONE: Look = Look { readable: false, on_target: false, moved: false };
    const IN: Look = Look { readable: true, on_target: true, moved: false };
    const MOVED: Look = Look { readable: true, on_target: true, moved: true };

    #[test]
    fn a_window_is_shown_cloaked_once_in_place_and_uncloaked_at_the_next_look() {
        // Not there yet, then moved onto its pixels, then in place: shown; in place: uncloaked.
        assert_eq!(looks(&[NONE, MOVED, IN, IN]), [PlaceStep::Wait, PlaceStep::Wait, PlaceStep::Show, PlaceStep::Uncloak { put_back: false }]);
    }

    #[test]
    fn a_scale_change_on_show_is_put_back_before_the_window_is_uncloaked() {
        // Shown; on show winit resized it to keep its size in points, so it was moved back to
        // its pixels in that look; the next look finds it in place.
        assert_eq!(looks(&[IN, MOVED, IN]), [PlaceStep::Show, PlaceStep::Wait, PlaceStep::Uncloak { put_back: true }]);
        // No change on show.
        assert_eq!(looks(&[IN, IN]), [PlaceStep::Show, PlaceStep::Uncloak { put_back: false }]);
    }

    #[test]
    fn every_look_counts_and_a_lost_window_ends_shown_within_the_limit() {
        // Found once, then its handle goes bad: every look after that reads nothing.
        let mut seq = vec![MOVED];
        seq.extend(std::iter::repeat_n(NONE, MAX_LOOKS as usize - 1));
        let steps = looks(&seq);
        assert_eq!(steps[MAX_LOOKS as usize - 1], PlaceStep::Anyway { shown: false }, "shown wherever egui put it at the last look");
        assert!(steps[..MAX_LOOKS as usize - 1].iter().all(|s| *s == PlaceStep::Wait));
        // Lost after it was shown: uncloaked where it is at the limit.
        let mut seq = vec![IN];
        seq.extend(std::iter::repeat_n(NONE, MAX_LOOKS as usize - 1));
        assert_eq!(*looks(&seq).last().unwrap(), PlaceStep::Anyway { shown: true });
        // After that nothing more is decided, so nothing more is repainted for.
        let mut stage = PlaceStage::Seeking;
        for (i, l) in seq.iter().enumerate() {
            let s = place_step(stage, i as u8 + 1, *l);
            stage = place_advance(stage, s, *l);
        }
        assert_eq!(stage, PlaceStage::Done);
        assert_eq!(place_step(stage, 200, IN), PlaceStep::Wait);
        assert_eq!(place_advance(stage, PlaceStep::Wait, IN), PlaceStage::Done);
    }

    #[test]
    fn every_step_that_shows_or_uncloaks_a_window_asks_it_to_paint() {
        assert!(!paints_after(PlaceStep::Wait));
        for step in [PlaceStep::Show, PlaceStep::Uncloak { put_back: false }, PlaceStep::Uncloak { put_back: true }, PlaceStep::Anyway { shown: false }, PlaceStep::Anyway { shown: true }] {
            assert!(paints_after(step), "{step:?}");
        }
    }

    #[test]
    fn a_pinned_window_moved_after_show_is_put_back_a_bounded_number_of_times() {
        assert_eq!(pin_step(true, true, 0, true), PinStep::Keep);
        assert_eq!(pin_step(false, false, 0, true), PinStep::Keep, "nothing to read: nothing to do");
        assert_eq!(pin_step(true, false, 0, false), PinStep::Keep, "shown off its pixels because placement gave up: left there");
        let mut corrections = 0;
        let mut steps = Vec::new();
        for _ in 0..6 {
            let s = pin_step(true, false, corrections, true);
            if let PinStep::PutBack { n } = s {
                corrections = n;
            }
            steps.push(s);
        }
        assert_eq!(steps[..3], [PinStep::PutBack { n: 1 }, PinStep::PutBack { n: 2 }, PinStep::PutBack { n: 3 }]);
        assert!(steps[3..].iter().all(|s| *s == PinStep::GiveUp));
        assert!(Placer::new("t", PxRect::new(0, 0, 10, 10), Focus::None).pinned().pinned);
        assert!(!Placer::new("t", PxRect::new(0, 0, 10, 10), Focus::None).pinned, "only the live draw layer is pinned");
    }

    #[test]
    fn a_window_that_never_settles_stops_at_the_limit() {
        let seq = vec![MOVED; MAX_LOOKS as usize];
        let steps = looks(&seq);
        assert_eq!(steps.last(), Some(&PlaceStep::Anyway { shown: false }));
        assert_eq!(steps.iter().filter(|s| **s != PlaceStep::Wait).count(), 1);
    }
}
