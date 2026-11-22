//! Live draw: drawing on the screen while recording.
//!
//! Draw mode covers the recorded area with a window of its own, kept out of
//! screen capture like the control bar, so the GIF never shows the window or
//! its toolbar. Each finished stroke is stamped with the recording clock; when
//! the recording stops, strokes become ordinary annotations with frame ranges,
//! drawn at export and editable in the GIF editor like any other.
//!
//! A stroke shows from when it is finished until it is undone, cleared, or
//! draw mode ends.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, CornerRadius, Pos2, Stroke, StrokeKind, TextureHandle, vec2};
use mythicshot_editor::{Bounds, Item, Pixmap, Pt, Rgba, Shape, Style, render_items_onto};
use mythicshot_recording::{ClockHandle, Recording};

use crate::theme;
use crate::icons::{self, Icon};
use crate::platform;

pub const DRAW_TITLE: &str = "Mythicshot live draw";

const COLORS: [Rgba; 5] = [Rgba::RED, Rgba::YELLOW, Rgba([46, 204, 113, 255]), Rgba([52, 152, 219, 255]), Rgba::WHITE];
const WIDTH: f32 = 4.0;

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Pen,
    Arrow,
    Rect,
    Ellipse,
}

struct LiveStroke {
    shape: Shape,
    style: Style,
    /// Recording time the stroke was finished, and when it went.
    from_ms: u64,
    until_ms: Option<u64>,
}

struct State {
    clock: Option<ClockHandle>,
    tool: Tool,
    color: Rgba,
    strokes: Vec<LiveStroke>,
    drag: Option<Vec<Pt>>,
    /// Bumped when the strokes on screen change, to redraw the texture.
    version: u64,
    texture: Option<(u64, TextureHandle)>,
    /// Draw nothing until the window is known to be out of screen capture.
    hidden_from_capture: bool,
    /// Esc or Done was pressed in the window.
    done: bool,
}

impl State {
    fn now(&self) -> u64 {
        self.clock.as_ref().map_or(0, |c| c.active_ms())
    }

    fn style(&self) -> Style {
        Style { stroke: self.color, width: WIDTH, ..Style::default() }
    }

    fn on_screen(&self) -> impl Iterator<Item = &LiveStroke> {
        self.strokes.iter().filter(|s| s.until_ms.is_none())
    }

    fn clear(&mut self) {
        let now = self.now();
        for s in self.strokes.iter_mut().filter(|s| s.until_ms.is_none()) {
            s.until_ms = Some(now);
        }
        self.version += 1;
    }

    fn undo(&mut self) {
        let now = self.now();
        if let Some(s) = self.strokes.iter_mut().rev().find(|s| s.until_ms.is_none()) {
            s.until_ms = Some(now);
            self.version += 1;
        }
    }

    /// The shape a drag from its first to its last point makes.
    fn shape(&self, pts: &[Pt]) -> Option<Shape> {
        let (a, b) = (*pts.first()?, *pts.last()?);
        let shape = match self.tool {
            Tool::Pen => Shape::Freehand(pts.to_vec()),
            Tool::Arrow => Shape::Arrow(a, b),
            Tool::Rect => Shape::Rect(Bounds::new(a, b)),
            Tool::Ellipse => Shape::Ellipse(Bounds::new(a, b)),
        };
        Some(shape)
    }
}

pub struct LiveDraw {
    state: Arc<Mutex<State>>,
    on: bool,
}

impl Default for LiveDraw {
    fn default() -> Self {
        let state = State {
            clock: None,
            tool: Tool::Pen,
            color: Rgba::RED,
            strokes: Vec::new(),
            drag: None,
            version: 0,
            texture: None,
            hidden_from_capture: false,
            done: false,
        };
        Self { state: Arc::new(Mutex::new(state)), on: false }
    }
}

impl LiveDraw {
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Turn draw mode on (with the recording clock) or off. Off clears the screen.
    pub fn set_on(&mut self, on: bool, clock: Option<ClockHandle>) {
        if on == self.on {
            return;
        }
        let Ok(mut s) = self.state.lock() else { return };
        if on {
            s.clock = clock;
            s.done = false;
            s.hidden_from_capture = false;
        } else {
            s.drag = None;
            s.clear();
        }
        self.on = on;
    }

    /// Esc or Done was pressed in the draw window since the last call.
    pub fn take_done(&self) -> bool {
        self.state.lock().map(|mut s| std::mem::take(&mut s.done)).unwrap_or(false)
    }

    /// Forget every stroke (the take was restarted).
    pub fn reset(&mut self) {
        self.set_on(false, None);
        if let Ok(mut s) = self.state.lock() {
            s.strokes.clear();
        }
    }

    /// The strokes as annotations on `rec`'s frames. Strokes still on screen
    /// last to the end.
    pub fn into_items(self, rec: &Recording) -> Vec<Item> {
        let Ok(s) = self.state.lock() else { return Vec::new() };
        let times: Vec<u64> = rec.frames.iter().map(|f| f.t_ms).collect();
        s.strokes
            .iter()
            .filter_map(|st| {
                let frames = frame_range(&times, st.from_ms, st.until_ms.unwrap_or(u64::MAX))?;
                Some(Item { id: 0, shape: st.shape.clone(), style: st.style, frames: Some(frames), pin: None })
            })
            .collect()
    }

    /// The draw window over `rect` (desktop pixels), while draw mode is on.
    pub fn show(&mut self, ctx: &egui::Context, rect: mythicshot_capture::PxRect) {
        if !self.on {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let builder = egui::ViewportBuilder::default()
            .with_title(DRAW_TITLE)
            .with_position(egui::pos2(rect.x as f32 / ppp, rect.y as f32 / ppp))
            .with_inner_size([rect.w as f32 / ppp, rect.h as f32 / ppp])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(true);
        let state = self.state.clone();
        let size = (rect.w, rect.h);
        ctx.show_viewport_deferred(egui::ViewportId::from_hash_of("live-draw"), builder, move |ui, _| {
            let Ok(mut s) = state.lock() else { return };
            draw_window(ui, &mut s, size);
        });
        let hidden = self.state.lock().map(|s| s.hidden_from_capture).unwrap_or(true);
        if !hidden {
            if let Some(h) = platform::find_window(DRAW_TITLE) {
                if platform::exclude_from_capture(h) {
                    if let Ok(mut s) = self.state.lock() {
                        s.hidden_from_capture = true;
                    }
                    platform::set_foreground(h);
                }
            }
        }
    }
}

/// Frames `i` with `from <= t[i] < until`, as an inclusive range.
fn frame_range(times: &[u64], from: u64, until: u64) -> Option<(u32, u32)> {
    let a = times.iter().position(|t| *t >= from)?;
    let b = times.iter().rposition(|t| *t < until)?;
    (a <= b).then_some((a as u32, b as u32))
}

fn color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0[0], c.0[1], c.0[2], c.0[3])
}

fn draw_window(ui: &mut egui::Ui, s: &mut State, size: (u32, u32)) {
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let full = ctx.content_rect();
    let painter = ui.painter().clone();
    if !s.hidden_from_capture {
        // Not out of the recording yet: show nothing.
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
        return;
    }
    painter.rect_filled(full, 0.0, theme::SCRIM_FAINT);

    // Finished strokes, drawn by the export renderer so they look the same as in the GIF.
    if s.texture.as_ref().is_none_or(|(v, _)| *v != s.version) {
        let items: Vec<Item> = s.on_screen().map(|st| Item { id: 0, shape: st.shape.clone(), style: st.style, frames: None, pin: None }).collect();
        if let Some(mut pm) = Pixmap::new(size.0.max(1), size.1.max(1)) {
            render_items_onto(&mut pm, &items);
            let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
            s.texture = Some((s.version, ctx.load_texture("live-draw", img, egui::TextureOptions::LINEAR)));
        }
    }
    if let Some((_, tex)) = &s.texture {
        painter.image(tex.id(), full, egui::Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), theme::IMAGE_TINT);
    }

    let over_bar = toolbar(ui, s, full);

    // Drawing: points in frame pixels.
    let to_px = |p: Pos2| Pt { x: (p.x - full.min.x) * ppp, y: (p.y - full.min.y) * ppp };
    let (pressed, down, released, pos) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released(), i.pointer.interact_pos()));
    if pressed && !over_bar {
        if let Some(p) = pos {
            s.drag = Some(vec![to_px(p)]);
        }
    }
    if down {
        if let (Some(d), Some(p)) = (s.drag.as_mut(), pos) {
            let p = to_px(p);
            if d.last().is_none_or(|l| (l.x - p.x).abs() + (l.y - p.y).abs() >= 1.0) {
                d.push(p);
            }
        }
    }
    if released {
        if let Some(pts) = s.drag.take() {
            let first = pts[0];
            let last = *pts.last().unwrap_or(&first);
            let moved = pts.iter().any(|p| (p.x - first.x).abs() + (p.y - first.y).abs() > 3.0);
            if moved || (s.tool != Tool::Pen && (last.x - first.x).abs() + (last.y - first.y).abs() > 3.0) {
                if let Some(shape) = s.shape(&pts) {
                    let (style, now) = (s.style(), s.now());
                    s.strokes.push(LiveStroke { shape, style, from_ms: now, until_ms: None });
                    s.version += 1;
                }
            }
        }
    }
    // The stroke being drawn, approximated with egui until it is finished.
    if let Some(pts) = &s.drag {
        let to_pt = |p: &Pt| egui::pos2(full.min.x + p.x / ppp, full.min.y + p.y / ppp);
        let stroke = Stroke::new(WIDTH / ppp, color32(s.color));
        let (a, b) = (to_pt(&pts[0]), to_pt(pts.last().unwrap_or(&pts[0])));
        match s.tool {
            Tool::Pen => {
                painter.add(egui::Shape::line(pts.iter().map(to_pt).collect(), stroke));
            }
            Tool::Arrow => painter.arrow(a, b - a, stroke),
            Tool::Rect => {
                painter.rect_stroke(egui::Rect::from_two_pos(a, b), 0.0, stroke, StrokeKind::Middle);
            }
            Tool::Ellipse => {
                let r = egui::Rect::from_two_pos(a, b);
                painter.add(egui::Shape::ellipse_stroke(r.center(), r.size() / 2.0, stroke));
            }
        };
    }

    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            s.done = true;
        }
        if i.modifiers.command && i.key_pressed(egui::Key::Z) {
            s.undo();
        } else if !i.modifiers.any() {
            for (key, tool) in [(egui::Key::P, Tool::Pen), (egui::Key::A, Tool::Arrow), (egui::Key::R, Tool::Rect), (egui::Key::E, Tool::Ellipse)] {
                if i.key_pressed(key) {
                    s.tool = tool;
                }
            }
            if i.key_pressed(egui::Key::C) {
                s.clear();
            }
            for (n, key) in [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5].into_iter().enumerate() {
                if i.key_pressed(key) {
                    s.color = COLORS[n];
                }
            }
        }
    });
    if s.drag.is_some() {
        ctx.request_repaint();
    }
}

/// The toolbar at the top of the draw window. Returns whether the pointer is on it.
fn toolbar(ui: &mut egui::Ui, s: &mut State, full: egui::Rect) -> bool {
    // Wraps onto more rows when the recorded area is narrow.
    let width = 420.0f32.min(full.width() - 8.0).max(60.0);
    let max = egui::Rect::from_min_size(egui::pos2(full.center().x - width / 2.0, full.min.y + 6.0), vec2(width, full.height() - 12.0));
    let shown = ui.scope_builder(egui::UiBuilder::new().max_rect(max), |ui| {
        egui::Frame::new().fill(theme::RAISED).stroke(Stroke::new(1.0, theme::BORDER_STRONG)).corner_radius(CornerRadius::same(theme::RADIUS_MD)).inner_margin(egui::Margin::symmetric(7, 4)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for (tool, icon, tip) in [(Tool::Pen, Icon::Freehand, "Pen (P)"), (Tool::Arrow, Icon::Arrow, "Arrow (A)"), (Tool::Rect, Icon::Rect, "Rectangle (R)"), (Tool::Ellipse, Icon::Ellipse, "Ellipse (E)")] {
                    if icons::button(ui, icon, 28.0, s.tool == tool, tip).clicked() {
                        s.tool = tool;
                    }
                }
                ui.separator();
                for (n, c) in COLORS.into_iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::click());
                    ui.painter().circle_filled(r.center(), 8.0, color32(c));
                    if s.color == c {
                        ui.painter().circle_stroke(r.center(), 10.0, Stroke::new(1.5, theme::TEXT));
                    }
                    if resp.on_hover_text(format!("Colour ({})", n + 1)).clicked() {
                        s.color = c;
                    }
                }
                ui.separator();
                if icons::button(ui, Icon::Undo, 28.0, false, "Undo (Ctrl+Z)").clicked() {
                    s.undo();
                }
                if icons::button(ui, Icon::Delete, 28.0, false, "Clear (C)").clicked() {
                    s.clear();
                }
                if ui.button("Done").on_hover_text("Stop drawing (Esc). The drawing is cleared.").clicked() {
                    s.done = true;
                }
            });
        });
    });
    ui.ctx().pointer_hover_pos().is_some_and(|p| shown.response.rect.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_cover_frames_between_finish_and_removal() {
        let t = [0, 100, 200, 300, 400];
        assert_eq!(frame_range(&t, 150, 350), Some((2, 3)));
        assert_eq!(frame_range(&t, 100, u64::MAX), Some((1, 4)));
        // Drawn and undone between two frames: never on a frame.
        assert_eq!(frame_range(&t, 110, 190), None);
        // Finished after the last frame.
        assert_eq!(frame_range(&t, 450, u64::MAX), None);
    }
}
