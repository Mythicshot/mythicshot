//! Sizing and encoding of tour step images.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageEncoder, RgbaImage};

pub const MAX_STEP_WIDTH: u32 = 1920;
/// The tallest step image the server takes (its `imageSide`).
pub const MAX_STEP_HEIGHT: u32 = 4096;
/// The largest step image file the server takes (its `imageBytes`).
pub const MAX_STEP_BYTES: usize = 2_000_000;
pub const PNG_LIMIT_BYTES: usize = 400_000;
pub const JPEG_QUALITY: u8 = 85;
/// The JPEG qualities tried, in order, for a step image still over [`MAX_STEP_BYTES`].
const LOWER_QUALITIES: [u8; 6] = [75, 65, 55, 45, 35, 25];

/// Size after shrinking to at most `max_w` wide, keeping the shape. Never enlarges.
pub fn fit_width(w: u32, h: u32, max_w: u32) -> (u32, u32) {
    if w <= max_w {
        return (w, h);
    }
    let nh = (u64::from(h) * u64::from(max_w) + u64::from(w) / 2) / u64::from(w);
    (max_w, (nh as u32).max(1))
}

/// Size after shrinking to at most `max_w` wide and `max_h` tall, keeping the shape. Never enlarges.
pub fn fit_box(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let (w, h) = fit_width(w, h, max_w);
    let (nh, nw) = fit_width(h, w, max_h);
    (nw, nh)
}

/// The image shrunk to at most MAX_STEP_WIDTH wide and MAX_STEP_HEIGHT tall.
pub fn fit_image(img: RgbaImage) -> RgbaImage {
    let (w, h) = fit_box(img.width(), img.height(), MAX_STEP_WIDTH, MAX_STEP_HEIGHT);
    if (w, h) == (img.width(), img.height()) {
        return img;
    }
    image::imageops::resize(&img, w, h, FilterType::Lanczos3)
}

fn jpeg(img: &RgbaImage, quality: u8) -> Result<Vec<u8>, String> {
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let mut jpg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpg, quality)
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG encode failed: {e}"))?;
    Ok(jpg)
}

/// A step image as [`encode_step`] writes it, made to fit in `limit` bytes: re-encoded as JPEG at
/// lower qualities in steps and, should even the lowest be too big, shrunk by a quarter at a time.
/// Returns the bytes, the extension, the image as encoded (its size goes in the manifest) and
/// whether it had to be squeezed.
pub fn encode_within(img: RgbaImage, limit: usize) -> Result<(Vec<u8>, &'static str, RgbaImage, bool), String> {
    let (bytes, ext) = encode_step(&img)?;
    if bytes.len() <= limit {
        return Ok((bytes, ext, img, false));
    }
    let mut img = img;
    loop {
        for q in LOWER_QUALITIES {
            let jpg = jpeg(&img, q)?;
            if jpg.len() <= limit {
                return Ok((jpg, "jpg", img, true));
            }
        }
        if img.width() <= 1 && img.height() <= 1 {
            return Err("A step image could not be made small enough to publish.".to_string());
        }
        let (w, h) = ((img.width() * 3 / 4).max(1), (img.height() * 3 / 4).max(1));
        img = image::imageops::resize(&img, w, h, FilterType::Lanczos3);
    }
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
    let jpg = jpeg(img, JPEG_QUALITY)?;
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
    fn tall_images_shrink_to_4096_high() {
        assert_eq!(fit_box(1000, 5000, 1920, 4096), (819, 4096));
        assert_eq!(fit_box(3840, 9000, 1920, 4096), (1748, 4096));
        assert_eq!(fit_box(2560, 1440, 1920, 4096), (1920, 1080));
        assert_eq!(fit_box(800, 4096, 1920, 4096), (800, 4096));
        let img = fit_image(RgbaImage::from_pixel(200, 5000, image::Rgba([1, 2, 3, 255])));
        assert_eq!((img.width(), img.height()), (164, 4096));
    }

    fn noise(w: u32, h: u32) -> RgbaImage {
        let mut s = 1u32;
        RgbaImage::from_fn(w, h, |_, _| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            image::Rgba([(s >> 24) as u8, (s >> 16) as u8, (s >> 8) as u8, 255])
        })
    }

    #[test]
    fn an_image_over_the_byte_cap_is_reencoded_at_lower_quality_until_it_fits() {
        let img = noise(300, 200);
        let (first, _) = encode_step(&img).unwrap();
        let limit = first.len() * 2 / 3;
        let (bytes, ext, out, squeezed) = encode_within(img.clone(), limit).unwrap();
        assert!(squeezed && bytes.len() <= limit && ext == "jpg", "{} > {limit}", bytes.len());
        assert_eq!((out.width(), out.height()), (300, 200), "lower quality was enough, so the size stays");
        // One that already fits is left alone.
        let (same, _, _, squeezed) = encode_within(img, first.len()).unwrap();
        assert!(!squeezed && same == first);
        // A limit no quality can meet shrinks the picture until it fits.
        let (bytes, _, out, squeezed) = encode_within(noise(300, 200), 3_000).unwrap();
        assert!(squeezed && bytes.len() <= 3_000 && out.width() < 300);
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
