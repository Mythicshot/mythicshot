//! Tour settings and guide cards: everything the Tour view edits that is not an
//! annotation, and how it is sanitised into the version 3 manifest.
//!
//! Every string is written to the manifest as plain data; caps and ranges are
//! applied here on export, so a hand-edited or stale value can never reach a bundle.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::codecs::jpeg::JpegDecoder;
use image::codecs::png::PngDecoder;
use image::{DynamicImage, ImageDecoder, ImageEncoder};
use mythicshot_recording::Recording;

use super::{JPEG_QUALITY, Point, fit_width, pct};
use crate::video::{Edit, crop_px};

pub const DEFAULT_ACCENT: &str = "#5dcaa5";
pub const LOGO_MAX_SIDE: u32 = 512;
pub const LOGO_MAX_BYTES: usize = 200_000;

pub const CARD_TITLE_MAX: usize = 80;
pub const CARD_BODY_MAX: usize = 300;
pub const NEXT_LABEL_MAX: usize = 24;
pub const COVER_TITLE_MAX: usize = 80;
pub const COVER_BODY_MAX: usize = 200;
pub const COVER_BUTTON_MAX: usize = 24;
pub const BUTTON_LABEL_MAX: usize = 24;
pub const END_TEXT_MAX: usize = 200;
pub const LINK_MAX: usize = 500;
pub const ZOOM_MIN: u16 = 105;
pub const ZOOM_MAX: u16 = 140;
pub const AUTOPLAY_MIN: u8 = 2;
pub const AUTOPLAY_MAX: u8 = 20;

/// A logo file larger than this is refused before it is decoded.
const LOGO_MAX_FILE_BYTES: u64 = 30_000_000;
/// A logo wider or taller than this is refused from its header, before it is decoded.
pub const LOGO_MAX_SOURCE_SIDE: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CardStyle {
    #[default]
    Dark,
    Light,
    Compact,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HotspotStyle {
    #[default]
    Pulse,
    Dot,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Anchor {
    #[default]
    Hotspot,
    Point,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(not(test), expect(dead_code, reason = "the Tour view always places cards by the shared rule; the sides are for a manifest that asks for one"))]
pub enum Placement {
    #[default]
    Auto,
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Advance {
    #[default]
    Hotspot,
    Anywhere,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Theme {
    pub card_style: CardStyle,
    pub accent: String,
    pub hotspot: HotspotStyle,
    pub progress_bar: bool,
    /// "auto" or "#rrggbb".
    pub backdrop: String,
    pub spotlight_soft: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self { card_style: CardStyle::Dark, accent: DEFAULT_ACCENT.to_string(), hotspot: HotspotStyle::Pulse, progress_bar: true, backdrop: "auto".to_string(), spotlight_soft: true }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Zoom {
    pub enabled: bool,
    pub percent: u16,
}

impl Default for Zoom {
    fn default() -> Self {
        Self { enabled: true, percent: 118 }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Link {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Cover {
    pub title: String,
    pub body: String,
    pub button: String,
    /// The logo's file name in the bundle; set on export from [`TourDoc::logo_path`].
    pub logo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct EndCard {
    pub text: String,
    pub primary: Option<Link>,
    pub secondary: Option<Link>,
    pub replay: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Autoplay {
    pub enabled: bool,
    pub seconds: u8,
}

impl Default for Autoplay {
    fn default() -> Self {
        Self { enabled: false, seconds: 5 }
    }
}

/// `point` is in frame pixels in the editor and in percent in the manifest.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Card {
    pub title: String,
    pub body: String,
    pub next_label: String,
    pub anchor: Anchor,
    pub point: Option<Point>,
    pub placement: Placement,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StepExtras {
    pub card: Option<Card>,
    pub advance: Advance,
    pub zoom: Option<bool>,
}

/// Everything the Tour view edits that is not an annotation. Lives on `Edit`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TourDoc {
    pub theme: Theme,
    pub zoom: Zoom,
    pub cover: Option<Cover>,
    pub logo_path: Option<PathBuf>,
    pub cta: Option<Link>,
    pub end: Option<EndCard>,
    pub autoplay: Autoplay,
    /// Per step, keyed by the step's source frame.
    pub steps: BTreeMap<usize, StepExtras>,
}

/// True for a Unicode format (Cf) character: soft hyphen, bidi controls and isolates, zero-width
/// characters, the byte order mark, tags and the like. None of them is ever wanted in tour text.
fn is_format(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// Tour text cleaned and capped: `\r\n` and `\r` become `\n`; a tab becomes a space; every other
/// control (Cc) and format (Cf) character is removed; `\n` is kept only when `multiline` and becomes
/// a space otherwise; then the text is trimmed and cut at `max`. The cap counts Unicode code points
/// (Rust `char`s) after this cleaning, the same way the server counts them.
pub(super) fn clean(s: &str, max: usize, multiline: bool) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned: String = s
        .chars()
        .filter_map(|c| match c {
            '\n' if multiline => Some('\n'),
            '\n' | '\t' => Some(' '),
            c if c.is_control() || is_format(c) => None,
            c => Some(c),
        })
        .collect();
    cleaned.trim().chars().take(max).collect()
}

/// A single-line field (titles, labels): see [`clean`].
fn line(s: &str, max: usize) -> String {
    clean(s, max, false)
}

/// A multi-line field (card body, cover body, end text): see [`clean`].
fn text(s: &str, max: usize) -> String {
    clean(s, max, true)
}

/// `#rrggbb` lowercased, or None.
pub(super) fn hex_colour(s: &str) -> Option<String> {
    let s = s.trim();
    let digits = s.strip_prefix('#')?;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// `https://` (the scheme in any case, written back lowercase), then an authority with no `@` that is
/// a host of two or more non-empty labels of ASCII letters, digits and `-` separated by `.`, no label
/// starting or ending with `-` and the last label holding at least one letter (so no bare names or IP
/// addresses), with an optional `:` and a port of 1 to 5 digits from 1 to 65535; no whitespace, control
/// (Cc) or format (Cf) character anywhere; at most 500 characters. `tour_core.js` gives the same answers.
pub fn safe_url(s: &str) -> Option<String> {
    let s = s.trim();
    if s.chars().count() > LINK_MAX || s.chars().any(|c| c.is_whitespace() || c.is_control() || is_format(c)) {
        return None;
    }
    let scheme = s.get(..8)?;
    if !scheme.eq_ignore_ascii_case("https://") {
        return None;
    }
    let rest = &s[8..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let host_ok = labels.len() >= 2 && labels.iter().all(label_ok) && labels.last().is_some_and(|l| l.bytes().any(|b| b.is_ascii_alphabetic()));
    let port_ok = port.is_none_or(|p| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n)));
    (host_ok && port_ok).then(|| format!("https://{rest}"))
}

/// A button with its label cut at the cap, or None when its link is not safe or it has no label.
fn safe_link(l: &Link) -> Option<Link> {
    let label = line(&l.label, BUTTON_LABEL_MAX);
    let url = safe_url(&l.url)?;
    (!label.is_empty()).then_some(Link { label, url })
}

/// The tour-level manifest fields: trimmed, cut at the caps, clamped, colours lowercased, bad colours
/// replaced by the default, buttons with bad links dropped, a cover with no title dropped.
/// Steps are copied unchanged; [`step_card`] sanitises each card.
pub fn sanitize(doc: &TourDoc) -> TourDoc {
    let backdrop = if doc.theme.backdrop.trim() == "auto" { "auto".to_string() } else { hex_colour(&doc.theme.backdrop).unwrap_or_else(|| "auto".to_string()) };
    let theme = Theme { accent: hex_colour(&doc.theme.accent).unwrap_or_else(|| DEFAULT_ACCENT.to_string()), backdrop, ..doc.theme.clone() };
    let cover = doc.cover.as_ref().and_then(|c| {
        let title = line(&c.title, COVER_TITLE_MAX);
        let button = line(&c.button, COVER_BUTTON_MAX);
        let button = if button.is_empty() { "Start".to_string() } else { button };
        (!title.is_empty()).then(|| Cover { title, body: text(&c.body, COVER_BODY_MAX), button, logo: None })
    });
    let end = doc.end.as_ref().map(|e| EndCard { text: text(&e.text, END_TEXT_MAX), primary: e.primary.as_ref().and_then(safe_link), secondary: e.secondary.as_ref().and_then(safe_link), replay: e.replay });
    TourDoc {
        theme,
        zoom: Zoom { enabled: doc.zoom.enabled, percent: doc.zoom.percent.clamp(ZOOM_MIN, ZOOM_MAX) },
        cover,
        logo_path: doc.logo_path.clone(),
        cta: doc.cta.as_ref().and_then(safe_link),
        end,
        autoplay: Autoplay { enabled: doc.autoplay.enabled, seconds: doc.autoplay.seconds.clamp(AUTOPLAY_MIN, AUTOPLAY_MAX) },
        steps: doc.steps.clone(),
    }
}

/// The step's card for the manifest, or None when it has neither title nor body. `click` is the step's hotspot in percent.
pub fn step_card(rec: &Recording, edit: &Edit, src: usize, click: Option<(f32, f32)>) -> Option<Card> {
    let c = edit.tour_doc.steps.get(&src)?.card.as_ref()?;
    let (title, body) = (line(&c.title, CARD_TITLE_MAX), text(&c.body, CARD_BODY_MAX));
    if title.is_empty() && body.is_empty() {
        return None;
    }
    let next_label = line(&c.next_label, NEXT_LABEL_MAX);
    let next_label = if next_label.is_empty() { "Next".to_string() } else { next_label };
    let (anchor, point) = match c.anchor {
        Anchor::Hotspot if click.is_some() => (Anchor::Hotspot, None),
        Anchor::Point => {
            let (cx, cy, cw, ch) = crop_px(rec, edit);
            let (x0, y0, x1, y1) = (cx as f32, cy as f32, (cx + cw) as f32, (cy + ch) as f32);
            match &c.point {
                Some(p) if p.x >= x0 && p.x < x1 && p.y >= y0 && p.y < y1 => (Anchor::Point, Some(Point { x: pct((p.x - x0) / (x1 - x0) * 100.0), y: pct((p.y - y0) / (y1 - y0) * 100.0) })),
                _ => (Anchor::None, None),
            }
        }
        _ => (Anchor::None, None),
    };
    Some(Card { title, body, next_label, anchor, point, placement: c.placement })
}

/// The logo ready for the bundle: bytes and "png" or "jpg". It is decoded, turned upright by its EXIF
/// orientation and encoded again (which also drops any metadata), shrunk to at most [`LOGO_MAX_SIDE`]
/// on either side, and refused when it is not a PNG or JPEG by its first bytes, is over
/// [`LOGO_MAX_SOURCE_SIDE`] on either side, or is still over [`LOGO_MAX_BYTES`].
pub fn load_logo(path: &Path) -> Result<(Vec<u8>, &'static str), String> {
    let name = path.display();
    let size = std::fs::metadata(path).map_err(|e| format!("Could not read the cover logo {name}: {e}"))?.len();
    if size > LOGO_MAX_FILE_BYTES {
        return Err(format!("The cover logo {name} is too large; use a PNG or JPEG under {} KB.", LOGO_MAX_BYTES / 1000));
    }
    let raw = std::fs::read(path).map_err(|e| format!("Could not read the cover logo {name}: {e}"))?;
    let unreadable = |e: image::ImageError| format!("The cover logo {name} could not be read as a PNG or JPEG image: {e}");
    let (img, ext) = if raw.starts_with(b"\x89PNG\r\n\x1a\n") {
        (decode_upright(PngDecoder::new(std::io::Cursor::new(&raw[..])).map_err(unreadable)?, &name)?, "png")
    } else if raw.starts_with(b"\xFF\xD8\xFF") {
        (decode_upright(JpegDecoder::new(std::io::Cursor::new(&raw[..])).map_err(unreadable)?, &name)?, "jpg")
    } else {
        return Err(format!("The cover logo {name} is not a PNG or JPEG image."));
    };
    let (w, h) = (img.width(), img.height());
    let (nw, nh) = if w >= h { fit_width(w, h, LOGO_MAX_SIDE) } else { let (a, b) = fit_width(h, w, LOGO_MAX_SIDE); (b, a) };
    let img = if (nw, nh) == (w, h) { img } else { img.resize_exact(nw, nh, FilterType::Lanczos3) };
    let mut out = Vec::new();
    if ext == "png" {
        let rgba = img.to_rgba8();
        image::codecs::png::PngEncoder::new(&mut out).write_image(rgba.as_raw(), rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8).map_err(|e| format!("Could not encode the cover logo: {e}"))?;
    } else {
        let rgb = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8).map_err(|e| format!("Could not encode the cover logo: {e}"))?;
    }
    if out.len() > LOGO_MAX_BYTES {
        return Err(format!("The cover logo {name} is still over {} KB after shrinking to {LOGO_MAX_SIDE} pixels; use a simpler or smaller image.", LOGO_MAX_BYTES / 1000));
    }
    Ok((out, ext))
}

/// Decode a logo whose header has been read, refusing it first when either side is over
/// [`LOGO_MAX_SOURCE_SIDE`], then turned upright by its EXIF orientation.
fn decode_upright(mut decoder: impl ImageDecoder, name: &impl std::fmt::Display) -> Result<DynamicImage, String> {
    let (w, h) = decoder.dimensions();
    if w > LOGO_MAX_SOURCE_SIDE || h > LOGO_MAX_SOURCE_SIDE {
        return Err(format!("The cover logo {name} is {w} x {h} pixels; use an image at most {LOGO_MAX_SOURCE_SIDE} pixels on either side."));
    }
    // A missing or unreadable orientation leaves the image as it is.
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| format!("The cover logo {name} could not be read as a PNG or JPEG image: {e}"))?;
    img.apply_orientation(orientation);
    Ok(img)
}
