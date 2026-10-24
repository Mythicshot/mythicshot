//! Draws a document with tiny-skia. The editor shows this exact output and
//! export saves it, so screen and file always match.

use image::RgbaImage;
use tiny_skia::{BlendMode, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, PremultipliedColorU8, Rect, Stroke, Transform};

use crate::text::{draw_lines, line_height_px, wrap_text};
use crate::{Bounds, Item, ObfuscateMode, Pt, Rgba, Shape, Style, cone_points, measure_text, slice_points, step_radius};

const SHADOW_OFFSET: f32 = 3.0;
const SHADOW_BLUR: usize = 3;
const SHADOW_ALPHA: f32 = 0.45;

pub fn to_pixmap(img: &RgbaImage) -> Pixmap {
    let mut pm = Pixmap::new(img.width().max(1), img.height().max(1)).expect("pixmap size");
    for (d, s) in pm.pixels_mut().iter_mut().zip(img.pixels()) {
        let [r, g, b, a] = s.0;
        let m = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
        *d = PremultipliedColorU8::from_rgba(m(r), m(g), m(b), a).expect("premultiplied");
    }
    pm
}

pub fn to_rgba_image(pm: &Pixmap) -> RgbaImage {
    let mut img = RgbaImage::new(pm.width(), pm.height());
    for (d, s) in img.pixels_mut().zip(pm.pixels()) {
        let c = s.demultiply();
        d.0 = [c.red(), c.green(), c.blue(), c.alpha()];
    }
    img
}

/// Base image with every item drawn on top, as a straight-alpha image.
pub fn render(base: &RgbaImage, items: &[Item]) -> RgbaImage {
    let mut pm = to_pixmap(base);
    render_items_onto(&mut pm, items);
    to_rgba_image(&pm)
}

pub fn render_items_onto(pm: &mut Pixmap, items: &[Item]) {
    for it in items {
        draw_item(pm, it);
    }
}

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.0[0], c.0[1], c.0[2], c.0[3]);
    p.anti_alias = true;
    p
}

fn stroke(width: f32) -> Stroke {
    Stroke { width: width.max(0.5), line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

fn rect_of(b: &Bounds) -> Option<Rect> {
    let n = b.norm();
    Rect::from_ltrb(n.x0, n.y0, n.x1.max(n.x0 + 0.5), n.y1.max(n.y0 + 0.5))
}

fn draw_item(pm: &mut Pixmap, it: &Item) {
    match &it.shape {
        Shape::Highlight(b) => return highlight(pm, b, it.style.fill.unwrap_or(Rgba::YELLOW)),
        Shape::Obfuscate { rect, mode } => return obfuscate(pm, rect, *mode),
        _ => {}
    }
    if it.style.shadow {
        shadow(pm, it);
    }
    draw_shape(pm, &it.shape, &it.style, None, Transform::identity());
}

/// Draw a shape. With `mono`, every colour is replaced by it (for shadows).
fn draw_shape(pm: &mut Pixmap, shape: &Shape, st: &Style, mono: Option<Rgba>, t: Transform) {
    let col = |c: Rgba| mono.unwrap_or(c);
    let line = paint(col(st.stroke));
    let s = stroke(st.width);
    match shape {
        Shape::Rect(b) => {
            if let Some(r) = rect_of(b) {
                let path = PathBuilder::from_rect(r);
                if let Some(f) = st.fill {
                    pm.fill_path(&path, &paint(col(f)), FillRule::Winding, t, None);
                }
                pm.stroke_path(&path, &line, &s, t, None);
            }
        }
        Shape::Ellipse(b) => {
            if let Some(path) = rect_of(b).and_then(PathBuilder::from_oval) {
                if let Some(f) = st.fill {
                    pm.fill_path(&path, &paint(col(f)), FillRule::Winding, t, None);
                }
                pm.stroke_path(&path, &line, &s, t, None);
            }
        }
        Shape::Line(a, b) => {
            if let Some(path) = polyline(&[*a, *b]) {
                pm.stroke_path(&path, &line, &s, t, None);
            }
        }
        Shape::Arrow(a, b) => arrow(pm, *a, *b, st.width, &line, t),
        Shape::Freehand(ps) => {
            if let Some(path) = polyline(ps) {
                pm.stroke_path(&path, &line, &s, t, None);
            }
        }
        Shape::Text { at, text } => {
            let (w, h) = measure_text(text, st.font_size);
            if let (Some(f), Some(r)) = (st.fill, Rect::from_xywh(at.x - 4.0, at.y - 2.0, w + 8.0, h + 4.0)) {
                pm.fill_path(&PathBuilder::from_rect(r), &paint(col(f)), FillRule::Winding, t, None);
            }
            let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
            let (dx, dy) = (t.tx, t.ty);
            draw_lines(pm, &lines, at.x + dx, at.y + dy, st.font_size, col(st.stroke));
        }
        Shape::Bubble { rect, tail, text } => bubble(pm, rect, *tail, text, st, mono, t),
        Shape::Step { center, number } => {
            let r = step_radius(st.font_size);
            if let Some(path) = PathBuilder::from_circle(center.x, center.y, r) {
                pm.fill_path(&path, &line, FillRule::Winding, t, None);
            }
            let label = number.to_string();
            let size = st.font_size;
            let (w, _) = measure_text(&label, size);
            let lh = line_height_px(size);
            draw_lines(pm, &[label], center.x - w / 2.0 + t.tx, center.y - lh / 2.0 + t.ty, size, col(Rgba::WHITE));
        }
        Shape::Image { rect, image } => {
            let Some(r) = rect_of(rect) else { return };
            match mono {
                // The shadow is the picture's outline.
                Some(c) => pm.fill_rect(r, &paint(c), t, None),
                None => {
                    let (sx, sy) = (r.width() / image.width() as f32, r.height() / image.height() as f32);
                    let place = t.pre_translate(r.left(), r.top()).pre_scale(sx, sy);
                    let p = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
                    pm.draw_pixmap(0, 0, image.0.as_ref().as_ref(), &p, place, None);
                }
            }
        }
        Shape::Circle(b) => {
            let (c, r) = circle_in(b);
            if let Some(path) = PathBuilder::from_circle(c.x, c.y, r) {
                fill_and_stroke(pm, &path, st, mono, &s, t);
            }
        }
        Shape::Donut(b) => {
            let (c, r) = circle_in(b);
            let mut pb = PathBuilder::new();
            pb.push_circle(c.x, c.y, r);
            pb.push_circle(c.x, c.y, r * DONUT_HOLE);
            if let Some(path) = pb.finish() {
                if let Some(f) = st.fill {
                    pm.fill_path(&path, &paint(col(f)), FillRule::EvenOdd, t, None);
                }
                pm.stroke_path(&path, &line, &s, t, None);
            }
        }
        Shape::Group(b) => {
            if let Some(r) = rect_of(b) {
                let path = PathBuilder::from_rect(r);
                if let Some(f) = st.fill {
                    pm.fill_path(&path, &paint(col(f)), FillRule::Winding, t, None);
                }
                let dash = st.width.max(1.0) * 3.0;
                let dashed = Stroke { dash: tiny_skia::StrokeDash::new(vec![dash, dash * 0.7], 0.0), line_cap: LineCap::Butt, ..s.clone() };
                pm.stroke_path(&path, &line, &dashed, t, None);
            }
        }
        Shape::Cross(b) => {
            let n = b.norm();
            let (w, h) = (n.width(), n.height());
            // An X with arms of even width, as one outline.
            const D: f32 = 0.1;
            let unit = [(0.0, D), (D, 0.0), (0.5, 0.5 - D), (1.0 - D, 0.0), (1.0, D), (0.5 + D, 0.5), (1.0, 1.0 - D), (1.0 - D, 1.0), (0.5, 0.5 + D), (D, 1.0), (0.0, 1.0 - D), (0.5 - D, 0.5)];
            let ps: Vec<Pt> = unit.iter().map(|(x, y)| Pt { x: n.x0 + x * w, y: n.y0 + y * h }).collect();
            if let Some(path) = polygon(&ps) {
                fill_and_stroke(pm, &path, st, mono, &s, t);
            }
        }
        Shape::Asterisk(b) => {
            let n = b.norm();
            let (cx, cy) = ((n.x0 + n.x1) / 2.0, (n.y0 + n.y1) / 2.0);
            let (hw, hh) = (n.width() / 2.0, n.height() / 2.0);
            let bar = Stroke { width: (n.width().min(n.height()) * 0.11).max(st.width), line_cap: LineCap::Butt, ..Default::default() };
            let mut pb = PathBuilder::new();
            for (dx, dy) in [(hw, 0.0), (0.0, hh), (hw * 0.78, hh * 0.78), (hw * 0.78, -hh * 0.78)] {
                pb.move_to(cx - dx, cy - dy);
                pb.line_to(cx + dx, cy + dy);
            }
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &line, &bar, t, None);
            }
        }
        Shape::Cone(a, b) => {
            if let Some(path) = polygon(&cone_points(*a, *b)) {
                fill_and_stroke(pm, &path, st, mono, &s, t);
            }
        }
        Shape::Slice(a, b) => {
            if let Some(path) = polygon(&slice_points(*a, *b)) {
                fill_and_stroke(pm, &path, st, mono, &s, t);
            }
        }
        Shape::Highlight(_) | Shape::Obfuscate { .. } => {}
    }
}

/// How big a donut's hole is, as part of its radius.
const DONUT_HOLE: f32 = 0.5;

/// The biggest circle centred in `b`.
fn circle_in(b: &Bounds) -> (Pt, f32) {
    let n = b.norm();
    (Pt { x: (n.x0 + n.x1) / 2.0, y: (n.y0 + n.y1) / 2.0 }, (n.width().min(n.height()) / 2.0).max(0.5))
}

fn polygon(ps: &[Pt]) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let first = ps.first()?;
    pb.move_to(first.x, first.y);
    for p in &ps[1..] {
        pb.line_to(p.x, p.y);
    }
    pb.close();
    pb.finish()
}

/// The fill colour if the style has one, then the outline.
fn fill_and_stroke(pm: &mut Pixmap, path: &Path, st: &Style, mono: Option<Rgba>, s: &Stroke, t: Transform) {
    let col = |c: Rgba| mono.unwrap_or(c);
    if let Some(f) = st.fill {
        pm.fill_path(path, &paint(col(f)), FillRule::Winding, t, None);
    }
    pm.stroke_path(path, &paint(col(st.stroke)), s, t, None);
}

fn polyline(ps: &[Pt]) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let first = ps.first()?;
    pb.move_to(first.x, first.y);
    if ps.len() == 1 {
        pb.line_to(first.x + 0.01, first.y);
    }
    for p in &ps[1..] {
        pb.line_to(p.x, p.y);
    }
    pb.finish()
}

fn arrow(pm: &mut Pixmap, a: Pt, b: Pt, width: f32, line: &Paint, t: Transform) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let head = (width * 3.5 + 8.0).min(len * 0.6);
    let half = head * 0.55;
    let base = Pt { x: b.x - ux * head, y: b.y - uy * head };
    if let Some(path) = polyline(&[a, Pt { x: base.x + ux * 1.0, y: base.y + uy * 1.0 }]) {
        pm.stroke_path(&path, line, &stroke(width), t, None);
    }
    let mut pb = PathBuilder::new();
    pb.move_to(b.x, b.y);
    pb.line_to(base.x - uy * half, base.y + ux * half);
    pb.line_to(base.x + uy * half, base.y - ux * half);
    pb.close();
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, line, FillRule::Winding, t, None);
    }
}

pub(crate) fn rounded_rect(r: Rect, radius: f32) -> Option<Path> {
    let rad = radius.min(r.width() / 2.0).min(r.height() / 2.0);
    let (l, tp, rt, bt) = (r.left(), r.top(), r.right(), r.bottom());
    let k = 0.4477; // control-point inset for a quarter circle
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, tp);
    pb.line_to(rt - rad, tp);
    pb.cubic_to(rt - rad * k, tp, rt, tp + rad * k, rt, tp + rad);
    pb.line_to(rt, bt - rad);
    pb.cubic_to(rt, bt - rad * k, rt - rad * k, bt, rt - rad, bt);
    pb.line_to(l + rad, bt);
    pb.cubic_to(l + rad * k, bt, l, bt - rad * k, l, bt - rad);
    pb.line_to(l, tp + rad);
    pb.cubic_to(l, tp + rad * k, l + rad * k, tp, l + rad, tp);
    pb.close();
    pb.finish()
}

fn bubble(pm: &mut Pixmap, rect: &Bounds, tail: Pt, text: &str, st: &Style, mono: Option<Rgba>, t: Transform) {
    let col = |c: Rgba| mono.unwrap_or(c);
    let Some(r) = rect_of(rect) else { return };
    let fill = paint(col(st.fill.unwrap_or(Rgba::WHITE)));
    let line = paint(col(st.stroke));
    let s = stroke(st.width);

    // Tail base on the side of the box facing the tail point.
    let (cx, cy) = (r.left() + r.width() / 2.0, r.top() + r.height() / 2.0);
    let (tx, ty) = (tail.x - cx, tail.y - cy);
    let spread = (r.width().min(r.height()) * 0.18).max(6.0);
    let inset = st.width + 1.0;
    let (b1, b2) = if ty.abs() * r.width() >= tx.abs() * r.height() {
        let y = if ty > 0.0 { r.bottom() - inset } else { r.top() + inset };
        (Pt { x: cx - spread, y }, Pt { x: cx + spread, y })
    } else {
        let x = if tx > 0.0 { r.right() - inset } else { r.left() + inset };
        (Pt { x, y: cy - spread }, Pt { x, y: cy + spread })
    };
    let tri = |a: Pt, b: Pt| {
        let mut pb = PathBuilder::new();
        pb.move_to(a.x, a.y);
        pb.line_to(tail.x, tail.y);
        pb.line_to(b.x, b.y);
        pb.close();
        pb.finish()
    };
    let body = rounded_rect(r, 12.0);
    if let Some(body) = &body {
        pm.fill_path(body, &fill, FillRule::Winding, t, None);
        pm.stroke_path(body, &line, &s, t, None);
    }
    if let Some(tri) = tri(b1, b2) {
        pm.fill_path(&tri, &fill, FillRule::Winding, t, None);
        // Only the two outer edges of the tail get an outline, so it joins the box cleanly.
        for side in [[b1, tail], [tail, b2]] {
            if let Some(p) = polyline(&side) {
                pm.stroke_path(&p, &line, &s, t, None);
            }
        }
    }

    if mono.is_none() {
        let pad = 10.0 + st.width;
        let lines = wrap_text(text, st.font_size, (r.width() - pad * 2.0).max(10.0));
        let lh = line_height_px(st.font_size);
        let total = lines.len() as f32 * lh;
        let y = r.top() + ((r.height() - total) / 2.0).max(pad / 2.0);
        draw_lines(pm, &lines, r.left() + pad + t.tx, y + t.ty, st.font_size, st.stroke);
    }
}

/// A soft shadow: the shape in black on a scratch layer, blurred, then
/// composited down and to the right.
fn shadow(pm: &mut Pixmap, it: &Item) {
    let b = it.bounds();
    let pad = it.style.width + SHADOW_BLUR as f32 * 3.0 + SHADOW_OFFSET + 4.0;
    let (x0, y0) = ((b.x0 - pad).floor(), (b.y0 - pad).floor());
    let (w, h) = ((b.width() + pad * 2.0).ceil() as u32, (b.height() + pad * 2.0).ceil() as u32);
    let Some(mut layer) = Pixmap::new(w.max(1), h.max(1)) else { return };
    let t = Transform::from_translate(-x0, -y0);
    draw_shape(&mut layer, &it.shape, &it.style, Some(Rgba::BLACK), t);
    box_blur(layer.data_mut(), w as usize, h as usize, SHADOW_BLUR, 3);
    let mut p = tiny_skia::PixmapPaint::default();
    p.opacity = SHADOW_ALPHA;
    pm.draw_pixmap((x0 + SHADOW_OFFSET) as i32, (y0 + SHADOW_OFFSET) as i32, layer.as_ref(), &p, Transform::identity(), None);
}

fn highlight(pm: &mut Pixmap, b: &Bounds, c: Rgba) {
    if let Some(r) = rect_of(b) {
        let mut p = paint(c);
        p.blend_mode = BlendMode::Multiply;
        pm.fill_rect(r, &p, Transform::identity(), None);
    }
}

fn obfuscate(pm: &mut Pixmap, b: &Bounds, mode: ObfuscateMode) {
    let n = b.norm();
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let x0 = (n.x0.floor() as i32).clamp(0, pw);
    let y0 = (n.y0.floor() as i32).clamp(0, ph);
    let x1 = (n.x1.ceil() as i32).clamp(0, pw);
    let y1 = (n.y1.ceil() as i32).clamp(0, ph);
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    if w == 0 || h == 0 {
        return;
    }
    let stride = pw as usize * 4;
    let mut region = vec![0u8; w * h * 4];
    {
        let data = pm.data();
        for row in 0..h {
            let src = (y0 as usize + row) * stride + x0 as usize * 4;
            region[row * w * 4..(row + 1) * w * 4].copy_from_slice(&data[src..src + w * 4]);
        }
    }
    match mode {
        ObfuscateMode::Pixelate => pixelate(&mut region, w, h, 12),
        ObfuscateMode::Blur => box_blur(&mut region, w, h, 8, 3),
    }
    let data = pm.data_mut();
    for row in 0..h {
        let dst = (y0 as usize + row) * stride + x0 as usize * 4;
        data[dst..dst + w * 4].copy_from_slice(&region[row * w * 4..(row + 1) * w * 4]);
    }
}

fn pixelate(buf: &mut [u8], w: usize, h: usize, block: usize) {
    for by in (0..h).step_by(block) {
        for bx in (0..w).step_by(block) {
            let (ex, ey) = ((bx + block).min(w), (by + block).min(h));
            let mut sum = [0u32; 4];
            let n = ((ex - bx) * (ey - by)) as u32;
            for y in by..ey {
                for x in bx..ex {
                    let i = (y * w + x) * 4;
                    for c in 0..4 {
                        sum[c] += buf[i + c] as u32;
                    }
                }
            }
            let avg = sum.map(|s| (s / n) as u8);
            for y in by..ey {
                for x in bx..ex {
                    let i = (y * w + x) * 4;
                    buf[i..i + 4].copy_from_slice(&avg);
                }
            }
        }
    }
}

/// Repeated box blur, an approximation of a Gaussian. Works on premultiplied
/// RGBA, so transparent areas blur correctly.
pub(crate) fn box_blur(buf: &mut [u8], w: usize, h: usize, radius: usize, passes: usize) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0u8; buf.len()];
    for _ in 0..passes {
        blur_1d(buf, &mut tmp, w, h, radius, true);
        blur_1d(&tmp, buf, w, h, radius, false);
    }
}

fn blur_1d(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, horizontal: bool) {
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horizontal { (o * w + i) * 4 } else { (i * w + o) * 4 };
    let win = (2 * r + 1) as u32;
    for o in 0..outer {
        let mut sum = [0u32; 4];
        for k in 0..=r {
            let i = idx(o, k.min(inner - 1));
            for c in 0..4 {
                sum[c] += src[i + c] as u32 * if k == 0 { r as u32 + 1 } else { 1 };
            }
        }
        for i in 0..inner {
            let d = idx(o, i);
            for c in 0..4 {
                dst[d + c] = (sum[c] / win) as u8;
            }
            let add = idx(o, (i + r + 1).min(inner - 1));
            let sub = idx(o, i.saturating_sub(r));
            for c in 0..4 {
                sum[c] = sum[c] + src[add + c] as u32 - src[sub + c] as u32;
            }
        }
    }
}
