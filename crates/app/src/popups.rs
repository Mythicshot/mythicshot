//! Two small windows: the destination picker ("Ask every time") and the toast
//! that confirms where a capture went. Both are deferred viewports with shared
//! state, like the settings window.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers, RichText};
use image::RgbaImage;
use mythicshot_destinations::DestId;

use crate::icons::{self, Icon};
use crate::platform;
use crate::theme;

const PICKER_SIZE: [f32; 2] = [270.0, 262.0];
const TOAST_SIZE: [f32; 2] = [380.0, 96.0];
const TOAST_TIME: Duration = Duration::from_secs(5);

/// What the picker resolved to.
pub enum Pick {
    Waiting,
    Chosen(DestId),
    Cancelled,
}

pub struct Picker {
    pub img: RgbaImage,
    pub title: String,
    pub pick: Pick,
    had_focus: bool,
}

pub struct PickerWindow {
    id: egui::ViewportId,
    pub state: Arc<Mutex<Picker>>,
    pos: egui::Pos2,
    first: bool,
}

impl PickerWindow {
    /// A picker for `img`, placed at the mouse pointer (in points) and kept on screen.
    pub fn new(img: RgbaImage, title: String, ppp: f32) -> Self {
        let (cx, cy) = platform::cursor_pos().unwrap_or((200, 200));
        let mon = mythicshot_capture::list_monitors().into_iter().find(|m| m.rect.contains(cx, cy));
        let (mut x, mut y) = (cx as f32 + 12.0, cy as f32 + 12.0);
        if let Some(m) = mon {
            x = x.min((m.rect.right() as f32) - PICKER_SIZE[0] * ppp - 8.0);
            y = y.min((m.rect.bottom() as f32) - PICKER_SIZE[1] * ppp - 48.0);
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            id: egui::ViewportId::from_hash_of(("picker", n)),
            state: Arc::new(Mutex::new(Picker { img, title, pick: Pick::Waiting, had_focus: false })),
            pos: egui::pos2(x / ppp, y / ppp),
            first: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let builder = egui::ViewportBuilder::default()
            .with_title("Send capture to")
            .with_inner_size(PICKER_SIZE)
            .with_position(self.pos)
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_taskbar(false);
        let state = self.state.clone();
        ctx.show_viewport_deferred(self.id, builder, move |ui, _| {
            if let Ok(mut p) = state.lock() {
                p.ui(ui);
            }
        });
        if std::mem::take(&mut self.first) {
            ctx.send_viewport_cmd_to(self.id, egui::ViewportCommand::Focus);
        }
    }
}

impl Picker {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
        if focused {
            self.had_focus = true;
        } else if self.had_focus {
            // Clicking elsewhere dismisses the picker, like a menu.
            self.pick = Pick::Cancelled;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.pick = Pick::Cancelled;
        }
        // A compact menu like Greenshot's destination menu.
        theme::window_card(ui, 6.0, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new(format!("  Send capture ({} × {}) to", self.img.width(), self.img.height())).weak().size(12.0));
            ui.add_space(4.0);
            for d in DestId::ALL.iter().filter(|d| **d != DestId::Picker) {
                if icons::menu_row(ui, dest_icon(*d), d.label()).on_hover_text(d.help()).clicked() {
                    self.pick = Pick::Chosen(*d);
                }
            }
            ui.add_space(2.0);
            ui.separator();
            if icons::menu_row(ui, Icon::Close, "Close").clicked() {
                self.pick = Pick::Cancelled;
            }
        });
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

fn dest_icon(d: DestId) -> Icon {
    match d {
        DestId::Editor => Icon::Editor,
        DestId::File => Icon::Save,
        DestId::FileAs => Icon::SaveAs,
        DestId::Clipboard => Icon::Copy,
        DestId::Print => Icon::Print,
        DestId::Email => Icon::Email,
        DestId::Picker => Icon::Send,
    }
}

pub struct ToastState {
    msg: String,
    path: Option<PathBuf>,
    at: Instant,
    hovered: bool,
    dismissed: bool,
}

/// A short message in the bottom-right corner of the primary screen. It never
/// takes focus, so it does not interrupt a game.
pub struct Toast {
    state: Arc<Mutex<Option<ToastState>>>,
}

impl Toast {
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(None)) }
    }

    pub fn post(&self, msg: String, path: Option<PathBuf>) {
        if let Ok(mut s) = self.state.lock() {
            *s = Some(ToastState { msg, path, at: Instant::now(), hovered: false, dismissed: false });
        }
    }

    pub fn show(&self, ctx: &egui::Context) {
        let visible = {
            let Ok(mut s) = self.state.lock() else { return };
            let keep = s.as_ref().is_some_and(|t| !t.dismissed && (t.hovered || t.at.elapsed() < TOAST_TIME));
            if !keep {
                *s = None;
            }
            keep
        };
        if !visible {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let primary = mythicshot_capture::list_monitors().into_iter().find(|m| m.primary);
        let pos = primary
            .map(|m| egui::pos2((m.rect.right() as f32) / ppp - TOAST_SIZE[0] - 16.0, (m.rect.bottom() as f32) / ppp - TOAST_SIZE[1] - 64.0))
            .unwrap_or(egui::pos2(100.0, 100.0));
        let builder = egui::ViewportBuilder::default()
            .with_title("Mythicshot")
            .with_inner_size(TOAST_SIZE)
            .with_position(pos)
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_active(false);
        let state = self.state.clone();
        ctx.show_viewport_deferred(egui::ViewportId::from_hash_of("toast"), builder, move |ui, _| {
            let Ok(mut guard) = state.lock() else { return };
            let Some(t) = guard.as_mut() else { return };
            t.ui(ui);
        });
    }
}

impl ToastState {
    /// The toast window's contents.
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.hovered = ui.ctx().input(|i| i.pointer.has_pointer());
        theme::window_card(ui, 12.0, |ui| {
            ui.label(RichText::new(&self.msg).color(theme::TEXT));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if let Some(p) = self.path.clone() {
                    if ui.button("Open").clicked() {
                        platform::open_path(&p);
                        self.dismissed = true;
                    }
                    if ui.button("Show in folder").clicked() {
                        platform::reveal(&p);
                        self.dismissed = true;
                    }
                }
                if ui.button("Dismiss").clicked() {
                    self.dismissed = true;
                }
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::checks;

    fn window(size: [f32; 2]) -> egui::RawInput {
        egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size.into())), ..Default::default() }
    }

    #[test]
    fn the_destination_picker_and_the_toast_paint_their_whole_window_opaque() {
        let ctx = theme::test_context();
        let mut picker = Picker { img: RgbaImage::new(640, 480), title: "Capture".into(), pick: Pick::Waiting, had_focus: false };
        let mut toast = ToastState { msg: "Saved to Pictures".into(), path: Some(PathBuf::from("shot.png")), at: Instant::now(), hovered: false, dismissed: false };
        for _ in 0..2 {
            let mut out = ctx.run_ui(window(PICKER_SIZE), |ui| picker.ui(ui));
            out.textures_delta.clear();
            checks::assert_window_opaque(&out, egui::Rect::from_min_size(egui::Pos2::ZERO, PICKER_SIZE.into()), "the destination picker");
            let mut out = ctx.run_ui(window(TOAST_SIZE), |ui| toast.ui(ui));
            out.textures_delta.clear();
            checks::assert_window_opaque(&out, egui::Rect::from_min_size(egui::Pos2::ZERO, TOAST_SIZE.into()), "the toast");
            // The card on it: surface-raised with its border and corners.
            assert!(checks::fills(&out).iter().any(|(_, c, r)| *c == theme::RAISED && *r == theme::RADIUS_LG), "the toast's card");
        }
        // Filled in pixel by pixel: not one pixel lets the desktop through, not even at the card's rounded corners.
        assert_eq!(crate::snapshot::uncovered(&theme::test_context(), PICKER_SIZE.into(), 1.25, 2, |ui| picker.ui(ui)), 0, "the destination picker");
        assert_eq!(crate::snapshot::uncovered(&theme::test_context(), TOAST_SIZE.into(), 1.25, 2, |ui| toast.ui(ui)), 0, "the toast");
    }
}
