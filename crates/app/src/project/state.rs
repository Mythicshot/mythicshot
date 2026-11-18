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
use mythicshot_editor::overlays::{ClickStyle, InputEvent, InputKind, KeyStyle, MouseButton, OverlayStyle};
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
/// A `project.json` longer than this is refused as damaged before it is read as JSON.
pub const MAX_PROJECT_JSON_BYTES: usize = 64 * 1_048_576;

/// A picture file larger than this is not loaded.
const PICTURE_MAX_FILE_BYTES: u64 = 256 * 1_048_576;
/// How many pixels one picture, and all the pictures of a project together, may hold when the
/// project is opened; a picture past either is left out.
const LIMITS: Limits = Limits { picture_pixels: 40_000_000, total_pixels: 120_000_000 };
/// An item is kept when it lies within one frame size of the frame, and at least this many pixels.
/// The editor keeps what is drawn close to the frame; anything further out was put there by hand.
const ITEM_MARGIN_MIN: f32 = 1024.0;
const FONT_SIZE_MIN: f32 = 4.0;
const FONT_SIZE_MAX: f32 = 200.0;
const LINE_WIDTH_MAX: f32 = 200.0;

#[derive(Debug, Clone, Copy)]
struct Limits {
    picture_pixels: u64,
    total_pixels: u64,
}

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
    pub items: Vec<ItemEntry>,
}

/// One entry of `items`. An entry that does not have an item's shape is kept as it was read, so
/// it costs only itself when the project is opened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ItemEntry {
    Item(SavedItem),
    Unreadable(serde_json::Value),
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
    pub overlays: SavedOverlays,
    pub tour_removed: BTreeSet<usize>,
    pub tour_doc: SavedTourDoc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedOverlays {
    /// "off", "chips" or "keyboard".
    pub keys: String,
    /// "off", "ripple", "mouse" or "both".
    pub clicks: String,
    pub halo: bool,
    pub pointer: bool,
}

impl Default for SavedOverlays {
    fn default() -> Self {
        save_overlays(&OverlayStyle::default())
    }
}

impl Default for SavedEdit {
    fn default() -> Self {
        Self { trim: None, deleted: BTreeSet::new(), speed: 1.0, reverse: false, crop: None, scale: 100, overlays: SavedOverlays::default(), tour_removed: BTreeSet::new(), tour_doc: SavedTourDoc::default() }
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

/// What a cover logo that could not be saved or restored is called in a list of such things
/// ([`Restored::dropped`]): the words the two places that leave one out push.
pub const COVER_LOGO: &str = "the cover logo";

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
names!(key_style_name, key_style_from, KeyStyle { Off => "off", Chips => "chips", Keyboard => "keyboard" });
names!(click_style_name, click_style_from, ClickStyle { Off => "off", Ripple => "ripple", Mouse => "mouse", Both => "both" });

/// The first `chars` hex characters of the SHA-256 of `bytes`.
fn hex_hash(bytes: &[u8], chars: usize) -> String {
    let hex: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    hex[..chars.min(hex.len())].to_string()
}

/// The first 16 hex characters of the SHA-256 of `bytes`: what a file's name normally holds.
// The tests name files with it; a save goes through `hex_hash`, which can lengthen a name.
#[cfg(test)]
fn hash16(bytes: &[u8]) -> String {
    hex_hash(bytes, HASH_CHARS[0])
}

/// The hash lengths a name may hold; the longer ones only when two files would share a name.
const HASH_CHARS: [usize; 3] = [16, 32, 64];

fn is_hash(s: &str) -> bool {
    HASH_CHARS.contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Only `pictures/<hash>.png`, so a file edited by hand cannot name a file outside the project.
pub(super) fn is_picture_name(name: &str) -> bool {
    name.strip_prefix("pictures/").and_then(|r| r.strip_suffix(".png")).is_some_and(is_hash)
}

/// Only `logo-<hash>.<extension>`.
pub(super) fn is_logo_name(name: &str) -> bool {
    let ext_ok = |e: &str| (1..=8).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    name.strip_prefix("logo-").and_then(|r| r.split_once('.')).is_some_and(|(hash, ext)| is_hash(hash) && ext_ok(ext))
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
    let width = if s.width.is_finite() { s.width.clamp(0.0, LINE_WIDTH_MAX) } else { d.width };
    let font_size = if s.font_size.is_finite() { s.font_size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX) } else { d.font_size };
    Style { stroke: Rgba(s.stroke), fill: s.fill.map(Rgba), width, font_size, shadow: s.shadow }
}

fn save_theme(t: &Theme) -> SavedTheme {
    SavedTheme { card_style: card_style_name(t.card_style), accent: t.accent.clone(), hotspot: hotspot_name(t.hotspot), progress_bar: t.progress_bar, backdrop: t.backdrop.clone(), spotlight_soft: t.spotlight_soft }
}

fn save_overlays(o: &OverlayStyle) -> SavedOverlays {
    SavedOverlays { keys: key_style_name(o.keys), clicks: click_style_name(o.clicks), halo: o.halo, pointer: o.pointer }
}

fn load_overlays(o: &SavedOverlays) -> OverlayStyle {
    OverlayStyle { keys: key_style_from(&o.keys).unwrap_or_default(), clicks: click_style_from(&o.clicks).unwrap_or_default(), halo: o.halo, pointer: o.pointer }
}

/// The crop cut to the frame, or none when less than a pixel of the frame is inside it.
fn load_crop(c: [f32; 4], width: f32, height: f32) -> Option<Bounds> {
    if !c.iter().all(|v| v.is_finite()) {
        return None;
    }
    let n = load_rect(c).norm();
    let cut = Bounds { x0: n.x0.clamp(0.0, width), y0: n.y0.clamp(0.0, height), x1: n.x1.clamp(0.0, width), y1: n.y1.clamp(0.0, height) };
    (cut.x1 - cut.x0 >= 1.0 && cut.y1 - cut.y0 >= 1.0).then_some(cut)
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
struct Blobs {
    list: Vec<Blob>,
    /// Pictures already encoded, by the pixels they share.
    seen: HashMap<*const Pixmap, String>,
    /// What could not be saved.
    dropped: Vec<String>,
    /// [`hex_hash`]; a test puts a hash that collides here.
    hash: fn(&[u8], usize) -> String,
}

impl Default for Blobs {
    fn default() -> Self {
        Self { list: Vec::new(), seen: HashMap::new(), dropped: Vec::new(), hash: hex_hash }
    }
}

impl Blobs {
    /// The name `bytes` are kept under: `before`, their hash, `after`. Two files with different
    /// bytes never share a name: the second takes a longer hash. None when even the whole hash is taken.
    fn add(&mut self, before: &str, after: &str, bytes: Vec<u8>) -> Option<String> {
        for chars in HASH_CHARS {
            let name = format!("{before}{}{after}", (self.hash)(&bytes, chars));
            match self.list.iter().find(|b| b.name == name).map(|b| b.bytes == bytes) {
                Some(true) => return Some(name),
                Some(false) => continue,
                None => {
                    self.list.push(Blob { name: name.clone(), bytes });
                    return Some(name);
                }
            }
        }
        None
    }

    fn picture(&mut self, image: &ImageData) -> Option<String> {
        let shared = std::sync::Arc::as_ptr(&image.0);
        if let Some(name) = self.seen.get(&shared) {
            return Some(name.clone());
        }
        let Some(name) = png(image).and_then(|bytes| self.add("pictures/", ".png", bytes)) else {
            self.dropped.push("a pasted picture".to_string());
            return None;
        };
        self.seen.insert(shared, name.clone());
        Some(name)
    }

    /// The logo's source file, copied as it is. A logo that cannot be read, or is larger than the
    /// export accepts, is left out and named in `dropped`.
    fn logo(&mut self, path: &Path) -> Option<String> {
        let name = self.read_logo(path);
        if name.is_none() {
            self.dropped.push("the cover logo".to_string());
        }
        name
    }

    fn read_logo(&mut self, path: &Path) -> Option<String> {
        if std::fs::metadata(path).ok()?.len() > LOGO_MAX_FILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let ext: String = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let ext = if (1..=8).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) { ext } else { "img".to_string() };
        self.add("logo-", &format!(".{ext}"), bytes)
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

/// A picture file of the project and how many pixels it has, or None when it is missing, not a
/// PNG, or has more than `max_pixels`.
fn load_picture(path: &Path, max_pixels: u64) -> Option<(ImageData, u64)> {
    if std::fs::metadata(path).ok()?.len() > PICTURE_MAX_FILE_BYTES {
        return None;
    }
    let raw = std::fs::read(path).ok()?;
    let decoder = PngDecoder::new(std::io::Cursor::new(&raw[..])).ok()?;
    let (w, h) = decoder.dimensions();
    let pixels = w as u64 * h as u64;
    if pixels == 0 || pixels > max_pixels {
        return None;
    }
    let img = DynamicImage::from_decoder(decoder).ok()?.into_rgba8();
    Some((ImageData::from_rgba(&img), pixels))
}

/// What is read while a file is turned back into editor state.
struct Loader<'a> {
    dir: &'a Path,
    /// Frames in the recording, and the size of one.
    frames: usize,
    width: f32,
    height: f32,
    limits: Limits,
    /// Pixels the pictures still to be read may hold together.
    pixels_left: u64,
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
        let loaded = load_picture(&self.dir.join(name), self.limits.picture_pixels.min(self.pixels_left)).map(|(image, pixels)| {
            self.pixels_left -= pixels;
            image
        });
        self.pictures.insert(name.to_string(), loaded.clone());
        loaded
    }

    /// Whether the item lies within one frame size of the frame (at least [`ITEM_MARGIN_MIN`]),
    /// so drawing it can never ask for a layer many times the frame.
    fn near_frame(&self, item: &Item) -> bool {
        let n = item.bounds().norm();
        let (mx, my) = (self.width.max(ITEM_MARGIN_MIN), self.height.max(ITEM_MARGIN_MIN));
        [n.x0, n.y0, n.x1, n.y1].iter().all(|v| v.is_finite()) && n.x0 >= -mx && n.y0 >= -my && n.x1 <= self.width + mx && n.y1 <= self.height + my
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
    /// ends beyond it is cut at the last frame. An item far outside the frame is left out too.
    fn item(&mut self, item: SavedItem, id: u64) -> Option<Item> {
        let last = u32::try_from(self.frames.saturating_sub(1)).unwrap_or(u32::MAX);
        let outside = self.frames == 0 || item.pin.is_some_and(|p| p > last) || item.frames.is_some_and(|(a, b)| a.min(b) > last);
        if outside && (item.pin.is_some() || item.frames.is_some()) {
            self.dropped.push("an annotation on a frame the recording does not have".to_string());
            return None;
        }
        let frames = item.frames.map(|(a, b)| (a.min(b), a.max(b).min(last)));
        let style = load_style(&item.style);
        let item = Item { id, shape: self.shape(item.shape)?, style, frames, pin: item.pin };
        if !self.near_frame(&item) {
            self.dropped.push("an annotation far outside the picture".to_string());
            return None;
        }
        Some(item)
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
            crop: e.crop.and_then(|c| load_crop(c, self.width, self.height)),
            scale: e.scale.clamp(10, 100),
            overlays: load_overlays(&e.overlays),
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

/// The file for `snap`, the files that go beside it (the pasted pictures as PNG and the cover
/// logo's source file), and what could not be saved: an item whose picture cannot be encoded
/// ("a pasted picture") and a logo that cannot be read or is too large ("the cover logo") are
/// left out of the file and named in the third value.
pub fn to_file(key: &ProjectKey, snap: &Snapshot<'_>, now_ms: i64, frames_bytes: u64) -> (ProjectFile, Vec<Blob>, Vec<String>) {
    let mut blobs = Blobs::default();
    let e = snap.edit;
    let edit = SavedEdit {
        trim: Some(e.trim),
        deleted: e.deleted.clone(),
        speed: fin(e.speed),
        reverse: e.reverse,
        crop: e.crop.map(save_rect),
        scale: e.scale,
        overlays: save_overlays(&e.overlays),
        tour_removed: e.tour_removed.clone(),
        tour_doc: save_doc(&e.tour_doc, &mut blobs),
    };
    let items = snap.items.iter().filter_map(|it| Some(ItemEntry::Item(SavedItem { shape: save_shape(&it.shape, &mut blobs)?, style: save_style(&it.style), frames: it.frames, pin: it.pin }))).collect();
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
    (file, blobs.list, blobs.dropped)
}

/// The text of `project.json`: the form `golden-v1.json` pins.
pub fn to_text(file: &ProjectFile) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

/// Editor state from a parsed file whose pictures and logo are in `dir`. Frame numbers beyond
/// `rec` are cut or left out; a missing or oversized picture, a missing logo, and an item that
/// cannot be read or lies far outside the frame each cost only themselves and are named in
/// `dropped`; no input panics.
pub fn from_file(file: ProjectFile, dir: &Path, rec: &Recording) -> Restored {
    from_file_within(file, dir, rec, LIMITS)
}

fn from_file_within(file: ProjectFile, dir: &Path, rec: &Recording, limits: Limits) -> Restored {
    let mut loader = Loader { dir, frames: rec.frames.len(), width: rec.width as f32, height: rec.height as f32, limits, pixels_left: limits.total_pixels, pictures: HashMap::new(), dropped: Vec::new() };
    let edit = loader.edit(file.edit);
    let mut items = Vec::new();
    for entry in file.items {
        let saved = match entry {
            ItemEntry::Item(saved) => saved,
            ItemEntry::Unreadable(_) => {
                loader.dropped.push("an annotation that could not be read".to_string());
                continue;
            }
        };
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
    if text.len() > MAX_PROJECT_JSON_BYTES {
        return Err(ParseError::Damaged(format!("the file is larger than {} MB", MAX_PROJECT_JSON_BYTES / 1_048_576)));
    }
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
pub(super) mod tests {
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

    pub(in crate::project) fn picture() -> ImageData {
        let mut img = RgbaImage::from_pixel(2, 2, image::Rgba([200, 30, 40, 255]));
        img.put_pixel(1, 1, image::Rgba([0, 0, 255, 255]));
        img.put_pixel(0, 1, image::Rgba([200, 30, 40, 128]));
        ImageData::from_rgba(&img)
    }

    fn b(x0: f32, y0: f32, x1: f32, y1: f32) -> Bounds {
        Bounds { x0, y0, x1, y1 }
    }

    /// One of every kind of shape; some on a frame range, one pinned to a step, one restyled.
    pub(in crate::project) fn items() -> Vec<Item> {
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

    pub(in crate::project) fn events() -> Vec<InputEvent> {
        vec![
            InputEvent { t_ms: 150, kind: InputKind::MouseDown(MouseButton::Left, 50, 40) },
            InputEvent { t_ms: 160, kind: InputKind::KeyDown("Hunter2Key".into()) },
            InputEvent { t_ms: 210, kind: InputKind::MouseUp(MouseButton::Left, 50, 40) },
            InputEvent { t_ms: 230, kind: InputKind::KeyUp("Hunter2Key".into()) },
            InputEvent { t_ms: 480, kind: InputKind::MouseDown(MouseButton::Right, 75, -3) },
            InputEvent { t_ms: 520, kind: InputKind::MouseUp(MouseButton::Middle, 75, 41) },
        ]
    }

    pub(in crate::project) fn mouse_only(events: &[InputEvent]) -> Vec<InputEvent> {
        events.iter().filter(|e| matches!(e.kind, InputKind::MouseDown(..) | InputKind::MouseUp(..))).cloned().collect()
    }

    /// Every field of an `Edit` away from its default, with cards on two steps.
    pub(in crate::project) fn edit(rec: &Recording, logo: Option<&Path>) -> Edit {
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
    pub(in crate::project) fn logo_source() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Company Logo.PNG");
        std::fs::write(&path, LOGO).unwrap();
        (dir, path)
    }

    pub(in crate::project) fn logo_name() -> String {
        format!("logo-{}.png", hash16(LOGO))
    }

    fn build(rec: &Recording, edit: &Edit, events: &[InputEvent], items: &[Item]) -> (ProjectFile, Vec<Blob>) {
        let snap = Snapshot { rec, title: "Chapter one", tour_title: "Customer onboarding", steps: 3, edit, events, items };
        let (file, blobs, dropped) = to_file(&key(), &snap, SAVED_AT, FRAMES_BYTES);
        assert_eq!(dropped, Vec::<String>::new());
        (file, blobs)
    }

    fn saved(entry: &ItemEntry) -> &SavedItem {
        match entry {
            ItemEntry::Item(item) => item,
            ItemEntry::Unreadable(value) => panic!("not an item: {value}"),
        }
    }

    pub(in crate::project) fn image_item(id: u64, image: ImageData) -> Item {
        Item { id, shape: Shape::Image { rect: b(0.0, 0.0, 2.0, 2.0), image }, style: Style::default(), frames: None, pin: None }
    }

    /// A 2 x 2 picture of one colour.
    pub(in crate::project) fn plain(colour: [u8; 4]) -> ImageData {
        ImageData::from_rgba(&RgbaImage::from_pixel(2, 2, image::Rgba(colour)))
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
    pub(in crate::project) fn assert_same_items(got: &[Item], want: &[Item]) {
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

    pub(in crate::project) fn assert_same_edit(got: &Edit, want: &Edit) {
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
        assert!(file.items.iter().all(|i| matches!(&saved(i).shape, SavedShape::Image { picture, .. } if *picture == blobs[0].name)));
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
        // Cut off anywhere, the golden file is damaged, never newer and never a panic.
        let golden = GOLDEN.replace("\r\n", "\n");
        let whole = golden.trim_end();
        for cut in (0..whole.len()).filter(|i| whole.is_char_boundary(*i)) {
            assert!(damaged(&whole[..cut]), "cut at {cut}");
        }
        assert!(parse(whole).is_ok());
        // A byte order mark in front, as some editors leave one, is not damage.
        assert!(parse(&format!("\u{feff}{whole}")).is_ok());
        // Longer than a project file may be: refused before it is read as JSON.
        let mut long = whole.to_string();
        long.push_str(&" ".repeat(MAX_PROJECT_JSON_BYTES + 1 - whole.len()));
        assert_eq!(long.len(), MAX_PROJECT_JSON_BYTES + 1);
        assert!(damaged(&long));
        assert!(parse(&long[..MAX_PROJECT_JSON_BYTES]).is_ok());
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
        assert_eq!((back.items[3].style.width, back.items[3].style.font_size), (Style::default().width, 4.0));
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
        let picture = file.items.iter().find_map(|i| match &saved(i).shape { SavedShape::Image { picture, .. } => Some(picture.clone()), _ => None }).unwrap();
        write_blobs(project.path(), &[Blob { name: picture, bytes: b"not a png".to_vec() }]);
        let mut odd = file.clone();
        odd.edit.tour_doc.logo = None;
        odd.items.push(ItemEntry::Item(SavedItem::default()));
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
        let hash = hash16(&real);
        let (good_picture, good_logo) = (format!("pictures/{hash}.png"), format!("logo-{hash}.png"));
        for name in ["secret.png", "project/other.png", "project/pictures/short.png", "project/logo.png", &format!("project/{good_picture}"), &format!("project/{good_logo}")] {
            std::fs::write(outer.path().join(name), &real).unwrap();
        }
        // The two names the format does write are read.
        let mut file = parse(r#"{"format": 1}"#).unwrap();
        file.edit.tour_doc.logo = Some(good_logo.clone());
        file.items.push(ItemEntry::Item(SavedItem { shape: SavedShape::Image { rect: [0.0, 0.0, 2.0, 2.0], picture: good_picture.clone() }, ..SavedItem::default() }));
        let back = from_file(file, &project, &rec);
        assert_eq!((back.edit.tour_doc.logo_path, back.items.len(), back.dropped.len()), (Some(project.join(&good_logo)), 1, 0));
        // Other spellings that Windows would open as those same files, or as a file elsewhere.
        let upper = hash.to_ascii_uppercase();
        let spellings = [
            format!("pictures\\{hash}.png"), format!("pictures/{upper}.png"), format!("pictures/{hash}.PNG"), format!("pictures/{hash}.png::$DATA"), format!("pictures/{hash}.png."), format!("pictures/{hash}.png "),
            format!("pictures/./{hash}.png"), format!("pictures//{hash}.png"), format!(".\\{good_logo}"), format!("logo-{upper}.png"), format!("logo-{hash}.PNG"), format!("logo-{hash}.png::$DATA"), format!("logo-{hash}.png."), format!("logo-{hash}.png "),
            "..\\secret.png".to_string(), "pictures\\..\\..\\secret.png".to_string(), "C:secret.png".to_string(), "\\secret.png".to_string(),
        ];
        let outside = outer.path().join("secret.png").display().to_string().replace('\\', "/");
        for name in ["../secret.png", "pictures/../../secret.png", "other.png", "pictures/short.png", "logo.png", outside.as_str(), ""].into_iter().chain(spellings.iter().map(String::as_str)) {
            let mut file = parse(r#"{"format": 1}"#).unwrap();
            file.edit.tour_doc.logo = Some(name.to_string());
            file.items.push(ItemEntry::Item(SavedItem { shape: SavedShape::Image { rect: [0.0, 0.0, 2.0, 2.0], picture: name.to_string() }, ..SavedItem::default() }));
            let back = from_file(file, &project, &rec);
            assert_eq!(back.edit.tour_doc.logo_path, None, "{name:?}");
            assert!(back.items.is_empty(), "{name:?}");
            assert_eq!(back.dropped.len(), 2, "{name:?}");
        }
    }

    #[test]
    fn an_item_far_outside_the_frame_is_left_out_and_a_huge_font_or_line_is_cut() {
        let rec = rec();
        let project = tempfile::tempdir().unwrap();
        let text = r#"{"format": 1, "items": [
            {"shape": {"kind": "rect", "rect": [-50, -50, 150, 130]}},
            {"shape": {"kind": "rect", "rect": [0, 0, 1000000, 10]}},
            {"shape": {"kind": "line", "from": [0, 0], "to": [0, -900000]}},
            {"shape": {"kind": "freehand", "points": [[1, 1], [1e9, 5]]}},
            {"shape": {"kind": "spotlight", "rect": [-1e7, -1e7, 1e7, 1e7], "dim": 50}},
            {"shape": {"kind": "bubble", "rect": [10, 10, 50, 40], "tail": [5e6, 20], "text": "x"}},
            {"shape": {"kind": "text", "at": [10, 10], "text": "Hi"}, "style": {"font_size": 100000}},
            {"shape": {"kind": "step", "center": [20, 20], "number": 1}, "style": {"font_size": 0.001, "width": 1e9}}
        ]}"#;
        let back = from_file(parse(text).unwrap(), project.path(), &rec);
        assert_eq!(back.items.len(), 3);
        assert_eq!(back.items[0].shape, Shape::Rect(b(-50.0, -50.0, 150.0, 130.0)));
        assert_eq!(back.items[1].style.font_size, 200.0);
        assert_eq!((back.items[2].style.font_size, back.items[2].style.width), (4.0, 200.0));
        assert_eq!(back.dropped, vec!["an annotation far outside the picture".to_string(); 5]);
        // Whatever is kept lies within one frame size, or 1024 pixels, of the frame.
        for item in &back.items {
            let n = item.bounds().norm();
            assert!(n.x0 >= -1024.0 && n.y0 >= -1024.0 && n.x1 <= 1124.0 && n.y1 <= 1104.0, "{item:?}");
        }
    }

    #[test]
    fn a_crop_is_cut_to_the_frame_and_one_outside_it_is_no_crop() {
        let rec = rec();
        let project = tempfile::tempdir().unwrap();
        let crop = |c: &str| from_file(parse(&format!(r#"{{"format": 1, "edit": {{"crop": {c}}}}}"#)).unwrap(), project.path(), &rec).edit.crop;
        assert_eq!(crop("[10, 5, 90, 70]"), Some(b(10.0, 5.0, 90.0, 70.0)));
        assert_eq!(crop("[-20, -10, 50, 40]"), Some(b(0.0, 0.0, 50.0, 40.0)));
        assert_eq!(crop("[90, 70, 300, 300]"), Some(b(90.0, 70.0, 100.0, 80.0)));
        assert_eq!(crop("[50, 40, 10, 5]"), Some(b(10.0, 5.0, 50.0, 40.0)));
        // Less than a pixel of the frame, or none of it.
        assert_eq!(crop("[99.5, 0, 200, 50]"), None);
        assert_eq!(crop("[0, 79.5, 50, 500]"), None);
        assert_eq!(crop("[150, 10, 200, 50]"), None);
        assert_eq!(crop("[-50, -50, -10, -10]"), None);
        assert_eq!(crop("[30, 30, 30, 30]"), None);
        assert_eq!(crop("null"), None);
    }

    #[test]
    fn an_overlay_word_this_build_does_not_know_is_the_default() {
        let rec = rec();
        let project = tempfile::tempdir().unwrap();
        let text = r#"{"format": 1, "edit": {"speed": 2.0, "overlays": {"keys": "hologram", "clicks": "both", "halo": true}}}"#;
        let back = from_file(parse(text).unwrap(), project.path(), &rec);
        assert_eq!(back.edit.overlays, OverlayStyle { keys: KeyStyle::Off, clicks: ClickStyle::Both, halo: true, pointer: true });
        assert_eq!(back.edit.speed, 2.0);
        let text = r#"{"format": 1, "edit": {"overlays": {"keys": "keyboard", "clicks": "sparkle", "pointer": false}}}"#;
        let back = from_file(parse(text).unwrap(), project.path(), &rec);
        assert_eq!(back.edit.overlays, OverlayStyle { keys: KeyStyle::Keyboard, clicks: ClickStyle::Ripple, halo: false, pointer: false });
    }

    #[test]
    fn an_item_with_a_field_of_the_wrong_shape_costs_only_that_item() {
        let rec = rec();
        let project = tempfile::tempdir().unwrap();
        let text = r#"{"format": 1, "title": "Kept", "items": [
            {"shape": {"kind": "rect"}},
            {"shape": {"kind": "rect", "rect": [1, 2, 3, 4]}, "frames": "all"},
            {"shape": {"kind": "rect", "rect": [1, 2, 3, 4]}, "style": {"stroke": [1, 2, 3]}},
            {"shape": {"kind": "step", "center": [5, 6], "number": -1}},
            7, "x", null,
            {"shape": {"kind": "circle", "rect": [0, 0, 5, 5]}, "pin": 2}
        ]}"#;
        let back = from_file(parse(text).unwrap(), project.path(), &rec);
        assert_eq!(back.title, "Kept");
        assert_eq!(back.items, vec![Item { id: 1, shape: Shape::Circle(b(0.0, 0.0, 5.0, 5.0)), style: Style::default(), frames: None, pin: Some(2) }]);
        assert_eq!(back.dropped, vec!["an annotation that could not be read".to_string(); 7]);
    }

    #[test]
    fn pictures_past_the_pixel_budget_are_left_out() {
        let rec = rec();
        let first = plain([10, 20, 30, 255]);
        // Two items share the first picture; the second and third are pictures of their own.
        let items = vec![image_item(1, first.clone()), image_item(2, first), image_item(3, plain([40, 50, 60, 255])), image_item(4, plain([70, 80, 90, 255]))];
        let (file, blobs) = build(&rec, &Edit::new(&rec, OverlayStyle::default()), &[], &items);
        assert_eq!(blobs.len(), 3);
        let project = tempfile::tempdir().unwrap();
        write_blobs(project.path(), &blobs);
        let open = |picture_pixels, total_pixels| from_file_within(file.clone(), project.path(), &rec, Limits { picture_pixels, total_pixels });
        // Room for all three pictures of four pixels each.
        let back = open(4, 12);
        assert_eq!((back.items.len(), back.dropped.len()), (4, 0));
        // Room for two: the shared picture counts once, the last one is left out.
        let back = open(4, 11);
        assert_eq!(back.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(back.dropped, vec!["a pasted picture".to_string()]);
        let back = open(4, 7);
        assert_eq!((back.items.len(), back.dropped), (2, vec!["a pasted picture".to_string(); 2]));
        // Each picture is larger than one picture may be.
        let back = open(3, 1000);
        assert_eq!((back.items.len(), back.dropped.len()), (0, 4));
        // The real limits take them all.
        assert_eq!(from_file(file.clone(), project.path(), &rec).items.len(), 4);
    }

    #[test]
    fn two_different_files_never_share_a_name() {
        // A hash whose first 16 characters are the same for every file.
        fn colliding(bytes: &[u8], chars: usize) -> String {
            if chars == 16 { "0123456789abcdef".to_string() } else { hex_hash(bytes, chars) }
        }
        let mut blobs = Blobs { hash: colliding, ..Blobs::default() };
        let (one, two) = (plain([1, 2, 3, 255]), plain([4, 5, 6, 255]));
        let first = blobs.picture(&one).unwrap();
        let second = blobs.picture(&two).unwrap();
        // The same pixels again, as a picture of its own, take the name they already have.
        let again = blobs.picture(&plain([4, 5, 6, 255])).unwrap();
        assert_eq!(first, "pictures/0123456789abcdef.png");
        assert_eq!(second, format!("pictures/{}.png", hex_hash(&png(&two).unwrap(), 32)));
        assert_eq!(again, second);
        assert_eq!(blobs.list.len(), 2);
        assert_eq!((&blobs.list[0].bytes, &blobs.list[1].bytes), (&png(&one).unwrap(), &png(&two).unwrap()));
        assert!(is_picture_name(&first) && is_picture_name(&second));
        assert!(blobs.dropped.is_empty());
        // The longer name loads like any other.
        let rec = rec();
        let project = tempfile::tempdir().unwrap();
        write_blobs(project.path(), &blobs.list);
        let mut file = parse(r#"{"format": 1}"#).unwrap();
        file.items.push(ItemEntry::Item(SavedItem { shape: SavedShape::Image { rect: [0.0, 0.0, 2.0, 2.0], picture: second }, ..SavedItem::default() }));
        let back = from_file(file, project.path(), &rec);
        assert!(matches!(&back.items[0].shape, Shape::Image { image, .. } if pixels(image) == pixels(&two)));
        // A logo whose name is taken by other bytes gets the longer name too.
        let mut blobs = Blobs { hash: colliding, ..Blobs::default() };
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.png"), dir.path().join("b.png"));
        std::fs::write(&a, b"first logo").unwrap();
        std::fs::write(&b, b"second logo").unwrap();
        let (first, second) = (blobs.logo(&a).unwrap(), blobs.logo(&b).unwrap());
        assert_eq!(first, "logo-0123456789abcdef.png");
        assert_eq!(second, format!("logo-{}.png", hex_hash(b"second logo", 32)));
        assert!(is_logo_name(&first) && is_logo_name(&second));
    }

    #[test]
    fn a_logo_that_cannot_be_saved_is_named_and_costs_only_itself() {
        let rec = rec();
        let dir = tempfile::tempdir().unwrap();
        let (items, events) = (items(), events());
        let build = |logo: &Path| {
            let edit = edit(&rec, Some(logo));
            let snap = Snapshot { rec: &rec, title: "Chapter one", tour_title: "Customer onboarding", steps: 3, edit: &edit, events: &events, items: &items };
            to_file(&key(), &snap, SAVED_AT, FRAMES_BYTES)
        };
        // A logo file that is gone.
        let (file, blobs, dropped) = build(&dir.path().join("gone.png"));
        assert_eq!(dropped, vec!["the cover logo".to_string()]);
        assert_eq!(file.edit.tour_doc.logo, None);
        assert_eq!((file.items.len(), blobs.len()), (19, 1));
        assert!(file.edit.tour_doc.cover.is_some());
        // A logo file larger than the export accepts is not copied.
        let huge = dir.path().join("huge.png");
        std::fs::File::create(&huge).unwrap().set_len(LOGO_MAX_FILE_BYTES + 1).unwrap();
        let (file, blobs, dropped) = build(&huge);
        assert_eq!(dropped, vec!["the cover logo".to_string()]);
        assert_eq!((file.edit.tour_doc.logo, blobs.len()), (None, 1));
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
