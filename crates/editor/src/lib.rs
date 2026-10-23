//! The annotation document behind the editor.
//!
//! A [`Document`] is a base image plus a list of annotations ([`Item`]s) that
//! stay editable until export. Everything that reaches the screen or a file is
//! drawn by [`render`], so what you see is what you save. Crop keeps items
//! editable (they move with the crop); effects, resize and rotate flatten the
//! items into the base image first, like Greenshot.
//!
//! Undo is snapshot based: the base image is shared through an `Arc`, so a
//! snapshot costs only the item list unless the base itself changed.

pub mod effects;
pub mod overlays;
mod render;
mod text;

use std::sync::Arc;

pub use image::RgbaImage;
pub use tiny_skia::Pixmap;
pub use render::{render, render_items_onto, to_pixmap, to_rgba_image};
pub use text::{measure_text, wrap_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const RED: Rgba = Rgba([229, 72, 77, 255]);
    pub const WHITE: Rgba = Rgba([255, 255, 255, 255]);
    pub const BLACK: Rgba = Rgba([0, 0, 0, 255]);
    pub const YELLOW: Rgba = Rgba([255, 230, 0, 255]);
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

pub fn pt(x: f32, y: f32) -> Pt {
    Pt { x, y }
}

/// An axis-aligned rectangle; `x0,y0` may be greater than `x1,y1` while dragging.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Bounds {
    pub fn new(a: Pt, b: Pt) -> Self {
        Self { x0: a.x, y0: a.y, x1: b.x, y1: b.y }
    }

    pub fn norm(&self) -> Bounds {
        Bounds { x0: self.x0.min(self.x1), y0: self.y0.min(self.y1), x1: self.x0.max(self.x1), y1: self.y0.max(self.y1) }
    }

    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).abs()
    }

    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).abs()
    }

    pub fn contains(&self, p: Pt, pad: f32) -> bool {
        let n = self.norm();
        p.x >= n.x0 - pad && p.x <= n.x1 + pad && p.y >= n.y0 - pad && p.y <= n.y1 + pad
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Bounds {
        Bounds { x0: self.x0 + dx, y0: self.y0 + dy, x1: self.x1 + dx, y1: self.y1 + dy }
    }

    pub fn union(&self, o: &Bounds) -> Bounds {
        let (a, b) = (self.norm(), o.norm());
        Bounds { x0: a.x0.min(b.x0), y0: a.y0.min(b.y0), x1: a.x1.max(b.x1), y1: a.y1.max(b.y1) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObfuscateMode {
    Pixelate,
    Blur,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Rect(Bounds),
    Ellipse(Bounds),
    Line(Pt, Pt),
    Arrow(Pt, Pt),
    Freehand(Vec<Pt>),
    /// `at` is the top-left of the first line.
    Text { at: Pt, text: String },
    /// A box with text and a tail pointing at `tail`.
    Bubble { rect: Bounds, tail: Pt, text: String },
    Step { center: Pt, number: u32 },
    Highlight(Bounds),
    Obfuscate { rect: Bounds, mode: ObfuscateMode },
}

impl Shape {
    pub fn translate(&mut self, dx: f32, dy: f32) {
        let mv = |p: &mut Pt| {
            p.x += dx;
            p.y += dy;
        };
        match self {
            Shape::Rect(b) | Shape::Ellipse(b) | Shape::Highlight(b) | Shape::Obfuscate { rect: b, .. } => *b = b.translate(dx, dy),
            Shape::Line(a, b) | Shape::Arrow(a, b) => {
                mv(a);
                mv(b);
            }
            Shape::Freehand(ps) => ps.iter_mut().for_each(mv),
            Shape::Text { at, .. } => mv(at),
            Shape::Bubble { rect, tail, .. } => {
                *rect = rect.translate(dx, dy);
                mv(tail);
            }
            Shape::Step { center, .. } => mv(center),
        }
    }

    /// The rectangle a shape is resized through, if it has one.
    pub fn rect(&self) -> Option<Bounds> {
        match self {
            Shape::Rect(b) | Shape::Ellipse(b) | Shape::Highlight(b) | Shape::Obfuscate { rect: b, .. } | Shape::Bubble { rect: b, .. } => Some(*b),
            _ => None,
        }
    }

    pub fn set_rect(&mut self, nb: Bounds) {
        match self {
            Shape::Rect(b) | Shape::Ellipse(b) | Shape::Highlight(b) | Shape::Obfuscate { rect: b, .. } | Shape::Bubble { rect: b, .. } => *b = nb,
            _ => {}
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Shape::Text { text, .. } | Shape::Bubble { text, .. } => Some(text),
            _ => None,
        }
    }

    pub fn set_text(&mut self, new: String) {
        if let Shape::Text { text, .. } | Shape::Bubble { text, .. } = self {
            *text = new;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub stroke: Rgba,
    /// Fill for rectangles, ellipses and bubbles. Bubbles default to white.
    pub fill: Option<Rgba>,
    pub width: f32,
    pub font_size: f32,
    pub shadow: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self { stroke: Rgba::RED, fill: None, width: 3.0, font_size: 20.0, shadow: true }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: u64,
    pub shape: Shape,
    pub style: Style,
    /// For recordings (Phase 5): the frames this item shows on. `None` = all.
    pub frames: Option<(u32, u32)>,
}

impl Item {
    /// Area the item covers on the canvas, used for selection and hit testing.
    pub fn bounds(&self) -> Bounds {
        let pad = self.style.width / 2.0;
        match &self.shape {
            Shape::Rect(b) | Shape::Ellipse(b) | Shape::Highlight(b) | Shape::Obfuscate { rect: b, .. } => b.norm(),
            Shape::Line(a, b) | Shape::Arrow(a, b) => Bounds::new(*a, *b).norm(),
            Shape::Freehand(ps) => {
                let mut b = Bounds { x0: f32::MAX, y0: f32::MAX, x1: f32::MIN, y1: f32::MIN };
                for p in ps {
                    b.x0 = b.x0.min(p.x);
                    b.y0 = b.y0.min(p.y);
                    b.x1 = b.x1.max(p.x);
                    b.y1 = b.y1.max(p.y);
                }
                b
            }
            Shape::Text { at, text } => {
                let (w, h) = measure_text(text, self.style.font_size);
                Bounds { x0: at.x, y0: at.y, x1: at.x + w, y1: at.y + h }
            }
            Shape::Bubble { rect, tail, .. } => rect.norm().union(&Bounds::new(*tail, *tail)),
            Shape::Step { center, .. } => {
                let r = step_radius(self.style.font_size) + pad;
                Bounds { x0: center.x - r, y0: center.y - r, x1: center.x + r, y1: center.y + r }
            }
        }
    }

    /// Whether `p` (canvas pixels) touches the item, within `tol` pixels.
    pub fn hit(&self, p: Pt, tol: f32) -> bool {
        let tol = tol + self.style.width / 2.0;
        match &self.shape {
            Shape::Line(a, b) | Shape::Arrow(a, b) => seg_dist(p, *a, *b) <= tol,
            Shape::Freehand(ps) => ps.windows(2).any(|w| seg_dist(p, w[0], w[1]) <= tol),
            Shape::Rect(b) if self.style.fill.is_none() => {
                let n = b.norm();
                n.contains(p, tol) && !Bounds { x0: n.x0 + tol, y0: n.y0 + tol, x1: n.x1 - tol, y1: n.y1 - tol }.contains(p, 0.0)
            }
            _ => self.bounds().contains(p, tol),
        }
    }
}

pub fn step_radius(font_size: f32) -> f32 {
    font_size * 0.8
}

fn seg_dist(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (a.x + t * dx, a.y + t * dy);
    ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt()
}

#[derive(Clone)]
struct Snapshot {
    base: Arc<RgbaImage>,
    items: Vec<Item>,
    next_id: u64,
}

const UNDO_LIMIT: usize = 100;

pub struct Document {
    base: Arc<RgbaImage>,
    items: Vec<Item>,
    next_id: u64,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Bumped on every change, so views know when to redraw.
    pub version: u64,
    /// Changed since the last save.
    pub dirty: bool,
}

impl Document {
    pub fn new(base: RgbaImage) -> Self {
        Self { base: Arc::new(base), items: Vec::new(), next_id: 1, undo: Vec::new(), redo: Vec::new(), version: 0, dirty: false }
    }

    pub fn width(&self) -> u32 {
        self.base.width()
    }

    pub fn height(&self) -> u32 {
        self.base.height()
    }

    pub fn base(&self) -> &RgbaImage {
        &self.base
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, id: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { base: self.base.clone(), items: self.items.clone(), next_id: self.next_id }
    }

    fn restore(&mut self, s: Snapshot) {
        self.base = s.base;
        self.items = s.items;
        self.next_id = s.next_id;
        self.touch();
    }

    fn touch(&mut self) {
        self.version += 1;
        self.dirty = true;
    }

    /// Save the current state for undo. Call once before a change, or once at
    /// the start of a drag that changes an item many times.
    pub fn checkpoint(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        self.redo.push(self.snapshot());
        self.restore(s);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        self.undo.push(self.snapshot());
        self.restore(s);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn add(&mut self, shape: Shape, style: Style) -> u64 {
        self.checkpoint();
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Item { id, shape, style, frames: None });
        self.touch();
        id
    }

    /// Change an item without a checkpoint (the caller made one at drag start).
    pub fn update(&mut self, id: u64, f: impl FnOnce(&mut Item)) {
        if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
            f(it);
            self.touch();
        }
    }

    pub fn remove(&mut self, id: u64) {
        if self.items.iter().any(|i| i.id == id) {
            self.checkpoint();
            self.items.retain(|i| i.id != id);
            self.touch();
        }
    }

    pub fn bring_to_front(&mut self, id: u64) {
        if let Some(pos) = self.items.iter().position(|i| i.id == id) {
            self.checkpoint();
            let it = self.items.remove(pos);
            self.items.push(it);
            self.touch();
        }
    }

    /// Topmost item under `p`.
    pub fn hit(&self, p: Pt, tol: f32) -> Option<u64> {
        self.items.iter().rev().find(|i| i.hit(p, tol)).map(|i| i.id)
    }

    pub fn next_step_number(&self) -> u32 {
        self.items.iter().filter_map(|i| if let Shape::Step { number, .. } = i.shape { Some(number) } else { None }).max().unwrap_or(0) + 1
    }

    /// Crop the canvas; items move with it and stay editable.
    pub fn crop(&mut self, b: Bounds) {
        let n = b.norm();
        let x0 = n.x0.round().clamp(0.0, self.width() as f32) as u32;
        let y0 = n.y0.round().clamp(0.0, self.height() as f32) as u32;
        let x1 = n.x1.round().clamp(0.0, self.width() as f32) as u32;
        let y1 = n.y1.round().clamp(0.0, self.height() as f32) as u32;
        if x1 <= x0 + 1 || y1 <= y0 + 1 {
            return;
        }
        self.checkpoint();
        self.base = Arc::new(image::imageops::crop_imm(self.base.as_ref(), x0, y0, x1 - x0, y1 - y0).to_image());
        for it in &mut self.items {
            it.shape.translate(-(x0 as f32), -(y0 as f32));
        }
        self.touch();
    }

    /// The final image: base plus every item.
    pub fn flatten(&self) -> RgbaImage {
        render(&self.base, &self.items)
    }

    /// Flatten, then replace the base with `effect(flattened)`.
    pub fn apply(&mut self, effect: impl FnOnce(&RgbaImage) -> RgbaImage) {
        let flat = self.flatten();
        self.checkpoint();
        self.base = Arc::new(effect(&flat));
        self.items.clear();
        self.touch();
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Show a different picture under the items (the next frame of a
    /// recording). Not an edit: no undo step, not marked unsaved.
    pub fn replace_base(&mut self, base: RgbaImage) {
        self.base = Arc::new(base);
        self.version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::new(RgbaImage::from_pixel(100, 80, image::Rgba([255, 255, 255, 255])))
    }

    #[test]
    fn undo_redo_add_and_remove() {
        let mut d = doc();
        let id = d.add(Shape::Rect(Bounds::new(pt(10.0, 10.0), pt(40.0, 30.0))), Style::default());
        d.remove(id);
        assert!(d.items().is_empty());
        assert!(d.undo());
        assert_eq!(d.items().len(), 1);
        assert!(d.undo());
        assert!(d.items().is_empty());
        assert!(d.redo());
        assert!(d.redo());
        assert!(d.items().is_empty());
        assert!(!d.redo());
    }

    #[test]
    fn crop_moves_items_and_undoes() {
        let mut d = doc();
        d.add(Shape::Step { center: pt(50.0, 50.0), number: 1 }, Style::default());
        d.crop(Bounds::new(pt(20.0, 20.0), pt(80.0, 70.0)));
        assert_eq!((d.width(), d.height()), (60, 50));
        assert!(matches!(d.items()[0].shape, Shape::Step { center: Pt { x: 30.0, y: 30.0 }, .. }));
        d.undo();
        assert_eq!((d.width(), d.height()), (100, 80));
    }

    #[test]
    fn hit_testing() {
        let mut d = doc();
        let line = d.add(Shape::Line(pt(0.0, 0.0), pt(100.0, 0.0)), Style::default());
        let rect = d.add(Shape::Rect(Bounds::new(pt(20.0, 20.0), pt(60.0, 60.0))), Style::default());
        assert_eq!(d.hit(pt(50.0, 2.0), 2.0), Some(line));
        assert_eq!(d.hit(pt(20.0, 40.0), 2.0), Some(rect));
        // An unfilled rectangle is only hit on its outline.
        assert_eq!(d.hit(pt(40.0, 40.0), 2.0), None);
        assert_eq!(d.next_step_number(), 1);
    }

    #[test]
    fn effect_flattens_and_undoes() {
        let mut d = doc();
        d.add(Shape::Rect(Bounds::new(pt(10.0, 10.0), pt(40.0, 30.0))), Style::default());
        d.apply(effects::invert);
        assert!(d.items().is_empty());
        assert_eq!(d.base().get_pixel(0, 0).0, [0, 0, 0, 255]);
        d.undo();
        assert_eq!(d.items().len(), 1);
        assert_eq!(d.base().get_pixel(0, 0).0, [255, 255, 255, 255]);
    }
}
