//! Pictures of the app's windows without a window: frames are drawn into a test's own egui
//! context, tessellated by egui, and filled into an image here, triangle by triangle, the way the
//! OpenGL painter blends them (premultiplied colour, in gamma space). Nothing is shown on screen.
//!
//! The image starts magenta, so any part of a window that paints nothing, or paints see-through,
//! shows up at once. The tests that write pictures are ignored by default; run them with
//! `MYTHICSHOT_SNAPSHOTS=<folder> cargo test -p mythicshot snapshot -- --ignored`.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Color32, TextureId, epaint};
use image::RgbaImage;

/// What shows through a window that paints nothing.
pub const SEE_THROUGH: [u8; 4] = [255, 0, 255, 255];

/// The folder pictures go to, or `None` when no folder was asked for.
pub fn folder() -> Option<PathBuf> {
    std::env::var_os("MYTHICSHOT_SNAPSHOTS").map(PathBuf::from)
}

struct Texture {
    size: [usize; 2],
    pixels: Vec<Color32>,
}

/// The textures egui has handed out so far, kept up to date frame by frame.
#[derive(Default)]
pub struct Painter {
    textures: HashMap<TextureId, Texture>,
}

impl Painter {
    /// Take in a frame's texture changes: whole images, and patches of the font atlas.
    pub fn update(&mut self, delta: &mut egui::TexturesDelta) {
        for (id, deltas) in &delta.set {
            for d in deltas {
                let epaint::ImageData::Color(image) = &d.image;
                match d.pos {
                    None => {
                        self.textures.insert(
                            *id,
                            Texture {
                                size: image.size,
                                pixels: image.pixels.clone(),
                            },
                        );
                    }
                    Some([x0, y0]) => {
                        if let Some(t) = self.textures.get_mut(id) {
                            for y in 0..image.size[1] {
                                for x in 0..image.size[0] {
                                    t.pixels[(y0 + y) * t.size[0] + x0 + x] = image.pixels[y * image.size[0] + x];
                                }
                            }
                        }
                    }
                }
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
        delta.clear();
    }

    fn sample(&self, id: TextureId, uv: egui::Pos2) -> Color32 {
        let Some(t) = self.textures.get(&id) else { return Color32::WHITE };
        // Bilinear, as the GPU samples.
        let (w, h) = (t.size[0] as f32, t.size[1] as f32);
        let (fx, fy) = (uv.x * w - 0.5, uv.y * h - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let at = |x: f32, y: f32| {
            let x = (x as i64).clamp(0, t.size[0] as i64 - 1) as usize;
            let y = (y as i64).clamp(0, t.size[1] as i64 - 1) as usize;
            t.pixels[y * t.size[0] + x]
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        let mix = |i: usize| {
            let v = |p: Color32| f32::from(p.to_array()[i]);
            let top = v(a) * (1.0 - tx) + v(b) * tx;
            let bottom = v(c) * (1.0 - tx) + v(d) * tx;
            (top * (1.0 - ty) + bottom * ty).round() as u8
        };
        Color32::from_rgba_premultiplied(mix(0), mix(1), mix(2), mix(3))
    }

    /// Fill a frame's shapes into an image `size` points big at `ppp` pixels per point, over magenta.
    pub fn paint(&self, ctx: &egui::Context, shapes: Vec<epaint::ClippedShape>, size: egui::Vec2, ppp: f32) -> RgbaImage {
        self.paint_over(ctx, shapes, size, ppp, SEE_THROUGH)
    }

    /// [`Self::paint`] over `behind`, what the desktop would show where the window lets it through.
    pub fn paint_over(&self, ctx: &egui::Context, shapes: Vec<epaint::ClippedShape>, size: egui::Vec2, ppp: f32, behind: [u8; 4]) -> RgbaImage {
        let (w, h) = ((size.x * ppp).round() as u32, (size.y * ppp).round() as u32);
        let mut px: Vec<[f32; 4]> = vec![behind.map(f32::from); (w * h) as usize];
        for prim in ctx.tessellate(shapes, ppp) {
            let epaint::Primitive::Mesh(mesh) = prim.primitive else { continue };
            let clip = prim.clip_rect;
            let (cx0, cy0) = (((clip.min.x * ppp).round().max(0.0)) as i64, ((clip.min.y * ppp).round().max(0.0)) as i64);
            let (cx1, cy1) = (((clip.max.x * ppp).round().min(w as f32)) as i64, ((clip.max.y * ppp).round().min(h as f32)) as i64);
            for tri in mesh.indices.chunks_exact(3) {
                let v = [mesh.vertices[tri[0] as usize], mesh.vertices[tri[1] as usize], mesh.vertices[tri[2] as usize]];
                let p = v.map(|v| egui::pos2(v.pos.x * ppp, v.pos.y * ppp));
                let area = (p[1].x - p[0].x) * (p[2].y - p[0].y) - (p[1].y - p[0].y) * (p[2].x - p[0].x);
                if area.abs() < 1e-6 {
                    continue;
                }
                let minx = (p.iter().map(|q| q.x).fold(f32::INFINITY, f32::min).floor() as i64).max(cx0);
                let maxx = (p.iter().map(|q| q.x).fold(f32::NEG_INFINITY, f32::max).ceil() as i64).min(cx1);
                let miny = (p.iter().map(|q| q.y).fold(f32::INFINITY, f32::min).floor() as i64).max(cy0);
                let maxy = (p.iter().map(|q| q.y).fold(f32::NEG_INFINITY, f32::max).ceil() as i64).min(cy1);
                for y in miny..maxy {
                    for x in minx..maxx {
                        let c = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                        let edge = |a: egui::Pos2, b: egui::Pos2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
                        let (w0, w1, w2) = (edge(p[1], p[2]) / area, edge(p[2], p[0]) / area, edge(p[0], p[1]) / area);
                        if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                            continue;
                        }
                        let uv = egui::pos2(v[0].uv.x * w0 + v[1].uv.x * w1 + v[2].uv.x * w2, v[0].uv.y * w0 + v[1].uv.y * w1 + v[2].uv.y * w2);
                        let tex = self.sample(mesh.texture_id, uv).to_array();
                        let mut src = [0.0f32; 4];
                        for (i, s) in src.iter_mut().enumerate() {
                            let vc = f32::from(v[0].color.to_array()[i]) * w0 + f32::from(v[1].color.to_array()[i]) * w1 + f32::from(v[2].color.to_array()[i]) * w2;
                            *s = vc * f32::from(tex[i]) / 255.0;
                        }
                        let d = &mut px[(y as u32 * w + x as u32) as usize];
                        let keep = 1.0 - src[3] / 255.0;
                        for i in 0..4 {
                            d[i] = src[i] + d[i] * keep;
                        }
                    }
                }
            }
        }
        RgbaImage::from_fn(w, h, |x, y| {
            let p = px[(y * w + x) as usize];
            image::Rgba([p[0].round().clamp(0.0, 255.0) as u8, p[1].round().clamp(0.0, 255.0) as u8, p[2].round().clamp(0.0, 255.0) as u8, 255])
        })
    }
}

/// Draw `frames` frames of `ui` in a window `size` points big, a second apart (so anything that
/// fades in has), at `ppp` pixels per point, and return a picture of the last one.
pub fn picture(ctx: &egui::Context, size: egui::Vec2, ppp: f32, frames: usize, mut ui: impl FnMut(&mut egui::Ui)) -> RgbaImage {
    ctx.set_pixels_per_point(ppp);
    let mut painter = Painter::default();
    let mut last = None;
    for _ in 0..frames.max(1) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(ctx.input(|i| i.time) + 1.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, &mut ui);
        painter.update(&mut out.textures_delta);
        last = Some(out.shapes);
    }
    painter.paint(ctx, last.unwrap_or_default(), size, ppp)
}

/// How many pixels of the last of `frames` frames of `ui` let what is behind the window show,
/// even a little: the frame is filled in twice, over magenta and over green, and every pixel that
/// comes out different was not covered by something opaque.
pub fn uncovered(ctx: &egui::Context, size: egui::Vec2, ppp: f32, frames: usize, mut ui: impl FnMut(&mut egui::Ui)) -> usize {
    ctx.set_pixels_per_point(ppp);
    let mut painter = Painter::default();
    let mut last = None;
    for _ in 0..frames.max(1) {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), time: Some(ctx.input(|i| i.time) + 1.0), ..Default::default() };
        let mut out = ctx.run_ui(input, &mut ui);
        painter.update(&mut out.textures_delta);
        last = Some(out.shapes);
    }
    let shapes = last.unwrap_or_default();
    let a = painter.paint_over(ctx, shapes.clone(), size, ppp, SEE_THROUGH);
    let b = painter.paint_over(ctx, shapes, size, ppp, [0, 255, 0, 255]);
    a.pixels().zip(b.pixels()).filter(|(p, q)| p != q).count()
}

/// Save `image` as `<folder>/<name>.png` when a folder was asked for; how many of its pixels
/// show what is behind the window.
pub fn save(image: &RgbaImage, name: &str) -> usize {
    let see_through = image.pixels().filter(|p| p.0 == SEE_THROUGH).count();
    if let Some(dir) = folder() {
        std::fs::create_dir_all(&dir).unwrap();
        image.save(dir.join(format!("{name}.png"))).unwrap();
    }
    see_through
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filled_rectangle_comes_out_where_and_as_it_was_painted() {
        let ctx = crate::theme::test_context();
        let image = picture(&ctx, egui::vec2(40.0, 30.0), 1.0, 2, |ui| {
            ui.painter()
                .rect_filled(egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(20.0, 10.0)), 0.0, crate::theme::PRIMARY);
        });
        assert_eq!((image.width(), image.height()), (40, 30));
        let p = crate::theme::PRIMARY;
        assert_eq!(image.get_pixel(20, 15).0, [p.r(), p.g(), p.b(), 255]);
        assert_eq!(image.get_pixel(2, 2).0, SEE_THROUGH, "nothing painted there");
    }
}
