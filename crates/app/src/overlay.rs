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
}

pub struct Overlay {
    pub desktop: Desktop,
    /// The last frame, for the capture log.
    pub note: FrameNote,
    /// The selection of the last capture, in points of the frame it was made in.
    pub selection_pt: Option<Rect>,
    texture: Option<TextureHandle>,
    window_mode: bool,
    /// Where a drag started, in desktop pixels: it stays on that pixel if the window is put
    /// back on its target (or moves) during the drag.
    drag_from: Option<(i32, i32)>,
    /// The desktop pixel the primary button went down on, read with the mapping of that frame:
    /// egui only says a drag started some frames later, maybe after the mapping changed.
    press_px: Option<(i32, i32)>,
    /// The mapping of the last frame: when it changes and egui has no pointer event since, the
    /// pointer's position in points is from the old mapping and is read from the OS instead.
    last_map: Option<Mapping>,
    /// The pointer in desktop pixels as the OS reports it (a stand-in in tests).
    os_cursor: fn() -> Option<(i32, i32)>,
    pub magnifier: bool,
    /// Selecting an area to record rather than to screenshot.
    pub record: bool,
}

impl Overlay {
    pub fn new(desktop: Desktop, magnifier: bool, record: bool) -> Self {
        Self {
            desktop,
            note: FrameNote::default(),
            selection_pt: None,
            texture: None,
            window_mode: false,
            drag_from: None,
            press_px: None,
            last_map: None,
            os_cursor: platform::cursor_pos,
            magnifier,
            record,
        }
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
        if arrows != (0, 0) {
            platform::nudge_cursor(arrows.0, arrows.1);
        }

        // The window was put back (or moved) since the last frame and the pointer has not moved
        // since: egui's position is in the old window's points. The OS says where it is.
        let map_changed = self.last_map.is_some_and(|m| m != map);
        self.last_map = Some(map);
        let pointer_event = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_))));
        let from_os = if map_changed && !pointer_event { (self.os_cursor)().map(|(x, y)| map.px_to_pt(x, y)) } else { None };
        // Before the mouse moves, egui has no pointer position; ask the OS.
        let hover = from_os
            .or_else(|| response.hover_pos())
            .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
            .or_else(|| (self.os_cursor)().map(|(x, y)| map.px_to_pt(x, y)));
        let cursor = hover.map(to_px);
        self.note = FrameNote { content: (full.min.x, full.min.y, full.width(), full.height()), ppp, client, hover_px: cursor, texture_new };
        let hovered_window = cursor.and_then(|(x, y)| self.desktop.window_at(x, y)).map(|w| w.rect);

        if ctx.input(|i| i.pointer.primary_pressed()) {
            self.press_px = ctx.input(|i| i.pointer.press_origin()).map(to_px);
        }
        if !self.window_mode {
            if response.drag_started() {
                self.drag_from = self.press_px.or_else(|| response.interact_pointer_pos().map(to_px));
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
                if self.record {
                    painter.rect_stroke(h, 0.0, Stroke::new(3.0, theme::RECORD), StrokeKind::Outside);
                } else {
                    // Over the darkened desktop, which can be light: two-tone, as on a picture.
                    theme::selection_outline(painter, h, 0.0, 2.0);
                }
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
    painter.rect_filled(rect, f32::from(theme::control_radius(rect.height())), theme::RAISED);
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

    /// Where the frozen picture (the first textured mesh) was painted, in points.
    fn picture(ctx: &egui::Context, out: egui::FullOutput, ppp: f32) -> Rect {
        ctx.tessellate(out.shapes, ppp)
            .iter()
            .find_map(|c| match &c.primitive {
                egui::epaint::Primitive::Mesh(m) if m.texture_id != egui::TextureId::default() => Some(m.calc_bounds()),
                _ => None,
            })
            .expect("the picture is painted")
    }

    /// One frame on a context kept across frames, with these events.
    fn step(ctx: &egui::Context, ov: &mut Overlay, ppp: f32, client: PxRect, events: Vec<egui::Event>) -> Outcome {
        let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(client.w as f32 / ppp, client.h as f32 / ppp))), events, ..Default::default() };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        let mut outcome = Outcome::Continue;
        let mut out = ctx.run_ui(input, |ui| outcome = ov.ui(ui, Some(client)));
        out.textures_delta.clear();
        outcome
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn a_drag_across_a_correction_keeps_its_anchor_pixel_and_ends_on_the_pointers_pixel() {
        let bounds = PxRect::new(0, 0, 900, 300);
        // Pressed while the window was the shrunk one: a third of the desktop at 100%.
        let shrunk = PxRect::new(0, 1, 300, 100);
        let ctx = egui::Context::default();
        let mut ov = Overlay::new(desktop(bounds), false, false);
        ov.os_cursor = || None;
        let press = pos2(10.0, 20.0);
        // egui hit-tests against the widgets of the frame before: one frame first.
        step(&ctx, &mut ov, 1.0, shrunk, vec![egui::Event::PointerMoved(press)]);
        assert!(matches!(step(&ctx, &mut ov, 1.0, shrunk, vec![button(press, true)]), Outcome::Continue));
        assert_eq!(ov.note.hover_px, Some((10, 21)));
        // Put back on the bounds at 200% before egui says the drag started: the press stays on
        // the pixel it was made on, not on its points read with the new mapping.
        let to = pos2(200.0, 100.0);
        assert!(matches!(step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(to - vec2(1.0, 0.0))]), Outcome::Continue));
        assert!(matches!(step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(to)]), Outcome::Continue));
        assert_eq!(ov.drag_from, Some((10, 21)), "the anchor pixel (read with the new mapping it would be 20,40)");
        assert_eq!(ov.note.hover_px, Some((400, 200)));
        match step(&ctx, &mut ov, 2.0, bounds, vec![button(to, false)]) {
            Outcome::Capture(sel) => assert_eq!(sel, PxRect::from_corners(10, 21, 400, 200)),
            _ => panic!("the drag ends in a capture"),
        }
    }

    #[test]
    fn after_a_correction_with_no_pointer_event_the_pointer_is_read_from_the_os() {
        let bounds = PxRect::new(0, 0, 900, 300);
        let ctx = egui::Context::default();
        let mut ov = Overlay::new(desktop(bounds), false, false);
        ov.os_cursor = || Some((640, 250));
        step(&ctx, &mut ov, 1.0, PxRect::new(0, 1, 300, 100), vec![egui::Event::PointerMoved(pos2(10.0, 20.0))]);
        assert_eq!(ov.note.hover_px, Some((10, 21)), "egui's pointer while the mapping holds");
        // The window was put back; the pointer did not move, so egui's position is in the old
        // window's points (10,20 would read as 20,40 now).
        step(&ctx, &mut ov, 2.0, bounds, Vec::new());
        assert_eq!(ov.note.hover_px, Some((640, 250)), "where the OS says the pointer is");
        // The mapping holds again: egui's pointer is used.
        step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(pos2(5.0, 6.0))]);
        assert_eq!(ov.note.hover_px, Some((10, 12)));
    }

    #[test]
    fn the_pointer_and_picture_follow_the_real_window_not_the_bounds() {
        let bounds = PxRect::new(-1920, 0, 400, 200);
        // The window a scale change left half-size and 161 px to the right of the bounds.
        let stray = PxRect::new(-1759, 0, 200, 100);
        let mut ov = Overlay::new(desktop(bounds), false, false);
        let (ctx, out) = frame(&mut ov, 1.0, stray, Some(stray), pos2(10.0, 20.0));
        assert_eq!(ov.note.hover_px, Some((-1749, 20)), "the pixel under the pointer, from the real window");
        assert!(ov.note.texture_new);
        // The picture starts 161 px left of that window, at the bounds, at full size: each of
        // its pixels on the screen pixel it was captured from (the window shows part of it).
        let pic = picture(&ctx, out, 1.0);
        assert_eq!((pic.min, pic.max), (pos2(-161.0, 0.0), pos2(239.0, 200.0)));
        // The same misplaced window at 200%.
        let mut ov = Overlay::new(desktop(bounds), false, false);
        let (ctx, out) = frame(&mut ov, 2.0, stray, Some(stray), pos2(10.0, 20.0));
        assert_eq!(ov.note.hover_px, Some((-1739, 40)));
        let pic = picture(&ctx, out, 2.0);
        assert_eq!((pic.min, pic.max), (pos2(-80.5, 0.0), pos2(119.5, 100.0)), "400x200 pixels at 200% from -1920,0");
        // On its bounds at 200%: a pointer at 10,20 points is 20,40 pixels into the desktop.
        let mut ov = Overlay::new(desktop(bounds), false, false);
        let (ctx, out) = frame(&mut ov, 2.0, bounds, Some(bounds), pos2(10.0, 20.0));
        assert_eq!(ov.note.hover_px, Some((-1900, 40)));
        assert_eq!(ov.note.ppp, 2.0);
        assert_eq!((ov.note.content.2, ov.note.content.3), (200.0, 100.0));
        // On the bounds the picture covers the window pixel for pixel.
        let pic = picture(&ctx, out, 2.0);
        assert_eq!((pic.min, pic.max), (pos2(0.0, 0.0), pos2(200.0, 100.0)), "200x100 points at 200% = 400x200 pixels, the picture's size");
    }
}
