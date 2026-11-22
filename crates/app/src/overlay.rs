//! The region selector (screen 2 of the mockup): the frozen desktop, dimmed,
//! with a selection, a magnifier and a hint bar. In record mode the selection
//! is red and the result starts a recording instead of a screenshot; Tab
//! switches between the two.

use eframe::egui::{self, Align2, ColorImage, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2};
use mythicshot_capture::{Desktop, PxRect};

use crate::theme;
use crate::platform;

pub mod geometry;
use geometry::Mapping;

const MAGNIFIER: f32 = 128.0;
const ZOOM: f32 = 8.0;

pub enum Outcome {
    Continue,
    Cancel,
    Capture(PxRect),
}

/// What one frame of the selector saw, for the capture log.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameNote {
    /// egui's content area, in points.
    pub content: (f32, f32, f32, f32),
    pub ppp: f32,
    /// The window's client area the mapping used, in desktop pixels.
    pub client: Option<PxRect>,
    /// The desktop pixel the selector takes to be under the pointer.
    pub hover_px: Option<(i32, i32)>,
    /// The frozen picture was handed to the GPU this frame.
    pub texture_new: bool,
    pub texture_ready: bool,
}

pub struct Overlay {
    pub desktop: Desktop,
    /// The last frame, for the capture log.
    pub note: FrameNote,
    /// The selection of the last capture, in points of the frame it was made in.
    pub selection_pt: Option<Rect>,
    texture: Option<TextureHandle>,
    window_mode: bool,
    drag_from: Option<(i32, i32)>,
    pub magnifier: bool,
    /// Selecting an area to record rather than to screenshot.
    pub record: bool,
}

impl Overlay {
    pub fn new(desktop: Desktop, magnifier: bool, record: bool) -> Self {
        Self { desktop, note: FrameNote::default(), selection_pt: None, texture: None, window_mode: false, drag_from: None, magnifier, record }
    }

    /// One frame. `client` is the window's client area in desktop pixels as Windows reports
    /// it now; every mapping comes from it and the current scale, never from where the window
    /// was asked to go. None (no native window) takes it to be on the desktop's bounds.
    pub fn ui(&mut self, ui: &mut egui::Ui, client: Option<PxRect>) -> Outcome {
        let ctx = ui.ctx().clone();
        let b = self.desktop.bounds;
        let texture_new = self.texture.is_none();
        let tex = self
            .texture
            .get_or_insert_with(|| {
                let img = ColorImage::from_rgba_unmultiplied([b.w as usize, b.h as usize], self.desktop.image.as_raw());
                ctx.load_texture("frozen-desktop", img, TextureOptions::NEAREST)
            })
            .clone();
        let ppp = ctx.pixels_per_point();
        let map = Mapping { client: client.unwrap_or(b), ppp };
        let to_px = |p: Pos2| map.to_px(p);
        let to_pt = |r: PxRect| map.to_pt(r);

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
            .or_else(|| platform::cursor_pos().map(|(x, y)| map.px_to_pt(x, y)));
        let cursor = hover.map(to_px);
        self.note = FrameNote { content: (full.min.x, full.min.y, full.width(), full.height()), ppp, client, hover_px: cursor, texture_new, texture_ready: true };
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
                self.selection_pt = Some(to_pt(sel));
                return Outcome::Capture(sel);
            }
            self.drag_from = None;
        }
        if response.clicked() || enter {
            // A click without a drag takes the window under the pointer, in either mode.
            if let Some(w) = hovered_window {
                self.selection_pt = Some(to_pt(w));
                return Outcome::Capture(w);
            }
        }

        let painter = ui.painter();
        // Each pixel of the frozen desktop on one pixel of the screen, where it was captured.
        painter.image(tex.id(), map.picture_rect(b), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), theme::IMAGE_TINT);

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
                self.draw_magnifier(painter, &tex, p, full, map);
            }
        }

        let screen = cursor.and_then(|(x, y)| self.desktop.monitor_at(x, y)).map(|m| to_pt(m.rect).intersect(full)).filter(|r| r.is_positive()).unwrap_or(full);
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

    fn draw_magnifier(&self, painter: &egui::Painter, tex: &TextureHandle, p: Pos2, full: Rect, map: Mapping) {
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
        // The pointer in pixels of the frozen picture.
        let (px, py) = map.to_px(p);
        let (cx, cy) = ((px - self.desktop.bounds.x) as f32, (py - self.desktop.bounds.y) as f32);
        let uv = Rect::from_min_max(pos2((cx - half) / w, (cy - half) / h), pos2((cx + half) / w, (cy + half) / h));
        painter.rect_filled(rect, 4.0, theme::PICTURE_BACKDROP);
        painter.image(tex.id(), rect, uv, theme::IMAGE_TINT);
        let red = Stroke::new(1.0, theme::RECORD);
        painter.hline(rect.x_range(), rect.center().y, red);
        painter.vline(rect.center().x, rect.y_range(), red);
        painter.rect_stroke(rect, 4.0, Stroke::new(2.0, theme::ON_PICTURE), StrokeKind::Outside);
        label(painter, rect.center_bottom() + vec2(0.0, 20.0), Align2::CENTER_BOTTOM, &format!("x {px} · y {py}"));
    }
}

fn label(painter: &egui::Painter, at: Pos2, align: Align2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(13.0), theme::TEXT);
    let rect = align.anchor_size(at, galley.size()).expand2(vec2(8.0, 4.0));
    painter.rect_filled(rect, f32::from(theme::RADIUS_MD), theme::RAISED);
    painter.galley(rect.min + vec2(8.0, 4.0), galley, theme::TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mythicshot_capture::MonitorInfo;
    use mythicshot_capture::image::RgbaImage;

    fn desktop(bounds: PxRect) -> Desktop {
        let monitors = vec![MonitorInfo { name: String::new(), primary: true, rect: bounds, scale: 2.0 }];
        Desktop { bounds, image: RgbaImage::new(bounds.w, bounds.h), monitors, windows: Vec::new() }
    }

    /// One frame of the selector at `ppp`, a window of `client_px`, the pointer at `pointer` points.
    fn frame(ov: &mut Overlay, ppp: f32, client_px: PxRect, client: Option<PxRect>, pointer: Pos2) -> (egui::Context, egui::FullOutput) {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(client_px.w as f32 / ppp, client_px.h as f32 / ppp))),
            events: vec![egui::Event::PointerMoved(pointer)],
            ..Default::default()
        };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        let mut out = ctx.run_ui(input, |ui| {
            let _ = ov.ui(ui, client);
        });
        out.textures_delta.clear();
        (ctx, out)
    }

    #[test]
    fn the_pointer_and_picture_follow_the_real_window_not_the_bounds() {
        let bounds = PxRect::new(-1920, 0, 400, 200);
        // The window a scale change left half-size and 161 px to the right of the bounds.
        let stray = PxRect::new(-1759, 0, 200, 100);
        let mut ov = Overlay::new(desktop(bounds), false, false);
        frame(&mut ov, 1.0, stray, Some(stray), pos2(10.0, 20.0));
        assert_eq!(ov.note.hover_px, Some((-1749, 20)), "the pixel under the pointer, from the real window");
        assert!(ov.note.texture_new && ov.note.texture_ready);
        // On its bounds at 200%: a pointer at 10,20 points is 20,40 pixels into the desktop.
        let mut ov = Overlay::new(desktop(bounds), false, false);
        let (ctx, out) = frame(&mut ov, 2.0, bounds, Some(bounds), pos2(10.0, 20.0));
        assert_eq!(ov.note.hover_px, Some((-1900, 40)));
        assert_eq!(ov.note.ppp, 2.0);
        assert_eq!((ov.note.content.2, ov.note.content.3), (200.0, 100.0));
        // The frozen picture is the first textured mesh: it covers the window pixel for pixel.
        let meshes = ctx.tessellate(out.shapes, 2.0);
        let picture = meshes
            .iter()
            .find_map(|c| match &c.primitive {
                egui::epaint::Primitive::Mesh(m) if m.texture_id != egui::TextureId::default() => Some(m.calc_bounds()),
                _ => None,
            })
            .expect("the picture is painted");
        assert_eq!((picture.min, picture.max), (pos2(0.0, 0.0), pos2(200.0, 100.0)), "200x100 points at 200% = 400x200 pixels, the picture's size");
    }
}
