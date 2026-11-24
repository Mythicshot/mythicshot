//! The glass band around an area being recorded: four thin native windows just outside it (top,
//! bottom, left, right; top and bottom take the corners), from the start of the countdown until
//! the recording ends. Each strip is a layered window whose pixels are one premultiplied bitmap
//! handed to Windows with `UpdateLayeredWindow` before the window is shown: nothing is ever
//! painted into it later, so there is no frame to miss and no layered style taken while hidden.
//! The strips are topmost, never take focus, are not in the taskbar or the Alt+Tab list, let
//! every click through, and are kept out of every capture (see [`crate::platform::band_open`]).
//!
//! What this file decides is pure and tested without a window: which pixels the band covers
//! ([`crate::placement::band_strips`]), each strip's bitmap ([`bitmap`]), when the band is up
//! ([`Life`]) and the line `capture.log` gets ([`shown_line`]).

use std::time::{Duration, Instant};

use mythicshot_capture::PxRect;

use crate::capture_log;
use crate::placement::{self, Strip};
use crate::platform::{self, BandWindows, OpenedStrip, StripPixels};
use crate::theme;

/// How often the strips are put back on top of every other topmost window.
pub const RAISE_EVERY: Duration = Duration::from_millis(250);

/// The colour of the desktop pixel `(x, y)` in the band around `area`, premultiplied BGRA as a
/// layered window takes it: the layer [`theme::GLASS_LAYERS`] its distance from the area falls
/// in. The distance is counted the same way at the sides and at the corners, so the corners are
/// square and every layer goes all the way round. Fully transparent inside the area and past
/// the band (no strip covers either).
pub fn pixel(area: PxRect, x: i32, y: i32) -> [u8; 4] {
    let dx = (area.x - x).max(x - (area.right() - 1)).max(0);
    let dy = (area.y - y).max(y - (area.bottom() - 1)).max(0);
    let d = dx.max(dy);
    if d == 0 {
        return [0; 4];
    }
    let mut from = 0;
    for (px, c) in theme::GLASS_LAYERS {
        if d <= from + px as i32 {
            return [c.b(), c.g(), c.r(), c.a()];
        }
        from += px as i32;
    }
    [0; 4]
}

/// A strip's pixels, top row first, four bytes each (premultiplied BGRA).
pub fn bitmap(area: PxRect, strip: PxRect) -> Vec<u8> {
    let mut out = Vec::with_capacity(strip.w as usize * strip.h as usize * 4);
    for y in strip.y..strip.bottom() {
        for x in strip.x..strip.right() {
            out.extend_from_slice(&pixel(area, x, y));
        }
    }
    out
}

/// Where a recording is, as far as its band goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Life {
    /// Not started.
    Idle,
    Counting,
    Live { paused: bool },
    /// Stopped, cancelled or failed: the band is gone for good.
    Over,
}

/// What happens to a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Countdown,
    Live,
    Pause,
    Resume,
    /// The take is thrown away and recording starts again at once.
    Restart,
    Stop,
    Cancel,
    Fail,
}

impl Life {
    pub fn next(self, e: Event) -> Life {
        match (self, e) {
            (Life::Over, _) => Life::Over,
            (_, Event::Stop | Event::Cancel | Event::Fail) => Life::Over,
            (Life::Idle, Event::Countdown) => Life::Counting,
            (Life::Idle | Life::Counting, Event::Live) => Life::Live { paused: false },
            (Life::Live { .. }, Event::Pause) => Life::Live { paused: true },
            (Life::Live { .. }, Event::Resume) => Life::Live { paused: false },
            (Life::Live { .. } | Life::Counting, Event::Restart) => Life::Live { paused: false },
            (life, _) => life,
        }
    }

    /// Whether the band is on screen. Paused or not, it looks the same.
    pub fn shows_band(self) -> bool {
        matches!(self, Life::Counting | Life::Live { .. })
    }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// The one line `capture.log` gets when the band is first shown: each strip's pixels and how
/// Windows composites its window, so a band missing on screen can be told apart from one never
/// made. A layered window given its pixels with `UpdateLayeredWindow` reads as "attributes NOT
/// set" in the composition note: that is expected; "pixels set" says whether they were given.
pub fn shown_line(time: &str, area: PxRect, strips: &[(Strip, PxRect, OpenedStrip)]) -> String {
    if strips.is_empty() {
        return format!("-- {time} glass band: none around {} (a whole monitor, or no room on the desktop)", capture_log::px(area));
    }
    let each: Vec<String> = strips
        .iter()
        .map(|(s, r, o)| match &o.window {
            None => format!("{} {}: NOT CREATED ({})", s.name(), capture_log::px(*r), o.note),
            Some(_) => format!(
                "{} {}: {} · pixels set {} · hidden from capture {} · visible {}",
                s.name(),
                capture_log::px(*r),
                o.note,
                yes(o.pixels_set),
                yes(o.excluded),
                yes(o.visible)
            ),
        })
        .collect();
    format!("-- {time} glass band shown around {}: {}", capture_log::px(area), each.join("; "))
}

/// The band of one recording. Its windows go when it is dropped, whatever ended the recording.
pub struct Band {
    area: PxRect,
    strips: Vec<(Strip, PxRect)>,
    life: Life,
    windows: Option<BandWindows>,
    raised: Option<Instant>,
}

impl Band {
    /// The band around `area`, on the desktop made of `monitors`. Nothing is shown yet.
    pub fn new(area: PxRect, monitors: &[PxRect]) -> Band {
        Band { area, strips: placement::band_strips(area, monitors), life: Life::Idle, windows: None, raised: None }
    }

    #[cfg(test)]
    pub fn life(&self) -> Life {
        self.life
    }

    /// Whether its windows are open now.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.windows.is_some()
    }

    /// Something happened to the recording: open the windows when the band first shows, close
    /// them when it is over.
    pub fn on(&mut self, e: Event) {
        let was = self.life;
        self.life = self.life.next(e);
        if self.life.shows_band() && !was.shows_band() && was == Life::Idle {
            self.open();
        } else if !self.life.shows_band() {
            self.close();
        }
    }

    fn open(&mut self) {
        let wanted: Vec<StripPixels> = self.strips.iter().map(|&(_, r)| StripPixels { rect: r, pixels: bitmap(self.area, r) }).collect();
        let (windows, opened) = if wanted.is_empty() { (None, Vec::new()) } else { platform::band_open(&wanted) };
        let logged: Vec<(Strip, PxRect, OpenedStrip)> = self.strips.iter().zip(opened).map(|(&(s, r), o)| (s, r, o)).collect();
        capture_log::append(&[shown_line(&capture_log::now(), self.area, &logged)]);
        self.windows = windows;
        self.raised = Some(Instant::now());
    }

    /// Each tick of the bar: put the strips back on top of every other topmost window, without
    /// activating them, at most every [`RAISE_EVERY`].
    pub fn keep_on_top(&mut self, now: Instant) {
        let Some(w) = &self.windows else { return };
        if self.raised.is_some_and(|t| now.saturating_duration_since(t) < RAISE_EVERY) {
            return;
        }
        self.raised = Some(now);
        platform::band_raise(w);
    }

    fn close(&mut self) {
        if let Some(w) = self.windows.take() {
            platform::band_close(w);
        }
    }
}

impl Drop for Band {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mythicshot_destinations::stubs;

    const EDGE: [u8; 4] = [242, 242, 242, 242];
    const FILL: [u8; 4] = [77, 77, 77, 77];
    const RIM: [u8; 4] = [0, 0, 0, 153];

    fn at(bits: &[u8], strip: PxRect, x: i32, y: i32) -> [u8; 4] {
        let i = (((y - strip.y) as u32 * strip.w + (x - strip.x) as u32) * 4) as usize;
        bits[i..i + 4].try_into().unwrap()
    }

    /// Each strip's bitmap: premultiplied BGRA, the bright edge on the row or column touching the
    /// area, the translucent band, then the dark edge outermost, on every side and at every corner.
    #[test]
    fn each_strips_bitmap_has_the_bright_edge_inside_and_the_dark_edge_outermost() {
        let area = PxRect::new(100, 100, 40, 30);
        let monitors = [PxRect::new(0, 0, 400, 300)];
        let strips = placement::band_strips(area, &monitors);
        assert_eq!(strips.len(), 4);
        for &(s, r) in &strips {
            let bits = bitmap(area, r);
            assert_eq!(bits.len(), (r.w * r.h * 4) as usize, "{s:?}");
            for px in bits.chunks(4) {
                assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "{s:?}: {px:?} is premultiplied");
                assert!(px == EDGE || px == FILL || px == RIM, "{s:?}: only the band's three colours, {px:?}");
            }
            // A line from the area outwards through the middle of the strip.
            let line: Vec<[u8; 4]> = match s {
                Strip::Top => (0..8).map(|i| at(&bits, r, 120, area.y - 1 - i)).collect(),
                Strip::Bottom => (0..8).map(|i| at(&bits, r, 120, area.bottom() + i)).collect(),
                Strip::Left => (0..8).map(|i| at(&bits, r, area.x - 1 - i, 115)).collect(),
                Strip::Right => (0..8).map(|i| at(&bits, r, area.right() + i, 115)).collect(),
            };
            assert_eq!(line, [EDGE, FILL, FILL, FILL, FILL, FILL, FILL, RIM], "{s:?}");
        }
        // The corners, in the top and bottom strips: from the area's corner outwards diagonally,
        // the same layers, and the outermost row and column are the dark edge all the way.
        let (top, bottom) = (strips[0].1, strips[1].1);
        let tb = bitmap(area, top);
        let bb = bitmap(area, bottom);
        let diag_tl: Vec<_> = (0..8).map(|i| at(&tb, top, area.x - 1 - i, area.y - 1 - i)).collect();
        let diag_tr: Vec<_> = (0..8).map(|i| at(&tb, top, area.right() + i, area.y - 1 - i)).collect();
        let diag_bl: Vec<_> = (0..8).map(|i| at(&bb, bottom, area.x - 1 - i, area.bottom() + i)).collect();
        let diag_br: Vec<_> = (0..8).map(|i| at(&bb, bottom, area.right() + i, area.bottom() + i)).collect();
        for (name, d) in [("top-left", diag_tl), ("top-right", diag_tr), ("bottom-left", diag_bl), ("bottom-right", diag_br)] {
            assert_eq!(d, [EDGE, FILL, FILL, FILL, FILL, FILL, FILL, RIM], "{name} corner");
        }
        for x in top.x..top.right() {
            assert_eq!(at(&tb, top, x, top.y), RIM, "the top strip's outer row");
            assert_eq!(at(&bb, bottom, x, bottom.bottom() - 1), RIM, "the bottom strip's outer row");
        }
        for y in top.y..top.bottom() {
            assert_eq!(at(&tb, top, top.x, y), RIM, "the top-left corner's outer column");
            assert_eq!(at(&tb, top, top.right() - 1, y), RIM, "the top-right corner's outer column");
        }
        // The bright edge rings the area exactly: the strips' pixels touching it are all bright.
        for &(_, r) in &strips {
            let bits = bitmap(area, r);
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    let touching = PxRect::new(area.x - 1, area.y - 1, area.w + 2, area.h + 2).contains(x, y);
                    assert_eq!(at(&bits, r, x, y) == EDGE, touching, "{x},{y}");
                }
            }
        }
    }

    /// A strip cut at the desktop's edge keeps the colours of the pixels it still has: the bright
    /// edge still faces the area.
    #[test]
    fn a_clipped_strip_keeps_its_layers() {
        let area = PxRect::new(2000, 500, 557, 200);
        let monitors = [PxRect::new(0, 0, 2560, 1440)];
        let strips = placement::band_strips(area, &monitors);
        let (s, right) = strips[3];
        assert_eq!((s, right.w), (Strip::Right, 3));
        let bits = bitmap(area, right);
        assert_eq!([at(&bits, right, 2557, 600), at(&bits, right, 2558, 600), at(&bits, right, 2559, 600)], [EDGE, FILL, FILL]);
    }

    /// The band's colours are neutral and the same whatever the recording is doing: the bitmap
    /// depends on the area alone, never red or gold.
    #[test]
    fn the_band_is_neutral_recording_and_paused() {
        let area = PxRect::new(10, 10, 20, 20);
        for (_, c) in theme::GLASS_LAYERS {
            for not in [theme::RECORD, theme::RECORD_PAUSED, theme::GOLD] {
                assert_ne!(c, not);
            }
        }
        assert_eq!(pixel(area, 9, 15), EDGE);
        assert_eq!(pixel(area, 15, 15), [0; 4], "nothing over the recorded pixels");
        assert_eq!(pixel(area, 1, 15), [0; 4], "nothing past the band");
    }

    /// The band's life: up from the countdown, kept through pause, resume and restart, gone on
    /// stop, cancel and failure, and never back once gone.
    #[test]
    fn the_band_shows_from_the_countdown_until_the_recording_ends() {
        use Event::*;
        assert!(!Life::Idle.shows_band());
        let counting = Life::Idle.next(Countdown);
        assert_eq!(counting, Life::Counting);
        assert!(counting.shows_band());
        let live = counting.next(Live);
        assert_eq!(live, Life::Live { paused: false });
        let paused = live.next(Pause);
        assert_eq!(paused, Life::Live { paused: true });
        assert!(paused.shows_band(), "kept while paused");
        assert!(paused.next(Resume).shows_band());
        assert!(paused.next(Restart).shows_band(), "kept through a restart");
        // A countdown of 0 goes live at once.
        assert!(Life::Idle.next(Live).shows_band());
        for end in [Stop, Cancel, Fail] {
            for from in [counting, live, paused] {
                let over = from.next(end);
                assert_eq!(over, Life::Over, "{from:?} then {end:?}");
                for again in [Countdown, Live, Resume, Restart] {
                    assert!(!over.next(again).shows_band(), "never back after {end:?}");
                }
            }
        }
    }

    /// The windows follow the life: opened once at the countdown, raised on a timer without
    /// being reopened, kept through pause and resume, closed on stop, cancel or failure, and
    /// closed when the band is dropped. Nothing real is opened in a test build.
    #[test]
    fn the_windows_open_at_the_countdown_and_close_when_the_recording_ends() {
        let monitors = [PxRect::new(0, 0, 1920, 1080)];
        let area = PxRect::new(100, 100, 640, 480);
        for end in [Some(Event::Stop), Some(Event::Cancel), Some(Event::Fail), None] {
            let _ = stubs::take();
            let mut band = Band::new(area, &monitors);
            assert!(!band.is_open());
            band.on(Event::Countdown);
            assert!(band.is_open());
            let t0 = Instant::now();
            band.keep_on_top(t0);
            band.keep_on_top(t0 + RAISE_EVERY + Duration::from_millis(1));
            band.keep_on_top(t0 + RAISE_EVERY + Duration::from_millis(2));
            band.on(Event::Live);
            band.on(Event::Pause);
            band.on(Event::Resume);
            band.on(Event::Restart);
            assert!(band.is_open());
            match end {
                Some(e) => band.on(e),
                None => drop(band),
            }
            let calls = stubs::take();
            assert_eq!(
                calls,
                [
                    "band open 92,92 656x8; 92,580 656x8; 92,100 8x480; 740,100 8x480".to_string(),
                    "band raise 4".to_string(),
                    "band close 4".to_string()
                ],
                "{end:?}"
            );
        }
        // A whole monitor: nothing is opened, and the log says why.
        let _ = stubs::take();
        let mut band = Band::new(monitors[0], &monitors);
        band.on(Event::Countdown);
        band.keep_on_top(Instant::now() + Duration::from_secs(1));
        drop(band);
        assert!(stubs::take().is_empty(), "nothing is opened");
    }

    #[test]
    fn the_log_says_how_each_strip_is_composited() {
        let area = PxRect::new(100, 100, 640, 480);
        let ok = |n: isize| OpenedStrip { window: Some(n), note: "exstyle 0x80088 · layered, attributes NOT set · cloaked 0".into(), pixels_set: true, excluded: true, visible: true };
        let strips = vec![
            (Strip::Top, PxRect::new(92, 92, 656, 8), ok(1)),
            (Strip::Left, PxRect::new(92, 100, 8, 480), OpenedStrip { window: None, note: "CreateWindowExW failed: 0x80070008".into(), pixels_set: false, excluded: false, visible: false }),
        ];
        assert_eq!(
            shown_line("T", area, &strips),
            "-- T glass band shown around 100,100 640x480: top 92,92 656x8: exstyle 0x80088 · layered, attributes NOT set · cloaked 0 · pixels set yes · hidden from capture yes · visible yes; left 92,100 8x480: NOT CREATED (CreateWindowExW failed: 0x80070008)"
        );
        assert_eq!(shown_line("T", area, &[]), "-- T glass band: none around 100,100 640x480 (a whole monitor, or no room on the desktop)");
    }

    /// The red frame this replaces was one egui viewport that stayed blank on screen. The band
    /// is built from native strips only: no source file asks egui for a frame viewport again, and
    /// the strips are given their pixels before they are shown.
    #[test]
    fn the_band_is_not_an_egui_viewport() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for file in ["recorder_ui.rs", "glass_band.rs", "main.rs"] {
            let text = std::fs::read_to_string(root.join(file)).unwrap().replace("\r\n", "\n");
            let code: String = text.split("#[cfg(test)]\nmod tests").next().unwrap().to_string();
            for gone in ["recording frame", "rec-frame", "show_frame", "with_mouse_passthrough"] {
                assert!(!code.contains(gone), "{file} has {gone:?}");
            }
        }
        let platform = std::fs::read_to_string(root.join("platform.rs")).unwrap().replace("\r\n", "\n");
        let open = &platform[platform.find("pub fn open_strip").expect("the native strip")..];
        let open = &open[..open.find("\n    }\n").expect("its end")];
        let (create, pixels, show) = (open.find("CreateWindowExW(").expect("made"), open.find("set_pixels(").expect("pixels given"), open.find("HWND_TOPMOST").expect("shown topmost"));
        assert!(create < pixels && pixels < show, "made, given its pixels, then shown");
        assert!(open[..create].contains("WS_EX_LAYERED") && !open.contains("WS_VISIBLE"), "layered from creation on, and created hidden");
        let set = &platform[platform.find("fn set_pixels").expect("the pixels")..];
        assert!(set[..set.find("\n    }\n").unwrap()].contains("UpdateLayeredWindow("), "the pixels go through UpdateLayeredWindow");
    }
}
