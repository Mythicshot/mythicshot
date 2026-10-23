//! Text layout and drawing with the font egui ships (Ubuntu Light), so the
//! editor needs no font file of its own.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use tiny_skia::Pixmap;

use crate::Rgba;

fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).expect("bundled font"))
}

fn line_height(size: f32) -> f32 {
    let f = font().as_scaled(PxScale::from(size));
    f.ascent() - f.descent() + f.line_gap()
}

fn line_width(line: &str, size: f32) -> f32 {
    let f = font().as_scaled(PxScale::from(size));
    let mut w = 0.0;
    let mut prev = None;
    for c in line.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            w += f.kern(p, id);
        }
        w += f.h_advance(id);
        prev = Some(id);
    }
    w
}

/// Width and height of (possibly multi-line) text.
pub fn measure_text(text: &str, size: f32) -> (f32, f32) {
    let lines: Vec<&str> = if text.is_empty() { vec![""] } else { text.split('\n').collect() };
    let w = lines.iter().map(|l| line_width(l, size)).fold(0.0, f32::max);
    (w.max(size * 0.3), lines.len() as f32 * line_height(size))
}

/// Break text into lines no wider than `max_w`, keeping explicit newlines.
pub fn wrap_text(text: &str, size: f32, max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if !line.is_empty() && line_width(&candidate, size) > max_w {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        out.push(line);
    }
    out
}

/// Draw lines of text with their top-left at `(x, y)`.
pub(crate) fn draw_lines(pm: &mut Pixmap, lines: &[String], x: f32, y: f32, size: f32, color: Rgba) {
    let scale = PxScale::from(size);
    let f = font().as_scaled(scale);
    let lh = line_height(size);
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let px = pm.pixels_mut();
    for (i, line) in lines.iter().enumerate() {
        let baseline = y + f.ascent() + i as f32 * lh;
        let mut caret = x;
        let mut prev = None;
        for c in line.chars() {
            let id = f.glyph_id(c);
            if let Some(p) = prev {
                caret += f.kern(p, id);
            }
            let glyph = id.with_scale_and_position(scale, point(caret, baseline));
            caret += f.h_advance(id);
            prev = Some(id);
            let Some(outlined) = font().outline_glyph(glyph) else { continue };
            let b = outlined.px_bounds();
            outlined.draw(|gx, gy, cov| {
                let (tx, ty) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if tx < 0 || ty < 0 || tx >= w || ty >= h {
                    return;
                }
                let a = cov.clamp(0.0, 1.0) * color.0[3] as f32 / 255.0;
                let d = &mut px[(ty * w + tx) as usize];
                let inv = 1.0 - a;
                let alpha = (255.0 * a + d.alpha() as f32 * inv).round() as u8;
                // Premultiplied channels may never exceed alpha; rounding could push them over.
                let blend = |src: u8, dst: u8| ((src as f32 * a + dst as f32 * inv).round() as u8).min(alpha);
                *d = tiny_skia::PremultipliedColorU8::from_rgba(blend(color.0[0], d.red()), blend(color.0[1], d.green()), blend(color.0[2], d.blue()), alpha)
                    .unwrap_or(*d);
            });
        }
    }
}

pub(crate) fn line_height_px(size: f32) -> f32 {
    line_height(size)
}
