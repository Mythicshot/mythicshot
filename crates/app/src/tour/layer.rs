//! The tour layer: pinned text, bubbles and spotlights, written to the
//! manifest as data instead of being drawn into the step images.
//!
//! Positions and sizes are percentages of the exported (cropped) image.

use mythicshot_editor::{Bounds, Item, MAX_DIM, Rgba, Shape};
use mythicshot_recording::Recording;

use super::{Point, pct};
use crate::video::{Edit, crop_px};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Note {
    Text { x: f32, y: f32, text: String, size: f32, color: String },
    Bubble { x: f32, y: f32, w: f32, h: f32, tail: Point, text: String, size: f32, color: String, fill: String },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StepLayer {
    pub spotlight: Option<Area>,
    pub notes: Vec<Note>,
}

/// True for a pinned text, bubble or spotlight: written as data, never burned in.
pub fn is_layer(it: &Item) -> bool {
    it.pin.is_some() && matches!(it.shape, Shape::Text { .. } | Shape::Bubble { .. } | Shape::Spotlight { .. })
}

fn hex(c: Rgba) -> String {
    let [r, g, b, _] = c.0;
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The crop rectangle in frame pixels, and turning frame pixels into percentages of it.
struct Frame {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Frame {
    fn w(&self) -> f32 {
        self.x1 - self.x0
    }

    fn h(&self) -> f32 {
        self.y1 - self.y0
    }

    fn inside(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    fn px(&self, x: f32) -> f32 {
        pct((x - self.x0) / self.w() * 100.0)
    }

    fn py(&self, y: f32) -> f32 {
        pct((y - self.y0) / self.h() * 100.0)
    }

    fn pw(&self, w: f32) -> f32 {
        pct(w / self.w() * 100.0)
    }

    fn ph(&self, h: f32) -> f32 {
        pct(h / self.h() * 100.0)
    }
}

/// The layer of the step whose source frame is `src`, mapped through the crop.
pub fn step_layer(rec: &Recording, edit: &Edit, items: &[Item], src: usize) -> StepLayer {
    let (cx, cy, cw, ch) = crop_px(rec, edit);
    let f = Frame { x0: cx as f32, y0: cy as f32, x1: (cx + cw) as f32, y1: (cy + ch) as f32 };
    let mut layer = StepLayer::default();
    for it in items.iter().filter(|it| is_layer(it) && it.pin.is_some_and(|p| p as usize == src)) {
        let size = pct(it.style.font_size / f.h() * 100.0);
        let color = hex(it.style.stroke);
        match &it.shape {
            Shape::Spotlight { rect, .. } => {
                // The last pinned spotlight in the list wins; one that leaves the crop is dropped.
                let b = rect.norm();
                let c = Bounds { x0: b.x0.max(f.x0), y0: b.y0.max(f.y0), x1: b.x1.min(f.x1), y1: b.y1.min(f.y1) };
                layer.spotlight = if c.x1 - c.x0 >= 1.0 && c.y1 - c.y0 >= 1.0 { Some(Area { x: f.px(c.x0), y: f.py(c.y0), w: f.pw(c.x1 - c.x0), h: f.ph(c.y1 - c.y0) }) } else { None };
            }
            Shape::Text { at, text } => {
                if f.inside(at.x, at.y) {
                    layer.notes.push(Note::Text { x: f.px(at.x), y: f.py(at.y), text: text.clone(), size, color });
                }
            }
            Shape::Bubble { rect, tail, text } => {
                let b = rect.norm();
                if f.inside(b.x0, b.y0) {
                    let (w, h) = (b.x1.min(f.x1) - b.x0, b.y1.min(f.y1) - b.y0);
                    let tail = Point { x: f.px(tail.x.clamp(f.x0, f.x1)), y: f.py(tail.y.clamp(f.y0, f.y1)) };
                    let fill = hex(it.style.fill.unwrap_or(Rgba::WHITE));
                    layer.notes.push(Note::Bubble { x: f.px(b.x0), y: f.py(b.y0), w: f.pw(w), h: f.ph(h), tail, text: text.clone(), size, color, fill });
                }
            }
            _ => {}
        }
    }
    layer
}

/// The tour's darkening strength: the largest `dim` among pinned spotlights on `srcs`, or None.
pub fn tour_dim(items: &[Item], srcs: &[usize]) -> Option<u8> {
    items
        .iter()
        .filter(|it| it.pin.is_some_and(|p| srcs.contains(&(p as usize))))
        .filter_map(|it| match it.shape {
            Shape::Spotlight { dim, .. } => Some(dim.min(MAX_DIM)),
            _ => None,
        })
        .max()
}
