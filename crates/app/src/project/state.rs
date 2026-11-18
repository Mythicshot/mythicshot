//! The on-disk format of a saved project: `project.json`, `format: 1`.
//!
//! The file has its own types, so a change to the editor's types cannot change saved files
//! unnoticed; `golden-v1.json` pins the exact text. Pasted pictures and the cover logo are files
//! beside the JSON, named by their content. Only mouse events are saved: key presses stay in
//! memory and go with the recording, as Preferences promises. The token is not part of any type
//! here, so it cannot reach a project.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use image::codecs::png::{PngDecoder, PngEncoder};
use image::{DynamicImage, ImageDecoder, ImageEncoder};
use mythicshot_editor::overlays::{InputEvent, InputKind, MouseButton, OverlayStyle};
use mythicshot_editor::{Bounds, ImageData, Item, MAX_DIM, MIN_DIM, ObfuscateMode, Pixmap, Pt, Rgba, Shape, Style, to_rgba_image};
use mythicshot_recording::Recording;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::tour::{Advance, Anchor, Autoplay, Card, CardStyle, Cover, EndCard, HotspotStyle, Link, Placement, Point, StepExtras, Theme, TourDoc, Zoom};
use crate::video::Edit;

/// The format this build writes. A file with a higher number was written by a newer Mythicshot.
pub const FORMAT: u32 = 1;

/// A cover logo larger than this is not copied into a project; the tour export refuses it too.
const LOGO_MAX_FILE_BYTES: u64 = 30_000_000;
/// A picture file larger than this, or with more pixels than [`PICTURE_MAX_PIXELS`], is not loaded.
const PICTURE_MAX_FILE_BYTES: u64 = 256 * 1_048_576;
const PICTURE_MAX_PIXELS: u64 = 100_000_000;

/// What a project belongs to. `server` is the address as `publish::check_server` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectKey {
    pub server: String,
    pub tour_id: String,
    pub chapter_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub format: u32,
    /// The Mythicshot version that wrote the file.
    #[serde(default)]
    pub app: String,
    /// Milliseconds since 1970.
    #[serde(default)]
    pub saved_at: i64,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub tour_id: String,
    #[serde(default)]
    pub chapter_id: String,
    #[serde(default)]
    pub tour_title: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub steps: u32,
    /// The size of `frames.zst` when the project was saved.
    #[serde(default)]
    pub frames_bytes: u64,
    #[serde(default)]
    pub edit: SavedEdit,
    /// Mouse events only.
    #[serde(default)]
    pub events: Vec<SavedEvent>,
    #[serde(default)]
    pub items: Vec<SavedItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedEdit {
    /// First and last frame kept; none means the whole recording.
    pub trim: Option<(usize, usize)>,
    pub deleted: BTreeSet<usize>,
    pub speed: f32,
    pub reverse: bool,
    /// `[x0, y0, x1, y1]` in frame pixels.
    pub crop: Option<[f32; 4]>,
    pub scale: u32,
    pub overlays: OverlayStyle,
    pub tour_removed: BTreeSet<usize>,
    pub tour_doc: SavedTourDoc,
}

impl Default for SavedEdit {
    fn default() -> Self {
        Self { trim: None, deleted: BTreeSet::new(), speed: 1.0, reverse: false, crop: None, scale: 100, overlays: OverlayStyle::default(), tour_removed: BTreeSet::new(), tour_doc: SavedTourDoc::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTourDoc {
    pub theme: SavedTheme,
    pub zoom: SavedZoom,
    pub cover: Option<SavedCover>,
    /// The logo's file name inside the project folder.
    pub logo: Option<String>,
    pub cta: Option<SavedLink>,
    pub end: Option<SavedEnd>,
    pub autoplay: SavedAutoplay,
    /// Keyed by the step's source frame.
    pub steps: BTreeMap<usize, SavedStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTheme {
    pub card_style: String,
    pub accent: String,
    pub hotspot: String,
    pub progress_bar: bool,
    pub backdrop: String,
    pub spotlight_soft: bool,
}

impl Default for SavedTheme {
    fn default() -> Self {
        save_theme(&Theme::default())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedZoom {
    pub enabled: bool,
    pub percent: u16,
}

impl Default for SavedZoom {
    fn default() -> Self {
        let z = Zoom::default();
        Self { enabled: z.enabled, percent: z.percent }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedCover {
    pub title: String,
    pub body: String,
    pub button: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedEnd {
    pub text: String,
    pub primary: Option<SavedLink>,
    pub secondary: Option<SavedLink>,
    pub replay: bool,
}

impl Default for SavedEnd {
    fn default() -> Self {
        Self { text: String::new(), primary: None, secondary: None, replay: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedAutoplay {
    pub enabled: bool,
    pub seconds: u8,
}

impl Default for SavedAutoplay {
    fn default() -> Self {
        let a = Autoplay::default();
        Self { enabled: a.enabled, seconds: a.seconds }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedStep {
    pub card: Option<SavedCard>,
    pub advance: String,
    pub zoom: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedCard {
    pub title: String,
    pub body: String,
    pub next_label: String,
    pub anchor: String,
    /// `[x, y]` in frame pixels.
    pub point: Option<[f32; 2]>,
    pub placement: String,
}

/// A mouse button going down or up, at a position relative to the recorded area.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedEvent {
    pub t_ms: u64,
    /// "down" or "up".
    pub kind: String,
    /// "left", "right" or "middle".
    pub button: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedItem {
    pub shape: SavedShape,
    pub style: SavedStyle,
    pub frames: Option<(u32, u32)>,
    pub pin: Option<u32>,
}

/// Points are `[x, y]` and rectangles `[x0, y0, x1, y1]`, in frame pixels.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedShape {
    Rect { rect: [f32; 4] },
    Ellipse { rect: [f32; 4] },
    Line { from: [f32; 2], to: [f32; 2] },
    Arrow { from: [f32; 2], to: [f32; 2] },
    Freehand { points: Vec<[f32; 2]> },
    Text { at: [f32; 2], text: String },
    Bubble { rect: [f32; 4], tail: [f32; 2], text: String },
    Step { center: [f32; 2], number: u32 },
    Highlight { rect: [f32; 4] },
    /// `mode` is "pixelate" or "blur".
    Obfuscate { rect: [f32; 4], mode: String },
    /// `picture` is a file name inside the project folder.
    Image { rect: [f32; 4], picture: String },
    Circle { rect: [f32; 4] },
    Group { rect: [f32; 4] },
    Donut { rect: [f32; 4] },
    Cross { rect: [f32; 4] },
    Asterisk { rect: [f32; 4] },
    Cone { from: [f32; 2], to: [f32; 2] },
    Slice { from: [f32; 2], to: [f32; 2] },
    Spotlight { rect: [f32; 4], dim: u8 },
    /// A kind this build does not know; the item is left out when the project is opened.
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedStyle {
    /// Red, green, blue, alpha.
    pub stroke: [u8; 4],
    pub fill: Option<[u8; 4]>,
    pub width: f32,
    pub font_size: f32,
    pub shadow: bool,
}

impl Default for SavedStyle {
    fn default() -> Self {
        save_style(&Style::default())
    }
}

/// The editor state a project is saved from.
pub struct Snapshot<'a> {
    pub rec: &'a Recording,
    pub title: &'a str,
    pub tour_title: &'a str,
    pub steps: u32,
    pub edit: &'a Edit,
    pub events: &'a [InputEvent],
    pub items: &'a [Item],
}

/// A file that belongs beside `project.json`. `name` is its path inside the project folder, with `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// A project turned back into editor state. `dropped` names what could not be brought back.
#[derive(Debug, Clone)]
pub struct Restored {
    pub edit: Edit,
    pub events: Vec<InputEvent>,
    pub items: Vec<Item>,
    pub title: String,
    pub dropped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Not a project file this build can read; the text says why.
    Damaged(String),
    /// Written by a newer Mythicshot, in this format.
    Newer(u32),
}

/// The names of enum values in the file, apart from the editor's own serde names.
macro_rules! names {
    ($to:ident, $from:ident, $ty:ty { $($v:ident => $s:literal),+ $(,)? }) => {
        fn $to(v: $ty) -> String {
            match v { $(<$ty>::$v => $s),+ }.to_string()
        }
        fn $from(s: &str) -> Option<$ty> {
            match s { $($s => Some(<$ty>::$v),)+ _ => None }
        }
    };
}

names!(card_style_name, card_style_from, CardStyle { Dark => "dark", Light => "light", Compact => "compact", Auto => "auto" });
names!(hotspot_name, hotspot_from, HotspotStyle { Pulse => "pulse", Dot => "dot", None => "none" });
names!(anchor_name, anchor_from, Anchor { Hotspot => "hotspot", Point => "point", None => "none" });
names!(placement_name, placement_from, Placement { Auto => "auto", Top => "top", Bottom => "bottom", Left => "left", Right => "right" });
names!(advance_name, advance_from, Advance { Hotspot => "hotspot", Anywhere => "anywhere" });
names!(mode_name, mode_from, ObfuscateMode { Pixelate => "pixelate", Blur => "blur" });
names!(button_name, button_from, MouseButton { Left => "left", Right => "right", Middle => "middle" });

/// The first 16 hex characters of the SHA-256 of `bytes`.
fn hash16(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn is_hash16(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Only `pictures/<16 hex>.png`, so a file edited by hand cannot name a file outside the project.
fn is_picture_name(name: &str) -> bool {
    name.strip_prefix("pictures/").and_then(|r| r.strip_suffix(".png")).is_some_and(is_hash16)
}

/// Only `logo-<16 hex>.<extension>`.
fn is_logo_name(name: &str) -> bool {
    let ext_ok = |e: &str| (1..=8).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    name.strip_prefix("logo-").and_then(|r| r.split_once('.')).is_some_and(|(hash, ext)| is_hash16(hash) && ext_ok(ext))
}

/// A number that JSON can hold: not-a-number and the infinities become 0.
fn fin(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

fn save_pt(p: Pt) -> [f32; 2] {
    [fin(p.x), fin(p.y)]
}

fn load_pt(p: [f32; 2]) -> Pt {
    Pt { x: fin(p[0]), y: fin(p[1]) }
}

fn save_rect(b: Bounds) -> [f32; 4] {
    [fin(b.x0), fin(b.y0), fin(b.x1), fin(b.y1)]
}

fn load_rect(r: [f32; 4]) -> Bounds {
    Bounds { x0: fin(r[0]), y0: fin(r[1]), x1: fin(r[2]), y1: fin(r[3]) }
}

fn save_style(s: &Style) -> SavedStyle {
    SavedStyle { stroke: s.stroke.0, fill: s.fill.map(|f| f.0), width: fin(s.width), font_size: fin(s.font_size), shadow: s.shadow }
}

fn load_style(s: &SavedStyle) -> Style {
    let d = Style::default();
    let width = if s.width.is_finite() { s.width.clamp(0.0, 1000.0) } else { d.width };
    let font_size = if s.font_size.is_finite() { s.font_size.clamp(1.0, 1000.0) } else { d.font_size };
    Style { stroke: Rgba(s.stroke), fill: s.fill.map(Rgba), width, font_size, shadow: s.shadow }
}

fn save_theme(t: &Theme) -> SavedTheme {
    SavedTheme { card_style: card_style_name(t.card_style), accent: t.accent.clone(), hotspot: hotspot_name(t.hotspot), progress_bar: t.progress_bar, backdrop: t.backdrop.clone(), spotlight_soft: t.spotlight_soft }
}

fn save_link(l: &Link) -> SavedLink {
    SavedLink { label: l.label.clone(), url: l.url.clone() }
}

fn load_link(l: SavedLink) -> Link {
    Link { label: l.label, url: l.url }
}

/// A picture as PNG bytes, straight alpha.
fn png(image: &ImageData) -> Option<Vec<u8>> {
    let rgba = to_rgba_image(&image.0);
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(rgba.as_raw(), rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8).ok()?;
    Some(out)
}

/// The pictures and the logo collected while a file is built; each content is kept once.
#[derive(Default)]
struct Blobs {
    list: Vec<Blob>,
    /// Pictures already encoded, by the pixels they share.
    seen: HashMap<*const Pixmap, String>,
}

impl Blobs {
    fn add(&mut self, name: String, bytes: Vec<u8>) -> String {
        if !self.list.iter().any(|b| b.name == name) {
            self.list.push(Blob { name: name.clone(), bytes });
        }
        name
    }

    fn picture(&mut self, image: &ImageData) -> Option<String> {
        let shared = std::sync::Arc::as_ptr(&image.0);
        if let Some(name) = self.seen.get(&shared) {
            return Some(name.clone());
        }
        let bytes = png(image)?;
        let name = self.add(format!("pictures/{}.png", hash16(&bytes)), bytes);
        self.seen.insert(shared, name.clone());
        Some(name)
    }

    /// The logo's source file, copied as it is. A logo that cannot be read is left out.
    fn logo(&mut self, path: &Path) -> Option<String> {
        if std::fs::metadata(path).ok()?.len() > LOGO_MAX_FILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let ext: String = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let ext = if (1..=8).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) { ext } else { "img".to_string() };
        Some(self.add(format!("logo-{}.{ext}", hash16(&bytes)), bytes))
    }
}

/// None for a picture that cannot be encoded; that item is left out of the file.
fn save_shape(shape: &Shape, blobs: &mut Blobs) -> Option<SavedShape> {
    Some(match shape {
        Shape::Rect(b) => SavedShape::Rect { rect: save_rect(*b) },
        Shape::Ellipse(b) => SavedShape::Ellipse { rect: save_rect(*b) },
        Shape::Line(a, b) => SavedShape::Line { from: save_pt(*a), to: save_pt(*b) },
        Shape::Arrow(a, b) => SavedShape::Arrow { from: save_pt(*a), to: save_pt(*b) },
        Shape::Freehand(ps) => SavedShape::Freehand { points: ps.iter().map(|p| save_pt(*p)).collect() },
        Shape::Text { at, text } => SavedShape::Text { at: save_pt(*at), text: text.clone() },
        Shape::Bubble { rect, tail, text } => SavedShape::Bubble { rect: save_rect(*rect), tail: save_pt(*tail), text: text.clone() },
        Shape::Step { center, number } => SavedShape::Step { center: save_pt(*center), number: *number },
        Shape::Highlight(b) => SavedShape::Highlight { rect: save_rect(*b) },
        Shape::Obfuscate { rect, mode } => SavedShape::Obfuscate { rect: save_rect(*rect), mode: mode_name(*mode) },
        Shape::Image { rect, image } => SavedShape::Image { rect: save_rect(*rect), picture: blobs.picture(image)? },
        Shape::Circle(b) => SavedShape::Circle { rect: save_rect(*b) },
        Shape::Group(b) => SavedShape::Group { rect: save_rect(*b) },
        Shape::Donut(b) => SavedShape::Donut { rect: save_rect(*b) },
        Shape::Cross(b) => SavedShape::Cross { rect: save_rect(*b) },
        Shape::Asterisk(b) => SavedShape::Asterisk { rect: save_rect(*b) },
        Shape::Cone(a, b) => SavedShape::Cone { from: save_pt(*a), to: save_pt(*b) },
        Shape::Slice(a, b) => SavedShape::Slice { from: save_pt(*a), to: save_pt(*b) },
        Shape::Spotlight { rect, dim } => SavedShape::Spotlight { rect: save_rect(*rect), dim: *dim },
    })
}

fn save_doc(doc: &TourDoc, blobs: &mut Blobs) -> SavedTourDoc {
    SavedTourDoc {
        theme: save_theme(&doc.theme),
        zoom: SavedZoom { enabled: doc.zoom.enabled, percent: doc.zoom.percent },
        cover: doc.cover.as_ref().map(|c| SavedCover { title: c.title.clone(), body: c.body.clone(), button: c.button.clone() }),
        logo: doc.logo_path.as_deref().and_then(|p| blobs.logo(p)),
        cta: doc.cta.as_ref().map(save_link),
        end: doc.end.as_ref().map(|e| SavedEnd { text: e.text.clone(), primary: e.primary.as_ref().map(save_link), secondary: e.secondary.as_ref().map(save_link), replay: e.replay }),
        autoplay: SavedAutoplay { enabled: doc.autoplay.enabled, seconds: doc.autoplay.seconds },
        steps: doc.steps.iter().map(|(src, s)| (*src, save_step(s))).collect(),
    }
}

fn save_step(s: &StepExtras) -> SavedStep {
    let card = s.card.as_ref().map(|c| SavedCard {
        title: c.title.clone(),
        body: c.body.clone(),
        next_label: c.next_label.clone(),
        anchor: anchor_name(c.anchor),
        point: c.point.as_ref().map(|p| [fin(p.x), fin(p.y)]),
        placement: placement_name(c.placement),
    });
    SavedStep { card, advance: advance_name(s.advance), zoom: s.zoom }
}

/// Mouse events only: a key press never reaches a project.
fn save_event(e: &InputEvent) -> Option<SavedEvent> {
    let (kind, button, x, y) = match &e.kind {
        InputKind::MouseDown(b, x, y) => ("down", *b, *x, *y),
        InputKind::MouseUp(b, x, y) => ("up", *b, *x, *y),
        InputKind::KeyDown(_) | InputKind::KeyUp(_) => return None,
    };
    Some(SavedEvent { t_ms: e.t_ms, kind: kind.to_string(), button: button_name(button), x, y })
}

fn load_event(e: &SavedEvent) -> Option<InputEvent> {
    let button = button_from(&e.button)?;
    let kind = match e.kind.as_str() {
        "down" => InputKind::MouseDown(button, e.x, e.y),
        "up" => InputKind::MouseUp(button, e.x, e.y),
        _ => return None,
    };
    Some(InputEvent { t_ms: e.t_ms, kind })
}

/// A picture file of the project, or None when it is missing, too large or not a PNG.
fn load_picture(path: &Path) -> Option<ImageData> {
    if std::fs::metadata(path).ok()?.len() > PICTURE_MAX_FILE_BYTES {
        return None;
    }
    let raw = std::fs::read(path).ok()?;
    let decoder = PngDecoder::new(std::io::Cursor::new(&raw[..])).ok()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || w as u64 * h as u64 > PICTURE_MAX_PIXELS {
        return None;
    }
    let img = DynamicImage::from_decoder(decoder).ok()?.to_rgba8();
    Some(ImageData::from_rgba(&img))
}

/// What is read while a file is turned back into editor state.
struct Loader<'a> {
    dir: &'a Path,
    /// Frames in the recording.
    frames: usize,
    /// Pictures already read, so copies of one picture share its pixels again.
    pictures: HashMap<String, Option<ImageData>>,
    dropped: Vec<String>,
}

impl Loader<'_> {
    fn picture(&mut self, name: &str) -> Option<ImageData> {
        if !is_picture_name(name) {
            return None;
        }
        if let Some(known) = self.pictures.get(name) {
            return known.clone();
        }
        let loaded = load_picture(&self.dir.join(name));
        self.pictures.insert(name.to_string(), loaded.clone());
        loaded
    }

    fn shape(&mut self, shape: SavedShape) -> Option<Shape> {
        Some(match shape {
            SavedShape::Rect { rect } => Shape::Rect(load_rect(rect)),
            SavedShape::Ellipse { rect } => Shape::Ellipse(load_rect(rect)),
            SavedShape::Line { from, to } => Shape::Line(load_pt(from), load_pt(to)),
            SavedShape::Arrow { from, to } => Shape::Arrow(load_pt(from), load_pt(to)),
            SavedShape::Freehand { points } => Shape::Freehand(points.into_iter().map(load_pt).collect()),
            SavedShape::Text { at, text } => Shape::Text { at: load_pt(at), text },
            SavedShape::Bubble { rect, tail, text } => Shape::Bubble { rect: load_rect(rect), tail: load_pt(tail), text },
            SavedShape::Step { center, number } => Shape::Step { center: load_pt(center), number },
            SavedShape::Highlight { rect } => Shape::Highlight(load_rect(rect)),
            SavedShape::Obfuscate { rect, mode } => Shape::Obfuscate { rect: load_rect(rect), mode: mode_from(&mode).unwrap_or(ObfuscateMode::Pixelate) },
            SavedShape::Image { rect, picture } => match self.picture(&picture) {
                Some(image) => Shape::Image { rect: load_rect(rect), image },
                None => {
                    self.dropped.push("a pasted picture".to_string());
                    return None;
                }
            },
            SavedShape::Circle { rect } => Shape::Circle(load_rect(rect)),
            SavedShape::Group { rect } => Shape::Group(load_rect(rect)),
            SavedShape::Donut { rect } => Shape::Donut(load_rect(rect)),
            SavedShape::Cross { rect } => Shape::Cross(load_rect(rect)),
            SavedShape::Asterisk { rect } => Shape::Asterisk(load_rect(rect)),
            SavedShape::Cone { from, to } => Shape::Cone(load_pt(from), load_pt(to)),
            SavedShape::Slice { from, to } => Shape::Slice(load_pt(from), load_pt(to)),
            SavedShape::Spotlight { rect, dim } => Shape::Spotlight { rect: load_rect(rect), dim: dim.clamp(MIN_DIM, MAX_DIM) },
            SavedShape::Unknown => {
                self.dropped.push("an annotation of a kind this version does not know".to_string());
                return None;
            }
        })
    }

    /// An item whose frames or step lie wholly beyond the recording is left out; a range that
    /// ends beyond it is cut at the last frame.
    fn item(&mut self, item: SavedItem, id: u64) -> Option<Item> {
        let last = u32::try_from(self.frames.saturating_sub(1)).unwrap_or(u32::MAX);
        let outside = self.frames == 0 || item.pin.is_some_and(|p| p > last) || item.frames.is_some_and(|(a, b)| a.min(b) > last);
        if outside && (item.pin.is_some() || item.frames.is_some()) {
            self.dropped.push("an annotation on a frame the recording does not have".to_string());
            return None;
        }
        let frames = item.frames.map(|(a, b)| (a.min(b), a.max(b).min(last)));
        let style = load_style(&item.style);
        Some(Item { id, shape: self.shape(item.shape)?, style, frames, pin: item.pin })
    }

    fn doc(&mut self, doc: SavedTourDoc) -> TourDoc {
        let logo_path = doc.logo.and_then(|name| {
            let path = self.dir.join(&name);
            if is_logo_name(&name) && path.is_file() {
                Some(path)
            } else {
                self.dropped.push("the cover logo".to_string());
                None
            }
        });
        let frames = self.frames;
        TourDoc {
            theme: Theme {
                card_style: card_style_from(&doc.theme.card_style).unwrap_or_default(),
                accent: doc.theme.accent,
                hotspot: hotspot_from(&doc.theme.hotspot).unwrap_or_default(),
                progress_bar: doc.theme.progress_bar,
                backdrop: doc.theme.backdrop,
                spotlight_soft: doc.theme.spotlight_soft,
            },
            zoom: Zoom { enabled: doc.zoom.enabled, percent: doc.zoom.percent },
            cover: doc.cover.map(|c| Cover { title: c.title, body: c.body, button: c.button, logo: None }),
            logo_path,
            cta: doc.cta.map(load_link),
            end: doc.end.map(|e| EndCard { text: e.text, primary: e.primary.map(load_link), secondary: e.secondary.map(load_link), replay: e.replay }),
            autoplay: Autoplay { enabled: doc.autoplay.enabled, seconds: doc.autoplay.seconds },
            steps: doc.steps.into_iter().filter(|(src, _)| *src < frames).map(|(src, s)| (src, load_step(s))).collect(),
        }
    }

    fn edit(&mut self, e: SavedEdit) -> Edit {
        let (frames, last) = (self.frames, self.frames.saturating_sub(1));
        let trim = match e.trim {
            Some((a, b)) if a <= b && a <= last => (a, b.min(last)),
            _ => (0, last),
        };
        Edit {
            trim,
            deleted: e.deleted.into_iter().filter(|i| *i < frames).collect(),
            speed: if e.speed.is_finite() { e.speed.clamp(0.1, 10.0) } else { 1.0 },
            reverse: e.reverse,
            crop: e.crop.filter(|c| c.iter().all(|v| v.is_finite())).map(load_rect),
            scale: e.scale.clamp(10, 100),
            overlays: e.overlays,
            tour_removed: e.tour_removed.into_iter().filter(|i| *i < frames).collect(),
            tour_doc: self.doc(e.tour_doc),
        }
    }
}

fn load_step(s: SavedStep) -> StepExtras {
    let card = s.card.map(|c| Card {
        title: c.title,
        body: c.body,
        next_label: c.next_label,
        anchor: anchor_from(&c.anchor).unwrap_or_default(),
        point: c.point.map(|p| Point { x: fin(p[0]), y: fin(p[1]) }),
        placement: placement_from(&c.placement).unwrap_or_default(),
    });
    StepExtras { card, advance: advance_from(&s.advance).unwrap_or_default(), zoom: s.zoom }
}

/// The file for `snap` and the files that go beside it: the pasted pictures as PNG and the cover
/// logo's source file. An item whose picture cannot be encoded and a logo that cannot be read are
/// left out.
pub fn to_file(key: &ProjectKey, snap: &Snapshot<'_>, now_ms: i64, frames_bytes: u64) -> (ProjectFile, Vec<Blob>) {
    let mut blobs = Blobs::default();
    let e = snap.edit;
    let edit = SavedEdit {
        trim: Some(e.trim),
        deleted: e.deleted.clone(),
        speed: fin(e.speed),
        reverse: e.reverse,
        crop: e.crop.map(save_rect),
        scale: e.scale,
        overlays: e.overlays,
        tour_removed: e.tour_removed.clone(),
        tour_doc: save_doc(&e.tour_doc, &mut blobs),
    };
    let items = snap.items.iter().filter_map(|it| Some(SavedItem { shape: save_shape(&it.shape, &mut blobs)?, style: save_style(&it.style), frames: it.frames, pin: it.pin })).collect();
    let file = ProjectFile {
        format: FORMAT,
        app: env!("CARGO_PKG_VERSION").to_string(),
        saved_at: now_ms,
        server: key.server.clone(),
        tour_id: key.tour_id.clone(),
        chapter_id: key.chapter_id.clone(),
        tour_title: snap.tour_title.to_string(),
        title: snap.title.to_string(),
        steps: snap.steps,
        frames_bytes,
        edit,
        events: snap.events.iter().filter_map(save_event).collect(),
        items,
    };
    (file, blobs.list)
}

/// The text of `project.json`: the form `golden-v1.json` pins.
pub fn to_text(file: &ProjectFile) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

/// Editor state from a parsed file whose pictures and logo are in `dir`. Frame numbers beyond
/// `rec` are cut or left out, a missing picture or logo costs only itself and is named in
/// `dropped`, and no input panics.
pub fn from_file(file: ProjectFile, dir: &Path, rec: &Recording) -> Restored {
    let mut loader = Loader { dir, frames: rec.frames.len(), pictures: HashMap::new(), dropped: Vec::new() };
    let edit = loader.edit(file.edit);
    let mut items = Vec::new();
    for saved in file.items {
        // Ids count up from 1 in the saved order.
        if let Some(item) = loader.item(saved, items.len() as u64 + 1) {
            items.push(item);
        }
    }
    Restored { edit, events: file.events.iter().filter_map(load_event).collect(), items, title: file.title, dropped: loader.dropped }
}

/// Reads `project.json`. The format number is looked at first, so a file from a newer Mythicshot
/// is reported as newer whatever else it holds.
pub fn parse(text: &str) -> Result<ProjectFile, ParseError> {
    let value: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| ParseError::Damaged(e.to_string()))?;
    let format = value.get("format").and_then(|f| f.as_u64()).ok_or_else(|| ParseError::Damaged("the file has no format number".to_string()))?;
    if format > u64::from(FORMAT) {
        return Err(ParseError::Newer(u32::try_from(format).unwrap_or(u32::MAX)));
    }
    if format == 0 {
        return Err(ParseError::Damaged("the format number is 0".to_string()));
    }
    serde_json::from_value(value).map_err(|e| ParseError::Damaged(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;
    use mythicshot_editor::overlays::{ClickStyle, KeyStyle};
    use mythicshot_editor::pt;

    const GOLDEN: &str = include_str!("golden-v1.json");
    /// The picture's name in the golden file. A real name is the hash of the PNG bytes, which a
    /// new version of the PNG encoder may change; the format does not depend on it.
    const GOLDEN_PICTURE: &str = "pictures/00112233aabbccdd.png";
    /// The golden file's `app`: the real one moves with every release.
    const GOLDEN_APP: &str = "0.2.5";
    const LOGO: &[u8] = b"\x89PNG\r\n\x1a\nnot really a logo";
    const SAVED_AT: i64 = 1_763_460_000_000;
    const FRAMES_BYTES: u64 = 123_456;

    fn key() -> ProjectKey {
        ProjectKey { server: "https://tours.example.com".into(), tour_id: "tour_0123456789abcdef01234567".into(), chapter_id: "ch_0123456789abcdef01234567".into() }
    }

    /// Ten frames, 100 ms apart.
    fn rec() -> Recording {
        Recording::timing_only(100, 80, &[0, 100, 200, 300, 400, 500, 600, 700, 800, 900], 1000)
    }

    fn picture() -> ImageData {
        let mut img = RgbaImage::from_pixel(2, 2, image::Rgba([200, 30, 40, 255]));
        img.put_pixel(1, 1, image::Rgba([0, 0, 255, 255]));
        ImageData::from_rgba(&img)
    }

    fn b(x0: f32, y0: f32, x1: f32, y1: f32) -> Bounds {
        Bounds { x0, y0, x1, y1 }
    }

    /// One of every kind of shape; some on a frame range, one pinned to a step, one restyled.
    fn items() -> Vec<Item> {
        let shapes = vec![
            Shape::Rect(b(1.0, 2.0, 30.0, 40.0)),
            Shape::Ellipse(b(5.5, 6.25, 50.0, 60.0)),
            Shape::Line(pt(0.0, 0.0), pt(10.0, 20.0)),
            Shape::Arrow(pt(90.0, 70.0), pt(10.0, 20.0)),
            Shape::Freehand(vec![pt(1.0, 1.0), pt(2.5, 3.5), pt(4.0, 2.0)]),
            Shape::Text { at: pt(12.0, 14.0), text: "Click \"Save\"\nthen wait".into() },
            Shape::Bubble { rect: b(20.0, 20.0, 70.0, 50.0), tail: pt(80.0, 75.0), text: "Hello".into() },
            Shape::Step { center: pt(33.0, 44.0), number: 7 },
            Shape::Highlight(b(0.0, 10.0, 100.0, 20.0)),
            Shape::Obfuscate { rect: b(60.0, 5.0, 95.0, 15.0), mode: ObfuscateMode::Blur },
            Shape::Image { rect: b(10.0, 10.0, 40.0, 40.0), image: picture() },
            Shape::Circle(b(1.0, 1.0, 9.0, 9.0)),
            Shape::Group(b(2.0, 2.0, 8.0, 8.0)),
            Shape::Donut(b(3.0, 3.0, 7.0, 7.0)),
            Shape::Cross(b(4.0, 4.0, 6.0, 6.0)),
            Shape::Asterisk(b(5.0, 5.0, 15.0, 15.0)),
            Shape::Cone(pt(50.0, 40.0), pt(60.0, 10.0)),
            Shape::Slice(pt(50.0, 40.0), pt(20.0, 70.0)),
            Shape::Spotlight { rect: b(25.0, 20.0, 75.0, 60.0), dim: 45 },
        ];
        let mut items: Vec<Item> = shapes.into_iter().enumerate().map(|(i, shape)| Item { id: i as u64 + 1, shape, style: Style::default(), frames: None, pin: None }).collect();
        items[0].frames = Some((2, 6));
        items[1].style = Style { stroke: Rgba::YELLOW, fill: Some(Rgba([10, 20, 30, 128])), width: 5.5, font_size: 28.0, shadow: false };
        items[5].pin = Some(5);
        items[10].frames = Some((0, 9));
        items
    }

    fn events() -> Vec<InputEvent> {
        vec![
            InputEvent { t_ms: 150, kind: InputKind::MouseDown(MouseButton::Left, 50, 40) },
            InputEvent { t_ms: 160, kind: InputKind::KeyDown("Hunter2Key".into()) },
            InputEvent { t_ms: 210, kind: InputKind::MouseUp(MouseButton::Left, 50, 40) },
            InputEvent { t_ms: 230, kind: InputKind::KeyUp("Hunter2Key".into()) },
            InputEvent { t_ms: 480, kind: InputKind::MouseDown(MouseButton::Right, 75, -3) },
            InputEvent { t_ms: 520, kind: InputKind::MouseUp(MouseButton::Middle, 75, 41) },
        ]
    }

    fn mouse_only(events: &[InputEvent]) -> Vec<InputEvent> {
        events.iter().filter(|e| matches!(e.kind, InputKind::MouseDown(..) | InputKind::MouseUp(..))).cloned().collect()
    }

    /// Every field of an `Edit` away from its default, with cards on two steps.
    fn edit(rec: &Recording, logo: Option<&Path>) -> Edit {
        let mut e = Edit::new(rec, OverlayStyle { keys: KeyStyle::Chips, clicks: ClickStyle::Both, halo: true, pointer: false });
        e.trim = (1, 8);
        e.deleted = BTreeSet::from([3, 4]);
        e.speed = 1.5;
        e.reverse = true;
        e.crop = Some(b(10.0, 5.0, 90.0, 70.0));
        e.scale = 75;
        e.tour_removed = BTreeSet::from([6]);
        let link = |label: &str, url: &str| Some(Link { label: label.into(), url: url.into() });
        e.tour_doc = TourDoc {
            theme: Theme { card_style: CardStyle::Light, accent: "#ff8800".into(), hotspot: HotspotStyle::Dot, progress_bar: false, backdrop: "#101418".into(), spotlight_soft: false },
            zoom: Zoom { enabled: false, percent: 125 },
            cover: Some(Cover { title: "A quick tour".into(), body: "Two minutes,\na few clicks.".into(), button: "Begin".into(), logo: None }),
            logo_path: logo.map(Path::to_path_buf),
            cta: link("Book a demo", "https://tours.example.com/book"),
            end: Some(EndCard { text: "That is all.".into(), primary: link("Sign up", "https://example.com/signup"), secondary: link("Read more", "https://example.com/docs"), replay: false }),
            autoplay: Autoplay { enabled: true, seconds: 8 },
            steps: BTreeMap::from([
                (2, StepExtras { card: Some(Card { title: "Open the menu".into(), body: "It is **here**.".into(), next_label: "Go on".into(), anchor: Anchor::Point, point: Some(Point { x: 40.0, y: 30.5 }), placement: Placement::Top }), advance: Advance::Anywhere, zoom: Some(false) }),
                (5, StepExtras { card: Some(Card { title: "Save".into(), body: String::new(), next_label: String::new(), anchor: Anchor::Hotspot, point: None, placement: Placement::Auto }), advance: Advance::Hotspot, zoom: None }),
            ]),
        };
        e
    }

    /// A folder holding the logo's source file, and its path.
    fn logo_source() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Company Logo.PNG");
        std::fs::write(&path, LOGO).unwrap();
        (dir, path)
    }

    fn logo_name() -> String {
        format!("logo-{}.png", hash16(LOGO))
    }

    fn build(rec: &Recording, edit: &Edit, events: &[InputEvent], items: &[Item]) -> (ProjectFile, Vec<Blob>) {
        let snap = Snapshot { rec, title: "Chapter one", tour_title: "Customer onboarding", steps: 3, edit, events, items };
        to_file(&key(), &snap, SAVED_AT, FRAMES_BYTES)
    }

    fn write_blobs(dir: &Path, blobs: &[Blob]) {
        for blob in blobs {
            let path = dir.join(&blob.name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &blob.bytes).unwrap();
        }
    }

    fn pixels(image: &ImageData) -> (u32, u32, Vec<u8>) {
        (image.width(), image.height(), image.0.data().to_vec())
    }

    /// Equal items; two pictures are equal when their pixels are.
    fn assert_same_items(got: &[Item], want: &[Item]) {
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            match (&g.shape, &w.shape) {
                (Shape::Image { rect: gr, image: gi }, Shape::Image { rect: wr, image: wi }) => {
                    assert_eq!(gr, wr);
                    assert_eq!(pixels(gi), pixels(wi));
                    assert_eq!((g.id, g.style, g.frames, g.pin), (w.id, w.style, w.frames, w.pin));
                }
                _ => assert_eq!(g, w),
            }
        }
    }

    fn assert_same_edit(got: &Edit, want: &Edit) {
        assert_eq!(got.trim, want.trim);
        assert_eq!(got.deleted, want.deleted);
        assert_eq!(got.speed, want.speed);
        assert_eq!(got.reverse, want.reverse);
        assert_eq!(got.crop, want.crop);
        assert_eq!(got.scale, want.scale);
        assert_eq!(got.overlays, want.overlays);
        assert_eq!(got.tour_removed, want.tour_removed);
        assert_eq!(got.tour_doc, want.tour_doc);
    }

    #[test]
    fn every_kind_of_item_and_every_edit_field_survives_a_round_trip() {
        let rec = rec();
        let (_keep, logo) = logo_source();
        let (edit, events, items) = (edit(&rec, Some(&logo)), events(), items());
        let (file, blobs) = build(&rec, &edit, &events, &items);
        assert_eq!((file.format, file.app.as_str(), file.saved_at, file.frames_bytes, file.steps), (FORMAT, env!("CARGO_PKG_VERSION"), SAVED_AT, FRAMES_BYTES, 3));
        assert_eq!((file.server.as_str(), file.tour_id.as_str(), file.chapter_id.as_str()), ("https://tours.example.com", "tour_0123456789abcdef01234567", "ch_0123456789abcdef01234567"));
        assert_eq!((file.tour_title.as_str(), file.title.as_str()), ("Customer onboarding", "Chapter one"));
        // One picture and the logo, each named by its content.
        let mut names: Vec<&str> = blobs.iter().map(|b| b.name.as_str()).collect();
        names.sort();
        let picture = blobs.iter().find(|b| b.name.starts_with("pictures/")).unwrap();
        assert_eq!(names, vec![logo_name().as_str(), format!("pictures/{}.png", hash16(&picture.bytes)).as_str()]);
        assert_eq!(blobs.iter().find(|b| b.name == logo_name()).unwrap().bytes, LOGO);

        let project = tempfile::tempdir().unwrap();
        write_blobs(project.path(), &blobs);
        let parsed = parse(&to_text(&file).unwrap()).unwrap();
        assert_eq!(parsed, file);
        let back = from_file(parsed, project.path(), &rec);

        assert_eq!(back.dropped, Vec::<String>::new());
        assert_eq!(back.title, "Chapter one");
        assert_same_items(&back.items, &items);
        // The logo now lives in the project folder; everything else is as it was.
        let mut want = edit.clone();
        want.tour_doc.logo_path = Some(project.path().join(logo_name()));
        assert_same_edit(&back.edit, &want);
        assert_eq!(std::fs::read(back.edit.tour_doc.logo_path.as_ref().unwrap()).unwrap(), LOGO);

        // Saving what was loaded gives the same file and the same pictures again.
        let (again, blobs_again) = build(&rec, &back.edit, &back.events, &back.items);
        assert_eq!(again, file);
        assert_eq!(blobs_again.len(), 2);
        assert!(blobs_again.iter().all(|b| blobs.contains(b)));
    }

    #[test]
    fn two_items_with_the_same_picture_share_one_file() {
        let rec = rec();
        let image = picture();
        let item = |id, image: ImageData| Item { id, shape: Shape::Image { rect: b(0.0, 0.0, 2.0, 2.0), image }, style: Style::default(), frames: None, pin: None };
        // A copy sharing the pixels, and a second picture with the same pixels.
        let items = vec![item(1, image.clone()), item(2, image), item(3, picture())];
        let (file, blobs) = build(&rec, &Edit::new(&rec, OverlayStyle::default()), &[], &items);
        assert_eq!(blobs.len(), 1);
        assert_eq!(file.items.len(), 3);
        assert!(file.items.iter().all(|i| matches!(&i.shape, SavedShape::Image { picture, .. } if *picture == blobs[0].name)));
    }

    /// The golden state as a file, with the two values that are not part of the format pinned.
    fn golden_text() -> String {
        let rec = rec();
        let (_keep, logo) = logo_source();
        let (mut file, blobs) = build(&rec, &edit(&rec, Some(&logo)), &events(), &items());
        file.app = GOLDEN_APP.to_string();
        let picture = blobs.iter().find(|b| b.name.starts_with("pictures/")).unwrap();
        to_text(&file).unwrap().replace(&picture.name, GOLDEN_PICTURE)
    }

    #[test]
    fn the_saved_file_matches_the_golden_file() {
        assert_eq!(golden_text(), GOLDEN.replace("\r\n", "\n"));
    }

    #[test]
    fn the_golden_file_of_format_1_still_loads() {
        let rec = rec();
        let project = tempfile::tempdir().unwrap();
        write_blobs(project.path(), &[Blob { name: GOLDEN_PICTURE.into(), bytes: png(&picture()).unwrap() }, Blob { name: logo_name(), bytes: LOGO.to_vec() }]);
        let file = parse(GOLDEN).unwrap();
        assert_eq!((file.format, file.app.as_str(), file.saved_at, file.frames_bytes, file.steps), (1, GOLDEN_APP, SAVED_AT, FRAMES_BYTES, 3));
        assert_eq!(ProjectKey { server: file.server.clone(), tour_id: file.tour_id.clone(), chapter_id: file.chapter_id.clone() }, key());
        assert_eq!(file.tour_title, "Customer onboarding");
        let back = from_file(file, project.path(), &rec);
        assert_eq!(back.dropped, Vec::<String>::new());
        assert_eq!(back.title, "Chapter one");
        assert_same_items(&back.items, &items());
        assert_eq!(back.events, mouse_only(&events()));
        assert_same_edit(&back.edit, &edit(&rec, Some(&project.path().join(logo_name()))));
    }

    #[test]
    fn a_file_with_a_higher_format_is_reported_as_newer_and_not_as_damaged() {
        // Everything but the format number is of a shape this build has never seen.
        assert_eq!(parse(r#"{"format": 2, "edit": "elsewhere", "items": {"a": 1}, "events": 7}"#), Err(ParseError::Newer(2)));
        assert_eq!(parse(r#"{"items": [1, 2], "format": 4000000000}"#), Err(ParseError::Newer(4_000_000_000)));
        assert_eq!(parse(r#"{"format": 99999999999999}"#), Err(ParseError::Newer(u32::MAX)));
        let newer = GOLDEN.replacen("\"format\": 1", "\"format\": 2", 1);
        assert_ne!(newer, GOLDEN);
        assert_eq!(parse(&newer), Err(ParseError::Newer(2)));
    }

    #[test]
    fn text_that_is_cut_off_or_not_json_is_damaged() {
        let damaged = |text: &str| matches!(parse(text), Err(ParseError::Damaged(why)) if !why.is_empty());
        for text in ["", " ", "project", "{", "[]", "null", "7", "\"format\"", "{}", r#"{"format": "1"}"#, r#"{"format": -1}"#, r#"{"format": 0}"#, r#"{"format": 1.5}"#, r#"{"format": null}"#] {
            assert!(damaged(text), "{text:?}");
        }
        // The right format with a field of the wrong shape.
        assert!(damaged(r#"{"format": 1, "items": 5}"#));
        assert!(damaged(r#"{"format": 1, "edit": {"deleted": [-1]}}"#));
        assert!(damaged(r#"{"format": 1, "items": [{"shape": {"kind": "rect"}}]}"#));
        // Cut off anywhere, the golden file is damaged, never newer and never a panic.
        let golden = GOLDEN.replace("\r\n", "\n");
        let whole = golden.trim_end();
        for cut in (0..whole.len()).filter(|i| whole.is_char_boundary(*i)) {
            assert!(damaged(&whole[..cut]), "cut at {cut}");
        }
        assert!(parse(whole).is_ok());
        // A byte order mark in front, as some editors leave one, is not damage.
        assert!(parse(&format!("\u{feff}{whole}")).is_ok());
    }

    #[test]
    fn a_file_with_missing_fields_loads_with_defaults() {
        let rec = rec();
        let project = tempfile::tempdir().unwrap();
        let file = parse(r#"{"format": 1}"#).unwrap();
        assert_eq!((file.app.as_str(), file.saved_at, file.steps, file.frames_bytes), ("", 0, 0, 0));
        let back = from_file(file, project.path(), &rec);
        assert_same_edit(&back.edit, &Edit::new(&rec, OverlayStyle::default()));
        assert_eq!((back.items.len(), back.events.len(), back.title.as_str(), back.dropped.len()), (0, 0, "", 0));

        // Parts with fields missing take the editor's own defaults.
        let text = r##"{"format": 1, "title": "Half", "unknown_to_this_build": [1, 2],
            "edit": {"speed": 2.0, "tour_doc": {"cover": {"title": "Hi"}, "end": {}, "theme": {"accent": "#123456"}, "steps": {"3": {"card": {"title": "T"}}}}},
            "events": [{"t_ms": 5, "kind": "down", "button": "left"}],
            "items": [{"shape": {"kind": "rect", "rect": [1, 2, 3, 4]}}, {"shape": {"kind": "step", "center": [5, 6], "number": 2}, "pin": 3}]}"##;
        let back = from_file(parse(text).unwrap(), project.path(), &rec);
        assert_eq!(back.title, "Half");
        assert_eq!(back.edit.speed, 2.0);
        assert_eq!((back.edit.trim, back.edit.scale, back.edit.crop), ((0, 9), 100, None));
        let doc = &back.edit.tour_doc;
        assert_eq!(doc.theme, Theme { accent: "#123456".into(), ..Theme::default() });
        assert_eq!((doc.zoom.clone(), doc.autoplay.clone()), (Zoom::default(), Autoplay::default()));
        assert_eq!(doc.cover, Some(Cover { title: "Hi".into(), body: String::new(), button: String::new(), logo: None }));
        assert_eq!(doc.end, Some(EndCard { text: String::new(), primary: None, secondary: None, replay: true }));
        assert_eq!(doc.steps[&3], StepExtras { card: Some(Card { title: "T".into(), body: String::new(), next_label: String::new(), anchor: Anchor::default(), point: None, placement: Placement::default() }), advance: Advance::default(), zoom: None });
        assert_eq!(back.events, vec![InputEvent { t_ms: 5, kind: InputKind::MouseDown(MouseButton::Left, 0, 0) }]);
        assert_eq!(back.items, vec![
            Item { id: 1, shape: Shape::Rect(b(1.0, 2.0, 3.0, 4.0)), style: Style::default(), frames: None, pin: None },
            Item { id: 2, shape: Shape::Step { center: pt(5.0, 6.0), number: 2 }, style: Style::default(), frames: None, pin: Some(3) },
        ]);
        assert_eq!(back.dropped, Vec::<String>::new());
    }

    #[test]
    fn frames_and_pins_beyond_the_recording_are_made_safe() {
        // Saved from ten frames, opened on a recording of five.
        let short = Recording::timing_only(100, 80, &[0, 100, 200, 300, 400], 500);
        let project = tempfile::tempdir().unwrap();
        let text = r#"{"format": 1,
            "edit": {"trim": [2, 99], "deleted": [1, 5, 50], "tour_removed": [3, 70], "speed": 1e30, "scale": 4000, "crop": [0, 0, 1e39, 5],
                "tour_doc": {"steps": {"4": {"advance": "anywhere"}, "5": {"advance": "anywhere"}, "18446744073709551615": {}}}},
            "items": [
                {"shape": {"kind": "rect", "rect": [1, 2, 3, 4]}, "frames": [1, 40]},
                {"shape": {"kind": "rect", "rect": [1, 2, 3, 4]}, "frames": [10, 20]},
                {"shape": {"kind": "rect", "rect": [1, 2, 3, 4]}, "frames": [4, 3]},
                {"shape": {"kind": "rect", "rect": [1, 2, 3, 4]}, "pin": 4},
                {"shape": {"kind": "rect", "rect": [1, 2, 3, 4]}, "pin": 5},
                {"shape": {"kind": "rect", "rect": [1e39, 2, 3, 4]}, "style": {"width": 1e39, "font_size": -5}},
                {"shape": {"kind": "spotlight", "rect": [1, 2, 3, 4], "dim": 255}}
            ]}"#;
        let back = from_file(parse(text).unwrap(), project.path(), &short);
        assert_eq!(back.edit.trim, (2, 4));
        assert_eq!(back.edit.deleted, BTreeSet::from([1]));
        assert_eq!(back.edit.tour_removed, BTreeSet::from([3]));
        assert_eq!((back.edit.speed, back.edit.scale, back.edit.crop), (10.0, 100, None));
        assert_eq!(back.edit.tour_doc.steps.keys().copied().collect::<Vec<_>>(), vec![4]);
        let kept: Vec<(Option<(u32, u32)>, Option<u32>)> = back.items.iter().map(|i| (i.frames, i.pin)).collect();
        assert_eq!(kept, vec![(Some((1, 4)), None), (Some((3, 4)), None), (None, Some(4)), (None, None), (None, None)]);
        assert_eq!(back.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
        assert_eq!(back.items[3].shape, Shape::Rect(b(0.0, 2.0, 3.0, 4.0)));
        assert_eq!((back.items[3].style.width, back.items[3].style.font_size), (Style::default().width, 1.0));
        assert_eq!(back.items[4].shape, Shape::Spotlight { rect: b(1.0, 2.0, 3.0, 4.0), dim: MAX_DIM });
        assert_eq!(back.dropped.len(), 2);

        // A trim that lies wholly beyond the recording, or is back to front, is the whole recording.
        for trim in ["[7, 9]", "[3, 1]"] {
            let back = from_file(parse(&format!(r#"{{"format": 1, "edit": {{"trim": {trim}}}}}"#)).unwrap(), project.path(), &short);
            assert_eq!(back.edit.trim, (0, 4));
        }

        // A recording without frames: nothing panics and nothing points at a frame.
        let empty = Recording::timing_only(100, 80, &[], 0);
        let back = from_file(parse(text).unwrap(), project.path(), &empty);
        assert_eq!(back.edit.trim, (0, 0));
        assert!(back.edit.deleted.is_empty() && back.edit.tour_removed.is_empty() && back.edit.tour_doc.steps.is_empty());
        assert!(back.items.iter().all(|i| i.frames.is_none() && i.pin.is_none()));
        assert_eq!(back.items.len(), 2);
        let back = from_file(parse(GOLDEN).unwrap(), project.path(), &empty);
        assert!(back.items.iter().all(|i| i.frames.is_none() && i.pin.is_none()));
    }

    #[test]
    fn a_missing_picture_or_logo_costs_only_that_item() {
        let rec = rec();
        let (_keep, logo) = logo_source();
        let (edit, events, items) = (edit(&rec, Some(&logo)), events(), items());
        let (file, _blobs) = build(&rec, &edit, &events, &items);
        // The project folder holds neither the picture nor the logo.
        let project = tempfile::tempdir().unwrap();
        let back = from_file(file.clone(), project.path(), &rec);
        assert_eq!(back.dropped, vec!["the cover logo".to_string(), "a pasted picture".to_string()]);
        let without: Vec<Item> = items.iter().filter(|i| !matches!(i.shape, Shape::Image { .. })).cloned().collect();
        assert_eq!(back.items.iter().map(|i| (&i.shape, i.style, i.frames, i.pin)).collect::<Vec<_>>(), without.iter().map(|i| (&i.shape, i.style, i.frames, i.pin)).collect::<Vec<_>>());
        let mut want = edit.clone();
        want.tour_doc.logo_path = None;
        assert_same_edit(&back.edit, &want);
        assert_eq!(back.events, mouse_only(&events));

        // A picture file that is not a picture, and a kind of shape this build does not know.
        let picture = file.items.iter().find_map(|i| match &i.shape { SavedShape::Image { picture, .. } => Some(picture.clone()), _ => None }).unwrap();
        write_blobs(project.path(), &[Blob { name: picture, bytes: b"not a png".to_vec() }]);
        let mut odd = file.clone();
        odd.edit.tour_doc.logo = None;
        odd.items.push(SavedItem::default());
        let back = from_file(odd, project.path(), &rec);
        assert_eq!(back.items.len(), without.len());
        assert_eq!(back.dropped.len(), 2);
        let text = r#"{"format": 1, "items": [{"shape": {"kind": "hologram", "depth": 3}}, {"shape": {"kind": "circle", "rect": [0, 0, 5, 5]}}]}"#;
        let back = from_file(parse(text).unwrap(), project.path(), &rec);
        assert_eq!((back.items.len(), back.dropped.len()), (1, 1));
        assert_eq!(back.items[0].shape, Shape::Circle(b(0.0, 0.0, 5.0, 5.0)));
    }

    #[test]
    fn a_name_that_points_outside_the_project_is_never_read() {
        let rec = rec();
        let outer = tempfile::tempdir().unwrap();
        let project = outer.path().join("project");
        std::fs::create_dir_all(project.join("pictures")).unwrap();
        // Real files, outside the project folder and inside it under names the format never writes.
        let real = png(&picture()).unwrap();
        for name in ["secret.png", "project/other.png", "project/pictures/short.png", "project/logo.png"] {
            std::fs::write(outer.path().join(name), &real).unwrap();
        }
        let outside = outer.path().join("secret.png").display().to_string().replace('\\', "/");
        for name in ["../secret.png", "pictures/../../secret.png", "other.png", "pictures/short.png", "logo.png", outside.as_str(), ""] {
            let mut file = parse(r#"{"format": 1}"#).unwrap();
            file.edit.tour_doc.logo = Some(name.to_string());
            file.items.push(SavedItem { shape: SavedShape::Image { rect: [0.0, 0.0, 2.0, 2.0], picture: name.to_string() }, ..SavedItem::default() });
            let back = from_file(file, &project, &rec);
            assert_eq!(back.edit.tour_doc.logo_path, None, "{name:?}");
            assert!(back.items.is_empty(), "{name:?}");
            assert_eq!(back.dropped.len(), 2, "{name:?}");
        }
    }

    #[test]
    fn nothing_in_the_file_holds_the_token() {
        let token = crate::publish::tests::TOKEN;
        let rec = rec();
        let (_keep, logo) = logo_source();
        // The state an upload is made from: the address and ids the token was used for, and the edit.
        let (file, blobs) = build(&rec, &edit(&rec, Some(&logo)), &events(), &items());
        let text = to_text(&file).unwrap();
        assert!(text.contains("https://tours.example.com") && text.contains("tour_0123456789abcdef01234567"));
        assert!(!text.contains(token) && !text.contains(&token[3..]));
        assert!(!text.to_ascii_lowercase().contains("token"));
        assert!(blobs.iter().all(|b| !b.bytes.windows(token.len()).any(|w| w == token.as_bytes())));
    }

    #[test]
    fn key_presses_are_saved_only_as_open_point_3_decides() {
        // Open point 3, decided 2026-11-18: mouse events only, never key presses.
        let rec = rec();
        let events = events();
        let (file, _) = build(&rec, &Edit::new(&rec, OverlayStyle::default()), &events, &[]);
        assert_eq!(file.events.len(), 4);
        assert!(file.events.iter().all(|e| matches!(e.kind.as_str(), "down" | "up") && matches!(e.button.as_str(), "left" | "right" | "middle")));
        let text = to_text(&file).unwrap();
        assert!(!text.contains("Hunter2Key") && !text.contains("key_"));
        let project = tempfile::tempdir().unwrap();
        let back = from_file(parse(&text).unwrap(), project.path(), &rec);
        assert_eq!(back.events, mouse_only(&events));
        // A file edited by hand to hold a key press gives none back either.
        let text = r#"{"format": 1, "events": [{"t_ms": 1, "kind": "key_down", "button": "S"}, {"t_ms": 2, "kind": "down", "button": "S"}, {"t_ms": 3, "kind": "up", "button": "right", "x": 4, "y": 5}]}"#;
        let back = from_file(parse(text).unwrap(), project.path(), &rec);
        assert_eq!(back.events, vec![InputEvent { t_ms: 3, kind: InputKind::MouseUp(MouseButton::Right, 4, 5) }]);
    }
}
