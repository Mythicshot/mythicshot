//! Sizing and encoding of tour step images.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageEncoder, RgbaImage};

pub const MAX_STEP_WIDTH: u32 = 1920;
pub const PNG_LIMIT_BYTES: usize = 400_000;
pub const JPEG_QUALITY: u8 = 85;

/// Size after shrinking to at most `max_w` wide, keeping the shape. Never enlarges.
pub fn fit_width(w: u32, h: u32, max_w: u32) -> (u32, u32) {
    if w <= max_w {
        return (w, h);
    }
    let nh = (u64::from(h) * u64::from(max_w) + u64::from(w) / 2) / u64::from(w);
    (max_w, (nh as u32).max(1))
}

/// The image shrunk to at most MAX_STEP_WIDTH wide.
pub fn fit_image(img: RgbaImage) -> RgbaImage {
    let (w, h) = fit_width(img.width(), img.height(), MAX_STEP_WIDTH);
    if (w, h) == (img.width(), img.height()) {
        return img;
    }
    image::imageops::resize(&img, w, h, FilterType::Lanczos3)
}

/// Encoded bytes and file extension ("png" or "jpg").
pub fn encode_step(img: &RgbaImage) -> Result<(Vec<u8>, &'static str), String> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    if png.len() <= PNG_LIMIT_BYTES {
        return Ok((png, "png"));
    }
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let mut jpg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpg, JPEG_QUALITY)
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG encode failed: {e}"))?;
    if jpg.len() < png.len() { Ok((jpg, "jpg")) } else { Ok((png, "png")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_width_shrinks_wide_images_and_leaves_small_ones() {
        assert_eq!(fit_width(2560, 1440, 1920), (1920, 1080));
        assert_eq!(fit_width(5120, 1440, 1920), (1920, 540));
        assert_eq!(fit_width(800, 600, 1920), (800, 600));
        assert_eq!(fit_width(1920, 1, 1920), (1920, 1));
        assert_eq!(fit_width(4000, 1, 1920), (1920, 1));
    }

    #[test]
    fn fit_image_resizes_to_the_fitted_size() {
        let img = fit_image(RgbaImage::from_pixel(2560, 1440, image::Rgba([10, 20, 30, 255])));
        assert_eq!((img.width(), img.height()), (1920, 1080));
    }

    #[test]
    fn flat_screens_stay_png() {
        let img = RgbaImage::from_pixel(1920, 1080, image::Rgba([240, 240, 240, 255]));
        let (bytes, ext) = encode_step(&img).unwrap();
        assert_eq!(ext, "png");
        assert!(bytes.len() <= PNG_LIMIT_BYTES);
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn busy_screens_fall_back_to_jpeg() {
        // Deterministic noise: PNG cannot compress it under the limit.
        let mut s = 1u32;
        let img = RgbaImage::from_fn(1920, 1080, |_, _| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            image::Rgba([(s >> 24) as u8, (s >> 16) as u8, (s >> 8) as u8, 255])
        });
        let (bytes, ext) = encode_step(&img).unwrap();
        assert_eq!(ext, "jpg");
        assert_eq!(&bytes[..2], b"\xFF\xD8");
    }
}
