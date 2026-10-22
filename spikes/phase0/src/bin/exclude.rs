//! Test 4: a window that screen capture can't see, plus the transparent-window
//! check from the plan.
//!
//! Modes (first argument):
//! - `control`: opaque magenta window, no exclusion. Capture should see it.
//! - `exclude`: same window with `WDA_EXCLUDEFROMCAPTURE`. Capture should not.
//! - `transparent`: transparent window with a magenta square in the middle.
//!   The square should show and the rest should match the screen underneath.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect, pos2, vec2};

const X: u32 = 300;
const Y: u32 = 300;
const W: u32 = 400;
const H: u32 = 300;
const SQUARE: f32 = 100.0;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Control,
    Exclude,
    Transparent,
}

struct App {
    mode: Mode,
    started: Instant,
    frames: u32,
    excluded: bool,
    baseline: Option<image::RgbaImage>,
    result: Option<Receiver<image::RgbaImage>>,
}

fn grab_rect() -> image::RgbaImage {
    let monitor = xcap::Monitor::from_point(X as i32 + 1, Y as i32 + 1).expect("monitor at window");
    let full = monitor.capture_image().expect("capture monitor");
    let full = image::RgbaImage::from_raw(full.width(), full.height(), full.into_raw()).expect("image buffer");
    let mx = monitor.x().unwrap_or_default() as u32;
    let my = monitor.y().unwrap_or_default() as u32;
    image::imageops::crop_imm(&full, X - mx, Y - my, W, H).to_image()
}

fn is_magenta(p: &image::Rgba<u8>) -> bool {
    p[0] > 235 && p[1] < 20 && p[2] > 235
}

impl App {
    fn report(&self, img: &image::RgbaImage) {
        let half = SQUARE as u32 / 2;
        let (cx, cy) = (W / 2, H / 2);
        let in_square = |x: u32, y: u32| x + half >= cx && x < cx + half && y + half >= cy && y < cy + half;
        let (mut magenta_in, mut total_in, mut magenta_out, mut total_out, mut diff_out) = (0u64, 0u64, 0u64, 0u64, 0u64);
        for (x, y, p) in img.enumerate_pixels() {
            if in_square(x, y) {
                total_in += 1;
                magenta_in += is_magenta(p) as u64;
            } else {
                total_out += 1;
                magenta_out += is_magenta(p) as u64;
                if let Some(b) = &self.baseline {
                    let q = b.get_pixel(x, y);
                    diff_out += (0..3).map(|i| (p[i] as i32 - q[i] as i32).unsigned_abs() as u64).sum::<u64>();
                }
            }
        }
        let pct = |a: u64, b: u64| 100.0 * a as f64 / b.max(1) as f64;
        let path = phase0::out_dir().join(format!("exclude-{}.png", self.name()));
        img.save(&path).expect("save capture");
        println!("affinity set: {}", self.excluded);
        println!("magenta inside centre square: {:.1}%  outside: {:.1}%", pct(magenta_in, total_in), pct(magenta_out, total_out));
        let ok = match self.mode {
            Mode::Control => pct(magenta_in + magenta_out, total_in + total_out) > 95.0,
            Mode::Exclude => self.excluded && pct(magenta_in + magenta_out, total_in + total_out) < 1.0,
            Mode::Transparent => {
                let avg = diff_out as f64 / (total_out.max(1) * 3) as f64;
                println!("average difference from the screen underneath (outside square): {avg:.1} of 255");
                pct(magenta_in, total_in) > 95.0 && avg < 12.0
            }
        };
        println!("capture saved -> {}", path.display());
        println!("{}", if ok { "RESULT: PASS" } else { "RESULT: FAIL" });
    }

    fn name(&self) -> &'static str {
        match self.mode {
            Mode::Control => "control",
            Mode::Exclude => "exclude",
            Mode::Transparent => "transparent",
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.mode == Mode::Transparent { [0.0, 0.0, 0.0, 0.0] } else { [1.0, 0.0, 1.0, 1.0] }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frames += 1;
        let full = ui.ctx().content_rect();
        if self.mode == Mode::Transparent {
            let square = Rect::from_center_size(full.center(), vec2(SQUARE, SQUARE));
            ui.painter().rect_filled(square, 0.0, Color32::from_rgb(255, 0, 255));
        } else {
            ui.painter().rect_filled(Rect::from_min_max(pos2(0.0, 0.0), full.max), 0.0, Color32::from_rgb(255, 0, 255));
        }

        if self.mode == Mode::Exclude && !self.excluded && self.frames >= 2 {
            self.excluded = set_exclude(frame);
        }
        if self.result.is_none() && self.started.elapsed() > Duration::from_millis(1200) {
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(grab_rect());
            });
            self.result = Some(rx);
        }
        if let Some(img) = self.result.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.report(&img);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint();
    }
}

#[cfg(windows)]
fn set_exclude(frame: &eframe::Frame) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};
    let Ok(handle) = frame.window_handle() else { return false };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return false };
    unsafe { SetWindowDisplayAffinity(HWND(h.hwnd.get() as *mut _), WDA_EXCLUDEFROMCAPTURE).is_ok() }
}

#[cfg(not(windows))]
fn set_exclude(_frame: &eframe::Frame) -> bool {
    false
}

fn main() -> eframe::Result {
    let mode = match std::env::args().nth(1).as_deref() {
        Some("control") => Mode::Control,
        Some("transparent") => Mode::Transparent,
        _ => Mode::Exclude,
    };
    let renderer = match std::env::args().nth(2).as_deref() {
        Some("glow") => eframe::Renderer::Glow,
        _ => eframe::Renderer::Wgpu,
    };
    let baseline = (mode == Mode::Transparent).then(grab_rect);
    let options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("Mythicshot phase 0")
            .with_inner_size([W as f32, H as f32])
            .with_position([X as f32, Y as f32])
            .with_decorations(false)
            .with_always_on_top()
            .with_transparent(mode == Mode::Transparent),
        ..Default::default()
    };
    eframe::run_native(
        "mythicshot-phase0",
        options,
        Box::new(move |_cc| Ok(Box::new(App { mode, started: Instant::now(), frames: 0, excluded: false, baseline, result: None }))),
    )
}
