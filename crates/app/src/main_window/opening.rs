//! The size and place the main window opens at, and remembering the size the user gives it.
//!
//! The window opens at the size the user last gave it by hand, or at [`super::SIZE`] when there is
//! none, made to fit the work area of the monitor it opens on (the primary monitor: Windows
//! creates a window given no monitor there) with its title bar and borders on screen, and
//! centred in it. Sizes are in points, so Windows scaling is in them already: a 1920×1080
//! monitor at 150% is a work area of about 1280×672 points.
//!
//! Two kinds of points meet here. Windows' points are pixels at the monitor's scale (96 DPI is
//! 100%): the work area, the window's frame, the size saved and the size read back from Windows
//! are in them. egui's points also carry egui's zoom (Ctrl+= and Ctrl+-, for the whole app, for
//! the run): egui-winit gives a window `zoom × scale` pixels per point, and multiplies the inner
//! size, the smallest size and the position a viewport builder asks for by that. So the fitting
//! is done in Windows' points and the result divided by the zoom for the builder; the default
//! size, designed in egui's points, is multiplied by it first, so the pages fit it at any zoom.
//!
//! What is remembered is the size the user chose, never the size one opening was cut down to:
//! a size saved on a large monitor opens fitted on a small one, and is still the size saved.
//! Only a size that differs from the size the window opened at (or was last saved at) counts as
//! a resize by hand; it is saved once it has stayed the same for [`SETTLE`], or when the window
//! closes, never on every frame of a drag.
//!
//! The size watched is read from Windows (see [`NormalSize`]), and only while the window is in its
//! normal state: never while it is maximized, minimized or snapped, so none of those sizes is
//! saved, and the window never opens maximized or snapped. A size made by hand before is kept
//! meanwhile, and saved when it settles, when the window closes, or when the app quits.

use eframe::egui;

use super::{MIN_SIZE, SIZE};

/// The window's frame around its inner size, in points: the borders (with the invisible resize
/// edges Windows counts in a window's outer rectangle) and the title bar. Windows scales these
/// with the monitor's scale, as it does points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// A standard window's frame on Windows 10 and 11 at 100%: 8 px of border on the sides and
/// below, 31 px of title bar and border above (what `AdjustWindowRectExForDpi` gives for
/// `WS_OVERLAPPEDWINDOW` at 96 DPI).
pub const FRAME: Frame = Frame { left: 8.0, top: 31.0, right: 8.0, bottom: 8.0 };

/// Room kept free around the window, inside the work area, in points.
pub const SCREEN_MARGIN: f32 = 8.0;

/// The largest size a saved value may hold before it is taken for nonsense, in points: bigger
/// than any monitor's work area at 100%.
pub const MAX_REMEMBERED: f32 = 16384.0;

/// How long a size made by hand stays the same before it is saved, in seconds.
pub const SETTLE: f64 = 0.5;

/// Within this many points of the size the window opened at (or was last saved at), a size is
/// that size: what Windows reports back differs from what was asked by the rounding to whole
/// pixels.
pub const SAME_SIZE: f32 = 2.0;

/// Where and how big the window opens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Opening {
    /// Its outer top-left corner, in egui's points, for the viewport builder.
    pub pos: Option<egui::Pos2>,
    /// Its inner size in egui's points, for the viewport builder.
    pub size: egui::Vec2,
    /// Its inner size in Windows' points: what is saved and what Windows reports back.
    pub window: egui::Vec2,
}

/// The inner size and outer position for a window that wants `want` points inside, on a work
/// area `work`, all in the same points. The size is cut down to what fits with `frame` and
/// `margin` around it (never below `min`, the smallest the window can be), and the window is
/// centred. When even `min` does not fit, it goes to the work area's top-left corner, so its
/// title bar is on screen and it can be moved, and the rest runs off the right or bottom.
pub fn fit(want: egui::Vec2, work: egui::Rect, frame: Frame, margin: f32, min: egui::Vec2) -> (egui::Pos2, egui::Vec2) {
    let want = if want.x.is_finite() && want.y.is_finite() { want } else { egui::vec2(SIZE[0], SIZE[1]) };
    let room = egui::vec2(work.width() - 2.0 * margin - frame.left - frame.right, work.height() - 2.0 * margin - frame.top - frame.bottom);
    // NaN never reaches max/min here: a work area that is not finite leaves `want` as it is.
    let room = egui::vec2(if room.x.is_finite() { room.x } else { want.x }, if room.y.is_finite() { room.y } else { want.y });
    let size = want.min(room).max(min);
    let outer = size + egui::vec2(frame.left + frame.right, frame.top + frame.bottom);
    let left = work.left() + ((work.width() - outer.x) / 2.0).max(0.0);
    let top = work.top() + ((work.height() - outer.y) / 2.0).max(0.0);
    let pos = egui::pos2(if left.is_finite() { left } else { 0.0 }, if top.is_finite() { top } else { 0.0 });
    (pos, size)
}

/// A work area in desktop pixels at a monitor's DPI (96 is 100%), in points.
pub fn work_area_points(x: i32, y: i32, w: u32, h: u32, dpi: u32) -> Option<egui::Rect> {
    if dpi == 0 || w == 0 || h == 0 {
        return None;
    }
    let s = dpi as f32 / 96.0;
    Some(egui::Rect::from_min_size(egui::pos2(x as f32 / s, y as f32 / s), egui::vec2(w as f32 / s, h as f32 / s)))
}

/// The saved size, if it can be used: a size that is not a number, or is zero, negative or
/// larger than [`MAX_REMEMBERED`], is ignored (the window opens at the default); one smaller than
/// [`MIN_SIZE`] is raised to it.
pub fn remembered(saved: Option<[f32; 2]>) -> Option<egui::Vec2> {
    let [w, h] = saved?;
    let usable = |v: f32| v.is_finite() && v > 0.0 && v <= MAX_REMEMBERED;
    (usable(w) && usable(h)).then(|| egui::vec2(w.max(MIN_SIZE[0]), h.max(MIN_SIZE[1])))
}

/// The opening for a window whose saved size is `saved` (in Windows' points), on the work area
/// `work` (in Windows' points; None when it could not be read: the size unfitted, and Windows
/// picks the place), with egui's zoom at `zoom`.
pub fn opening(saved: Option<[f32; 2]>, work: Option<egui::Rect>, zoom: f32) -> Opening {
    let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
    // egui-winit makes the smallest size `MIN_SIZE` egui points, so this many of Windows'.
    let min = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]) * zoom;
    let want = remembered(saved).unwrap_or(egui::vec2(SIZE[0], SIZE[1]) * zoom).max(min);
    let (pos, window) = match work {
        Some(work) => {
            let (pos, size) = fit(want, work, FRAME, SCREEN_MARGIN, min);
            (Some(pos), size)
        }
        None => (None, want),
    };
    Opening { pos: pos.map(|p| egui::pos2(p.x / zoom, p.y / zoom)), size: window / zoom, window }
}

/// Where the window's size comes from: its inner size in Windows' points while it is in its
/// normal state, and nothing while it is maximized, minimized or snapped. The app reads it from
/// Windows ([`WindowsNormalSize`]); tests give their own.
pub trait NormalSize: Send {
    /// The size now, for the window egui describes in `info` at `ppp` pixels per point; None
    /// when the window is not in its normal state or cannot be read.
    fn read(&mut self, info: &egui::ViewportInfo, ppp: f32) -> Option<egui::Vec2>;
    /// The window is about to be made anew: anything known about the old one is dropped.
    fn forget(&mut self) {}
}

/// The size as Windows reports it: the window's client size at its DPI, in Windows' points, read
/// only in its normal state ([`crate::platform::normal_client_px`]). The window is this process's
/// visible window titled [`super::TITLE`] whose client area is the size egui reports for the
/// viewport; found once per opening, and looked for again whenever the handle kept is no longer
/// a window of this process with that title.
#[derive(Debug, Default)]
pub struct WindowsNormalSize {
    window: Option<crate::platform::WindowId>,
}

impl NormalSize for WindowsNormalSize {
    fn read(&mut self, info: &egui::ViewportInfo, ppp: f32) -> Option<egui::Vec2> {
        if self.window.is_some_and(|w| !crate::platform::is_own_window_titled(w, super::TITLE)) {
            self.window = None;
        }
        if self.window.is_none() {
            let client = info.inner_rect?.size() * ppp;
            self.window = crate::platform::find_visible_own_window(super::TITLE, (client.x.round() as i32, client.y.round() as i32), 2);
        }
        points(crate::platform::normal_client_px(self.window?)?)
    }

    fn forget(&mut self) {
        self.window = None;
    }
}

/// A size in pixels at a DPI (96 is 100%), in points.
pub fn points((w, h, dpi): (i32, i32, u32)) -> Option<egui::Vec2> {
    (dpi > 0 && w > 0 && h > 0).then(|| egui::vec2(w as f32, h as f32) * 96.0 / dpi as f32)
}

/// Watches the open window's normal size and says when a size made by hand is to be saved.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SizeMemory {
    /// The size the window opened at, or was last saved at: a size near it is not a resize.
    /// None while the window is closed.
    baseline: Option<egui::Vec2>,
    /// A size made by hand that is not saved yet, and when it was first seen at that size.
    pending: Option<(egui::Vec2, f64)>,
}

fn same(a: egui::Vec2, b: egui::Vec2) -> bool {
    (a.x - b.x).abs() <= SAME_SIZE && (a.y - b.y).abs() <= SAME_SIZE
}

impl SizeMemory {
    /// The window opens at `size`.
    pub fn open(&mut self, size: egui::Vec2) {
        *self = SizeMemory { baseline: Some(size), pending: None };
    }

    pub fn is_open(&self) -> bool {
        self.baseline.is_some()
    }

    /// Whether a size is waiting to settle, so the window should be drawn again to see it settle.
    pub fn is_waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// One frame at time `now` (seconds), with the window's normal size as read (None when it
    /// could not be): the size to save, once a size made by hand has settled.
    pub fn observe(&mut self, normal: Option<egui::Vec2>, now: f64) -> Option<egui::Vec2> {
        let baseline = self.baseline?;
        let size = normal.filter(|s| s.x.is_finite() && s.y.is_finite() && s.x > 0.0 && s.y > 0.0)?;
        match self.pending {
            _ if same(size, baseline) => {
                self.pending = None;
                None
            }
            // Still: saved once it has stayed so for the settle time. Any change at all, however
            // small, is the drag going on, and the time starts again.
            Some((p, since)) if size == p => (now - since >= SETTLE).then(|| self.saved(p)),
            _ => {
                self.pending = Some((size, now));
                None
            }
        }
    }

    /// The size made by hand that has not settled yet, if any, leaving the window open.
    pub fn unsettled(&self) -> Option<egui::Vec2> {
        self.pending.map(|(p, _)| p)
    }

    /// The window closes: the size made by hand that had not settled yet, if any.
    pub fn close(&mut self) -> Option<egui::Vec2> {
        let pending = self.unsettled();
        *self = SizeMemory::default();
        pending
    }

    fn saved(&mut self, size: egui::Vec2) -> egui::Vec2 {
        self.baseline = Some(size);
        self.pending = None;
        size
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const TASKBAR: f32 = 48.0;

    /// A monitor of `w`×`h` pixels at `scale`, with the taskbar along its bottom, as a work area
    /// in points (Windows draws the taskbar 48 points high).
    pub(crate) fn work(w: u32, h: u32, scale: f32) -> egui::Rect {
        let taskbar_px = (TASKBAR * scale).round() as u32;
        work_area_points(0, 0, w, h - taskbar_px, (scale * 96.0).round() as u32).unwrap()
    }

    pub(crate) fn monitors() -> Vec<(String, egui::Rect)> {
        let mut out = Vec::new();
        for (w, h, scales) in [(1920u32, 1080u32, &[1.0f32, 1.25, 1.5, 1.75][..]), (2560, 1440, &[1.0, 1.25, 1.5][..]), (3840, 2160, &[1.0, 1.5, 2.0, 2.5][..])] {
            for &s in scales {
                out.push((format!("{w}x{h} at {}%", s * 100.0), work(w, h, s)));
            }
        }
        out
    }

    fn outer(pos: egui::Pos2, size: egui::Vec2) -> egui::Rect {
        egui::Rect::from_min_size(pos, size + egui::vec2(FRAME.left + FRAME.right, FRAME.top + FRAME.bottom))
    }

    fn default() -> egui::Vec2 {
        egui::vec2(SIZE[0], SIZE[1])
    }

    #[test]
    fn the_default_opens_whole_where_it_fits_and_cut_to_the_work_area_where_not() {
        let mut cut = Vec::new();
        for (name, work) in monitors() {
            let o = opening(None, Some(work), 1.0);
            let pos = o.pos.unwrap();
            let r = outer(pos, o.size);
            assert!(work.shrink(SCREEN_MARGIN - 0.01).contains_rect(r), "{name}: {r:?} with its frame and margin inside {work:?}");
            assert!(o.size.x >= MIN_SIZE[0] && o.size.y >= MIN_SIZE[1] && o.size.x <= SIZE[0] && o.size.y <= SIZE[1], "{name}: {:?}", o.size);
            // Centred.
            assert!((r.center().x - work.center().x).abs() < 0.01 && (r.center().y - work.center().y).abs() < 0.01, "{name}");
            // Cut only where it does not fit, and then to exactly what fits.
            let room = work.size() - egui::vec2(2.0 * SCREEN_MARGIN + FRAME.left + FRAME.right, 2.0 * SCREEN_MARGIN + FRAME.top + FRAME.bottom);
            assert_eq!(o.size, default().min(room), "{name}");
            if o.size != default() {
                cut.push((name, o.size));
            }
        }
        // 1080p at 150% and 175% are too low for the default; every other monitor takes it whole.
        let names: Vec<&str> = cut.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["1920x1080 at 150%", "1920x1080 at 175%"], "{cut:?}");
        // 1080p at 150%: 1280×672 points; the full width, 617 points high.
        let o = opening(None, Some(work(1920, 1080, 1.5)), 1.0);
        assert_eq!(o.size.x, SIZE[0]);
        assert!((o.size.y - (672.0 - 16.0 - 39.0)).abs() < 0.01, "{:?}", o.size);
    }

    #[test]
    fn a_work_area_smaller_than_the_smallest_window_gets_the_smallest_window_at_its_corner() {
        let work = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(500.0, 300.0));
        let o = opening(None, Some(work), 1.0);
        assert_eq!(o.size, egui::vec2(MIN_SIZE[0], MIN_SIZE[1]), "never smaller than the window can be");
        assert_eq!(o.pos, Some(work.min), "its title bar on screen");
        // Nothing at all, or nonsense: no panic, a usable size.
        for work in [egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::ZERO), egui::Rect::from_min_size(egui::pos2(-10.0, -10.0), egui::vec2(-50.0, -50.0)), egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(f32::NAN, f32::INFINITY))] {
            let o = opening(None, Some(work), 1.0);
            assert!(o.size.x >= MIN_SIZE[0] && o.size.y >= MIN_SIZE[1] && o.size.x.is_finite() && o.size.y.is_finite(), "{work:?}: {o:?}");
            assert!(o.pos.unwrap().x.is_finite() && o.pos.unwrap().y.is_finite(), "{work:?}: {o:?}");
        }
        assert_eq!(work_area_points(0, 0, 0, 100, 96), None);
        assert_eq!(work_area_points(0, 0, 100, 100, 0), None);
        // No work area: the size as wanted, and Windows picks the place.
        assert_eq!(opening(None, None, 1.0), Opening { pos: None, size: default(), window: default() });
    }

    #[test]
    fn a_work_area_off_the_origin_keeps_the_window_on_it() {
        // A taskbar on the left or top moves the work area's corner.
        let work = egui::Rect::from_min_size(egui::pos2(48.0, 40.0), egui::vec2(1872.0, 1040.0));
        let o = opening(None, Some(work), 1.0);
        assert!(work.contains_rect(outer(o.pos.unwrap(), o.size)));
    }

    #[test]
    fn a_saved_size_is_used_only_when_it_can_be() {
        assert_eq!(remembered(None), None);
        assert_eq!(remembered(Some([1000.0, 800.0])), Some(egui::vec2(1000.0, 800.0)));
        for bad in [[f32::NAN, 700.0], [700.0, f32::INFINITY], [f32::NEG_INFINITY, 700.0], [0.0, 700.0], [800.0, 0.0], [-900.0, 700.0], [900.0, -1.0], [1e9, 700.0], [800.0, MAX_REMEMBERED + 1.0]] {
            assert_eq!(remembered(Some(bad)), None, "{bad:?}");
            assert_eq!(opening(Some(bad), None, 1.0).size, default(), "{bad:?}: the default");
        }
        // Smaller than the window can be: raised to that.
        assert_eq!(remembered(Some([100.0, 50.0])), Some(egui::vec2(MIN_SIZE[0], MIN_SIZE[1])));
        assert_eq!(remembered(Some([700.0, 50.0])), Some(egui::vec2(700.0, MIN_SIZE[1])));
    }

    /// A size saved on a large monitor opens fitted on every monitor of the table, and a small one
    /// opens as it is.
    #[test]
    fn a_remembered_size_is_fitted_to_the_monitor_like_the_default() {
        let big = [1800.0, 1000.0];
        let small = [700.0, 480.0];
        for (name, work) in monitors() {
            let o = opening(Some(big), Some(work), 1.0);
            assert!(work.shrink(SCREEN_MARGIN - 0.01).contains_rect(outer(o.pos.unwrap(), o.size)), "{name}: {o:?}");
            let room = work.size() - egui::vec2(2.0 * SCREEN_MARGIN + FRAME.left + FRAME.right, 2.0 * SCREEN_MARGIN + FRAME.top + FRAME.bottom);
            assert_eq!(o.size, egui::vec2(big[0], big[1]).min(room).max(egui::vec2(MIN_SIZE[0], MIN_SIZE[1])), "{name}");
            assert_eq!(opening(Some(small), Some(work), 1.0).size, egui::vec2(small[0], small[1]), "{name}");
        }
    }

    /// Frames 1/60 s apart from `t`, each reading the normal size `normal`; what each said to save.
    fn frames(m: &mut SizeMemory, t: &mut f64, n: usize, normal: Option<egui::Vec2>) -> Vec<egui::Vec2> {
        (0..n)
            .filter_map(|_| {
                *t += 1.0 / 60.0;
                m.observe(normal, *t)
            })
            .collect()
    }

    fn sz(w: f32, h: f32) -> Option<egui::Vec2> {
        Some(egui::vec2(w, h))
    }

    #[test]
    fn a_drag_saves_once_when_the_size_settles_not_on_every_frame() {
        let mut m = SizeMemory::default();
        let mut t = 0.0;
        assert_eq!(m.observe(sz(900.0, 700.0), 0.0), None, "a closed window saves nothing");
        m.open(default());
        // The size Windows reports back, a pixel off what was asked: not a resize.
        assert!(frames(&mut m, &mut t, 120, sz(SIZE[0] + 0.8, SIZE[1] - 0.6)).is_empty());
        // A drag over two seconds, one size a frame.
        let mut saved = Vec::new();
        for i in 0..120 {
            t += 1.0 / 60.0;
            saved.extend(m.observe(sz(SIZE[0] + 2.0 * i as f32, SIZE[1] + i as f32), t));
        }
        assert!(saved.is_empty(), "nothing while it moves: {saved:?}");
        // Let go: saved once, after it has stayed for the settle time.
        let end = sz(SIZE[0] + 238.0, SIZE[1] + 119.0);
        assert_eq!(frames(&mut m, &mut t, 120, end), vec![end.unwrap()]);
        assert!(!m.is_waiting());
        // A drag that stops at C for a third of a second and goes on to B: only B is saved.
        let (c, b) = (sz(1000.0, 800.0), sz(1100.0, 820.0));
        assert!(frames(&mut m, &mut t, 20, c).is_empty());
        assert_eq!(frames(&mut m, &mut t, 60, b), vec![b.unwrap()]);
        // Closed afterwards: nothing more.
        assert_eq!(m.close(), None);
        assert!(!m.is_open());
    }

    #[test]
    fn a_size_not_settled_yet_is_saved_when_the_window_closes() {
        let mut m = SizeMemory::default();
        m.open(default());
        assert_eq!(m.observe(sz(1000.0, 760.0), 1.0), None);
        assert!(m.is_waiting());
        assert_eq!(m.unsettled(), Some(egui::vec2(1000.0, 760.0)), "what the app saves if it quits now");
        assert_eq!(m.close(), Some(egui::vec2(1000.0, 760.0)));
        // Dragged back to where it opened before closing: nothing to save.
        m.open(default());
        m.observe(sz(1000.0, 760.0), 1.0);
        m.observe(sz(SIZE[0], SIZE[1]), 1.1);
        assert_eq!((m.unsettled(), m.close()), (None, None));
    }

    #[test]
    fn a_window_opened_cut_down_saves_nothing_until_resized_by_hand() {
        // Saved 1800×1000 on a large monitor; opened on 1080p at 150%, cut to what fits.
        let o = opening(Some([1800.0, 1000.0]), Some(work(1920, 1080, 1.5)), 1.0);
        let mut m = SizeMemory::default();
        m.open(o.size);
        let mut t = 0.0;
        assert!(frames(&mut m, &mut t, 120, Some(o.size)).is_empty(), "the fitted size is not the user's");
        assert_eq!(m.close(), None);
    }

    /// The memory decides on the sizes it is handed, nothing else. While nothing can be read
    /// (the window maximized, minimized or snapped, or not found), a size made by hand before
    /// stays pending: it is neither dropped nor saved early, it settles once readings come back
    /// at it, and the window's close hands it over.
    #[test]
    fn a_size_pending_when_readings_stop_is_kept_until_they_come_back_or_the_window_closes() {
        let a = sz(1000.0, 760.0);
        let mut m = SizeMemory::default();
        m.open(default());
        let mut t = 0.0;
        // Resized to A, then a tenth of a second later nothing can be read for a long while.
        assert!(frames(&mut m, &mut t, 6, a).is_empty());
        assert!(frames(&mut m, &mut t, 300, None).is_empty(), "never saved while nothing is read");
        assert_eq!(m.unsettled(), a, "still pending");
        // Readings back at A (unsnapped, restored): A settles, once.
        assert_eq!(frames(&mut m, &mut t, 60, a), vec![a.unwrap()]);
        assert_eq!(m.close(), None);
        // Resized to A, nothing read, closed: A is handed over.
        m.open(default());
        assert!(frames(&mut m, &mut t, 6, a).is_empty());
        assert!(frames(&mut m, &mut t, 60, None).is_empty());
        assert_eq!(m.close(), a);
        // Never resized, nothing read in between: nothing at all.
        m.open(default());
        assert!(frames(&mut m, &mut t, 60, None).is_empty());
        assert!(frames(&mut m, &mut t, 60, Some(default())).is_empty());
        assert_eq!(m.close(), None);
    }

    /// With egui's zoom at 1.1, 1.25 or 0.8, the window fitted to each monitor still fits its
    /// work area once egui-winit has multiplied the builder's size and position by the zoom; it
    /// is centred; it is never smaller than egui-winit lets it be; and the default holds the
    /// pages at that zoom (its size in egui's points is the default) wherever there is room.
    #[test]
    fn the_fit_is_right_under_egui_zoom() {
        for zoom in [0.8f32, 1.1, 1.25] {
            for (name, work) in monitors() {
                let o = opening(None, Some(work), zoom);
                // What egui-winit asks Windows for, in Windows' points.
                let (pos, size) = (o.pos.unwrap().to_vec2() * zoom, o.size * zoom);
                assert!((size - o.window).length() < 0.01, "{name} at {zoom}");
                let r = outer(pos.to_pos2(), size);
                // On each axis: inside the work area with the margin, or as small as egui-winit
                // lets the window be, and then from the work area's start, title bar on screen.
                let min = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]) * zoom;
                let inner = work.shrink(SCREEN_MARGIN - 0.01);
                let fits_x = r.left() >= inner.left() && r.right() <= inner.right();
                let fits_y = r.top() >= inner.top() && r.bottom() <= inner.bottom();
                assert!(fits_x || ((size.x - min.x).abs() < 0.01 && r.left() >= work.left() - 0.01), "{name} at {zoom}: {r:?} in {work:?}");
                assert!(fits_y || ((size.y - min.y).abs() < 0.01 && r.top() >= work.top() - 0.01), "{name} at {zoom}: {r:?} in {work:?}");
                // Centred where it fits.
                assert!((r.center() - work.center()).length() < 0.01 || !(fits_x && fits_y), "{name} at {zoom}: centred");
                assert!(o.size.x >= MIN_SIZE[0] - 0.01 && o.size.y >= MIN_SIZE[1] - 0.01, "{name} at {zoom}: {:?}", o.size);
                let room = work.size() - egui::vec2(2.0 * SCREEN_MARGIN + FRAME.left + FRAME.right, 2.0 * SCREEN_MARGIN + FRAME.top + FRAME.bottom);
                if room.x >= SIZE[0] * zoom && room.y >= SIZE[1] * zoom {
                    assert!((o.size - default()).length() < 0.01, "{name} at {zoom}: {:?}", o.size);
                }
            }
            // A saved size is a size on screen: the same in Windows' points at any zoom.
            let o = opening(Some([1000.0, 780.0]), Some(work(3840, 2160, 1.5)), zoom);
            assert!((o.window - egui::vec2(1000.0, 780.0)).length() < 0.01 && (o.size * zoom - o.window).length() < 0.01, "{zoom}: {o:?}");
        }
        // A zoom that is no number is taken as none.
        assert_eq!(opening(None, None, f32::NAN), opening(None, None, 1.0));
        assert_eq!(opening(None, None, 0.0), opening(None, None, 1.0));
    }

    #[test]
    fn a_normal_size_in_pixels_is_turned_into_points_at_its_dpi() {
        assert_eq!(points((1380, 1110, 144)), Some(egui::vec2(920.0, 740.0)));
        assert_eq!(points((920, 740, 96)), Some(egui::vec2(920.0, 740.0)));
        assert_eq!(points((1840, 1480, 192)), Some(egui::vec2(920.0, 740.0)));
        for bad in [(0, 740, 96), (920, -1, 96), (920, 740, 0)] {
            assert_eq!(points(bad), None, "{bad:?}");
        }
    }
}
