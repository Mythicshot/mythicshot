//! Edits to a recording and the one function that draws a finished frame.
//!
//! The GIF editor's preview and every export call [`decorate`], so what the
//! editor shows is what gets exported.

use std::collections::BTreeSet;

use image::RgbaImage;
use mythicshot_editor::overlays::{InputEvent, OverlayStyle, draw_overlays};
use mythicshot_editor::{Bounds, Item, render_items_onto, to_pixmap, to_rgba_image};
use mythicshot_recording::Recording;
use mythicshot_recording::export::OutFrame;

#[derive(Debug, Clone)]
pub struct Edit {
    /// First and last frame kept, inclusive.
    pub trim: (usize, usize),
    pub deleted: BTreeSet<usize>,
    /// Playback speed: 2.0 plays twice as fast.
    pub speed: f32,
    pub reverse: bool,
    /// Area kept on every frame, in frame pixels.
    pub crop: Option<Bounds>,
    /// Output size as a percentage of the (cropped) frame.
    pub scale: u32,
    pub overlays: OverlayStyle,
}

impl Edit {
    pub fn new(rec: &Recording, overlays: OverlayStyle) -> Self {
        Self { trim: (0, rec.frames.len().saturating_sub(1)), deleted: BTreeSet::new(), speed: 1.0, reverse: false, crop: None, scale: 100, overlays }
    }

    pub fn kept(&self, i: usize) -> bool {
        i >= self.trim.0 && i <= self.trim.1 && !self.deleted.contains(&i)
    }
}

/// How long frame `i` is shown in the recording.
pub fn frame_duration(rec: &Recording, i: usize) -> u64 {
    let next = rec.frames.get(i + 1).map(|f| f.t_ms).unwrap_or(rec.end_ms);
    next.saturating_sub(rec.frames[i].t_ms).max(1)
}

/// The output frames: trimmed, without deleted frames, reordered if reversed,
/// with times rebuilt from each kept frame's own duration and the speed.
/// Returns the frames and when the last one ends.
pub fn plan(rec: &Recording, edit: &Edit) -> (Vec<OutFrame>, u64) {
    let mut kept: Vec<usize> = (0..rec.frames.len()).filter(|i| edit.kept(*i)).collect();
    if edit.reverse {
        kept.reverse();
    }
    let speed = edit.speed.clamp(0.1, 10.0) as f64;
    let mut t = 0.0f64;
    let mut out = Vec::with_capacity(kept.len());
    for src in kept {
        out.push(OutFrame { src, t_ms: t.round() as u64 });
        t += frame_duration(rec, src) as f64 / speed;
    }
    (out, t.round() as u64)
}

pub(crate) fn crop_px(rec: &Recording, edit: &Edit) -> (u32, u32, u32, u32) {
    match edit.crop.map(|b| b.norm()) {
        Some(b) => {
            let x0 = b.x0.clamp(0.0, rec.width as f32) as u32;
            let y0 = b.y0.clamp(0.0, rec.height as f32) as u32;
            let x1 = (b.x1.clamp(0.0, rec.width as f32) as u32).max(x0 + 1);
            let y1 = (b.y1.clamp(0.0, rec.height as f32) as u32).max(y0 + 1);
            (x0, y0, x1 - x0, y1 - y0)
        }
        None => (0, 0, rec.width, rec.height),
    }
}

pub fn output_size(rec: &Recording, edit: &Edit) -> (u32, u32) {
    let (_, _, w, h) = crop_px(rec, edit);
    let s = edit.scale.clamp(10, 100);
    ((w * s / 100).max(1), (h * s / 100).max(1))
}

/// Annotations shown on frame `i`.
pub fn visible(items: &[Item], i: usize) -> Vec<Item> {
    items.iter().filter(|it| it.frames.is_none_or(|(a, b)| a as usize <= i && i <= b as usize)).cloned().collect()
}

/// The finished picture for source frame `src`: annotations, overlays, then
/// crop and scale. Pass `crop_and_scale: false` for the editor canvas, which
/// shows the whole frame with the crop drawn on top.
pub fn decorate(rec: &Recording, src: usize, img: RgbaImage, items: &[Item], events: &[InputEvent], edit: &Edit, crop_and_scale: bool) -> RgbaImage {
    let mut pm = to_pixmap(&img);
    render_items_onto(&mut pm, &visible(items, src));
    let meta = &rec.frames[src];
    draw_overlays(&mut pm, meta.t_ms, meta.cursor, events, &edit.overlays);
    let full = to_rgba_image(&pm);
    if !crop_and_scale {
        return full;
    }
    let (x, y, w, h) = crop_px(rec, edit);
    let cropped = if (x, y, w, h) == (0, 0, rec.width, rec.height) { full } else { image::imageops::crop_imm(&full, x, y, w, h).to_image() };
    let (ow, oh) = output_size(rec, edit);
    if (ow, oh) == (cropped.width(), cropped.height()) { cropped } else { image::imageops::resize(&cropped, ow, oh, image::imageops::FilterType::Triangle) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> Recording {
        // Five frames 100 ms apart, the last lasting 200 ms.
        Recording::timing_only(100, 80, &[0, 100, 200, 300, 400], 600)
    }

    #[test]
    fn plan_trims_deletes_and_retimes() {
        let r = rec();
        let mut e = Edit::new(&r, OverlayStyle::default());
        let (frames, end) = plan(&r, &e);
        assert_eq!(frames.iter().map(|f| (f.src, f.t_ms)).collect::<Vec<_>>(), vec![(0, 0), (1, 100), (2, 200), (3, 300), (4, 400)]);
        assert_eq!(end, 600);

        e.trim = (1, 4);
        e.deleted.insert(2);
        let (frames, end) = plan(&r, &e);
        assert_eq!(frames.iter().map(|f| (f.src, f.t_ms)).collect::<Vec<_>>(), vec![(1, 0), (3, 100), (4, 200)]);
        assert_eq!(end, 400);

        e.speed = 2.0;
        e.reverse = true;
        let (frames, end) = plan(&r, &e);
        assert_eq!(frames.iter().map(|f| (f.src, f.t_ms)).collect::<Vec<_>>(), vec![(4, 0), (3, 100), (1, 150)]);
        assert_eq!(end, 200);
    }

    #[test]
    fn crop_and_scale_set_the_output_size() {
        let r = rec();
        let mut e = Edit::new(&r, OverlayStyle::default());
        assert_eq!(output_size(&r, &e), (100, 80));
        e.crop = Some(Bounds { x0: 10.0, y0: 10.0, x1: 60.0, y1: 50.0 });
        e.scale = 50;
        assert_eq!(output_size(&r, &e), (25, 20));
    }
}
