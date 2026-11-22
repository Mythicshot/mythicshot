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
use crate::placement::{self, Focus, Placer, TOAST_SIZE};
use crate::platform;
use crate::theme;

/// The picker's window title, also how its window is found to be placed.
const PICKER_TITLE: &str = "Send capture to";
/// The toast's window title: its own, so it is never taken for the root window ("Mythicshot").
const TOAST_TITLE: &str = "Mythicshot notification";
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
    /// Puts the window beside the pointer in pixels, then shows it.
    placer: Placer,
    /// Its rectangle in points for egui, a first guess the placer corrects.
    guess: (egui::Pos2, egui::Vec2),
}

impl PickerWindow {
    /// A picker for `img`, beside the mouse pointer on its monitor, in that monitor's scale.
    pub fn new(img: RgbaImage, title: String) -> Self {
        let monitors = mythicshot_capture::list_monitors();
        let target = placement::picker_rect(platform::cursor_pos().unwrap_or((200, 200)), &monitors);
        let guess = placement::guess_points(target, placement::scale_for(target, &monitors));
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            id: egui::ViewportId::from_hash_of(("picker", n)),
            state: Arc::new(Mutex::new(Picker { img, title, pick: Pick::Waiting, had_focus: false })),
            placer: Placer::new(PICKER_TITLE, target, Focus::PlainThenWinit),
            guess,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        // Created hidden, put on its pixels, then shown and given the keyboard (see `Placer`).
        let builder = egui::ViewportBuilder::default()
            .with_title(PICKER_TITLE)
            .with_inner_size(self.guess.1)
            .with_position(self.guess.0)
            .with_visible(false)
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
        self.placer.tick(ctx, self.id);
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
    /// Puts the window in the corner in pixels while it is shown; None while there is no toast.
    placer: Option<(Placer, (egui::Pos2, egui::Vec2))>,
}

impl Toast {
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(None)), placer: None }
    }

    pub fn post(&self, msg: String, path: Option<PathBuf>) {
        if let Ok(mut s) = self.state.lock() {
            *s = Some(ToastState { msg, path, at: Instant::now(), hovered: false, dismissed: false });
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let visible = {
            let Ok(mut s) = self.state.lock() else { return };
            let keep = s.as_ref().is_some_and(|t| !t.dismissed && (t.hovered || t.at.elapsed() < TOAST_TIME));
            if !keep {
                *s = None;
            }
            keep
        };
        if !visible {
            // The window goes; the next toast gets a new one, placed again.
            self.placer = None;
            return;
        }
        let (placer, (pos, size)) = self.placer.get_or_insert_with(|| {
            let monitors = mythicshot_capture::list_monitors();
            let target = placement::toast_rect(&monitors).unwrap_or(mythicshot_capture::PxRect::new(100, 100, TOAST_SIZE[0] as u32, TOAST_SIZE[1] as u32));
            (Placer::new(TOAST_TITLE, target, Focus::None), placement::guess_points(target, placement::scale_for(target, &monitors)))
        });
        // Created hidden, put on its pixels, then shown without taking focus (see `Placer`).
        let builder = egui::ViewportBuilder::default()
            .with_title(TOAST_TITLE)
            .with_inner_size(*size)
            .with_position(*pos)
            .with_visible(false)
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
        placer.tick(ctx, egui::ViewportId::from_hash_of("toast"));
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
    use crate::placement::PICKER_SIZE;
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
