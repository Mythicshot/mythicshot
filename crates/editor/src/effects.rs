//! Whole-image effects (Greenshot's Effects menu). Each takes the flattened
//! image and returns a new one; [`crate::Document::apply`] handles undo.

use image::{Rgba as Px, RgbaImage, imageops};

use crate::render::{box_blur, to_pixmap, to_rgba_image};

/// A soft shadow below and to the right; the canvas grows to fit it.
pub fn drop_shadow(img: &RgbaImage) -> RgbaImage {
    const OFFSET: u32 = 6;
    const BLUR: usize = 4;
    let pad = BLUR as u32 * 3 + OFFSET;
    let (w, h) = (img.width() + pad * 2, img.height() + pad * 2);
    // Shadow layer: the image's alpha in black, blurred.
    let mut layer = vec![0u8; (w * h * 4) as usize];
    for (x, y, p) in img.enumerate_pixels() {
        let i = (((y + pad + OFFSET) * w + x + pad + OFFSET) * 4) as usize;
        layer[i + 3] = (p.0[3] as f32 * 0.55) as u8;
    }
    box_blur(&mut layer, w as usize, h as usize, BLUR, 3);
    let mut out = RgbaImage::from_raw(w, h, layer).expect("shadow layer");
    imageops::overlay(&mut out, img, pad as i64, pad as i64);
    out
}

/// Ragged, paper-like edges on all four sides.
pub fn torn_edge(img: &RgbaImage) -> RgbaImage {
    const TOOTH: u32 = 14;
    const DEPTH: f32 = 9.0;
    let (w, h) = (img.width(), img.height());
    let mut out = img.clone();
    // A fixed pseudo-random sequence, so the same image always tears the same way.
    let mut seed: u32 = 0x9E37_79B9;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 1000) as f32 / 1000.0
    };
    let mut edge = |len: u32| -> Vec<f32> {
        let peaks: Vec<f32> = (0..=len / TOOTH + 1).map(|i| if i % 2 == 0 { next() * DEPTH * 0.4 } else { DEPTH * (0.6 + next() * 0.4) }).collect();
        (0..len)
            .map(|i| {
                let k = (i / TOOTH) as usize;
                let f = (i % TOOTH) as f32 / TOOTH as f32;
                peaks[k] * (1.0 - f) + peaks[k + 1] * f
            })
            .collect()
    };
    let (top, bottom, left, right) = (edge(w), edge(w), edge(h), edge(h));
    for (x, y, p) in out.enumerate_pixels_mut() {
        let d = [y as f32 - top[x as usize], (h - 1 - y) as f32 - bottom[x as usize], x as f32 - left[y as usize], (w - 1 - x) as f32 - right[y as usize]];
        let inside = d.iter().cloned().fold(f32::MAX, f32::min);
        let cover = (inside + 0.5).clamp(0.0, 1.0);
        p.0[3] = (p.0[3] as f32 * cover) as u8;
    }
    out
}

pub fn border(img: &RgbaImage, width: u32, color: [u8; 4]) -> RgbaImage {
    let mut out = RgbaImage::from_pixel(img.width() + width * 2, img.height() + width * 2, Px(color));
    imageops::overlay(&mut out, img, width as i64, width as i64);
    out
}

pub fn greyscale(img: &RgbaImage) -> RgbaImage {
    let mut out = img.clone();
    for p in out.pixels_mut() {
        let [r, g, b, a] = p.0;
        let l = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32).round() as u8;
        p.0 = [l, l, l, a];
    }
    out
}

pub fn invert(img: &RgbaImage) -> RgbaImage {
    let mut out = img.clone();
    for p in out.pixels_mut() {
        let [r, g, b, a] = p.0;
        p.0 = [255 - r, 255 - g, 255 - b, a];
    }
    out
}

pub fn resize(img: &RgbaImage, percent: u32) -> RgbaImage {
    let w = (img.width() * percent / 100).max(1);
    let h = (img.height() * percent / 100).max(1);
    imageops::resize(img, w, h, imageops::FilterType::Lanczos3)
}

pub fn rotate_cw(img: &RgbaImage) -> RgbaImage {
    imageops::rotate90(img)
}

pub fn rotate_ccw(img: &RgbaImage) -> RgbaImage {
    imageops::rotate270(img)
}

/// Round-trip through tiny-skia, used by tests to check the converters agree.
pub fn passthrough(img: &RgbaImage) -> RgbaImage {
    to_rgba_image(&to_pixmap(img))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img() -> RgbaImage {
        RgbaImage::from_pixel(40, 30, Px([200, 100, 50, 255]))
    }

    #[test]
    fn sizes() {
        assert_eq!(border(&img(), 5, [0, 0, 0, 255]).dimensions(), (50, 40));
        assert_eq!(resize(&img(), 50).dimensions(), (20, 15));
        assert_eq!(rotate_cw(&img()).dimensions(), (30, 40));
        let s = drop_shadow(&img());
        assert!(s.width() > 40 && s.height() > 30);
        assert_eq!(torn_edge(&img()).dimensions(), (40, 30));
    }

    #[test]
    fn colours() {
        assert_eq!(invert(&img()).get_pixel(0, 0).0, [55, 155, 205, 255]);
        let g = greyscale(&img()).get_pixel(0, 0).0;
        assert_eq!(g[0], g[1]);
        assert_eq!(passthrough(&img()), img());
    }

    #[test]
    fn torn_edge_clears_corners_keeps_centre() {
        let t = torn_edge(&RgbaImage::from_pixel(200, 200, Px([255, 255, 255, 255])));
        assert_eq!(t.get_pixel(100, 100).0[3], 255);
        assert!(t.get_pixel(0, 0).0[3] < 255);
    }
}
