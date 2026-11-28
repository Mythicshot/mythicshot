//! The region selector (screen 2 of the mockup): the frozen desktop, dimmed,
//! with a selection, a magnifier and a hint bar. The selection carries the
//! glass band in both modes; in record mode the result starts a recording
//! instead of a screenshot. Tab switches between the two.

use eframe::egui::{self, Align2, ColorImage, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2};
use mythicshot_capture::{Desktop, PxRect};

use crate::theme;
use crate::platform;

pub mod geometry;
use geometry::Mapping;

const MAGNIFIER: f32 = 128.0;
const ZOOM: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// The selector is on screen and seen (set by the app before each frame).
    pub on_screen: bool,
    /// Whether Escape is down, as the OS reports it (a stand-in in tests).
    os_escape_down: fn() -> bool,
    escape: EscapeGate,
}

/// Escape ends a capture only when it is pressed on purpose, once the selector is on screen with
/// the keyboard: not an Escape held down as the selector appears, whose auto-repeats egui takes
/// for a first press (it marks a repeat only for a key it saw go down), nor one pressed for another
/// program just before. Until it is armed every Escape is ignored; it is armed once the selector
/// is ready, no Escape event of its own is still down, and the OS says Escape is up. Then a press
/// that is not an auto-repeat cancels at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EscapeGate {
    armed: bool,
    /// An Escape press reached the selector before it was armed, and no release yet.
    held: bool,
}

impl EscapeGate {
    /// One frame: `ready`, the selector is on screen with the keyboard; `events`, this frame's
    /// Escape events as (pressed, repeat); `down`, the OS says Escape is down. True: cancel.
    pub fn frame(&mut self, ready: bool, events: &[(bool, bool)], down: bool) -> bool {
        let mut cancel = false;
        for &(pressed, repeat) in events {
            if self.armed {
                cancel |= pressed && !repeat;
            } else {
                self.held = pressed;
            }
        }
        if !self.armed && ready && !self.held && !down {
            self.armed = true;
        }
        cancel
    }
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
            on_screen: false,
            os_escape_down: platform::escape_down,
            escape: EscapeGate::default(),
        }
    }

    /// One frame. `client` is the window's client area in desktop pixels as Windows reports
    /// it now; every mapping comes from it and the current scale, never from where the window
    /// was asked to go. None (no native window) takes it to be on the desktop's bounds. `moved`:
    /// the window was moved (placed or put back) just before this frame, after its input was taken.
    pub fn ui(&mut self, ui: &mut egui::Ui, client: Option<PxRect>, moved: bool) -> Outcome {
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

        let esc_space = ctx.input(|i| {
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
            (i.key_pressed(Key::Space), i.key_pressed(Key::Z), i.key_pressed(Key::Enter), i.key_pressed(Key::Tab), a)
        });
        let (space, z, enter, tab, arrows) = (esc_space.0, esc_space.1, esc_space.2, esc_space.3, esc_space.4);
        let escapes: Vec<(bool, bool)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key: Key::Escape, pressed, repeat, .. } => Some((*pressed, *repeat)),
                    _ => None,
                })
                .collect()
        });
        let ready = self.on_screen && ctx.input(|i| i.focused);
        let esc = self.escape.frame(ready, &escapes, (self.os_escape_down)());
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

        // The window was moved just before this frame, or the mapping changed since the last one:
        // egui's pointer positions of this frame (a move or a press) were measured in the window as
        // it was, so they are not read with this frame's mapping. The OS says where the pointer is.
        let map_changed = self.last_map.is_some_and(|m| m != map);
        self.last_map = Some(map);
        let os = if moved || map_changed { (self.os_cursor)() } else { None };
        let from_os = os.map(|(x, y)| map.px_to_pt(x, y));
        // Before the mouse moves, egui has no pointer position; ask the OS.
        let hover = from_os
            .or_else(|| response.hover_pos())
            .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
            .or_else(|| (self.os_cursor)().map(|(x, y)| map.px_to_pt(x, y)));
        let cursor = hover.map(to_px);
        self.note = FrameNote { content: (full.min.x, full.min.y, full.width(), full.height()), ppp, client, hover_px: cursor, texture_new };
        let hovered_window = cursor.and_then(|(x, y)| self.desktop.window_at(x, y)).map(|w| w.rect);

        if ctx.input(|i| i.pointer.primary_pressed()) {
            self.press_px = os.or_else(|| ctx.input(|i| i.pointer.press_origin()).map(to_px));
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
                // The same glass band in both modes, the one that goes around a recording, in
                // whole pixels: a bright edge, a translucent band and a dark edge read on any
                // desktop, dark or light.
                theme::glass_band(painter, h, ppp);
            }
            None => {
                painter.rect_filled(full, 0.0, theme::SCRIM);
            }
        }

        if let Some(sel) = hole {
            let r = to_pt(sel);
            let text = format!("{} × {} · at {}, {}", sel.w, sel.h, sel.x, sel.y);
            // Clear of the glass band: moved out by its thickness, above the region or below it.
            let band = theme::GLASS_BAND_PX as f32 / ppp;
            let at = if r.min.y > 28.0 + band { r.left_top() - vec2(0.0, 6.0 + band) } else { r.left_bottom() + vec2(0.0, 22.0 + band) };
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
            let _ = ov.ui(ui, client, false);
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
        step_moved(ctx, ov, ppp, client, false, events)
    }

    /// The same, `moved` saying whether the window was moved (put back) just before the frame.
    fn step_moved(ctx: &egui::Context, ov: &mut Overlay, ppp: f32, client: PxRect, moved: bool, events: Vec<egui::Event>) -> Outcome {
        let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(client.w as f32 / ppp, client.h as f32 / ppp))), events, ..Default::default() };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        let mut outcome = Outcome::Continue;
        let mut out = ctx.run_ui(input, |ui| outcome = ov.ui(ui, Some(client), moved));
        out.textures_delta.clear();
        outcome
    }

    fn escape(pressed: bool) -> egui::Event {
        egui::Event::Key { key: Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    fn os_down() -> bool {
        true
    }

    fn os_up() -> bool {
        false
    }

    /// The owner's capture: Escape is held down (for the game) as the selector comes on screen
    /// and takes the keyboard. Its auto-repeats reach the selector looking like first presses;
    /// none cancels. Once it is let go, a new press cancels.
    #[test]
    fn an_escape_held_as_the_selector_appears_does_not_cancel_and_a_new_press_does() {
        let bounds = PxRect::new(0, 0, 400, 200);
        let ctx = egui::Context::default();
        let mut ov = Overlay::new(desktop(bounds), false, false);
        ov.os_escape_down = os_down;
        ov.on_screen = true;
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, Vec::new()), Outcome::Continue, "on screen, Escape down");
        for _ in 0..10 {
            assert_eq!(step(&ctx, &mut ov, 1.0, bounds, vec![escape(true)]), Outcome::Continue, "an auto-repeat of the held key");
        }
        ov.os_escape_down = os_up;
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, vec![escape(false)]), Outcome::Continue, "let go");
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, vec![escape(true)]), Outcome::Cancel, "pressed again: cancelled");
    }

    /// An Escape pressed before the selector is on screen (for another program, or while it is
    /// still cloaked) is not taken for one meant for it; the next press is.
    #[test]
    fn an_escape_pressed_before_the_selector_is_on_screen_is_ignored() {
        let bounds = PxRect::new(0, 0, 400, 200);
        let ctx = egui::Context::default();
        let mut ov = Overlay::new(desktop(bounds), false, false);
        ov.os_escape_down = os_up;
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, vec![escape(true)]), Outcome::Continue, "not on screen yet");
        ov.on_screen = true;
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, vec![escape(true)]), Outcome::Continue, "still the press from before: no release seen");
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, vec![escape(false)]), Outcome::Continue);
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, vec![escape(true)]), Outcome::Cancel);
    }

    /// The ordinary case: the selector is up and Escape is pressed: cancelled at once, as before.
    #[test]
    fn an_escape_pressed_once_the_selector_is_up_cancels_at_once() {
        let bounds = PxRect::new(0, 0, 400, 200);
        let ctx = egui::Context::default();
        let mut ov = Overlay::new(desktop(bounds), false, false);
        ov.os_escape_down = os_up;
        ov.on_screen = true;
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, Vec::new()), Outcome::Continue);
        ov.os_escape_down = os_down;
        assert_eq!(step(&ctx, &mut ov, 1.0, bounds, vec![escape(true)]), Outcome::Cancel);
        // A repeat never cancels, also once armed.
        let mut gate = EscapeGate::default();
        assert!(!gate.frame(true, &[], false));
        assert!(!gate.frame(true, &[(true, true)], true));
        assert!(gate.frame(true, &[(true, false)], true));
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

    /// A correction puts the window back on the same client area at the same scale, so the mapping
    /// is the same as the frame before; but a pointer event of this frame was measured in the window
    /// as it was before it was put back. In that frame the pointer is read from the OS.
    #[test]
    fn a_pointer_event_in_the_frame_of_a_correction_is_not_read_with_the_mapping() {
        let bounds = PxRect::new(0, 0, 900, 300);
        let ctx = egui::Context::default();
        let mut ov = Overlay::new(desktop(bounds), false, false);
        ov.os_cursor = || Some((640, 250));
        step(&ctx, &mut ov, 1.0, bounds, vec![egui::Event::PointerMoved(pos2(10.0, 20.0))]);
        assert_eq!(ov.note.hover_px, Some((10, 20)), "egui's pointer while nothing moves");
        // Moved by a scale change, the pointer moved over the stray window, then put back before
        // this frame: same mapping, but egui's 50,60 is a point of the stray window.
        step_moved(&ctx, &mut ov, 1.0, bounds, true, vec![egui::Event::PointerMoved(pos2(50.0, 60.0))]);
        assert_eq!(ov.note.hover_px, Some((640, 250)), "where the OS says the pointer is, not 50,60");
        // The mapping changed and the pointer moved in the same frame: the OS too.
        step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(pos2(7.0, 8.0))]);
        assert_eq!(ov.note.hover_px, Some((640, 250)), "not 14,16 from a point measured before the change");
        // Nothing moved or changed: egui's pointer again, whatever the OS says.
        step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(pos2(5.0, 6.0))]);
        assert_eq!(ov.note.hover_px, Some((10, 12)));
        // No OS reading at all: egui's pointer is still better than none.
        ov.os_cursor = || None;
        step_moved(&ctx, &mut ov, 2.0, bounds, true, vec![egui::Event::PointerMoved(pos2(1.0, 2.0))]);
        assert_eq!(ov.note.hover_px, Some((2, 4)));
    }

    /// A press that arrives in the frame the window was put back was made in the window as it
    /// was: its anchor is the OS pointer's pixel, not its points read with the new mapping. A
    /// press in a frame where nothing moved keeps egui's press origin.
    #[test]
    fn a_press_in_the_frame_of_a_correction_anchors_on_the_os_pointer() {
        let bounds = PxRect::new(0, 0, 900, 300);
        let ctx = egui::Context::default();
        let mut ov = Overlay::new(desktop(bounds), false, false);
        ov.os_cursor = || Some((300, 120));
        let press = pos2(10.0, 20.0);
        step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(press)]);
        step_moved(&ctx, &mut ov, 2.0, bounds, true, vec![button(press, true)]);
        assert_eq!(ov.press_px, Some((300, 120)), "the OS pointer's pixel, not 20,40");
        let to = pos2(200.0, 100.0);
        step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(to - vec2(1.0, 0.0))]);
        step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(to)]);
        assert_eq!(ov.drag_from, Some((300, 120)), "the drag starts on that pixel");
        match step(&ctx, &mut ov, 2.0, bounds, vec![button(to, false)]) {
            Outcome::Capture(sel) => assert_eq!(sel, PxRect::from_corners(300, 120, 400, 200)),
            _ => panic!("the drag ends in a capture"),
        }
        // A press with nothing moved: egui's press origin, read with this frame's mapping.
        let mut ov = Overlay::new(desktop(bounds), false, false);
        ov.os_cursor = || Some((300, 120));
        let ctx = egui::Context::default();
        step(&ctx, &mut ov, 2.0, bounds, vec![egui::Event::PointerMoved(press)]);
        step(&ctx, &mut ov, 2.0, bounds, vec![button(press, true)]);
        assert_eq!(ov.press_px, Some((20, 40)));
    }

    /// The outlines a frame strokes around rectangles: each one's rectangle, width and colour.
    fn outlines(out: &egui::FullOutput) -> Vec<(Rect, f32, egui::Color32)> {
        fn walk(s: &egui::Shape, into: &mut Vec<(Rect, f32, egui::Color32)>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, into)),
                egui::Shape::Rect(r) if r.stroke.width > 0.0 => into.push((r.rect, r.stroke.width, r.stroke.color)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        out.shapes.iter().for_each(|c| walk(&c.shape, &mut all));
        all
    }

    /// The selector's hole, marked with the glass band and nothing else: the band's filled layers
    /// just outside `hole` (in points), never the old red line or the gold outline.
    fn assert_glass_band(out: &egui::FullOutput, hole: Rect, ppp: f32, what: &str) {
        theme::checks::assert_glass_band(out, hole, ppp, what);
        for (r, _, c) in outlines(out) {
            assert!(c != theme::RECORD && c != theme::SELECTION_EDGE && c != theme::SELECTION_HALO, "{what}: a {c:?} outline at {r:?}");
        }
    }

    /// The size label's box: the surface-raised fill under the text that gives the size.
    fn size_label(out: &egui::FullOutput) -> Rect {
        let mut text = None;
        fn walk(s: &egui::Shape, text: &mut Option<Rect>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, text)),
                egui::Shape::Text(t) if t.galley.text().contains('\u{d7}') => *text = Some(t.visual_bounding_rect()),
                _ => {}
            }
        }
        out.shapes.iter().for_each(|c| walk(&c.shape, &mut text));
        let text = text.expect("the size label is drawn");
        theme::checks::fills(out).into_iter().filter(|(r, c, _)| *c == theme::RAISED && r.contains(text.center())).map(|(r, ..)| r).next().expect("its box")
    }

    /// The size label is moved clear of the band: above the region when there is room, below it
    /// otherwise, its box never over any of the band's pixels.
    #[test]
    fn the_size_label_stays_clear_of_the_band() {
        let bounds = PxRect::new(0, 0, 800, 400);
        for ppp in [1.0, 1.25, 2.0] {
            for (from, to, above) in [(pos2(20.0, 80.0), pos2(160.0, 140.0), true), (pos2(20.0, 4.0), pos2(160.0, 70.0), false)] {
                // On whole pixels at this scale.
                let snap = |p: Pos2| pos2((p.x * ppp).round() / ppp, (p.y * ppp).round() / ppp);
                let (from, to) = (snap(from), snap(to));
                let ctx = egui::Context::default();
                let mut ov = Overlay::new(desktop(bounds), false, false);
                ov.os_cursor = || None;
                step(&ctx, &mut ov, ppp, bounds, vec![egui::Event::PointerMoved(from)]);
                step(&ctx, &mut ov, ppp, bounds, vec![button(from, true)]);
                step(&ctx, &mut ov, ppp, bounds, vec![egui::Event::PointerMoved(to - vec2(1.0, 0.0))]);
                let out = step_out(&ctx, &mut ov, ppp, bounds, vec![egui::Event::PointerMoved(to)]);
                let hole = Rect::from_min_max(from, to);
                let label = size_label(&out);
                assert_eq!(label.max.y <= hole.min.y, above, "{ppp}: the label {label:?} against the region {hole:?}");
                for (band, _) in theme::glass_band_rects(hole, ppp) {
                    assert!(!label.intersect(band).is_positive(), "{ppp}, above {above}: the label {label:?} covers the band at {band:?}");
                }
            }
        }
    }

    /// One frame with these events, its output kept.
    fn step_out(ctx: &egui::Context, ov: &mut Overlay, ppp: f32, client: PxRect, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(client.w as f32 / ppp, client.h as f32 / ppp))), events, ..Default::default() };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        let mut out = ctx.run_ui(input, |ui| {
            let _ = ov.ui(ui, Some(client), false);
        });
        out.textures_delta.clear();
        out
    }

    /// A dragged region carries the glass band in screenshot mode and in record mode alike.
    #[test]
    fn a_dragged_region_carries_the_glass_band_in_both_modes() {
        let bounds = PxRect::new(0, 0, 800, 400);
        for record in [false, true] {
            for ppp in [1.0, 2.0] {
                let ctx = egui::Context::default();
                let mut ov = Overlay::new(desktop(bounds), false, record);
                ov.os_cursor = || None;
                let (from, to) = (pos2(20.0, 30.0), pos2(120.0, 90.0));
                step(&ctx, &mut ov, ppp, bounds, vec![egui::Event::PointerMoved(from)]);
                step(&ctx, &mut ov, ppp, bounds, vec![button(from, true)]);
                step(&ctx, &mut ov, ppp, bounds, vec![egui::Event::PointerMoved(to - vec2(1.0, 0.0))]);
                let out = step_out(&ctx, &mut ov, ppp, bounds, vec![egui::Event::PointerMoved(to)]);
                assert!(ov.drag_from.is_some(), "a drag is under way");
                assert_glass_band(&out, Rect::from_min_max(from, to), ppp, &format!("record {record}, {ppp}"));
            }
        }
    }

    /// A window under the pointer in window mode carries the same band, in both modes.
    #[test]
    fn a_hovered_window_carries_the_glass_band_in_both_modes() {
        let bounds = PxRect::new(0, 0, 800, 400);
        for record in [false, true] {
            let ppp = 1.25;
            let mut d = desktop(bounds);
            d.windows.push(mythicshot_capture::WindowInfo { title: String::new(), app: String::new(), rect: PxRect::new(100, 50, 300, 200), focused: false });
            let ctx = egui::Context::default();
            let mut ov = Overlay::new(d, false, record);
            ov.os_cursor = || None;
            ov.window_mode = true;
            let out = step_out(&ctx, &mut ov, ppp, bounds, vec![egui::Event::PointerMoved(pos2(200.0, 100.0))]);
            let hole = Rect::from_min_max(pos2(100.0 / ppp, 50.0 / ppp), pos2(400.0 / ppp, 250.0 / ppp));
            assert_glass_band(&out, hole, ppp, &format!("window, record {record}"));
        }
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
