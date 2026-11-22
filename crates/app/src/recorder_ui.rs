//! A recording session on screen (screen 3 of the mockup): a countdown, a red
//! frame around the recorded area, and the control bar. The frame is a
//! transparent, click-through window; the bar is the root window. Both are
//! kept out of screen capture, so neither shows up in the GIF.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, FontId, RichText, Stroke, StrokeKind, vec2};
use mythicshot_capture::PxRect;
use mythicshot_editor::Item;
use mythicshot_editor::overlays::InputEvent;
use mythicshot_recording::export::{self, Format, Job};
use mythicshot_recording::{Recorder, Recording, Target};

use crate::theme::{self, Strong as _};
use crate::icons::{self, Icon};
use crate::live_draw::LiveDraw;
use crate::placement::{self, Focus, Placer};
use crate::platform::{self, WindowId};
use crate::video::{self, Edit};

pub const FRAME_TITLE: &str = "Mythicshot recording frame";
const BORDER: f32 = placement::FRAME_BORDER;
/// GIF size relative to the compressed frames on disk, measured on game
/// footage (94 MB of frames became a 13.4 MB GIF). Only an estimate.
const GIF_RATIO: f64 = 0.14;

enum Phase {
    Countdown { until: Instant },
    Live(Recorder),
}

pub enum BarAction {
    PauseToggle,
    /// Turn key logging on or off for the key overlay.
    ToggleKeys,
    /// Turn live draw on or off.
    ToggleDraw,
    Stop,
    Restart,
    Discard,
}

#[derive(Default)]
struct FrameState {
    countdown: Option<u32>,
    paused: bool,
}

pub struct Session {
    /// The recorded area in desktop pixels.
    pub rect: PxRect,
    pub title: String,
    target: Target,
    /// The monitor the area is on, to keep the bar on screen, and its scale.
    monitor: PxRect,
    monitor_scale: f32,
    /// Puts the red frame's window around the area in pixels, then shows it.
    frame_placer: Placer,
    /// The frame's rectangle in points for egui, a first guess the placer corrects.
    frame_guess: (egui::Pos2, egui::Vec2),
    phase: Phase,
    fps: u32,
    frame: Arc<Mutex<FrameState>>,
    frame_hidden_from_capture: bool,
    /// The app that had focus when recording began; it gets focus back.
    pub previous_foreground: Option<WindowId>,
    pub draw: LiveDraw,
}

impl Session {
    /// Prepare to record `rect` (desktop pixels). The area is clipped to the
    /// monitor holding its centre; recording starts after the countdown.
    pub fn new(rect: PxRect, title: String, countdown_secs: u32, fps: u32) -> Result<Session, String> {
        let monitors = mythicshot_capture::list_monitors();
        let (cx, cy) = (rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2);
        let mon = monitors.iter().find(|m| m.rect.contains(cx, cy)).or_else(|| monitors.iter().find(|m| m.primary)).ok_or("no monitor found")?;
        let clipped = rect.intersect(&mon.rect).ok_or("the area is not on a screen")?;
        if clipped.w < 16 || clipped.h < 16 {
            return Err("The area is too small to record".into());
        }
        let target = Target {
            display: mon.name.clone(),
            origin: (mon.rect.x, mon.rect.y),
            x: (clipped.x - mon.rect.x) as u32,
            y: (clipped.y - mon.rect.y) as u32,
            width: clipped.w,
            height: clipped.h,
        };
        let frame = placement::frame_rect(clipped, &monitors);
        let frame_guess = placement::guess_points(frame, placement::scale_for(frame, &monitors));
        let mut s = Session {
            rect: clipped,
            title,
            target,
            monitor: mon.rect,
            monitor_scale: mon.scale,
            frame_placer: Placer::new(FRAME_TITLE, frame, Focus::None),
            frame_guess,
            phase: Phase::Countdown { until: Instant::now() + Duration::from_secs(countdown_secs as u64) },
            fps,
            frame: Arc::new(Mutex::new(FrameState::default())),
            frame_hidden_from_capture: false,
            previous_foreground: platform::foreground_window(),
            draw: LiveDraw::default(),
        };
        if countdown_secs == 0 {
            s.go_live()?;
        }
        Ok(s)
    }

    fn go_live(&mut self) -> Result<(), String> {
        let rec = Recorder::start(self.target.clone(), self.fps)?;
        self.phase = Phase::Live(rec);
        Ok(())
    }

    /// Move from countdown to recording when it is time.
    pub fn tick(&mut self) -> Result<(), String> {
        if let Phase::Countdown { until } = self.phase {
            if Instant::now() >= until {
                self.go_live()?;
            }
        }
        if let Phase::Live(rec) = &self.phase {
            if let Some(e) = rec.error() {
                return Err(e);
            }
        }
        Ok(())
    }

    /// The recording clock, once recording has started.
    pub fn clock(&self) -> Option<mythicshot_recording::ClockHandle> {
        match &self.phase {
            Phase::Live(rec) => Some(rec.clock()),
            Phase::Countdown { .. } => None,
        }
    }

    pub fn toggle_pause(&mut self) {
        if let Phase::Live(rec) = &self.phase {
            rec.set_paused(!rec.is_paused());
        }
    }

    /// Stop. `None` if it was still counting down.
    pub fn stop(self) -> Option<Result<Recording, String>> {
        match self.phase {
            Phase::Countdown { .. } => None,
            Phase::Live(rec) => Some(rec.stop()),
        }
    }

    pub fn discard(self) {
        if let Phase::Live(rec) = self.phase {
            rec.discard();
        }
    }

    /// Throw away what was recorded and start again straight away.
    pub fn restart(&mut self) -> Result<(), String> {
        if let Phase::Live(rec) = std::mem::replace(&mut self.phase, Phase::Countdown { until: Instant::now() }) {
            rec.discard();
        }
        self.draw.reset();
        self.go_live()
    }

    /// Where the control bar goes, in desktop pixels, sized in its monitor's scale.
    pub fn bar_rect(&self) -> PxRect {
        placement::bar_rect(self.rect, self.monitor, self.monitor_scale)
    }

    /// The red frame: a transparent, click-through window just outside the area.
    pub fn show_frame(&mut self, ctx: &egui::Context) {
        {
            let mut f = self.frame.lock().expect("frame state");
            f.countdown = match self.phase {
                Phase::Countdown { until } => Some(until.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u32),
                Phase::Live(_) => None,
            };
            f.paused = matches!(&self.phase, Phase::Live(r) if r.is_paused());
        }
        // Created hidden, put on its pixels around the area, then shown (see `Placer`).
        let builder = egui::ViewportBuilder::default()
            .with_title(FRAME_TITLE)
            .with_position(self.frame_guess.0)
            .with_inner_size(self.frame_guess.1)
            .with_visible(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_mouse_passthrough(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false);
        let state = self.frame.clone();
        let frame_id = egui::ViewportId::from_hash_of("rec-frame");
        ctx.show_viewport_deferred(frame_id, builder, move |ui, _| {
            let Ok(f) = state.lock() else { return };
            let full = ui.ctx().content_rect();
            let painter = ui.painter();
            let color = if f.paused { theme::RECORD_PAUSED } else { theme::RECORD };
            painter.rect_stroke(full.shrink(BORDER / 2.0), 0.0, Stroke::new(BORDER - 1.0, color), StrokeKind::Middle);
            if let Some(n) = f.countdown.filter(|n| *n > 0) {
                painter.circle_filled(full.center(), 64.0, theme::SCRIM_COUNTDOWN);
                painter.text(full.center(), egui::Align2::CENTER_CENTER, n.to_string(), FontId::proportional(80.0), theme::ON_PICTURE);
            }
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        });
        self.frame_placer.tick(ctx, frame_id);
        if !self.frame_hidden_from_capture {
            if let Some(h) = platform::find_window(FRAME_TITLE) {
                self.frame_hidden_from_capture = platform::exclude_from_capture(h);
            }
        }
    }

    /// The control bar's contents (drawn in the root window).
    /// `keys_on` is whether key presses are being logged right now.
    pub fn bar_ui(&self, ui: &mut egui::Ui, keys_on: bool) -> Option<BarAction> {
        let mut action = None;
        // Opaque under the bar's edge, where its border and its fill each soften and together
        // left the desktop a little visible.
        theme::window_background(ui);
        let frame = egui::Frame::new().fill(theme::SURFACE).inner_margin(egui::Margin::symmetric(10, 7)).stroke(Stroke::new(1.0, theme::BORDER_STRONG));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (live, paused, stats) = match &self.phase {
                    Phase::Live(r) => (true, r.is_paused(), r.stats()),
                    Phase::Countdown { .. } => (false, false, Default::default()),
                };
                let dot = if paused { theme::RECORD_PAUSED } else { theme::RECORD };
                let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 6.0, dot);
                let t = stats.active_ms as f64 / 1000.0;
                ui.label(RichText::new(format!("{:02}:{:04.1}", (t / 60.0) as u32, t % 60.0)).monospace().size(theme::STRONG_SIZE).bold().color(theme::TEXT));
                let status = match (live, paused) {
                    (false, _) => "Starting…",
                    (true, true) => "Paused",
                    (true, false) => "Recording",
                };
                ui.label(RichText::new(status).color(theme::MUTED));
                ui.separator();
                let (icon, tip) = if paused { (Icon::Play, "Resume") } else { (Icon::Pause, "Pause") };
                if ui.add_enabled_ui(live, |ui| icons::button(ui, icon, 30.0, false, tip)).inner.clicked() {
                    action = Some(BarAction::PauseToggle);
                }
                if icons::button(ui, Icon::Stop, 30.0, false, "Stop and save the GIF").clicked() {
                    action = Some(BarAction::Stop);
                }
                if ui.add_enabled_ui(live, |ui| icons::button(ui, Icon::Restart, 30.0, false, "Restart (throw this take away)")).inner.clicked() {
                    action = Some(BarAction::Restart);
                }
                if icons::button(ui, Icon::Delete, 30.0, false, "Discard").clicked() {
                    action = Some(BarAction::Discard);
                }
                ui.separator();
                let keys = egui::Button::selectable(keys_on, "Keys").min_size(vec2(44.0, 28.0));
                if ui.add(keys).on_hover_text("Log key presses for the key overlay. Off by default: anything typed is logged, passwords too.").clicked() {
                    action = Some(BarAction::ToggleKeys);
                }
                let draw = egui::Button::selectable(self.draw.is_on(), "Draw").min_size(vec2(48.0, 28.0));
                if ui.add_enabled(live, draw).on_hover_text("Draw on the screen while recording. The drawing goes into the GIF; the toolbar does not.").clicked() {
                    action = Some(BarAction::ToggleDraw);
                }
                ui.separator();
                let est = stats.compressed_bytes as f64 * GIF_RATIO / 1_048_576.0;
                ui.label(RichText::new(format!("{} frames · {} fps · ≈{est:.1} MB", stats.frames, self.fps)).monospace().size(12.0).color(theme::MUTED));
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        action
    }
}

/// What to export, in what format.
pub struct ExportSpec {
    pub format: Format,
    pub quality: u8,
    pub loop_forever: bool,
    pub video_fps: u32,
}

/// An export running on a background thread. `T` is what a finished export
/// reports: the file and its size for GIF, APNG, MP4 and WebM.
/// The files being exported right now. An export writes straight to its final name, so until it
/// ends the file there is incomplete and "Recent" leaves it out.
static EXPORTING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The files being exported right now.
pub fn exporting_paths() -> Vec<PathBuf> {
    EXPORTING.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Marks a file as being exported until it is dropped, also when the export fails or panics.
struct Exporting(PathBuf);

impl Exporting {
    fn begin(path: &std::path::Path) -> Exporting {
        EXPORTING.lock().unwrap_or_else(|e| e.into_inner()).push(path.to_path_buf());
        Exporting(path.to_path_buf())
    }
}

impl Drop for Exporting {
    fn drop(&mut self) {
        let mut list = EXPORTING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = list.iter().position(|p| *p == self.0) {
            list.remove(i);
        }
    }
}

pub struct EncodeJob<T = (PathBuf, u64)> {
    progress: Arc<AtomicU32>,
    result: Arc<Mutex<Option<Result<T, String>>>>,
}

impl EncodeJob {
    /// Export `rec` with `edit` applied, drawing `items` and the overlays for
    /// `events`. With `delete_after`, the recording's temp files go when done.
    pub fn start(rec: Arc<Recording>, items: Vec<Item>, events: Vec<InputEvent>, edit: Edit, spec: ExportSpec, path: PathBuf, delete_after: bool) -> EncodeJob {
        let progress = Arc::new(AtomicU32::new(0));
        let result = Arc::new(Mutex::new(None));
        let (p, r, out) = (progress.clone(), result.clone(), path);
        let exporting = Exporting::begin(&out);
        std::thread::spawn(move || {
            let _exporting = exporting;
            let (frames, end_ms) = video::plan(&rec, &edit);
            let (width, height) = video::output_size(&rec, &edit);
            let job = Job { frames, end_ms, width, height, format: spec.format, quality: spec.quality, loop_forever: spec.loop_forever, video_fps: spec.video_fps };
            let source = rec.clone();
            let res = export::export(
                rec.clone(),
                &job,
                &out,
                |src, img| video::decorate(&source, src, img, &items, &events, &edit, true),
                |f| {
                    p.store((f * 1000.0) as u32, Ordering::Relaxed);
                    true
                },
            );
            if delete_after {
                rec.delete_files();
            }
            if let Ok(mut slot) = r.lock() {
                *slot = Some(res.map(|size| (out, size)));
            }
        });
        EncodeJob { progress, result }
    }
}

impl<T: Send + 'static> EncodeJob<T> {
    /// Run `work` on a background thread. It reports progress, 0.0 to 1.0,
    /// through the function it is given.
    pub fn spawn(work: impl FnOnce(&dyn Fn(f32)) -> Result<T, String> + Send + 'static) -> EncodeJob<T> {
        let progress = Arc::new(AtomicU32::new(0));
        let result = Arc::new(Mutex::new(None));
        let (p, r) = (progress.clone(), result.clone());
        std::thread::spawn(move || {
            let res = work(&|f| p.store((f * 1000.0) as u32, Ordering::Relaxed));
            if let Ok(mut slot) = r.lock() {
                *slot = Some(res);
            }
        });
        EncodeJob { progress, result }
    }

    pub fn progress(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / 1000.0
    }

    /// The outcome once the export has finished.
    pub fn take_result(&self) -> Option<Result<T, String>> {
        self.result.lock().ok().and_then(|mut r| r.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session counting down to a recording that never starts: the bar as it first shows.
    /// Nothing is recorded and no screen is asked about.
    pub(crate) fn counting_down() -> Session {
        let rect = PxRect { x: 0, y: 0, w: 640, h: 480 };
        Session {
            rect,
            title: "t".into(),
            target: Target { display: String::new(), origin: (0, 0), x: 0, y: 0, width: 640, height: 480 },
            monitor: rect,
            monitor_scale: 1.0,
            frame_placer: Placer::new(FRAME_TITLE, placement::frame_rect(rect, &[]), Focus::None),
            frame_guess: placement::guess_points(rect, 1.0),
            phase: Phase::Countdown { until: Instant::now() + Duration::from_secs(3600) },
            fps: 15,
            frame: Default::default(),
            frame_hidden_from_capture: false,
            previous_foreground: None,
            draw: LiveDraw::default(),
        }
    }

    /// The recording bar, a window of its own, paints all of itself opaque, pixel by pixel at 125%.
    #[test]
    fn the_recording_bar_is_opaque_pixel_by_pixel() {
        let s = counting_down();
        let size = vec2(placement::BAR_SIZE[0], placement::BAR_SIZE[1]);
        let shot = crate::snapshot::shot(&crate::theme::test_context(), size, 1.25, 3, |ui| {
            s.bar_ui(ui, false);
        });
        assert_eq!(shot.uncovered, 0, "pixels at {:?}", shot.uncovered_at);
        // And everything on it fits its fixed 680 by 46 points: the bar itself holds every label.
        let ctx = crate::theme::test_context();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            s.bar_ui(ui, false);
        });
        out.textures_delta.clear();
        assert!(crate::theme::checks::assert_texts_fit(&out, "the recording bar") >= 3);
        let bar = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let mut texts = Vec::new();
        fn collect(s: &egui::Shape, into: &mut Vec<egui::Rect>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                egui::Shape::Text(t) if !t.galley.text().trim().is_empty() => into.push(t.visual_bounding_rect()),
                _ => {}
            }
        }
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        assert!(texts.iter().all(|t| bar.contains_rect(*t)), "a label leaves the bar: {texts:?}");
    }

    #[test]
    fn a_file_counts_as_being_exported_until_its_export_ends() {
        let path = PathBuf::from("C:/captures/only-in-this-test-a1b2.gif");
        assert!(!exporting_paths().contains(&path));
        let first = Exporting::begin(&path);
        // The same file exported again before the first ends stays marked until both end.
        let second = Exporting::begin(&path);
        assert!(exporting_paths().contains(&path));
        drop(first);
        assert!(exporting_paths().contains(&path));
        drop(second);
        assert!(!exporting_paths().contains(&path));
        // A panic in the export thread still clears the mark.
        let marked = Exporting::begin(&path);
        let _ = std::thread::spawn(move || {
            let _marked = marked;
            panic!("export failed");
        })
        .join();
        assert!(!exporting_paths().contains(&path));
    }
}
