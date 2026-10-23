//! A recording session on screen (screen 3 of the mockup): a countdown, a red
//! frame around the recorded area, and the control bar. The frame is a
//! transparent, click-through window; the bar is the root window. Both are
//! kept out of screen capture, so neither shows up in the GIF.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, RichText, Stroke, StrokeKind, vec2};
use mythicshot_capture::PxRect;
use mythicshot_recording::{Recorder, Recording, Target, export};

use crate::icons::{self, Icon};
use crate::platform::{self, WindowId};

pub const FRAME_TITLE: &str = "Mythicshot recording frame";
pub const BAR_SIZE: [f32; 2] = [560.0, 46.0];
const BORDER: f32 = 4.0;
const RED: Color32 = Color32::from_rgb(229, 72, 77);
const AMBER: Color32 = Color32::from_rgb(245, 165, 36);
const BAR_BG: Color32 = Color32::from_rgb(27, 31, 39);
const MUTED: Color32 = Color32::from_rgb(163, 170, 184);
/// GIF size relative to the compressed frames on disk, measured on game
/// footage (94 MB of frames became a 13.4 MB GIF). Only an estimate.
const GIF_RATIO: f64 = 0.14;

enum Phase {
    Countdown { until: Instant },
    Live(Recorder),
}

pub enum BarAction {
    PauseToggle,
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
    /// The monitor the area is on, to keep the bar on screen.
    monitor: PxRect,
    phase: Phase,
    fps: u32,
    frame: Arc<Mutex<FrameState>>,
    frame_hidden_from_capture: bool,
    /// The app that had focus when recording began; it gets focus back.
    pub previous_foreground: Option<WindowId>,
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
        let mut s = Session {
            rect: clipped,
            title,
            target,
            monitor: mon.rect,
            phase: Phase::Countdown { until: Instant::now() + Duration::from_secs(countdown_secs as u64) },
            fps,
            frame: Arc::new(Mutex::new(FrameState::default())),
            frame_hidden_from_capture: false,
            previous_foreground: platform::foreground_window(),
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
        self.go_live()
    }

    /// Where the control bar goes, in points: above the area, else below it,
    /// else inside its top edge (it is hidden from capture either way).
    pub fn bar_position(&self, ppp: f32) -> egui::Pos2 {
        let (bw, bh) = ((BAR_SIZE[0] * ppp) as i32, (BAR_SIZE[1] * ppp) as i32);
        let gap = (10.0 * ppp) as i32;
        let x = (self.rect.x + self.rect.w as i32 / 2 - bw / 2).clamp(self.monitor.x, self.monitor.right() - bw);
        let y = if self.rect.y - bh - gap >= self.monitor.y {
            self.rect.y - bh - gap
        } else if self.rect.bottom() + gap + bh <= self.monitor.bottom() {
            self.rect.bottom() + gap
        } else {
            self.rect.y + gap
        };
        egui::pos2(x as f32 / ppp, y as f32 / ppp)
    }

    /// The red frame: a transparent, click-through window just outside the area.
    pub fn show_frame(&mut self, ctx: &egui::Context) {
        let ppp = ctx.pixels_per_point();
        {
            let mut f = self.frame.lock().expect("frame state");
            f.countdown = match self.phase {
                Phase::Countdown { until } => Some(until.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u32),
                Phase::Live(_) => None,
            };
            f.paused = matches!(&self.phase, Phase::Live(r) if r.is_paused());
        }
        let r = self.rect;
        let builder = egui::ViewportBuilder::default()
            .with_title(FRAME_TITLE)
            .with_position(egui::pos2(r.x as f32 / ppp - BORDER, r.y as f32 / ppp - BORDER))
            .with_inner_size([r.w as f32 / ppp + BORDER * 2.0, r.h as f32 / ppp + BORDER * 2.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_mouse_passthrough(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false);
        let state = self.frame.clone();
        ctx.show_viewport_deferred(egui::ViewportId::from_hash_of("rec-frame"), builder, move |ui, _| {
            let Ok(f) = state.lock() else { return };
            let full = ui.ctx().content_rect();
            let painter = ui.painter();
            let color = if f.paused { AMBER } else { RED };
            painter.rect_stroke(full.shrink(BORDER / 2.0), 0.0, Stroke::new(BORDER - 1.0, color), StrokeKind::Middle);
            if let Some(n) = f.countdown.filter(|n| *n > 0) {
                painter.circle_filled(full.center(), 64.0, Color32::from_black_alpha(170));
                painter.text(full.center(), egui::Align2::CENTER_CENTER, n.to_string(), FontId::proportional(80.0), Color32::WHITE);
            }
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        });
        if !self.frame_hidden_from_capture {
            if let Some(h) = platform::find_window(FRAME_TITLE) {
                self.frame_hidden_from_capture = platform::exclude_from_capture(h);
            }
        }
    }

    /// The control bar's contents (drawn in the root window).
    pub fn bar_ui(&self, ui: &mut egui::Ui) -> Option<BarAction> {
        let mut action = None;
        let frame = egui::Frame::new().fill(BAR_BG).inner_margin(egui::Margin::symmetric(10, 7)).stroke(Stroke::new(1.0, Color32::from_rgb(52, 59, 72)));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (live, paused, stats) = match &self.phase {
                    Phase::Live(r) => (true, r.is_paused(), r.stats()),
                    Phase::Countdown { .. } => (false, false, Default::default()),
                };
                let dot = if paused { AMBER } else { RED };
                let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 6.0, dot);
                let t = stats.active_ms as f64 / 1000.0;
                ui.label(RichText::new(format!("{:02}:{:04.1}", (t / 60.0) as u32, t % 60.0)).monospace().size(15.0).strong().color(Color32::WHITE));
                let status = match (live, paused) {
                    (false, _) => "Starting…",
                    (true, true) => "Paused",
                    (true, false) => "Recording",
                };
                ui.label(RichText::new(status).color(MUTED));
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
                let est = stats.compressed_bytes as f64 * GIF_RATIO / 1_048_576.0;
                ui.label(RichText::new(format!("{} frames · {} fps · ≈{est:.1} MB", stats.frames, self.fps)).monospace().size(12.0).color(MUTED));
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        action
    }
}

/// A GIF being encoded on a background thread.
pub struct EncodeJob {
    progress: Arc<AtomicU32>,
    result: Arc<Mutex<Option<Result<(PathBuf, u64), String>>>>,
}

impl EncodeJob {
    pub fn start(rec: Recording, path: PathBuf, quality: u8, draw_cursor: bool) -> EncodeJob {
        let progress = Arc::new(AtomicU32::new(0));
        let result = Arc::new(Mutex::new(None));
        let (p, r, out) = (progress.clone(), result.clone(), path);
        std::thread::spawn(move || {
            let res = export::encode_gif(&rec, &out, quality, draw_cursor, |f| {
                p.store((f * 1000.0) as u32, Ordering::Relaxed);
                true
            });
            rec.delete();
            if let Ok(mut slot) = r.lock() {
                *slot = Some(res.map(|size| (out, size)));
            }
        });
        EncodeJob { progress, result }
    }

    pub fn progress(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / 1000.0
    }

    /// The outcome once encoding has finished.
    pub fn take_result(&self) -> Option<Result<(PathBuf, u64), String>> {
        self.result.lock().ok().and_then(|mut r| r.take())
    }
}
