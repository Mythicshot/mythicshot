//! Where the region selector's window goes and how its points map to desktop pixels.
//!
//! The frozen desktop is in physical pixels, and so is the selector's window: it is placed
//! over the desktop's bounds in pixels, never through a size in points, because a window that
//! spans monitors with different scale factors changes its scale while it is moved and sized.
//! Windows gives such a window one scale, that of the monitor it is assigned to, and never
//! scales its content per monitor, so one linear map, `pixel = client origin + point × scale`,
//! holds on every monitor. Every frame takes the window's real client rectangle and the
//! current scale; nothing assumes the window is where it was asked to go.

use eframe::egui::{Pos2, Rect, pos2, vec2};
use mythicshot_capture::PxRect;

/// Frames the window may stay hidden while it is placed and its scale settles. After that it
/// is shown anyway, and the log says so.
pub const MAX_HIDDEN_FRAMES: u32 = 8;

/// Tolerance for a size in points times the scale against a size in pixels.
const PX_EPSILON: f32 = 0.5;

/// How one frame of the selector maps between its points and desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mapping {
    /// The window's client area in desktop pixels, as Windows reports it this frame.
    pub client: PxRect,
    /// Pixels per point this frame.
    pub ppp: f32,
}

impl Mapping {
    /// The desktop pixel under a point of the window. A pointer position is a whole pixel
    /// divided by the scale, so rounding gives that pixel back exactly.
    pub fn to_px(&self, p: Pos2) -> (i32, i32) {
        (self.client.x + (p.x * self.ppp).round() as i32, self.client.y + (p.y * self.ppp).round() as i32)
    }

    /// The point of the window at a desktop pixel's top-left corner.
    pub fn px_to_pt(&self, x: i32, y: i32) -> Pos2 {
        pos2((x - self.client.x) as f32 / self.ppp, (y - self.client.y) as f32 / self.ppp)
    }

    /// A rectangle of desktop pixels in the window's points.
    pub fn to_pt(&self, r: PxRect) -> Rect {
        Rect::from_min_size(self.px_to_pt(r.x, r.y), vec2(r.w as f32 / self.ppp, r.h as f32 / self.ppp))
    }

    /// Where the frozen picture of `bounds` is painted, in points: each of its pixels on one
    /// pixel of the screen, wherever the window really is.
    pub fn picture_rect(&self, bounds: PxRect) -> Rect {
        self.to_pt(bounds)
    }
}

/// The client rectangle the selector's window is given for a desktop: the bounds themselves.
/// `inset` shrinks the window only (never the picture or the crop) by that many pixels on
/// each side; the app uses 0, see `window_rect_inset_keeps_the_picture_exact`.
pub fn window_rect(bounds: PxRect, inset: u32) -> PxRect {
    let i = inset.min(bounds.w / 2).min(bounds.h / 2);
    PxRect::new(bounds.x + i as i32, bounds.y + i as i32, bounds.w - 2 * i, bounds.h - 2 * i)
}

/// The union of monitor rectangles, as the capture computes its bounds.
pub fn union(rects: impl IntoIterator<Item = PxRect>) -> Option<PxRect> {
    let mut it = rects.into_iter();
    let first = it.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.right(), first.bottom());
    for r in it {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.right());
        y1 = y1.max(r.bottom());
    }
    Some(PxRect::from_corners(x0, y0, x1, y1))
}

/// The outer rectangle that gives a window the client rectangle `target`, keeping the margins
/// between its current outer and client rectangles (none for the selector's undecorated window).
pub fn outer_for(target: PxRect, outer: PxRect, client: PxRect) -> PxRect {
    let (left, top) = (client.x - outer.x, client.y - outer.y);
    let (right, bottom) = (outer.right() - client.right(), outer.bottom() - client.bottom());
    PxRect::new(target.x - left, target.y - top, (target.w as i32 + left + right).max(1) as u32, (target.h as i32 + top + bottom).max(1) as u32)
}

/// Whether egui's scale this frame is the window's real one and its content area covers the
/// client area: then what is painted lands where the mapping says.
pub fn scale_settled(ppp: f32, zoom: f32, window_dpi: Option<u32>, content_pt: (f32, f32), client: PxRect) -> bool {
    let dpi_ok = window_dpi.is_none_or(|dpi| (ppp - zoom * dpi as f32 / 96.0).abs() < 1e-3);
    dpi_ok && (content_pt.0 * ppp - client.w as f32).abs() <= PX_EPSILON && (content_pt.1 * ppp - client.h as f32).abs() <= PX_EPSILON
}

/// What to do with the hidden window after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    /// Keep it hidden: it was moved this frame, or is not where it belongs yet.
    Wait,
    /// The frame just painted is right: show it.
    Now,
    /// It did not settle in [`MAX_HIDDEN_FRAMES`]: show it anyway.
    Anyway,
}

/// After frame `frame` (1 for the first) of a capture: show the window when it was not moved
/// this frame, sits on its target, egui's scale is the window's and the picture is uploaded.
pub fn decide_show(frame: u32, moved: bool, on_target: bool, settled: bool, texture: bool) -> Show {
    if !moved && on_target && settled && texture {
        Show::Now
    } else if frame >= MAX_HIDDEN_FRAMES {
        Show::Anyway
    } else {
        Show::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monitor: its rectangle in desktop pixels and its scale factor.
    #[derive(Clone, Copy, Debug)]
    struct Mon {
        rect: PxRect,
        scale: f32,
    }

    fn mon(x: i32, y: i32, w: u32, h: u32, scale: f32) -> Mon {
        Mon { rect: PxRect::new(x, y, w, h), scale }
    }

    /// The pointer position egui gets for a pixel: winit's physical position in the client
    /// area divided by the scale, in f32 as egui-winit does it.
    fn pointer(m: &Mapping, x: i32, y: i32) -> Pos2 {
        pos2((x - m.client.x) as f32 / m.ppp, (y - m.client.y) as f32 / m.ppp)
    }

    /// What egui-glow does with a point: times the scale, in pixels of the client area.
    fn painted_px(m: &Mapping, p: Pos2) -> (f32, f32) {
        (m.client.x as f32 + p.x * m.ppp, m.client.y as f32 + p.y * m.ppp)
    }

    /// Every check of the mapping for one layout and every scale the window may be given.
    fn check_layout(name: &str, mons: &[Mon]) {
        let bounds = union(mons.iter().map(|m| m.rect)).unwrap();
        // Windows assigns the window the scale of one monitor; which one does not matter.
        let mut scales: Vec<f32> = mons.iter().map(|m| m.scale).collect();
        scales.extend([1.0, 1.25, 1.5, 1.75, 2.0]);
        for &ppp in &scales {
            // On the bounds; inset by a pixel; and where a scale change mid-move left the old
            // code's window (half size, nudged): the mapping follows the real client area.
            let stray = PxRect::new(bounds.x + 161, bounds.y, bounds.w / 2, bounds.h / 2);
            for (inset, client) in [(0, window_rect(bounds, 0)), (1, window_rect(bounds, 1)), (2, stray)] {
                let m = Mapping { client, ppp };
                let ctx = format!("{name}, scale {ppp}, client {client:?}");
                // The picture covers the bounds pixel for pixel: one texel per screen pixel.
                let pic = m.picture_rect(bounds);
                let (x0, y0) = painted_px(&m, pic.min);
                let (x1, y1) = painted_px(&m, pic.max);
                assert!((x0 - bounds.x as f32).abs() < 0.01 && (y0 - bounds.y as f32).abs() < 0.01, "{ctx}: picture origin {x0},{y0}");
                assert!((x1 - bounds.right() as f32).abs() < 0.01 && (y1 - bounds.bottom() as f32).abs() < 0.01, "{ctx}: picture end {x1},{y1}");
                for mon in mons {
                    let r = mon.rect;
                    // Every pixel on each edge of each monitor, and its corners, round trip.
                    let mut pixels = Vec::new();
                    for x in [r.x, r.x + 1, r.right() - 2, r.right() - 1] {
                        for y in (r.y..r.bottom()).step_by(7).chain([r.bottom() - 1]) {
                            pixels.push((x, y));
                        }
                    }
                    for y in [r.y, r.y + 1, r.bottom() - 2, r.bottom() - 1] {
                        for x in (r.x..r.right()).step_by(7).chain([r.right() - 1]) {
                            pixels.push((x, y));
                        }
                    }
                    for (x, y) in pixels {
                        let client = m.client;
                        if !client.contains(x, y) {
                            continue; // under the inset strip: not in the window
                        }
                        assert_eq!(m.to_px(pointer(&m, x, y)), (x, y), "{ctx}: pixel {x},{y}");
                        // The frozen pixel drawn there is the same desktop pixel.
                        let (px, py) = painted_px(&m, m.px_to_pt(x, y));
                        let tx = bounds.x as f32 + (px - x0) * bounds.w as f32 / (x1 - x0);
                        let ty = bounds.y as f32 + (py - y0) * bounds.h as f32 / (y1 - y0);
                        assert!((px - x as f32).abs() < 0.01 && (py - y as f32).abs() < 0.01, "{ctx}: point of {x},{y} painted at {px},{py}");
                        assert!((tx - x as f32).abs() < 0.01 && (ty - y as f32).abs() < 0.01, "{ctx}: texel {tx},{ty} drawn on pixel {x},{y}");
                    }
                    // A whole monitor selected by a drag from corner to corner crops exactly it.
                    let a = m.to_px(pointer(&m, r.x.max(m.client.x), r.y.max(m.client.y)));
                    let b = m.to_px(pointer(&m, (r.right() - 1).min(m.client.right() - 1), (r.bottom() - 1).min(m.client.bottom() - 1)));
                    let sel = PxRect::from_corners(a.0, a.1, b.0, b.1);
                    if inset == 0 {
                        assert_eq!(sel, PxRect::new(r.x, r.y, r.w - 1, r.h - 1), "{ctx}: drag over {r:?}");
                    }
                    // The selection's outline is drawn on exactly those pixels.
                    let pt = m.to_pt(sel);
                    let (sx, sy) = painted_px(&m, pt.min);
                    let (ex, ey) = painted_px(&m, pt.max);
                    assert!((sx - sel.x as f32).abs() < 0.01 && (sy - sel.y as f32).abs() < 0.01 && (ex - sel.right() as f32).abs() < 0.01 && (ey - sel.bottom() as f32).abs() < 0.01, "{ctx}: outline of {sel:?}");
                }
                // A selection across a monitor edge keeps every pixel: none lost or gained.
                if mons.len() > 1 {
                    let (l, r) = (mons[0].rect, mons[1].rect);
                    let (ax, ay) = (l.x + l.w as i32 / 2, l.y + 10);
                    let (bx, by) = (r.x + r.w as i32 / 2, r.y + 20);
                    if m.client.contains(ax, ay) && m.client.contains(bx, by) {
                        let a = m.to_px(pointer(&m, ax, ay));
                        let b = m.to_px(pointer(&m, bx, by));
                        let sel = PxRect::from_corners(a.0, a.1, b.0, b.1);
                        assert_eq!(sel, PxRect::from_corners(ax, ay, bx, by), "{ctx}: across the edge");
                    }
                }
            }
        }
    }

    #[test]
    fn all_monitors_at_100_percent() {
        check_layout("three at 100%", &[mon(0, 0, 2560, 1440, 1.0), mon(2560, 0, 2560, 1440, 1.0), mon(5120, 0, 1920, 1080, 1.0)]);
    }

    #[test]
    fn one_monitor_at_150_percent_as_primary() {
        check_layout("150% primary", &[mon(0, 0, 2560, 1440, 1.5), mon(2560, 0, 2560, 1440, 1.0), mon(-1920, 0, 1920, 1080, 1.0)]);
    }

    #[test]
    fn a_monitor_left_of_and_above_the_primary() {
        check_layout("negative origin", &[mon(0, 0, 2560, 1440, 1.0), mon(-1920, -1080, 1920, 1080, 1.25), mon(2560, 200, 2560, 1440, 1.5)]);
    }

    #[test]
    fn monitors_of_different_heights_leave_the_top_left_corner_in_a_gap() {
        // The 1080p monitor sits lower than the others: the desktop's top-left is in no monitor.
        let mons = [mon(-1920, 360, 1920, 1080, 1.0), mon(0, 0, 2560, 1440, 1.25), mon(2560, 0, 2560, 1440, 1.0)];
        let b = union(mons.iter().map(|m| m.rect)).unwrap();
        assert!(mons.iter().all(|m| !m.rect.contains(b.x, b.y)), "the corner is in a gap");
        check_layout("gap at top-left", &mons);
    }

    /// The owner's machine: two monitors at 100% and one at 200%. The 200% one is a
    /// 3840×2160 panel or a 1920×1080 one (the sizes known so far may be logical), primary
    /// or not, left, right or above the pair, holding the desktop's top-left corner or not.
    #[test]
    fn two_at_100_percent_and_one_at_200_percent() {
        for (w, h) in [(3840, 2160), (1920, 1080)] {
            for primary_200 in [true, false] {
                // The primary monitor is the one at 0,0.
                let (pair_x, big_x) = if primary_200 { (w as i32, 0) } else { (0, 5120) };
                let right = vec![mon(big_x, 0, w, h, 2.0), mon(pair_x, 0, 2560, 1440, 1.0), mon(pair_x + 2560, 0, 2560, 1440, 1.0)];
                check_layout(&format!("200% {w}x{h} right/primary={primary_200}"), &right);
                let (pair_x, big_x) = if primary_200 { (w as i32, 0) } else { (0, -(w as i32)) };
                let left = vec![mon(big_x, 0, w, h, 2.0), mon(pair_x, 0, 2560, 1440, 1.0), mon(pair_x + 2560, 0, 2560, 1440, 1.0)];
                let b = union(left.iter().map(|m| m.rect)).unwrap();
                assert!(left[0].rect.contains(b.x, b.y), "the 200% monitor holds the top-left corner");
                check_layout(&format!("200% {w}x{h} left/primary={primary_200}"), &left);
                // Above the pair, centred: the top-left corner falls in a gap beside it.
                let (pair_y, big_y) = if primary_200 { (h as i32, 0) } else { (0, -(h as i32)) };
                let cx = (5120 - w as i32) / 2;
                let above = vec![mon(cx, big_y, w, h, 2.0), mon(0, pair_y, 2560, 1440, 1.0), mon(2560, pair_y, 2560, 1440, 1.0)];
                check_layout(&format!("200% {w}x{h} above/primary={primary_200}"), &above);
                // Above and flush left: it holds the corner.
                let flush = vec![mon(0, big_y, w, h, 2.0), mon(0, pair_y, 2560, 1440, 1.0), mon(2560, pair_y, 2560, 1440, 1.0)];
                check_layout(&format!("200% {w}x{h} above-left/primary={primary_200}"), &flush);
            }
        }
    }

    /// What the selector did before this fix, from its own code: it took the window's origin
    /// to be the bounds' origin and the scale to be the one before the move. When the window
    /// ends up elsewhere, the cursor's pixel and the picture under it disagree.
    #[test]
    fn the_old_assumed_origin_misplaces_the_cursor_when_the_window_is_not_on_the_bounds() {
        // 200% 1920×1080 left of the 100% pair holds the top-left corner. Derived from
        // winit's WM_DPICHANGED handling: the window asked for 7040×1440 is halved to keep its
        // size in points and nudged onto the 100% monitor, to 3520×720 at -1759,0.
        let bounds = PxRect::new(-1920, 0, 7040, 1440);
        let real = Mapping { client: PxRect::new(-1759, 0, 3520, 720), ppp: 1.0 };
        let old_to_px = |p: Pos2| (bounds.x + (p.x * real.ppp).round() as i32, bounds.y + (p.y * real.ppp).round() as i32);
        let cursor = (100, 100);
        let p = pointer(&real, cursor.0, cursor.1);
        assert_eq!(old_to_px(p), (-61, 100), "the old code read the cursor 161 px to the left");
        assert_eq!(real.to_px(p), cursor, "the mapping from the real window does not");
        // And the old picture filled the window: the whole desktop at half size.
        let old_picture_px = (real.client.w, real.client.h);
        assert_eq!(old_picture_px, (bounds.w / 2, bounds.h / 2));
        let new_picture = real.picture_rect(bounds);
        assert_eq!((new_picture.width() * real.ppp, new_picture.height() * real.ppp), (bounds.w as f32, bounds.h as f32));
    }

    #[test]
    fn window_rect_inset_keeps_the_picture_exact() {
        let b = PxRect::new(-1920, -100, 7040, 1540);
        assert_eq!(window_rect(b, 0), b);
        assert_eq!(window_rect(b, 1), PxRect::new(-1919, -99, 7038, 1538));
        let m = Mapping { client: window_rect(b, 1), ppp: 1.5 };
        let pic = m.picture_rect(b);
        assert!((pic.min.x * 1.5 + m.client.x as f32 - b.x as f32).abs() < 0.01);
        assert!((pic.width() * 1.5 - b.w as f32).abs() < 0.01);
    }

    #[test]
    fn outer_rect_keeps_the_frame_margins() {
        let client = PxRect::new(10, 30, 100, 50);
        let outer = PxRect::new(2, 0, 116, 88);
        let target = PxRect::new(-1920, 0, 7040, 1440);
        let o = outer_for(target, outer, client);
        assert_eq!(o, PxRect::new(-1928, -30, 7056, 1478));
        // An undecorated window: outer and client are one rectangle.
        assert_eq!(outer_for(target, client, client), target);
    }

    #[test]
    fn the_window_is_shown_only_when_placed_settled_and_painted() {
        let client = PxRect::new(0, 0, 3000, 2000);
        assert!(scale_settled(1.5, 1.0, Some(144), (2000.0, 2000.0 / 1.5), client));
        assert!(!scale_settled(1.0, 1.0, Some(144), (3000.0, 2000.0), client), "egui has not caught up with the window's 150%");
        assert!(!scale_settled(1.5, 1.0, Some(144), (1000.0, 1000.0), client), "the content area is still the old size");
        assert!(scale_settled(2.0, 1.0, None, (1500.0, 1000.0), client), "no DPI to compare: the sizes decide");
        assert_eq!(decide_show(1, false, true, true, true), Show::Now);
        assert_eq!(decide_show(1, true, true, true, true), Show::Wait, "moved this frame: what was painted was laid out for the old size");
        assert_eq!(decide_show(2, false, false, true, true), Show::Wait);
        assert_eq!(decide_show(2, false, true, false, true), Show::Wait);
        assert_eq!(decide_show(2, false, true, true, false), Show::Wait);
        assert_eq!(decide_show(MAX_HIDDEN_FRAMES, true, false, false, false), Show::Anyway);
    }

    #[test]
    fn union_of_monitors_is_the_capture_bounds() {
        let u = union([PxRect::new(0, 0, 2560, 1440), PxRect::new(-1920, 360, 1920, 1080), PxRect::new(2560, -200, 2560, 1440)]).unwrap();
        assert_eq!(u, PxRect::new(-1920, -200, 7040, 1640));
        assert_eq!(union(std::iter::empty()), None);
    }
}
