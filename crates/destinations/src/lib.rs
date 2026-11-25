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
            // A test build never opens the system's file dialog: it is as if the user cancelled it.
            #[cfg(feature = "test-stubs")]
            if stubs::stubbed(format!("save as dialog: {name}")) {
                return Ok(Sent::Cancelled);
            }
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

/// Whether this build answers desktop calls with stubs (a test build). The app refuses to start
/// when it does, so a build with the stubs can never pass for a real one.
pub const STUBBED: bool = cfg!(feature = "test-stubs");

/// The stand-ins test builds use for every call that would reach the desktop: each records what
/// was asked for, on the calling thread, and does nothing else.
#[cfg(feature = "test-stubs")]
pub mod stubs {
    use std::cell::RefCell;

    thread_local! {
        static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Note that `what` was asked for. Always true: the caller returns the stub's answer.
    pub fn stubbed(what: impl Into<String>) -> bool {
        let what = what.into();
        CALLS.with(|c| c.borrow_mut().push(what));
        true
    }

    /// What this thread asked for since the last call, oldest first.
    pub fn take() -> Vec<String> {
        CALLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }
}

/// Longest web address [`open_url`] opens.
const MAX_LINK_CHARS: usize = 300;

/// Whether `url` is a plain web address: `https://`, or `http://` for localhost and
/// 127.0.0.1 only, a host name with an optional port, and a path of letters, digits and
/// `- . _ ~ /`. No user name, query, fragment, escape, space, quote or any other
/// character, so nothing in it can mean anything to a shell or to the system's "open".
pub fn is_web_link(url: &str) -> bool {
    if url.len() > MAX_LINK_CHARS {
        return false;
    }
    let (local_only, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (false, rest),
        (None, Some(rest)) => (true, rest),
        (None, None) => return false,
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let host_ok = !host.is_empty()
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..")
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && (!local_only || host == "localhost" || host == "127.0.0.1");
    let port_ok = port.is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()));
    let path_ok = path.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~/".contains(&b)) && !path.split('/').any(|seg| seg == "." || seg == "..");
    host_ok && port_ok && path_ok
}

/// Open a web address in the default browser. Anything that is not a plain web address
/// ([`is_web_link`]) is refused here, whatever the caller checked, so this can never be
/// used to start a program or open a file. The error never repeats the address.
pub fn open_url(url: &str) -> Result<(), String> {
    if !is_web_link(url) {
        return Err("that is not a web address that can be opened".to_string());
    }
    platform::open_url(url)
}

/// Open a web address at a place on its page: `url` as [`open_url`] takes it, then `#` and
/// `anchor`, which is 1 to 32 lowercase letters, digits and dashes and nothing else. The address
/// itself still has no fragment of its own, so nothing in it can mean anything to the system.
pub fn open_url_at(url: &str, anchor: &str) -> Result<(), String> {
    let plain_anchor = (1..=32).contains(&anchor.len()) && anchor.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !is_web_link(url) || !plain_anchor {
        return Err("that is not a web address that can be opened".to_string());
    }
    platform::open_url(&format!("{url}#{anchor}"))
}

/// A new, unused path in the captures folder from the file name pattern.
/// The pattern may contain `/` or `\` to make subfolders.
pub fn new_path(ctx: &Context) -> Result<PathBuf, String> {
    new_path_with_extension(ctx, ctx.format.extension())
}

/// Like [`new_path`] but with a given extension, for GIFs and other non-image formats.
pub fn new_path_with_extension(ctx: &Context, ext: &str) -> Result<PathBuf, String> {
    let rel = expand_pattern(&ctx.pattern, &ctx.title, chrono::Local::now());
    let path = ctx.output_dir.join(format!("{rel}.{ext}"));
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
    #[cfg(feature = "test-stubs")]
    if stubs::stubbed(format!("clipboard image {}x{}", img.width(), img.height())) {
        return Ok(());
    }
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    cb.set_image(arboard::ImageData { width: img.width() as usize, height: img.height() as usize, bytes: Cow::Borrowed(img.as_raw()) })
        .map_err(|e| format!("could not copy to clipboard: {e}"))
}

pub fn image_from_clipboard() -> Result<RgbaImage, String> {
    #[cfg(feature = "test-stubs")]
    if stubs::stubbed("read clipboard image") {
        return Err("There is no image on the clipboard".to_string());
    }
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

    #[test]
    fn only_a_plain_web_address_counts_as_a_link() {
        for ok in ["https://tours.example.com/t/abc123", "https://x.dev", "https://x.dev:8443/demo/t/a-b_c.d~e", "http://localhost:8787/t/abc", "http://127.0.0.1/t/abc", "https://localhost/t/abc"] {
            assert!(is_web_link(ok), "{ok}");
        }
        for bad in [
            "",
            "tours.example.com/t/abc",
            "http://tours.example.com/t/abc",
            "http://localhost.evil.example/t/abc",
            "http://127.0.0.1.evil.example/",
            "HTTPS://x.dev/t/a",
            "ftp://x.dev/a",
            "file:///C:/Windows/System32/calc.exe",
            "C:\\Windows\\System32\\calc.exe",
            "calc.exe",
            "\\\\server\\share\\run.bat",
            "mailto:?subject=x",
            "javascript:alert(1)",
            "ms-settings:privacy",
            "https://",
            "https:///t/abc",
            "https://user@x.dev/t/a",
            "https://user:pw@x.dev/t/a",
            "https://x.dev/t/a?b=1",
            "https://x.dev/t/a#b",
            "https://x.dev/t/a b",
            "https://x.dev/t/a%20b",
            "https://x.dev/t/a&calc",
            "https://x.dev/t/a|calc",
            "https://x.dev/t/\"a\"",
            "https://x.dev/t/a'b",
            "https://x.dev/t/a\\b",
            "https://x.dev/t/a\nb",
            "https://x.dev/../a",
            "https://x.dev/t/./a",
            "https://x.dev:/t/a",
            "https://x.dev:80a/t/a",
            "https://x.dev:123456/t/a",
            "https://-x.dev/t/a",
            "https://x..dev/t/a",
            "https://[::1]/t/a",
            "https://x.dev/t/\u{202e}a",
            "https://x.dév/t/a",
            " https://x.dev/t/a",
            "https://x.dev/t/a ",
        ] {
            assert!(!is_web_link(bad), "{bad:?}");
        }
        assert!(is_web_link(&format!("https://x.dev/{}", "a".repeat(MAX_LINK_CHARS - 14))));
        assert!(!is_web_link(&format!("https://x.dev/{}", "a".repeat(MAX_LINK_CHARS))));
    }

    #[test]
    fn open_url_refuses_anything_but_a_web_link_without_starting_anything() {
        // Each of these is refused before the system is asked, and the message does not repeat it.
        for bad in ["calc.exe", "file:///C:/Windows/System32/calc.exe", "mailto:?subject=x", "http://example.com/t/a", "https://x.dev/t/a b", "https://x.dev/t/a?b=1"] {
            let e = open_url(bad).unwrap_err();
            assert_eq!(e, "that is not a web address that can be opened");
            assert!(!e.contains(bad));
        }
    }

    #[test]
    fn open_url_at_takes_only_a_plain_anchor_on_a_plain_web_link() {
        for (url, anchor) in [("https://x.dev/changes/", "v0-2-35#b"), ("https://x.dev/changes/", ""), ("https://x.dev/changes/", "V0"), ("https://x.dev/changes/", "a b"), ("https://x.dev/changes/", "a&calc"), ("https://x.dev/changes/", &"a".repeat(33)), ("https://x.dev/t/a?b=1", "v0"), ("calc.exe", "v0"), ("https://x.dev/t/a#b", "v0")] {
            assert_eq!(open_url_at(url, anchor).unwrap_err(), "that is not a web address that can be opened", "{url} {anchor}");
        }
    }

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
