//! Where a finished capture goes. Phase 1 has two outputs, a PNG file and the
//! clipboard; Phase 3 turns these into the `Destination` trait.

use std::borrow::Cow;
use std::path::PathBuf;

use image::RgbaImage;

use crate::settings::Settings;

pub struct Delivered {
    pub file: Option<PathBuf>,
    pub clipboard: bool,
}

pub fn deliver(img: &RgbaImage, settings: &Settings) -> Result<Delivered, String> {
    let mut done = Delivered { file: None, clipboard: false };
    if settings.save_to_file {
        let path = capture_path(&settings.output_dir)?;
        save_image(img, &path)?;
        done.file = Some(path);
    }
    if settings.copy_to_clipboard {
        copy_to_clipboard(img)?;
        done.clipboard = true;
    }
    Ok(done)
}

/// A new, unused `Mythicshot <date time>.png` path in `dir`.
pub fn capture_path(dir: &std::path::Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let name = format!("Mythicshot {}.png", chrono::Local::now().format("%Y-%m-%d %H-%M-%S"));
    Ok(unique(dir.join(name)))
}

/// Save as PNG, or JPEG when the extension says so (JPEG has no transparency).
pub fn save_image(img: &RgbaImage, path: &std::path::Path) -> Result<(), String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let result = if ext == "jpg" || ext == "jpeg" { image::DynamicImage::ImageRgba8(img.clone()).to_rgb8().save(path) } else { img.save(path) };
    result.map_err(|e| format!("could not save {}: {e}", path.display()))
}

pub fn copy_to_clipboard(img: &RgbaImage) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    cb.set_image(arboard::ImageData { width: img.width() as usize, height: img.height() as usize, bytes: Cow::Borrowed(img.as_raw()) })
        .map_err(|e| format!("could not copy to clipboard: {e}"))
}

pub fn image_from_clipboard() -> Result<RgbaImage, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    let data = cb.get_image().map_err(|_| "There is no image on the clipboard".to_string())?;
    RgbaImage::from_raw(data.width as u32, data.height as u32, data.bytes.into_owned()).ok_or_else(|| "clipboard image has an unexpected size".to_string())
}

/// `name.png`, or `name (2).png` and so on if that file already exists.
fn unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Mythicshot").to_string();
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).png")))
        .find(|p| !p.exists())
        .expect("a free file name")
}
