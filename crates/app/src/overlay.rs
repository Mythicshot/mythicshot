//! The region selector (screen 2 of the mockup): the frozen desktop, dimmed,
//! with a selection, a magnifier and a hint bar. In record mode the selection
//! is red and the result starts a recording instead of a screenshot; Tab
//! switches between the two.

use eframe::egui::{self, Align2, ColorImage, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2};
use mythicshot_capture::{Desktop, PxRect};

use crate::theme;
use crate::platform;

const MAGNIFIER: f32 = 128.0;
const ZOOM: f32 = 8.0;

pub enum Outcome {
    Continue,
    Cancel,
    Capture(PxRect),
}

pub struct Overlay {
    pub desktop: Desktop,
    texture: Option<TextureHandle>,
    window_mode: bool,
    drag_from: Option<(i32, i32)>,
    pub magnifier: bool,
    /// Selecting an area to record rather than to screenshot.
    pub record: bool,
}

impl Overlay {
    pub fn new(desktop: Desktop, magnifier: bool, record: bool) -> Self {
        Self { desktop, texture: None, window_mode: false, drag_from: None, magnifier, record }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Outcome {
        let ctx = ui.ctx().clone();
        let b = self.desktop.bounds;
        let tex = self
            .texture
            .get_or_insert_with(|| {
                let img = ColorImage::from_rgba_unmultiplied([b.w as usize, b.h as usize], self.desktop.image.as_raw());
                ctx.load_texture("frozen-desktop", img, TextureOptions::NEAREST)
            })
            .clone();
        let ppp = ctx.pixels_per_point();
        let to_px = |p: Pos2| (b.x + (p.x * ppp).round() as i32, b.y + (p.y * ppp).round() as i32);
        let to_pt = |r: PxRect| {
            Rect::from_min_size(pos2((r.x - b.x) as f32 / ppp, (r.y - b.y) as f32 / ppp), vec2(r.w as f32 / ppp, r.h as f32 / ppp))
        };

        let full = ctx.content_rect();
        let response = ui.interact(full, ui.scope_id().with("overlay"), Sense::click_and_drag());
        ctx.set_cursor_icon(CursorIcon::Crosshair);

        let (esc, space, z, enter, tab, arrows) = ctx.input(|i| {
            let step = if i.modifiers.shift { 10 } else { 1 };
            let mut a = (0, 0);
            if i.key_pressed(Key::ArrowLeft) {
                a.0 -= step;
            }
            if i.key_pressed(Key::ArrowRight) {
                a.0 += step;
            }
            if i.key_pressed(Key::ArrowUp) {
                a.1 -= step;
            }
            if i.key_pressed(Key::ArrowDown) {
                a.1 += step;
            }
            (i.key_pressed(Key::Escape), i.key_pressed(Key::Space), i.key_pressed(Key::Z), i.key_pressed(Key::Enter), i.key_pressed(Key::Tab), a)
        });
        if esc {
            return Outcome::Cancel;
        }
        if space {
            self.window_mode = !self.window_mode;
            self.drag_from = None;
        }
        if z {
            self.magnifier = !self.magnifier;
        }
        if tab {
            self.record = !self.record;
        }
        let accent = if self.record { theme::RECORD } else { theme::SELECTION_EDGE };
        if arrows != (0, 0) {
            platform::nudge_cursor(arrows.0, arrows.1);
        }

        // Before the mouse moves, egui has no pointer position; ask the OS.
        let hover = response
            .hover_pos()
            .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
            .or_else(|| platform::cursor_pos().map(|(x, y)| pos2((x - b.x) as f32 / ppp, (y - b.y) as f32 / ppp)));
        let cursor = hover.map(to_px);
        let hovered_window = cursor.and_then(|(x, y)| self.desktop.window_at(x, y)).map(|w| w.rect);

        if !self.window_mode {
            if response.drag_started() {
                self.drag_from = response.interact_pointer_pos().map(to_px);
            }
        }
        let selection = match (self.drag_from, cursor) {
            (Some((ax, ay)), Some((cx, cy))) if !self.window_mode => Some(PxRect::from_corners(ax, ay, cx, cy)),
            _ => None,
        };

        if response.drag_stopped() {
            if let Some(sel) = selection.filter(|s| s.w >= 2 && s.h >= 2) {
                return Outcome::Capture(sel);
            }
            self.drag_from = None;
        }
        if response.clicked() || enter {
            // A click without a drag takes the window under the pointer, in either mode.
            if let Some(w) = hovered_window {
                return Outcome::Capture(w);
            }
        }

        let painter = ui.painter();
        painter.image(tex.id(), full, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), theme::IMAGE_TINT);

        let hole = selection.or(if self.window_mode || self.drag_from.is_none() { hovered_window.filter(|_| self.window_mode) } else { None });
        match hole.map(to_pt) {
            Some(h) => {
                painter.rect_filled(Rect::from_min_max(full.min, pos2(full.max.x, h.min.y)), 0.0, theme::SCRIM);
                painter.rect_filled(Rect::from_min_max(pos2(full.min.x, h.max.y), full.max), 0.0, theme::SCRIM);
                painter.rect_filled(Rect::from_min_max(pos2(full.min.x, h.min.y), pos2(h.min.x, h.max.y)), 0.0, theme::SCRIM);
                painter.rect_filled(Rect::from_min_max(pos2(h.max.x, h.min.y), pos2(full.max.x, h.max.y)), 0.0, theme::SCRIM);
                painter.rect_stroke(h, 0.0, Stroke::new(if self.record { 3.0 } else { 2.0 }, accent), StrokeKind::Outside);
            }
            None => {
                painter.rect_filled(full, 0.0, theme::SCRIM);
            }
        }

        if let Some(sel) = hole {
            let r = to_pt(sel);
            let text = format!("{} × {} · at {}, {}", sel.w, sel.h, sel.x, sel.y);
            let at = if r.min.y > 28.0 { r.left_top() - vec2(0.0, 6.0) } else { r.left_bottom() + vec2(0.0, 22.0) };
            label(painter, at, Align2::LEFT_BOTTOM, &text);
        }

        if let Some(p) = hover {
            if selection.is_none() && !self.window_mode {
                let line = Stroke::new(1.0, theme::PICTURE_GUIDE);
                painter.hline(full.x_range(), p.y, line);
                painter.vline(p.x, full.y_range(), line);
            }
            if self.magnifier {
                self.draw_magnifier(painter, &tex, p, full, ppp);
            }
        }

        let screen = cursor.and_then(|(x, y)| self.desktop.monitor_at(x, y)).map(|m| to_pt(m.rect)).unwrap_or(full);
        let mode = match (self.record, self.window_mode) {
            (false, false) => "Screenshot · Region",
            (false, true) => "Screenshot · Window",
            (true, false) => "Record GIF · Region",
            (true, true) => "Record GIF · Window",
        };
        label(painter, screen.center_top() + vec2(0.0, 18.0), Align2::CENTER_TOP, mode);
        label(
            painter,
            screen.center_bottom() - vec2(0.0, 18.0),
            Align2::CENTER_BOTTOM,
            "Drag  select    Click  window    Space  region / window    Tab  screenshot / record    Arrows  nudge 1 px (Shift: 10)    Z  magnifier    Esc  cancel",
        );

        ctx.request_repaint();
        Outcome::Continue
    }

    fn draw_magnifier(&self, painter: &egui::Painter, tex: &TextureHandle, p: Pos2, full: Rect, ppp: f32) {
        let mut at = p + vec2(24.0, 24.0);
        if at.x + MAGNIFIER > full.max.x {
            at.x = p.x - 24.0 - MAGNIFIER;
        }
        if at.y + MAGNIFIER + 22.0 > full.max.y {
            at.y = p.y - 24.0 - MAGNIFIER - 22.0;
        }
        let rect = Rect::from_min_size(at, vec2(MAGNIFIER, MAGNIFIER));
        let (w, h) = (self.desktop.bounds.w as f32, self.desktop.bounds.h as f32);
        let half = MAGNIFIER / ZOOM / 2.0;
        let (cx, cy) = (p.x * ppp, p.y * ppp);
        let uv = Rect::from_min_max(pos2((cx - half) / w, (cy - half) / h), pos2((cx + half) / w, (cy + half) / h));
        painter.rect_filled(rect, 4.0, theme::PICTURE_BACKDROP);
        painter.image(tex.id(), rect, uv, theme::IMAGE_TINT);
        let red = Stroke::new(1.0, theme::RECORD);
        painter.hline(rect.x_range(), rect.center().y, red);
        painter.vline(rect.center().x, rect.y_range(), red);
        painter.rect_stroke(rect, 4.0, Stroke::new(2.0, theme::ON_PICTURE), StrokeKind::Outside);
        let (x, y) = (self.desktop.bounds.x + cx.round() as i32, self.desktop.bounds.y + cy.round() as i32);
        label(painter, rect.center_bottom() + vec2(0.0, 20.0), Align2::CENTER_BOTTOM, &format!("x {x} · y {y}"));
    }
}

fn label(painter: &egui::Painter, at: Pos2, align: Align2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(13.0), theme::TEXT);
    let rect = align.anchor_size(at, galley.size()).expand2(vec2(8.0, 4.0));
    painter.rect_filled(rect, f32::from(theme::RADIUS_MD), theme::RAISED);
    painter.galley(rect.min + vec2(8.0, 4.0), galley, theme::TEXT);
}
