//! Destinations: where a finished capture goes (Greenshot's model).
//!
//! The app calls [`send`] with a [`DestId`]. Two destinations live in the app
//! instead, because they open windows: `Editor` and `Picker` (the "ask every
//! time" menu). Everything here works on a plain image and a [`Context`].

mod filename;
mod platform;

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use image::RgbaImage;
use serde::{Deserialize, Serialize};

pub use filename::{expand_pattern, DEFAULT_PATTERN};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestId {
    /// Show a menu after each capture and let the user choose.
    Picker,
    Editor,
    File,
    FileAs,
    Clipboard,
    Print,
    Email,
}

impl DestId {
    /// Every destination, in menu order.
    pub const ALL: &'static [DestId] = &[DestId::Picker, DestId::Editor, DestId::File, DestId::FileAs, DestId::Clipboard, DestId::Print, DestId::Email];

    pub fn label(self) -> &'static str {
        match self {
            DestId::Picker => "Ask every time",
            DestId::Editor => "Open in editor",
            DestId::File => "Save",
            DestId::FileAs => "Save as…",
            DestId::Clipboard => "Copy to clipboard",
            DestId::Print => "Print…",
            DestId::Email => "Email",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            DestId::Picker => "Show a menu of destinations after each capture",
            DestId::Editor => "Annotate before saving",
            DestId::File => "Save to the captures folder using the file name pattern",
            DestId::FileAs => "Choose the folder and name each time",
            DestId::Clipboard => "Paste it anywhere",
            DestId::Print => "Print with the system print dialog",
            DestId::Email => "Open a new email with the capture attached",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }
}

/// What a destination needs to know besides the image.
#[derive(Debug, Clone)]
pub struct Context {
    pub output_dir: PathBuf,
    pub pattern: String,
    pub format: ImageFormat,
    pub jpeg_quality: u8,
    /// Window title or a short description, for `${title}` in the pattern.
    pub title: String,
}

/// What happened, for the toast or status line.
#[derive(Debug, Clone, PartialEq)]
pub enum Sent {
    Saved(PathBuf),
    Copied,
    Printing,
    /// The mail app opened with the image attached.
    EmailOpened,
    /// No mail app took the attachment; the image is on the clipboard and a
    /// blank email was opened instead.
    EmailFallback,
    Cancelled,
}

impl Sent {
    pub fn describe(&self) -> String {
        match self {
            Sent::Saved(p) => format!("Saved {}", p.display()),
            Sent::Copied => "Copied to clipboard".into(),
            Sent::Printing => "Sent to the print dialog".into(),
            Sent::EmailOpened => "Email opened with the capture attached".into(),
            Sent::EmailFallback => "No mail app took the attachment: the capture is on the clipboard, paste it into the email".into(),
            Sent::Cancelled => "Cancelled".into(),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Sent::Saved(p) => Some(p),
            _ => None,
        }
    }
}

/// Send `img` to one destination. `Editor` and `Picker` are the app's job.
pub fn send(dest: DestId, img: &RgbaImage, ctx: &Context) -> Result<Sent, String> {
    match dest {
        DestId::File => {
            let path = new_path(ctx)?;
            save(img, &path, ctx.format, ctx.jpeg_quality)?;
            Ok(Sent::Saved(path))
        }
        DestId::FileAs => {
            let name = new_path(ctx)?.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let picked = rfd::FileDialog::new()
                .set_directory(&ctx.output_dir)
                .set_file_name(&name)
                .add_filter("PNG image", &["png"])
                .add_filter("JPEG image", &["jpg", "jpeg"])
                .save_file();
            match picked {
                Some(path) => {
                    let format = format_for(&path).unwrap_or(ctx.format);
                    save(img, &path, format, ctx.jpeg_quality)?;
                    Ok(Sent::Saved(path))
                }
                None => Ok(Sent::Cancelled),
            }
        }
        DestId::Clipboard => {
            copy_to_clipboard(img)?;
            Ok(Sent::Copied)
        }
        DestId::Print => {
            let path = temp_png(img, ctx)?;
            platform::print(&path)?;
            Ok(Sent::Printing)
        }
        DestId::Email => {
            let path = temp_png(img, ctx)?;
            let subject = if ctx.title.is_empty() { "Screenshot".to_string() } else { ctx.title.clone() };
            if platform::email(&path, &subject).is_ok() {
                return Ok(Sent::EmailOpened);
            }
            copy_to_clipboard(img)?;
            platform::open_url(&format!("mailto:?subject={}", url_escape(&subject)))?;
            Ok(Sent::EmailFallback)
        }
        DestId::Editor | DestId::Picker => Err(format!("{} is handled by the app", dest.label())),
    }
}

/// A new, unused path in the captures folder from the file name pattern.
/// The pattern may contain `/` or `\` to make subfolders.
pub fn new_path(ctx: &Context) -> Result<PathBuf, String> {
    let rel = expand_pattern(&ctx.pattern, &ctx.title, chrono::Local::now());
    let path = ctx.output_dir.join(format!("{rel}.{}", ctx.format.extension()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    Ok(unique(path))
}

fn format_for(path: &Path) -> Option<ImageFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        _ => None,
    }
}

/// Save as PNG or JPEG. The format follows the extension when it names one.
pub fn save(img: &RgbaImage, path: &Path, format: ImageFormat, jpeg_quality: u8) -> Result<(), String> {
    let err = |e: image::ImageError| format!("could not save {}: {e}", path.display());
    match format_for(path).unwrap_or(format) {
        ImageFormat::Png => img.save(path).map_err(err),
        ImageFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            let file = std::fs::File::create(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), jpeg_quality.clamp(1, 100));
            enc.encode_image(&rgb).map_err(err)
        }
    }
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

/// A PNG in the temp folder for print and email, named like a normal capture.
fn temp_png(img: &RgbaImage, ctx: &Context) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("Mythicshot");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let name = expand_pattern(&ctx.pattern, &ctx.title, chrono::Local::now()).replace(['/', '\\'], " ");
    let path = unique(dir.join(format!("{name}.png")));
    save(img, &path, ImageFormat::Png, 100)?;
    Ok(path)
}

/// `name.ext`, or `name (2).ext` and so on if that file already exists.
fn unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Mythicshot").to_string();
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("png").to_string();
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    (2..).map(|n| dir.join(format!("{stem} ({n}).{ext}"))).find(|p| !p.exists()).expect("a free file name")
}

fn url_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> Context {
        Context { output_dir: dir.to_path_buf(), pattern: "shots/${YYYY}/${title}".into(), format: ImageFormat::Jpeg, jpeg_quality: 85, title: "My: Window?".into() }
    }

    #[test]
    fn file_destination_uses_pattern_subfolders_and_format() {
        let dir = std::env::temp_dir().join(format!("mythicshot-dest-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let img = RgbaImage::from_pixel(8, 6, image::Rgba([10, 20, 30, 255]));
        let first = send(DestId::File, &img, &ctx(&dir)).unwrap();
        let second = send(DestId::File, &img, &ctx(&dir)).unwrap();
        let (Sent::Saved(a), Sent::Saved(b)) = (first, second) else { panic!("expected saved files") };
        assert!(a.starts_with(dir.join("shots")));
        assert_eq!(a.file_name().unwrap().to_str().unwrap(), "My_ Window_.jpg");
        assert_eq!(b.file_name().unwrap().to_str().unwrap(), "My_ Window_ (2).jpg");
        assert_eq!(image::open(&a).unwrap().width(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn url_escaping() {
        assert_eq!(url_escape("Capture 12:30"), "Capture%2012%3A30");
    }
}
