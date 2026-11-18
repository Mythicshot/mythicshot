//! Tour export: turns a recording into a tour bundle, one image per click.
//!
//! A tour step is the frame from just before a left click plus where the
//! click landed. Step images are drawn by [`crate::video::decorate`] with the
//! overlays off, so annotations and crop match every other export.

mod core;
mod doc;
mod encode;
mod layer;

pub use doc::*;
pub use encode::*;
pub use layer::*;
pub use self::core::{Placed, Side, parse_bold, place_card};

use std::path::{Path, PathBuf};

use image::RgbaImage;
use mythicshot_editor::Item;
use mythicshot_editor::overlays::{ClickStyle, InputEvent, InputKind, KeyStyle, MouseButton, OverlayStyle};
use mythicshot_recording::Recording;

use crate::video::{Edit, crop_px, decorate, step_items};

/// Presses closer together than this are one step.
pub const MERGE_MS: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepPlan {
    /// Source frame index in the recording.
    pub src: usize,
    /// Hotspot centre as a percentage of the cropped image, or None for the final step.
    pub click: Option<(f32, f32)>,
    /// Recording-clock time of the press, or of the frame for the final step.
    pub t_ms: u64,
}

const NO_CLICKS: &str = "This recording has no left clicks inside the kept frames, so there are no tour steps.";
const ALL_REMOVED: &str = "Every step has been removed from the tour.";

fn pct(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// One step per left press (merged within [`MERGE_MS`]), then a final step on the last kept frame.
/// Steps removed in the Tour view (`edit.tour_removed`) are dropped afterwards.
pub fn plan_steps(rec: &Recording, edit: &Edit, events: &[InputEvent]) -> Result<Vec<StepPlan>, String> {
    if rec.frames.is_empty() {
        return Err(NO_CLICKS.to_string());
    }
    let mut presses: Vec<(u64, i32, i32)> = events
        .iter()
        .filter_map(|e| match e.kind {
            InputKind::MouseDown(MouseButton::Left, x, y) => Some((e.t_ms, x, y)),
            _ => None,
        })
        .collect();
    presses.sort_by_key(|p| p.0);

    let (cx, cy, cw, ch) = crop_px(rec, edit);
    let mut steps: Vec<StepPlan> = Vec::new();
    let mut prev: Option<u64> = None;
    for (t, x, y) in presses {
        let merged = prev.is_some_and(|p| t - p <= MERGE_MS);
        prev = Some(t);
        if merged {
            continue;
        }
        let src = rec.frames.iter().rposition(|f| f.t_ms <= t).unwrap_or(0);
        if !edit.kept(src) {
            continue;
        }
        let inside_frame = x >= 0 && y >= 0 && (x as u32) < rec.width && (y as u32) < rec.height;
        let (ux, uy) = (x as u32, y as u32);
        if !inside_frame || ux < cx || uy < cy || ux >= cx + cw || uy >= cy + ch {
            continue;
        }
        let px = pct((ux - cx) as f32 / cw as f32 * 100.0);
        let py = pct((uy - cy) as f32 / ch as f32 * 100.0);
        steps.push(StepPlan { src, click: Some((px, py)), t_ms: t });
    }
    if steps.is_empty() {
        return Err(NO_CLICKS.to_string());
    }
    if let Some(last) = (0..rec.frames.len()).rev().find(|&i| edit.kept(i))
        && steps.last().is_some_and(|s| s.src != last)
    {
        steps.push(StepPlan { src: last, click: None, t_ms: rec.frames[last].t_ms });
    }
    steps.retain(|s| !edit.tour_removed.contains(&s.src));
    if steps.is_empty() {
        return Err(ALL_REMOVED.to_string());
    }
    Ok(steps)
}

pub const MANIFEST_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ManifestStep {
    pub image: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub click: Option<Point>,
    pub t_ms: u64,
    /// Tour layer: the step's spotlight (null when none) and its notes.
    pub spotlight: Option<Area>,
    pub notes: Vec<Note>,
    /// Version 3: the step's guide card (null when none), how the step advances and its zoom
    /// (null follows the tour).
    pub card: Option<Card>,
    pub advance: Advance,
    pub zoom: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Manifest {
    pub version: u32,
    pub generator: String,
    pub title: String,
    pub steps: Vec<ManifestStep>,
    /// Darkening strength in percent, shared by every spotlight; present only when a step has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spotlight_dim: Option<u8>,
    /// Version 3 tour settings, always written, sanitised by [`sanitize`].
    pub theme: Theme,
    pub zoom: Zoom,
    pub cover: Option<Cover>,
    pub cta: Option<Link>,
    pub end: Option<EndCard>,
    pub autoplay: Autoplay,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TourSummary {
    /// Where the tour was written: the bundle's folder, or the single file.
    pub path: PathBuf,
    pub steps: usize,
    /// For a bundle, the step images and logo together; for a single file, its size on disk.
    pub bytes: u64,
    /// Step images re-encoded at a lower quality (or shrunk) to fit [`MAX_STEP_BYTES`].
    pub squeezed: usize,
}

impl TourSummary {
    /// The sentence the export's status adds when step images had to be made smaller to fit
    /// [`MAX_STEP_BYTES`], else None.
    pub fn squeezed_note(&self) -> Option<String> {
        match self.squeezed {
            0 => None,
            1 => Some("1 picture was saved at lower quality to fit.".to_string()),
            n => Some(format!("{n} pictures were saved at lower quality to fit.")),
        }
    }
}

// The server's caps (`LIMITS` in its manifest rules). An export never writes a bundle over them.
/// The most steps a tour may have.
pub const MAX_STEPS: usize = 200;
/// The most notes (text and bubbles) one step may have.
pub const MAX_NOTES_PER_STEP: usize = 20;
/// The largest manifest, in bytes of compact JSON, the server takes.
pub const MAX_MANIFEST_BYTES: usize = 262_144;

/// The tour's title as the manifest holds it: cleaned like every other one-line text and capped at
/// [`TITLE_MAX`]; a title with nothing left becomes "Untitled tour", since the server needs one.
pub fn tour_title(title: &str) -> String {
    let t = doc::clean(title, TITLE_MAX, false);
    if t.is_empty() { "Untitled tour".to_string() } else { t }
}

/// One line of text cleaned like the tour's title and capped at `max` code points; empty when nothing is left.
#[cfg_attr(not(test), expect(dead_code, reason = "used by the site export, whose dialog is not built yet"))]
pub fn clean_line(text: &str, max: usize) -> String {
    doc::clean(text, max, false)
}

/// Refuse a tour with more steps, or a step with more notes, than the server takes.
fn check_counts(steps: &[ManifestStep]) -> Result<(), String> {
    if steps.len() > MAX_STEPS {
        return Err(format!("A tour can have at most {MAX_STEPS} steps, and this one has {}. Remove steps in the Tour view or trim the recording.", steps.len()));
    }
    if let Some((i, s)) = steps.iter().enumerate().find(|(_, s)| s.notes.len() > MAX_NOTES_PER_STEP) {
        return Err(format!("Step {} has {} notes, and a step can have at most {MAX_NOTES_PER_STEP}. Remove some of its text or bubbles.", i + 1, s.notes.len()));
    }
    Ok(())
}

/// Refuse a manifest larger than the server takes.
fn check_size(manifest: &Manifest) -> Result<(), String> {
    let n = serde_json::to_vec(manifest).map_err(|e| format!("Could not write tour.json: {e}"))?.len();
    if n > MAX_MANIFEST_BYTES {
        return Err(format!("This tour has too much text to publish: its tour.json would be {} KB, and the limit is {} KB. Shorten or remove some notes or cards.", n.div_ceil(1024), MAX_MANIFEST_BYTES / 1024));
    }
    Ok(())
}

/// Each step's manifest entry with everything but its image. Until the image is written its size and
/// bytes hold the largest values an image may have, so a size check on these entries is never smaller
/// than the manifest that will be written.
fn step_entries(rec: &Recording, edit: &Edit, items: &[Item], plan: &[StepPlan], doc: &TourDoc) -> Vec<ManifestStep> {
    plan.iter()
        .enumerate()
        .map(|(i, step)| {
            let layer = step_layer(rec, edit, items, step.src);
            let extras = doc.steps.get(&step.src);
            ManifestStep {
                image: format!("step-{:03}.png", i + 1),
                width: MAX_STEP_WIDTH,
                height: MAX_STEP_HEIGHT,
                bytes: MAX_STEP_BYTES as u64,
                click: step.click.map(|(x, y)| Point { x, y }),
                t_ms: step.t_ms,
                spotlight: layer.spotlight,
                notes: layer.notes,
                card: step_card(rec, edit, step.src, step.click),
                advance: extras.map(|x| x.advance).unwrap_or_default(),
                zoom: extras.and_then(|x| x.zoom),
            }
        })
        .collect()
}

fn manifest(title: String, steps: Vec<ManifestStep>, spotlight_dim: Option<u8>, doc: TourDoc) -> Manifest {
    Manifest {
        version: MANIFEST_VERSION,
        generator: format!("mythicshot {}", env!("CARGO_PKG_VERSION")),
        title,
        steps,
        spotlight_dim,
        theme: doc.theme,
        zoom: doc.zoom,
        cover: doc.cover,
        cta: doc.cta,
        end: doc.end,
        autoplay: doc.autoplay,
    }
}

const PREVIEW_TEMPLATE: &str = include_str!("tour_preview.html");
const JSON_PLACEHOLDER: &str = "/*TOUR_JSON*/";
/// The shared rules every player uses, inlined into the preview's first script block.
const TOUR_CORE_JS: &str = include_str!("tour_core.js");
const CORE_PLACEHOLDER: &str = "/*TOUR_CORE*/";
/// Where a single-file export puts its embedded pictures, before the page's scripts; a bundle's page drops it.
const IMAGES_PLACEHOLDER: &str = "<!--TOUR_IMAGES-->\n";
/// The page's image rule: a bundle's page loads its pictures from its own folder.
const FOLDER_IMG_SRC: &str = "img-src 'self';";
/// A single file loads them only from the `data:` URLs it carries, and nothing else.
const SINGLE_IMG_SRC: &str = "img-src data:;";

// Markers around the player's parts in the preview template, so a site's tour page and its shared files
// are cut from the same page and can never drift from it. The preview and the single file drop the
// markers, so their bytes are exactly what they were without them.
const CSS_BEGIN: &str = "/*PLAYER_CSS_BEGIN*/";
const CSS_END: &str = "/*PLAYER_CSS_END*/";
const HTML_BEGIN: &str = "<!--PLAYER_HTML_BEGIN-->";
const HTML_END: &str = "<!--PLAYER_HTML_END-->";
const JS_BEGIN: &str = "/*PLAYER_JS_BEGIN*/";
const JS_END: &str = "/*PLAYER_JS_END*/";
/// Around the one line only the local preview has (its window title); a site's page names itself.
const PREVIEW_ONLY_BEGIN: &str = "/*PREVIEW_ONLY_BEGIN*/";
const PREVIEW_ONLY_END: &str = "/*PREVIEW_ONLY_END*/";
const MARKERS: [&str; 8] = [CSS_BEGIN, CSS_END, HTML_BEGIN, HTML_END, JS_BEGIN, JS_END, PREVIEW_ONLY_BEGIN, PREVIEW_ONLY_END];

/// Template text with the player markers taken out.
fn strip_markers(s: &str) -> String {
    MARKERS.iter().fold(s.to_string(), |s, m| s.replacen(m, "", 1))
}

/// The template text between two markers.
fn between(begin: &str, end: &str) -> &'static str {
    let (_, rest) = PREVIEW_TEMPLATE.split_once(begin).expect("the preview template has the player's begin marker");
    rest.split_once(end).expect("the preview template has the player's end marker").0
}

/// The preview's style rules, as a site's shared stylesheet holds them.
pub fn player_css() -> &'static str {
    between(CSS_BEGIN, CSS_END)
}

/// The preview's page body from the image area to its status line, which the player script fills in.
pub fn player_html() -> &'static str {
    between(HTML_BEGIN, HTML_END)
}

/// The preview's player script, a function call that reads `TOUR`, without the preview's own window title.
pub fn player_js() -> String {
    let js = between(JS_BEGIN, JS_END);
    let (before, rest) = js.split_once(PREVIEW_ONLY_BEGIN).expect("the player has a preview-only part");
    let (_, after) = rest.split_once(PREVIEW_ONLY_END).expect("the preview-only part ends");
    format!("{before}{after}")
}

/// The shared rules every player uses.
pub fn tour_core_js() -> &'static str {
    TOUR_CORE_JS
}

/// The preview page in two parts, split where a single file's pictures go. The rules go in first, from
/// the template, so text in the manifest can never be taken for a placeholder or a marker. `single` swaps
/// the image rule of the Content-Security-Policy for the single-file one; nothing else differs.
fn page_parts(compact_json: &str, single: bool) -> (String, String) {
    let (head, tail) = PREVIEW_TEMPLATE.split_once(IMAGES_PLACEHOLDER).expect("the preview template has an images placeholder");
    let (head, tail) = (strip_markers(head), strip_markers(tail));
    let head = if single { head.replacen(FOLDER_IMG_SRC, SINGLE_IMG_SRC, 1) } else { head };
    (head, tail.replacen(CORE_PLACEHOLDER, TOUR_CORE_JS, 1).replacen(JSON_PLACEHOLDER, compact_json, 1))
}

/// The preview page for an already escaped compact manifest.
fn preview_page(compact_json: &str) -> String {
    let (head, tail) = page_parts(compact_json, false);
    head + &tail
}

/// The manifest as compact JSON for a script block: `<` becomes a JSON escape so no text can close it.
fn compact_json(manifest: &Manifest, file: &str) -> Result<String, String> {
    Ok(serde_json::to_string(manifest).map_err(|e| format!("Could not write {file}: {e}"))?.replace('<', r"\u003c"))
}

/// Everything a tour export knows before its first image: the steps, the draft manifest (images at their
/// largest) and the logo. Refusals happen here, so a refused tour writes nothing.
struct Prepared {
    plan: Vec<StepPlan>,
    draft: Manifest,
    logo: Option<(Vec<u8>, &'static str)>,
}

fn prepare(rec: &Recording, items: &[Item], edit: &Edit, title: &str, plan: Vec<StepPlan>) -> Result<Prepared, String> {
    // The settings are sanitised and the logo loaded before anything is written, so a bad logo writes nothing.
    // A logo travels only with a cover.
    let mut doc = sanitize(&edit.tour_doc);
    let logo = match (&doc.cover, &doc.logo_path) {
        (Some(_), Some(path)) => Some(load_logo(path)?),
        _ => None,
    };
    if let (Some(c), Some((_, ext))) = (doc.cover.as_mut(), &logo) {
        c.logo = Some(format!("logo.{ext}"));
    }
    // Everything but the images is known now, so a tour the server would refuse writes nothing.
    // The size check here counts every image at its largest, so a tour that is too big is refused before
    // any image is encoded; the exact check before the manifest is written can then never fail on its own.
    let steps = step_entries(rec, edit, items, &plan, &doc);
    check_counts(&steps)?;
    let srcs: Vec<usize> = plan.iter().map(|s| s.src).collect();
    let spotlight_dim = tour_dim(rec, edit, items, &srcs);
    let draft = manifest(tour_title(title), steps, spotlight_dim, doc);
    check_size(&draft)?;
    Ok(Prepared { plan, draft, logo })
}

/// Write a tour bundle into `dir`, which must not exist yet; its parent folder must.
/// `frame(i)` returns source frame `i`. `progress` gets 0.0-1.0 and returns false to cancel.
/// Only a folder this call created is ever removed: if anything fails after `create_dir`
/// succeeded, `dir` is removed again. An existing `dir` is never touched.
#[allow(clippy::too_many_arguments)]
pub fn write_bundle(
    rec: &Recording,
    items: &[Item],
    events: &[InputEvent],
    edit: &Edit,
    title: &str,
    dir: &Path,
    frame: impl FnMut(usize) -> Result<RgbaImage, String>,
    progress: impl FnMut(f32) -> bool,
) -> Result<TourSummary, String> {
    write_tour(rec, items, events, edit, title, dir, frame, progress, Page::Preview)
}

/// Which page a tour folder gets.
#[derive(Debug, Clone, Copy)]
pub enum Page<'a> {
    /// The local preview, with everything inline: what "Export tour" writes.
    Preview,
    /// A tour page in a site, using the site's shared player files, with a `thumb.jpg` for the index.
    #[cfg_attr(not(test), expect(dead_code, reason = "used by the site export, whose dialog is not built yet"))]
    Site(&'a crate::site::Assets),
}

/// The widest a site's thumbnail is.
pub const THUMB_WIDTH: u32 = 480;
/// The JPEG quality of a site's thumbnail.
const THUMB_QUALITY: u8 = 80;

/// [`write_bundle`] with the page chosen. [`Page::Site`] writes the site's tour page and a `thumb.jpg` (the
/// first step's picture at most [`THUMB_WIDTH`] wide, JPEG quality 80) in place of the preview page.
/// Everything else, the refusals, the clean-up and the messages, is the same.
#[allow(clippy::too_many_arguments)]
pub fn write_tour(
    rec: &Recording,
    items: &[Item],
    events: &[InputEvent],
    edit: &Edit,
    title: &str,
    dir: &Path,
    frame: impl FnMut(usize) -> Result<RgbaImage, String>,
    progress: impl FnMut(f32) -> bool,
    page: Page<'_>,
) -> Result<TourSummary, String> {
    let plan = plan_steps(rec, edit, events)?;
    if dir.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", dir.display()));
    }
    let prepared = prepare(rec, items, edit, title, plan)?;
    // Not create_dir_all: a missing parent is an error, and a folder that appears
    // between the check above and here makes this call fail instead of being reused.
    std::fs::create_dir(dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists { format!("{} already exists", dir.display()) } else { format!("Could not create {}: {e}", dir.display()) }
    })?;
    match write_into(rec, items, events, edit, prepared, &mut FolderSink { dir, page, thumb: false }, frame, progress) {
        Ok((steps, bytes, squeezed)) => Ok(TourSummary { path: dir.to_path_buf(), steps, bytes, squeezed }),
        Err(e) => match std::fs::remove_dir_all(dir) {
            Ok(()) => Err(e),
            Err(_) => Err(format!("{e} (the partly written folder {} could not be removed)", dir.display())),
        },
    }
}

/// A single file over this is still written, and the status says it is large.
pub const LARGE_FILE_BYTES: u64 = 25 * 1_048_576;
/// A single file over this is refused: a browser may not open it, and it is too big to send.
pub const MAX_FILE_BYTES: u64 = 200 * 1_048_576;

/// Write the tour as one self-contained HTML file at `path`: the same page as a bundle's `index.html`,
/// with every step image and the logo embedded, each in its own block the page decodes only when its
/// step is shown or about to be. Built by the same steps as [`write_bundle`], so the two never differ in
/// anything but where the pictures live.
///
/// The page is streamed into a temporary file beside `path` one image at a time (only one encoded
/// image is held at once), then renamed over `path`. Replacing an existing file is the caller's
/// decision (the save dialog asks); a refusal, failure or cancel removes the temporary file and leaves
/// `path` and everything else untouched. A file over [`MAX_FILE_BYTES`] is refused with its size, and
/// `path` is not written. The summary's `bytes` is the file's size on disk.
#[allow(clippy::too_many_arguments)]
pub fn write_single_file(
    rec: &Recording,
    items: &[Item],
    events: &[InputEvent],
    edit: &Edit,
    title: &str,
    path: &Path,
    frame: impl FnMut(usize) -> Result<RgbaImage, String>,
    progress: impl FnMut(f32) -> bool,
) -> Result<TourSummary, String> {
    write_single_file_within(rec, items, events, edit, title, path, frame, progress, MAX_FILE_BYTES)
}

/// The refusal for a single file over `cap` bytes.
fn too_big(size: u64, cap: u64) -> String {
    format!("As one file this tour would be {:.1} MB, and a single file can be at most {} MB. Export it as a folder instead.", size as f64 / 1_048_576.0, cap / 1_048_576)
}

#[allow(clippy::too_many_arguments)]
fn write_single_file_within(
    rec: &Recording,
    items: &[Item],
    events: &[InputEvent],
    edit: &Edit,
    title: &str,
    path: &Path,
    frame: impl FnMut(usize) -> Result<RgbaImage, String>,
    progress: impl FnMut(f32) -> bool,
    cap: u64,
) -> Result<TourSummary, String> {
    let plan = plan_steps(rec, edit, events)?;
    let prepared = prepare(rec, items, edit, title, plan)?;
    let shown = path.display();
    if path.is_dir() {
        return Err(format!("{shown} is a folder"));
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let tmp = tempfile::Builder::new().prefix(".mythicshot-tour-").suffix(".tmp").tempfile_in(parent).map_err(|e| format!("Could not write {shown}: {e}"))?;
    let (steps, squeezed) = {
        let mut sink = PageSink { out: Capped { inner: std::io::BufWriter::new(tmp.as_file()), written: 0, cap }, images: 0 };
        let (steps, _, squeezed) = write_into(rec, items, events, edit, prepared, &mut sink, frame, progress)?;
        if sink.out.written > cap {
            return Err(too_big(sink.out.written, cap));
        }
        let file = sink.out.inner.into_inner().map_err(|e| format!("Could not write {shown}: {}", e.error()))?;
        file.sync_all().map_err(|e| format!("Could not write {shown}: {e}"))?;
        (steps, squeezed)
    };
    // The temporary file is created readable by its owner only; the tour is a file to share.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644)).map_err(|e| format!("Could not write {shown}: {e}"))?;
    }
    // Dropping the temporary file on any error above removes it; a failed rename hands it back and drops it too.
    tmp.persist(path).map_err(|e| format!("Could not save {shown}: {}", e.error))?;
    let bytes = std::fs::metadata(path).map_err(|e| format!("Could not read the size of {shown}: {e}"))?.len();
    Ok(TourSummary { path: path.to_path_buf(), steps, bytes, squeezed })
}

/// A writer that counts every byte but stops passing them on past `cap`, so a file that turns out too
/// big is measured to the end without filling the disk.
struct Capped<W: std::io::Write> {
    inner: W,
    written: u64,
    cap: u64,
}

impl<W: std::io::Write> std::io::Write for Capped<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.written += buf.len() as u64;
        if self.written <= self.cap {
            self.inner.write_all(buf)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Where an export's pictures and manifest go. Everything before these calls is shared.
trait Sink {
    /// One picture, `ext` "png" or "jpg", under the name the manifest gives it.
    fn image(&mut self, name: &str, ext: &str, bytes: &[u8]) -> Result<(), String>;
    /// The finished manifest, after every picture.
    fn finish(&mut self, manifest: &Manifest) -> Result<(), String>;
}

/// A bundle: each picture as a file, then `tour.json` and `index.html`. A site's tour also gets
/// `thumb.jpg`, made from its first step.
struct FolderSink<'a> {
    dir: &'a Path,
    page: Page<'a>,
    /// Whether `thumb.jpg` has been written.
    thumb: bool,
}

impl Sink for FolderSink<'_> {
    fn image(&mut self, name: &str, _ext: &str, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(self.dir.join(name), bytes).map_err(|e| format!("Could not write {name}: {e}"))?;
        if matches!(self.page, Page::Site(_)) && !self.thumb && name.starts_with("step-") {
            let thumb = thumbnail(bytes).map_err(|e| format!("Could not write {}: {e}", crate::site::THUMB))?;
            std::fs::write(self.dir.join(crate::site::THUMB), thumb).map_err(|e| format!("Could not write {}: {e}", crate::site::THUMB))?;
            self.thumb = true;
        }
        Ok(())
    }

    fn finish(&mut self, manifest: &Manifest) -> Result<(), String> {
        let pretty = serde_json::to_string_pretty(manifest).map_err(|e| format!("Could not write tour.json: {e}"))?;
        std::fs::write(self.dir.join("tour.json"), pretty).map_err(|e| format!("Could not write tour.json: {e}"))?;
        let json = compact_json(manifest, "index.html")?;
        let html = match self.page {
            Page::Preview => preview_page(&json),
            Page::Site(assets) => crate::site::tour_page(&json, &manifest.title, assets),
        };
        std::fs::write(self.dir.join("index.html"), html).map_err(|e| format!("Could not write index.html: {e}"))
    }
}

/// A step picture as the index's thumbnail: at most [`THUMB_WIDTH`] wide, as a JPEG.
fn thumbnail(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgb8();
    let img = if img.width() > THUMB_WIDTH {
        let h = ((u64::from(img.height()) * u64::from(THUMB_WIDTH) + u64::from(img.width()) / 2) / u64::from(img.width())).max(1) as u32;
        image::imageops::resize(&img, THUMB_WIDTH, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, THUMB_QUALITY).encode_image(&img).map_err(|e| e.to_string())?;
    Ok(out)
}

/// A single page: its head, then one block per picture, written as each picture is made, then the rest
/// of the page with the manifest.
struct PageSink<W: std::io::Write> {
    out: W,
    images: usize,
}

impl<W: std::io::Write> PageSink<W> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.out.write_all(bytes).map_err(|e| format!("Could not write the tour file: {e}"))
    }
}

/// The start of one picture's block. Its type is one the browser never runs: the block is plain base64
/// text, kept out of every script, so no single string in the page holds more than one picture.
fn image_block_open(name: &str, mime: &str) -> String {
    format!("<script type=\"application/octet-stream\" data-tour-image=\"{name}\" data-type=\"{mime}\">")
}
const IMAGE_BLOCK_CLOSE: &str = "</script>\n";

/// A picture name the page can carry in an attribute as it is: letters, digits, `-` and `.` only.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

impl<W: std::io::Write> Sink for PageSink<W> {
    fn image(&mut self, name: &str, ext: &str, bytes: &[u8]) -> Result<(), String> {
        if !plain_name(name) {
            return Err(format!("A tour picture cannot be named {name:?}."));
        }
        let mime = image_mime(ext)?;
        if self.images == 0 {
            // The head depends on nothing in the manifest, so it can go first.
            let (head, _) = page_parts("", true);
            self.write(head.as_bytes())?;
        }
        self.images += 1;
        self.write(image_block_open(name, mime).as_bytes())?;
        base64_into(&mut self.out, bytes).map_err(|e| format!("Could not write the tour file: {e}"))?;
        self.write(IMAGE_BLOCK_CLOSE.as_bytes())
    }

    fn finish(&mut self, manifest: &Manifest) -> Result<(), String> {
        if self.images == 0 {
            return Err("A tour file needs at least one picture.".to_string());
        }
        let (_, tail) = page_parts(&compact_json(manifest, "the tour file")?, true);
        self.write(tail.as_bytes())?;
        self.out.flush().map_err(|e| format!("Could not write the tour file: {e}"))
    }
}

/// The picture type for an encoder's choice.
fn image_mime(ext: &str) -> Result<&'static str, String> {
    match ext {
        "png" => Ok("image/png"),
        "jpg" => Ok("image/jpeg"),
        other => Err(format!("A tour picture cannot be a .{other} file.")),
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, written in pieces of about 8 KB. Its alphabet has no `<`, `"` or `\`,
/// so whatever the bytes hold can never end a string or a script block.
fn base64_into(out: &mut impl std::io::Write, bytes: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(8192);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for k in 0..4 {
            buf.push(if k <= chunk.len() { BASE64[((n >> (18 - 6 * k)) & 63) as usize] } else { b'=' });
        }
        if buf.len() >= 8188 {
            out.write_all(&buf)?;
            buf.clear();
        }
    }
    out.write_all(&buf)
}

#[allow(clippy::too_many_arguments)]
fn write_into(
    rec: &Recording,
    items: &[Item],
    events: &[InputEvent],
    edit: &Edit,
    prepared: Prepared,
    sink: &mut impl Sink,
    mut frame: impl FnMut(usize) -> Result<RgbaImage, String>,
    mut progress: impl FnMut(f32) -> bool,
) -> Result<(usize, u64, usize), String> {
    let Prepared { plan, draft: mut manifest, logo } = prepared;
    // Same crop and annotations as every export, but no scale and no pointer, ripple, halo or keys.
    let mut tour_edit = edit.clone();
    tour_edit.scale = 100;
    tour_edit.overlays = OverlayStyle { keys: KeyStyle::Off, clicks: ClickStyle::Off, halo: false, pointer: false };

    let n = plan.len();
    let mut total = 0u64;
    let mut squeezed = 0;
    for (i, (step, entry)) in plan.iter().zip(manifest.steps.iter_mut()).enumerate() {
        if !progress(i as f32 / n as f32) {
            return Err("cancelled".to_string());
        }
        // Layer items are data; everything else on the step is burned in. Pins and ranges are
        // cleared because `step_items` already chose what this step shows.
        let burned: Vec<Item> = step_items(items, step.src).into_iter().filter(|it| !is_layer(it)).map(|it| Item { pin: None, frames: None, ..it }).collect();
        let img = decorate(rec, step.src, frame(step.src)?, &burned, events, &tour_edit, true);
        let (bytes, ext, img, smaller) = encode_within(fit_image(img), MAX_STEP_BYTES)?;
        squeezed += usize::from(smaller);
        let name = format!("step-{:03}.{ext}", i + 1);
        sink.image(&name, ext, &bytes)?;
        total += bytes.len() as u64;
        (entry.image, entry.width, entry.height, entry.bytes) = (name, img.width(), img.height(), bytes.len() as u64);
    }
    if let (Some(name), Some((bytes, ext))) = (manifest.cover.as_ref().and_then(|c| c.logo.clone()), &logo) {
        sink.image(&name, ext, bytes)?;
        total += bytes.len() as u64;
    }
    check_size(&manifest)?;
    sink.finish(&manifest)?;
    // A cancel that came after the last picture still stops the export before it is kept.
    if !progress(1.0) {
        return Err("cancelled".to_string());
    }
    Ok((n, total, squeezed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mythicshot_editor::overlays::{InputEvent, InputKind, MouseButton, OverlayStyle};
    use mythicshot_editor::Bounds;

    // Five frames 500 ms apart (wider than MERGE_MS), 100×80, ending at 2500 ms.
    fn rec() -> Recording { Recording::timing_only(100, 80, &[0, 500, 1000, 1500, 2000], 2500) }
    fn edit(r: &Recording) -> Edit { Edit::new(r, OverlayStyle::default()) }
    fn down(t_ms: u64, b: MouseButton, x: i32, y: i32) -> InputEvent { InputEvent { t_ms, kind: InputKind::MouseDown(b, x, y) } }
    fn left(t_ms: u64, x: i32, y: i32) -> InputEvent { down(t_ms, MouseButton::Left, x, y) }
    fn shape(s: &[StepPlan]) -> Vec<(usize, Option<(f32, f32)>)> { s.iter().map(|p| (p.src, p.click)).collect() }

    #[test]
    fn one_step_per_click_then_a_final_step() {
        let r = rec();
        let s = plan_steps(&r, &edit(&r), &[left(750, 50, 40), left(1750, 10, 20)]).unwrap();
        assert_eq!(shape(&s), vec![(1, Some((50.0, 50.0))), (3, Some((10.0, 25.0))), (4, None)]);
        assert_eq!(s.iter().map(|p| p.t_ms).collect::<Vec<_>>(), vec![750, 1750, 2000]);
    }

    #[test]
    fn presses_within_400_ms_of_the_previous_press_are_one_step() {
        let r = rec();
        // 750 → 1000 → 1300: each is within 400 ms of the one before, so a triple-click is one step.
        let s = plan_steps(&r, &edit(&r), &[left(750, 50, 40), left(1000, 50, 40), left(1300, 50, 40)]).unwrap();
        assert_eq!(shape(&s), vec![(1, Some((50.0, 50.0))), (4, None)]);
    }

    #[test]
    fn clicks_on_trimmed_or_deleted_frames_are_dropped() {
        let r = rec();
        let mut e = edit(&r);
        e.trim = (2, 4);
        e.deleted.insert(3);
        // 750 → frame 1 (trimmed), 1250 → frame 2 (kept), 1750 → frame 3 (deleted).
        let s = plan_steps(&r, &e, &[left(750, 50, 40), left(1250, 50, 40), left(1750, 50, 40)]).unwrap();
        assert_eq!(shape(&s), vec![(2, Some((50.0, 50.0))), (4, None)]);
    }

    #[test]
    fn crop_remaps_positions_and_drops_clicks_outside_it() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        let s = plan_steps(&r, &e, &[left(750, 75, 40), left(1750, 10, 20)]).unwrap();
        assert_eq!(shape(&s), vec![(1, Some((50.0, 50.0))), (4, None)]);
    }

    #[test]
    fn clicks_outside_the_recorded_area_are_dropped_not_clamped() {
        let r = rec();
        let ev = [left(750, -5, 10), left(1250, 100, 10), left(1750, 10, 80), left(2250, 99, 79)];
        let s = plan_steps(&r, &edit(&r), &ev).unwrap();
        assert_eq!(shape(&s), vec![(4, Some((99.0, 98.75)))]);
    }

    #[test]
    fn only_left_presses_make_steps_and_events_need_not_be_sorted() {
        let r = rec();
        let ev = [left(1750, 10, 20), down(1250, MouseButton::Right, 5, 5), InputEvent { t_ms: 1260, kind: InputKind::KeyDown("S".into()) }, left(750, 50, 40)];
        let s = plan_steps(&r, &edit(&r), &ev).unwrap();
        assert_eq!(shape(&s), vec![(1, Some((50.0, 50.0))), (3, Some((10.0, 25.0))), (4, None)]);
    }

    #[test]
    fn no_final_step_when_the_last_click_is_on_the_last_kept_frame() {
        let r = rec();
        let s = plan_steps(&r, &edit(&r), &[left(2250, 50, 40)]).unwrap();
        assert_eq!(shape(&s), vec![(4, Some((50.0, 50.0)))]);
    }

    #[test]
    fn a_click_before_the_first_frame_uses_frame_zero() {
        let r = Recording::timing_only(100, 80, &[50, 100], 200);
        let s = plan_steps(&r, &edit(&r), &[left(10, 50, 40)]).unwrap();
        assert_eq!(shape(&s), vec![(0, Some((50.0, 50.0))), (1, None)]);
    }

    #[test]
    fn no_usable_clicks_is_an_error() {
        let r = rec();
        let err = plan_steps(&r, &edit(&r), &[down(750, MouseButton::Right, 5, 5)]).unwrap_err();
        assert!(err.contains("no left clicks"), "{err}");
    }

    #[test]
    fn crop_edges_are_exact() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        let s = plan_steps(&r, &e, &[left(750, 49, 40), left(1250, 50, 40), left(1750, 100, 40)]).unwrap();
        assert_eq!(shape(&s), vec![(2, Some((0.0, 50.0))), (4, None)]);
    }

    #[test]
    fn a_recording_with_no_frames_is_an_error() {
        let r = Recording::timing_only(100, 80, &[], 0);
        let err = plan_steps(&r, &edit(&r), &[left(0, 50, 40)]).unwrap_err();
        assert!(err.contains("no left clicks"), "{err}");
    }

    fn flat(_: usize) -> Result<RgbaImage, String> { Ok(RgbaImage::from_pixel(100, 80, image::Rgba([200, 200, 200, 255]))) }
    fn read_json(dir: &Path) -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(dir.join("tour.json")).unwrap()).unwrap() }

    #[test]
    fn bundle_has_images_manifest_and_preview() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("my-tour");
        let sum = write_bundle(&r, &[], &[left(750, 50, 40), left(1750, 10, 20)], &edit(&r), "My tour", &dir, flat, |_| true).unwrap();
        assert_eq!(sum.steps, 3);
        let j = read_json(&dir);
        assert_eq!(j["version"], 3);
        assert_eq!(j["title"], "My tour");
        assert!(j["generator"].as_str().unwrap().starts_with("mythicshot "));
        assert_eq!(j["steps"].as_array().unwrap().len(), 3);
        assert_eq!(j["steps"][0]["image"], "step-001.png");
        assert_eq!(j["steps"][0]["width"], 100);
        assert_eq!(j["steps"][0]["height"], 80);
        assert_eq!(j["steps"][0]["click"]["x"], 50.0);
        assert_eq!(j["steps"][1]["click"]["y"], 25.0);
        assert!(j["steps"][2]["click"].is_null());
        for s in j["steps"].as_array().unwrap() {
            let f = dir.join(s["image"].as_str().unwrap());
            assert_eq!(std::fs::metadata(&f).unwrap().len(), s["bytes"].as_u64().unwrap());
        }
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(html.contains("step-001.png") && !html.contains("/*TOUR_JSON*/"));
    }

    #[test]
    fn step_images_have_crop_applied_and_no_overlays() {
        let mut r = rec();
        // The frame used for the step (frame 1, 500 ms) has a pointer inside the crop.
        r.frames[1].cursor = Some((75, 40));
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let mut e = Edit::new(&r, OverlayStyle { keys: KeyStyle::Chips, clicks: ClickStyle::Both, halo: true, pointer: true });
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        e.scale = 50; // must be ignored
        // A right press and a key just before the frame time, so a ripple and a key chip would be visible on it.
        let ev = [down(480, MouseButton::Right, 75, 40), InputEvent { t_ms: 490, kind: InputKind::KeyDown("K".into()) }, left(750, 75, 40)];
        write_bundle(&r, &[], &ev, &e, "t", &dir, flat, |_| true).unwrap();
        let img = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (50, 80));
        assert!(img.pixels().all(|p| p.0 == [200, 200, 200, 255]), "an overlay was drawn on the step image");
    }

    #[test]
    fn refuses_a_folder_that_already_exists() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let ev = [left(150, 50, 40)];

        let full = tmp.path().join("full");
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("keep.txt"), "x").unwrap();
        let err = write_bundle(&r, &[], &ev, &edit(&r), "t", &full, flat, |_| true).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read_dir(&full).unwrap().count(), 1);
        assert!(full.join("keep.txt").exists());

        let empty = tmp.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let err = write_bundle(&r, &[], &ev, &edit(&r), "t", &empty, flat, |_| true).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(empty.is_dir());
        assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);

        let parent = tmp.path().join("no-such-parent");
        assert!(write_bundle(&r, &[], &ev, &edit(&r), "t", &parent.join("t"), flat, |_| true).is_err());
        assert!(!parent.exists());
    }

    #[test]
    fn preview_controls_are_keyboard_reachable_buttons() {
        assert!(PREVIEW_TEMPLATE.contains(r#"<button id="ring""#) && PREVIEW_TEMPLATE.contains(r#"aria-label="Next step""#));
        assert!(PREVIEW_TEMPLATE.contains(r#"<button id="restart""#));
        assert!(PREVIEW_TEMPLATE.contains(":focus-visible"));
    }

    #[test]
    fn no_clicks_leaves_no_folder_behind() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        assert!(write_bundle(&r, &[], &[], &edit(&r), "t", &dir, flat, |_| true).is_err());
        assert!(!dir.exists());
    }

    #[test]
    fn cancel_or_a_frame_error_removes_the_partial_bundle() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let ev = [left(750, 50, 40), left(1750, 10, 20)];
        let dir = tmp.path().join("cancelled");
        let err = write_bundle(&r, &[], &ev, &edit(&r), "t", &dir, flat, |_| false).unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dir.exists());
        let dir = tmp.path().join("broken");
        let mut n = 0;
        let failing = |_: usize| { n += 1; if n == 2 { Err("disk read failed".to_string()) } else { flat(0) } };
        assert_eq!(write_bundle(&r, &[], &ev, &edit(&r), "t", &dir, failing, |_| true).unwrap_err(), "disk read failed");
        assert!(!dir.exists());
    }

    #[test]
    fn keys_and_script_breakouts_never_reach_the_bundle() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let ev = [left(150, 50, 40), InputEvent { t_ms: 160, kind: InputKind::KeyDown("Hunter2Key".into()) }];
        write_bundle(&r, &[], &ev, &edit(&r), "</script><b>x", &dir, flat, |_| true).unwrap();
        let json = std::fs::read_to_string(dir.join("tour.json")).unwrap();
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(!json.contains("Hunter2Key") && !html.contains("Hunter2Key"));
        assert!(!html.contains("</script><b>x"));
        assert_eq!(read_json(&dir)["title"], "</script><b>x");
    }

    use mythicshot_editor::{Item, Rgba, Shape, Style, pt};
    fn pinned(shape: Shape, src: u32) -> Item {
        Item { id: 0, shape, style: Style { stroke: Rgba([255, 0, 0, 255]), fill: None, width: 3.0, font_size: 20.0, shadow: false }, frames: None, pin: Some(src) }
    }
    fn text(x: f32, y: f32, s: &str, src: u32) -> Item { pinned(Shape::Text { at: pt(x, y), text: s.into() }, src) }
    fn spot(b: Bounds, src: u32) -> Item { pinned(Shape::Spotlight { rect: b, dim: 60 }, src) }
    const CLICKS: [(u64, i32, i32); 2] = [(750, 50, 40), (1750, 10, 20)]; // steps on frames 1, 3, then the end step on 4
    fn clicks() -> Vec<InputEvent> { CLICKS.iter().map(|&(t, x, y)| left(t, x, y)).collect() }

    #[test]
    fn pinned_text_and_spotlight_become_layer_data_as_percentages() {
        let r = rec();
        let items = [text(10.0, 20.0, "Unpaid invoices", 1), spot(Bounds { x0: 20.0, y0: 20.0, x1: 70.0, y1: 60.0 }, 1)];
        let l = step_layer(&r, &edit(&r), &items, 1);
        assert_eq!(l.spotlight, Some(Area { x: 20.0, y: 25.0, w: 50.0, h: 50.0 }));
        assert_eq!(l.notes, vec![Note::Text { x: 10.0, y: 25.0, text: "Unpaid invoices".into(), size: 25.0, color: "#ff0000".into() }]);
        assert_eq!(step_layer(&r, &edit(&r), &items, 3), StepLayer::default());
        assert_eq!(tour_dim(&r, &edit(&r), &items, &[1, 3, 4]), Some(60));
        assert_eq!(tour_dim(&r, &edit(&r), &items, &[3, 4]), None);
    }

    #[test]
    fn bubbles_carry_their_box_tail_and_fill() {
        let r = rec();
        let mut b = pinned(Shape::Bubble { rect: Bounds { x0: 50.0, y0: 40.0, x1: 90.0, y1: 60.0 }, tail: pt(25.0, 20.0), text: "Here".into() }, 1);
        b.style.fill = None; // bubbles default to white
        let l = step_layer(&r, &edit(&r), &[b], 1);
        assert_eq!(l.notes, vec![Note::Bubble { x: 50.0, y: 50.0, w: 40.0, h: 25.0, tail: Point { x: 25.0, y: 25.0 }, text: "Here".into(), size: 25.0, color: "#ff0000".into(), fill: "#ffffff".into() }]);
    }

    #[test]
    fn crop_clips_the_spotlight_and_drops_notes_outside_it() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        let items = [text(10.0, 20.0, "gone", 1), text(60.0, 20.0, "kept", 1), spot(Bounds { x0: 20.0, y0: 20.0, x1: 70.0, y1: 60.0 }, 1)];
        let l = step_layer(&r, &e, &items, 1);
        assert_eq!(l.spotlight, Some(Area { x: 0.0, y: 25.0, w: 40.0, h: 50.0 }));
        assert_eq!(l.notes.len(), 1);
        assert!(matches!(&l.notes[0], Note::Text { x, text, .. } if *x == 20.0 && text == "kept"));
        // A spotlight wholly outside the crop is dropped.
        let l = step_layer(&r, &e, &[spot(Bounds { x0: 0.0, y0: 0.0, x1: 40.0, y1: 40.0 }, 1)], 1);
        assert_eq!(l.spotlight, None);
    }

    #[test]
    fn layer_items_are_data_and_everything_else_is_burned_into_its_own_step() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let mut cover = pinned(Shape::Rect(Bounds { x0: 0.0, y0: 0.0, x1: 20.0, y1: 20.0 }), 1);
        cover.style.fill = Some(Rgba([255, 0, 0, 255]));
        let items = [text(40.0, 40.0, "note", 1), spot(Bounds { x0: 30.0, y0: 30.0, x1: 90.0, y1: 70.0 }, 1), cover];
        write_bundle(&r, &items, &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let one = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert_eq!(one.get_pixel(10, 10).0, [255, 0, 0, 255], "the pinned box is burned in");
        assert_eq!(one.get_pixel(95, 75).0, [200, 200, 200, 255], "the pinned spotlight is not");
        assert_eq!(one.get_pixel(45, 45).0, [200, 200, 200, 255], "the pinned text is not");
        let two = image::open(dir.join("step-002.png")).unwrap().to_rgba8();
        assert!(two.pixels().all(|p| p.0 == [200, 200, 200, 255]), "pinned items stay on their own step");
        let j = read_json(&dir);
        assert_eq!(j["version"], 3);
        assert_eq!(j["spotlight_dim"], 60);
        assert_eq!(j["steps"][0]["spotlight"]["w"], 60.0);
        assert_eq!(j["steps"][0]["notes"][0]["kind"], "text");
        assert!(j["steps"][1]["spotlight"].is_null());
        assert_eq!(j["steps"][1]["notes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn an_unpinned_spotlight_is_burned_in_and_a_plain_recording_has_an_empty_layer() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let mut s = spot(Bounds { x0: 30.0, y0: 30.0, x1: 90.0, y1: 70.0 }, 0);
        s.pin = None; // on every frame, as drawn in Timeline mode with its range stretched
        write_bundle(&r, &[s], &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let one = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert_eq!(one.get_pixel(5, 5).0, [80, 80, 80, 255]);
        let j = read_json(&dir);
        assert!(j.get("spotlight_dim").is_none());
        assert!(j["steps"].as_array().unwrap().iter().all(|s| s["spotlight"].is_null() && s["notes"].as_array().unwrap().is_empty()));
    }

    #[test]
    fn pinned_items_never_show_in_timeline_mode_or_file_exports() {
        let items = [text(10.0, 20.0, "tour only", 1)];
        assert!(crate::video::visible(&items, 1).is_empty());
        assert_eq!(crate::video::step_items(&items, 1).len(), 1);
        assert!(crate::video::step_items(&items, 3).is_empty());
    }

    #[test]
    fn removed_and_trimmed_steps_drop_out_with_their_layer_and_come_back() {
        let r = rec();
        let items = [text(10.0, 20.0, "Sticky", 1)];
        let mut e = edit(&r);
        e.tour_removed.insert(1);
        assert_eq!(shape(&plan_steps(&r, &e, &clicks()).unwrap()), vec![(3, Some((10.0, 25.0))), (4, None)]);
        e.tour_removed.clear();
        e.trim = (2, 4);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &items, &clicks(), &e, "t", &dir, flat, |_| true).unwrap();
        assert!(!std::fs::read_to_string(dir.join("tour.json")).unwrap().contains("Sticky"));
        e.trim = (0, 4);
        assert_eq!(step_layer(&r, &e, &items, 1).notes.len(), 1);
    }

    #[test]
    fn removing_every_step_is_an_error_and_writes_nothing() {
        let r = rec();
        let mut e = edit(&r);
        e.tour_removed.extend([1, 3, 4]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let err = write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).unwrap_err();
        assert_eq!(err, "Every step has been removed from the tour.");
        assert!(!dir.exists());
    }

    #[test]
    fn note_size_never_exceeds_100() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 0.0, y0: 0.0, x1: 100.0, y1: 10.0 });
        let l = step_layer(&r, &e, &[text(5.0, 2.0, "big", 1)], 1);
        assert!(matches!(&l.notes[0], Note::Text { size, .. } if *size == 100.0), "{:?}", l.notes);
    }

    #[test]
    fn rounding_never_pushes_a_box_past_the_edge() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 0.0, y0: 0.0, x1: 3.0, y1: 3.0 });
        // 0.00075 px is 0.03 percent of 3 px; the box to the edge is 99.975, which rounds up to 99.98.
        let b = Bounds { x0: 0.00075, y0: 0.00075, x1: 10.0, y1: 10.0 };
        let bubble = pinned(Shape::Bubble { rect: b, tail: pt(1.0, 1.0), text: "Edge".into() }, 1);
        let l = step_layer(&r, &e, &[bubble, spot(b, 1)], 1);
        let Note::Bubble { x, y, w, h, size, .. } = l.notes[0].clone() else { panic!("{:?}", l.notes) };
        assert!(x + w <= 100.0 && y + h <= 100.0, "{x} + {w}, {y} + {h}");
        assert!(size > 0.0 && size <= 100.0);
        let a = l.spotlight.unwrap();
        assert!(a.x + a.w <= 100.0 && a.y + a.h <= 100.0, "{a:?}");
    }

    #[test]
    fn tour_dim_counts_only_exported_spotlights_and_stays_in_range() {
        let r = rec();
        let dimmed = |b: Bounds, src: u32, dim: u8| pinned(Shape::Spotlight { rect: b, dim }, src);
        let inside = Bounds { x0: 60.0, y0: 10.0, x1: 90.0, y1: 40.0 };
        let left_half = Bounds { x0: 0.0, y0: 0.0, x1: 40.0, y1: 40.0 };
        assert_eq!(tour_dim(&r, &edit(&r), &[dimmed(inside, 1, 0)], &[1, 3, 4]), Some(10));
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        assert_eq!(tour_dim(&r, &e, &[dimmed(left_half, 1, 90), dimmed(inside, 3, 60)], &[1, 3, 4]), Some(60));
        assert_eq!(tour_dim(&r, &edit(&r), &[dimmed(inside, 1, 80), dimmed(left_half, 1, 40)], &[1, 3, 4]), Some(40));
    }

    #[test]
    fn empty_or_blank_text_is_not_a_note() {
        let r = rec();
        let blank = pinned(Shape::Bubble { rect: Bounds { x0: 50.0, y0: 40.0, x1: 90.0, y1: 60.0 }, tail: pt(25.0, 20.0), text: " \n\t".into() }, 1);
        let l = step_layer(&r, &edit(&r), &[text(10.0, 20.0, "", 1), text(10.0, 30.0, "   ", 1), blank], 1);
        assert!(l.notes.is_empty(), "{:?}", l.notes);
    }

    #[test]
    fn a_deleted_step_frame_drops_its_notes_and_undeleting_brings_them_back() {
        let r = rec();
        let items = [text(10.0, 20.0, "Sticky", 1)];
        let mut e = edit(&r);
        e.deleted.insert(1);
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        write_bundle(&r, &items, &clicks(), &e, "t", &gone, flat, |_| true).unwrap();
        assert!(!std::fs::read_to_string(gone.join("tour.json")).unwrap().contains("Sticky"));
        e.deleted.clear();
        let back = tmp.path().join("back");
        write_bundle(&r, &items, &clicks(), &e, "t", &back, flat, |_| true).unwrap();
        assert_eq!(read_json(&back)["steps"][0]["notes"][0]["text"], "Sticky");
    }

    #[test]
    fn preview_draws_the_layer_from_data_as_plain_text() {
        assert!(PREVIEW_TEMPLATE.contains("spotlight_dim") && PREVIEW_TEMPLATE.contains(".notes"));
        assert!(PREVIEW_TEMPLATE.contains("textContent"));
        for banned in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write"] {
            assert!(!PREVIEW_TEMPLATE.contains(banned), "{banned} must not be used");
        }
        assert!(PREVIEW_TEMPLATE.contains("prefers-reduced-motion"));
    }

    #[test]
    fn preview_notes_are_clipped_to_the_image_and_announced() {
        assert!(PREVIEW_TEMPLATE.contains(r#"<div id="layer" aria-live="polite">"#));
        assert!(PREVIEW_TEMPLATE.contains("#layer { position: absolute; inset: 0; z-index: 2; overflow: hidden;"));
    }

    #[test]
    fn note_text_cannot_break_out_of_the_preview_page() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let items = [text(10.0, 20.0, "</script><img src=x onerror=alert(1)>", 1)];
        write_bundle(&r, &items, &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(!html.contains("</script><img"));
        assert_eq!(read_json(&dir)["steps"][0]["notes"][0]["text"], "</script><img src=x onerror=alert(1)>");
    }

    const HOSTILE: &str = "</script><img src=x onerror=alert(1)>";
    fn card(title: &str, body: &str) -> Card { Card { title: title.into(), body: body.into(), next_label: String::new(), anchor: Anchor::Hotspot, point: None, placement: Placement::Auto } }

    #[test]
    fn a_plain_recording_writes_version_3_with_only_defaults() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let j = read_json(&dir);
        assert_eq!(j["version"], 3);
        assert_eq!(j["theme"], serde_json::json!({"card_style":"dark","accent":"#5dcaa5","hotspot":"pulse","progress_bar":true,"backdrop":"auto","spotlight_soft":true}));
        assert_eq!(j["zoom"], serde_json::json!({"enabled":true,"percent":118}));
        assert_eq!(j["autoplay"], serde_json::json!({"enabled":false,"seconds":5}));
        assert!(j["cover"].is_null() && j["cta"].is_null() && j["end"].is_null());
        for s in j["steps"].as_array().unwrap() {
            assert!(s["card"].is_null() && s["zoom"].is_null());
            assert_eq!(s["advance"], "hotspot");
        }
        assert_eq!(j["steps"][0]["click"]["x"], 50.0); // unchanged from version 2
    }

    #[test]
    fn cards_export_per_step_with_their_anchor_resolved() {
        let r = rec();
        let mut e = edit(&r);
        e.tour_doc.steps.insert(1, StepExtras { card: Some(card("Unpaid invoices", "One click away.\n**Try it**")), advance: Advance::Anywhere, zoom: Some(false) });
        let mut end = card("Done", "");
        end.next_label = "Finish".into();
        e.tour_doc.steps.insert(4, StepExtras { card: Some(end), ..Default::default() }); // the end step has no click
        let mut picked = card("Here", "");
        picked.anchor = Anchor::Point;
        picked.point = Some(Point { x: 20.0, y: 40.0 });
        e.tour_doc.steps.insert(3, StepExtras { card: Some(picked), ..Default::default() });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).unwrap();
        let j = read_json(&dir);
        assert_eq!(j["steps"][0]["card"], serde_json::json!({"title":"Unpaid invoices","body":"One click away.\n**Try it**","next_label":"Next","anchor":"hotspot","point":null,"placement":"auto"}));
        assert_eq!(j["steps"][0]["advance"], "anywhere");
        assert_eq!(j["steps"][0]["zoom"], false);
        assert_eq!(j["steps"][1]["card"]["anchor"], "point");
        assert_eq!(j["steps"][1]["card"]["point"], serde_json::json!({"x":20.0,"y":50.0}));
        assert_eq!(j["steps"][2]["card"]["anchor"], "none");
        assert_eq!(j["steps"][2]["card"]["next_label"], "Finish");
        // No card ever reaches the pixels.
        let img = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert!(img.pixels().all(|p| p.0 == [200, 200, 200, 255]));
    }

    #[test]
    fn a_card_is_dropped_when_empty_and_its_point_when_cropped_away() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        e.tour_doc.steps.insert(1, StepExtras { card: Some(card("  ", "")), ..Default::default() });
        assert_eq!(step_card(&r, &e, 1, Some((50.0, 50.0))), None);
        let mut c = card("Here", "");
        c.anchor = Anchor::Point;
        c.point = Some(Point { x: 20.0, y: 40.0 }); // left of the crop
        e.tour_doc.steps.insert(1, StepExtras { card: Some(c), ..Default::default() });
        assert_eq!(step_card(&r, &e, 1, Some((50.0, 50.0))).unwrap().anchor, Anchor::None);
    }

    #[test]
    fn only_https_links_survive() {
        assert_eq!(safe_url(" https://tours.example.com/book "), Some("https://tours.example.com/book".into()));
        for bad in ["javascript:alert(1)", "http://tours.example.com", "https://", "https://a b.example", "//tours.example.com", "data:text/html,x", "HTTPS://\u{0}x"] {
            assert_eq!(safe_url(bad), None, "{bad}");
        }
        assert_eq!(safe_url(&format!("https://tours.example.com/{}", "a".repeat(500))), None);
        for bad in ["https://@", "https://:443", "https://user:pw@evil.example", "https://tours.example.com@evil.example/", "https://tours\u{202E}.example.com", "https://tours\u{200B}example.com", "https://tours.example.com/\u{202E}x", "https://tours..example.com", "https://.example.com", "https://tours.example.com:", "https://tours.example.com:123456", "https://tours.example.com:44a", "https://tours_example.com", "https://tours.example.com\\@evil.example"] {
            assert_eq!(safe_url(bad), None, "{bad:?}");
        }
        assert_eq!(safe_url("HTTPS://Tours.example.com:8443/a?b=c#d"), Some("https://Tours.example.com:8443/a?b=c#d".into()));
        assert_eq!(safe_url("https://localhost"), None);
    }

    #[test]
    fn text_is_cleaned_before_it_is_capped() {
        let r = rec();
        let mut e = edit(&r);
        let mut c = card("A\u{202E}B\r\nC\u{0}\u{FEFF}", "x\r\ny\rz\u{2066}\n**w**");
        c.next_label = "Go\non\u{200B}".into();
        e.tour_doc.steps.insert(1, StepExtras { card: Some(c), ..Default::default() });
        let got = step_card(&r, &e, 1, Some((50.0, 50.0))).unwrap();
        assert_eq!((got.title.as_str(), got.body.as_str(), got.next_label.as_str()), ("AB C", "x\ny\nz\n**w**", "Go on"));
        // Caps count code points after cleaning: invisible characters never use up the cap.
        let long = format!("{}{}", "\u{200B}".repeat(10), "a".repeat(90));
        e.tour_doc.steps.insert(1, StepExtras { card: Some(card(&long, "")), ..Default::default() });
        assert_eq!(step_card(&r, &e, 1, None).unwrap().title, "a".repeat(80));
        let mut d = TourDoc::default();
        d.cover = Some(Cover { title: "Line\none".into(), body: "Para\r\ntwo".into(), button: "Sta\u{202D}rt\n".into(), logo: None });
        d.cta = Some(Link { label: "Book\r\na call".into(), url: "https://tours.example.com".into() });
        d.end = Some(EndCard { text: "Thanks\r\nBye\u{0}".into(), primary: None, secondary: None, replay: true });
        let s = sanitize(&d);
        let cover = s.cover.unwrap();
        assert_eq!((cover.title.as_str(), cover.body.as_str(), cover.button.as_str()), ("Line one", "Para\ntwo", "Start"));
        assert_eq!(s.cta.unwrap().label, "Book a call");
        assert_eq!(s.end.unwrap().text, "Thanks\nBye");
    }

    #[test]
    fn sanitize_cuts_clamps_and_drops() {
        let mut d = TourDoc::default();
        d.theme.accent = "RED;background:url(//x)".into();
        d.theme.backdrop = "#AABBCC".into();
        d.zoom.percent = 400;
        d.autoplay.seconds = 0;
        d.cta = Some(Link { label: "Book a call".into(), url: "javascript:alert(1)".into() });
        d.cover = Some(Cover { title: " ".into(), body: "x".into(), button: String::new(), logo: None });
        d.end = Some(EndCard { text: "t".repeat(300), primary: Some(Link { label: "L".repeat(40), url: "https://tours.example.com".into() }), secondary: Some(Link { label: "x".into(), url: "http://tours.example.com".into() }), replay: true });
        let s = sanitize(&d);
        assert_eq!(s.theme.accent, "#5dcaa5");
        assert_eq!(s.theme.backdrop, "#aabbcc");
        assert_eq!((s.zoom.percent, s.autoplay.seconds), (140, 2));
        assert!(s.cta.is_none() && s.cover.is_none());
        let end = s.end.unwrap();
        assert_eq!(end.text.chars().count(), 200);
        assert_eq!(end.primary.unwrap().label.chars().count(), 24);
        assert!(end.secondary.is_none());
    }

    #[test]
    fn hostile_text_is_kept_as_data_in_every_new_field() {
        let r = rec();
        let mut e = edit(&r);
        let mut c = card(HOSTILE, HOSTILE);
        c.next_label = "<b>go</b>".into();
        e.tour_doc.steps.insert(1, StepExtras { card: Some(c), ..Default::default() });
        e.tour_doc.cover = Some(Cover { title: HOSTILE.into(), body: HOSTILE.into(), button: "<i>".into(), logo: None });
        e.tour_doc.cta = Some(Link { label: HOSTILE.chars().take(24).collect(), url: "https://tours.example.com".into() });
        let short: String = HOSTILE.chars().take(24).collect();
        let button = |url: &str| Some(Link { label: short.clone(), url: url.into() });
        e.tour_doc.end = Some(EndCard { text: HOSTILE.into(), primary: button("https://tours.example.com/a"), secondary: button("https://tours.example.com/b"), replay: true });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).unwrap();
        let j = read_json(&dir);
        assert_eq!(j["steps"][0]["card"]["title"], HOSTILE);
        assert_eq!(j["steps"][0]["card"]["body"], HOSTILE);
        assert_eq!(j["steps"][0]["card"]["next_label"], "<b>go</b>");
        assert_eq!(j["cover"]["title"], HOSTILE);
        assert_eq!(j["cover"]["body"], HOSTILE);
        assert_eq!(j["cover"]["button"], "<i>");
        assert_eq!(j["cta"]["label"], short.as_str());
        assert_eq!(j["end"]["text"], HOSTILE);
        assert_eq!(j["end"]["primary"]["label"], short.as_str());
        assert_eq!(j["end"]["secondary"]["label"], short.as_str());
        assert!(!std::fs::read_to_string(dir.join("index.html")).unwrap().contains("</script><img"));
    }

    #[test]
    fn the_logo_is_checked_shrunk_and_named() {
        let tmp = tempfile::tempdir().unwrap();
        let html = tmp.path().join("logo.png");
        std::fs::write(&html, b"<html>").unwrap();
        assert!(load_logo(&html).unwrap_err().contains("PNG or JPEG"));
        let big = tmp.path().join("big.png");
        RgbaImage::from_pixel(4000, 1000, image::Rgba([10, 20, 30, 255])).save(&big).unwrap();
        let (bytes, ext) = load_logo(&big).unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!((img.width(), img.height(), ext), (512, 128, "png"));
        assert!(bytes.len() <= LOGO_MAX_BYTES);
        // A bad logo stops the export before any folder exists.
        let r = rec();
        let mut e = edit(&r);
        e.tour_doc.cover = Some(Cover { title: "Hi".into(), body: String::new(), button: String::new(), logo: None });
        e.tour_doc.logo_path = Some(html);
        let dir = tmp.path().join("t");
        assert!(write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).is_err());
        assert!(!dir.exists());
        // A good one lands in the bundle and in the manifest.
        e.tour_doc.logo_path = Some(big);
        write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).unwrap();
        assert_eq!(read_json(&dir)["cover"]["logo"], "logo.png");
        assert!(dir.join("logo.png").exists());
    }

    #[test]
    fn a_logo_over_the_byte_cap_after_shrinking_is_refused_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let noisy = tmp.path().join("noisy.png");
        let mut s = 7u32;
        RgbaImage::from_fn(512, 512, |_, _| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            image::Rgba([(s >> 24) as u8, (s >> 16) as u8, (s >> 8) as u8, 255])
        })
        .save(&noisy)
        .unwrap();
        assert!(load_logo(&noisy).unwrap_err().contains("200 KB"));
        let r = rec();
        let mut e = edit(&r);
        e.tour_doc.cover = Some(Cover { title: "Hi".into(), body: String::new(), button: String::new(), logo: None });
        e.tour_doc.logo_path = Some(noisy);
        let dir = tmp.path().join("t");
        assert!(write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).is_err());
        assert!(!dir.exists());
    }

    #[test]
    fn a_logo_over_8192_pixels_is_refused_before_decoding() {
        let tmp = tempfile::tempdir().unwrap();
        let wide = tmp.path().join("wide.png");
        RgbaImage::from_pixel(8193, 2, image::Rgba([1, 2, 3, 255])).save(&wide).unwrap();
        assert!(load_logo(&wide).unwrap_err().contains("8192"));
    }

    #[test]
    fn a_jpeg_logo_is_turned_upright_by_its_exif_orientation() {
        use image::ImageEncoder;
        let tmp = tempfile::tempdir().unwrap();
        let photo = tmp.path().join("photo.jpg");
        // A little-endian TIFF block with one entry: Orientation (0x0112) = 6, rotate 90 degrees clockwise.
        let exif: Vec<u8> = [&b"II*\0"[..], &8u32.to_le_bytes(), &1u16.to_le_bytes(), &0x0112u16.to_le_bytes(), &3u16.to_le_bytes(), &1u32.to_le_bytes(), &6u16.to_le_bytes(), &[0, 0], &0u32.to_le_bytes()].concat();
        let rgb = image::RgbImage::from_pixel(40, 20, image::Rgb([90, 120, 150]));
        let mut bytes = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90);
        enc.set_exif_metadata(exif).unwrap();
        enc.write_image(rgb.as_raw(), 40, 20, image::ExtendedColorType::Rgb8).unwrap();
        std::fs::write(&photo, &bytes).unwrap();
        let (out, ext) = load_logo(&photo).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height(), ext), (20, 40, "jpg"));
    }

    #[test]
    fn preview_inlines_the_shared_rules_and_uses_them() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(!html.contains("/*TOUR_CORE*/") && html.contains("placeCard") && html.contains("zoomOffset"));
        for needed in ["TourCore.safeUrl", "TourCore.parseBold", "prefers-reduced-motion", "noopener noreferrer", "cubic-bezier(.22,.8,.26,1)"] {
            assert!(PREVIEW_TEMPLATE.contains(needed), "{needed}");
        }
        for banned in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "localStorage", "sessionStorage", "document.cookie", "fetch(", "XMLHttpRequest"] {
            assert!(!PREVIEW_TEMPLATE.contains(banned), "{banned} must not be used");
        }
    }

    #[test]
    fn preview_still_reads_a_version_2_manifest() {
        // The page must not require any version 3 field.
        for field in ["TOUR.theme.", "TOUR.zoom.", "TOUR.autoplay.", "TOUR.cover.", "TOUR.end.", "TOUR.cta."] {
            assert!(!PREVIEW_TEMPLATE.contains(field), "{field} is read without a fallback");
        }
    }

    #[test]
    fn the_shared_rules_cannot_break_out_of_their_script_block() {
        // Inlined as-is, so the rules file must never close its script or hold the data placeholder.
        let lower = TOUR_CORE_JS.to_ascii_lowercase();
        assert!(!lower.contains("</script") && !lower.contains("<!--"));
        assert!(!TOUR_CORE_JS.contains(JSON_PLACEHOLDER) && !TOUR_CORE_JS.contains(CORE_PLACEHOLDER));
        // The rules come before the page script that uses them, and a title holding the
        // placeholder text stays data.
        let page = preview_page(r#"{"title":"/*TOUR_CORE*/","steps":[]}"#);
        let core_at = page.find("var TourCore").unwrap();
        let data_at = page.find("const TOUR = ").unwrap();
        assert!(core_at < data_at);
        assert!(page.contains(r#""title":"/*TOUR_CORE*/""#));
    }

    #[test]
    fn preview_links_open_safely_and_only_known_logo_names_load() {
        assert!(PREVIEW_TEMPLATE.contains(r#""_blank""#) && PREVIEW_TEMPLATE.contains("no-referrer"));
        assert!(PREVIEW_TEMPLATE.contains(r#""logo.png""#) && PREVIEW_TEMPLATE.contains(r#""logo.jpg""#));
        // Autoplay has a visible, keyboard-reachable pause control.
        assert!(PREVIEW_TEMPLATE.contains(r#"<button id="autoplay""#));
        assert!(PREVIEW_TEMPLATE.contains("Pause autoplay"));
        // Kept from version 2.
        assert!(PREVIEW_TEMPLATE.contains("Local preview"));
    }

    #[test]
    fn preview_allows_no_network_and_only_its_own_step_images() {
        let csp = PREVIEW_TEMPLATE.split("Content-Security-Policy\" content=\"").nth(1).expect("a CSP meta tag").split('"').next().unwrap();
        assert!(PREVIEW_TEMPLATE.find("Content-Security-Policy").unwrap() < PREVIEW_TEMPLATE.find("<script").unwrap());
        for part in ["default-src 'none'", "script-src 'unsafe-inline'", "style-src 'unsafe-inline'", "img-src 'self'", "base-uri 'none'", "form-action 'none'"] {
            assert!(csp.contains(part), "{part}");
        }
        assert!(!csp.contains("http") && !csp.contains('*') && !csp.contains("data:") && !csp.contains("connect-src"), "{csp}");
        // Step images are taken only by the bundle's own names, in the page and in the preload.
        assert!(PREVIEW_TEMPLATE.contains(r"/^step-[0-9]{3,}\.(png|jpg)$/"));
        assert!(!PREVIEW_TEMPLATE.contains("s.image;") && !PREVIEW_TEMPLATE.contains("= ahead.image"));
    }

    #[test]
    fn preview_draws_the_compact_bar_as_a_light_pill_and_aims_cards_at_a_picked_spot() {
        let compact = PREVIEW_TEMPLATE.split("#card.compact {").nth(1).unwrap().split('}').next().unwrap();
        assert!(compact.contains("border-radius: 999px") && compact.contains("background: #fcfcfd") && compact.contains("color: #1b2029"), "{compact}");
        assert!(PREVIEW_TEMPLATE.contains(r#"var palette = style() === "compact" ? "light" : style();"#));
        assert!(PREVIEW_TEMPLATE.contains("TourCore.placeCard(W, H, w, h, avoid, cardData.placement, aim)"));
        assert!(PREVIEW_TEMPLATE.contains(r#"picked: anchor === "point" && !!point"#));
    }

    /// A recording with `n` clicks, one per frame, the last on the last frame: exactly `n` steps.
    fn many(n: usize) -> (Recording, Vec<InputEvent>) {
        let times: Vec<u64> = (0..n as u64).map(|i| i * 500).collect();
        let r = Recording::timing_only(100, 80, &times, n as u64 * 500);
        let ev = (0..n as u64).map(|i| left(i * 500 + 250, 50, 40)).collect();
        (r, ev)
    }

    #[test]
    fn more_than_200_steps_is_refused_with_the_limit_and_the_count() {
        let (r, ev) = many(201);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let err = write_bundle(&r, &[], &ev, &edit(&r), "t", &dir, flat, |_| true).unwrap_err();
        assert!(err.contains("200") && err.contains("201"), "{err}");
        assert!(!dir.exists(), "nothing is written");
        let (r, ev) = many(200);
        assert_eq!(write_bundle(&r, &[], &ev, &edit(&r), "t", &dir, flat, |_| true).unwrap().steps, 200);
    }

    #[test]
    fn more_than_20_notes_on_a_step_is_refused_with_the_limit_and_the_count() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let notes: Vec<Item> = (0..21).map(|i| text(5.0 + i as f32, 20.0, "n", 3)).collect();
        let err = write_bundle(&r, &notes, &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap_err();
        assert!(err.contains("Step 2") && err.contains("21") && err.contains("20"), "{err}");
        assert!(!dir.exists());
        write_bundle(&r, &notes[..20], &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        assert_eq!(read_json(&dir)["steps"][1]["notes"].as_array().unwrap().len(), 20);
    }

    #[test]
    fn the_preview_pulses_as_often_as_the_tour_view() {
        assert!(PREVIEW_TEMPLATE.contains(&format!("--pulse-period: {}s;", crate::tour_view::PULSE_PERIOD)));
        assert!(PREVIEW_TEMPLATE.contains("animation: softpulse var(--pulse-period)"));
    }

    #[test]
    fn a_manifest_over_the_servers_size_is_refused_before_any_image_is_read() {
        let (r, ev) = many(200);
        let long = "a".repeat(500);
        let items: Vec<Item> = (0..200u32).flat_map(|s| (0..5).map(move |i| (s, i))).map(|(s, i)| text(5.0 + i as f32, 20.0, &long, s)).collect();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let mut read = 0;
        let counting = |i: usize| { read += 1; flat(i) };
        let err = write_bundle(&r, &items, &ev, &edit(&r), "t", &dir, counting, |_| true).unwrap_err();
        assert!(err.contains("256 KB"), "{err}");
        assert!(!dir.exists());
        assert_eq!(read, 0, "refused before any step image was read or encoded");
    }

    #[test]
    fn the_title_and_notes_are_cleaned_like_every_other_text() {
        assert_eq!(tour_title("My\ttour\r\nnow\u{2028}\u{202E}!"), "My tour now !");
        assert_eq!(tour_title(&"t".repeat(300)).chars().count(), 200);
        assert_eq!(tour_title(" \u{200B}\t"), "Untitled tour", "the server needs a title");
        let r = rec();
        let bubble = pinned(Shape::Bubble { rect: Bounds { x0: 50.0, y0: 40.0, x1: 90.0, y1: 60.0 }, tail: pt(25.0, 20.0), text: "One\u{2029}two\t3\u{0}".into() }, 1);
        let l = step_layer(&r, &edit(&r), &[text(10.0, 20.0, "A\r\nB\u{2028}C\u{FEFF}", 1), text(10.0, 30.0, "\u{200B}\u{200B}", 1), text(10.0, 40.0, &"x".repeat(700), 1), bubble], 1);
        let texts: Vec<String> = l.notes.iter().map(|n| match n { Note::Text { text, .. } | Note::Bubble { text, .. } => text.clone() }).collect();
        assert_eq!(texts, vec!["A\nB\nC".to_string(), "x".repeat(500), "One\ntwo 3".to_string()], "an invisible-only note is dropped");
    }

    #[test]
    fn a_bubble_the_crop_leaves_with_no_width_or_height_is_dropped() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 0.0, y0: 0.0, x1: 50.0, y1: 80.0 });
        let flat_bubble = |b: Bounds| pinned(Shape::Bubble { rect: b, tail: pt(10.0, 10.0), text: "Hi".into() }, 1);
        let zero_w = flat_bubble(Bounds { x0: 20.0, y0: 20.0, x1: 20.0, y1: 40.0 });
        let zero_h = flat_bubble(Bounds { x0: 20.0, y0: 30.0, x1: 40.0, y1: 30.0 });
        let kept = flat_bubble(Bounds { x0: 40.0, y0: 20.0, x1: 90.0, y1: 40.0 });
        let l = step_layer(&r, &e, &[zero_w, zero_h, kept], 1);
        assert_eq!(l.notes.len(), 1, "{:?}", l.notes);
        assert!(matches!(l.notes[0], Note::Bubble { w, h, .. } if w > 0.0 && h > 0.0));
    }

    #[test]
    fn a_tall_step_image_is_shrunk_and_a_heavy_one_reencoded_and_reported() {
        let r = Recording::timing_only(120, 5000, &[0, 500], 1000);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let tall = |_: usize| Ok(RgbaImage::from_pixel(120, 5000, image::Rgba([200, 200, 200, 255])));
        let sum = write_bundle(&r, &[], &[left(250, 60, 100)], &edit(&r), "t", &dir, tall, |_| true).unwrap();
        let j = read_json(&dir);
        assert_eq!((j["steps"][0]["width"].as_u64(), j["steps"][0]["height"].as_u64()), (Some(98), Some(4096)));
        let img = image::open(dir.join(j["steps"][0]["image"].as_str().unwrap())).unwrap();
        assert_eq!(img.height(), 4096);
        assert_eq!((sum.squeezed, sum.squeezed_note()), (0, None));
        let two = TourSummary { squeezed: 2, ..sum };
        assert_eq!(two.squeezed_note().unwrap(), "2 pictures were saved at lower quality to fit.");
        assert_eq!(TourSummary { squeezed: 1, ..two }.squeezed_note().unwrap(), "1 picture was saved at lower quality to fit.");
    }

    /// The server's caps, copied from `LIMITS` in the server's manifest rules (`src/manifest.ts` in the
    /// demo platform), so this test fails if the export ever writes what the server refuses.
    mod server {
        pub const STEPS: usize = 200;
        pub const IMAGE_BYTES: u64 = 2_000_000;
        pub const IMAGE_SIDE: u64 = 4096;
        pub const TITLE: usize = 200;
        pub const GENERATOR: usize = 100;
        pub const NOTES_PER_STEP: usize = 20;
        pub const NOTE_TEXT: usize = 500;
        pub const CARD_TITLE: usize = 80;
        pub const CARD_BODY: usize = 300;
        pub const NEXT_LABEL: usize = 24;
        pub const COVER_TITLE: usize = 80;
        pub const COVER_BODY: usize = 200;
        pub const COVER_BUTTON: usize = 24;
        pub const BUTTON_LABEL: usize = 24;
        pub const END_TEXT: usize = 200;
        pub const LINK: usize = 500;
        pub const MANIFEST_BYTES: usize = 262_144;
    }

    /// The server's text rule: a string with no control, format, line or paragraph separator characters
    /// (a line feed only when `multi`), already trimmed, from `min` to `max` code points.
    fn server_text(v: &serde_json::Value, min: usize, max: usize, multi: bool, what: &str) {
        let s = v.as_str().unwrap_or_else(|| panic!("{what} is not a string"));
        let n = s.chars().count();
        assert!((min..=max).contains(&n), "{what}: {n} characters, not {min} to {max}");
        assert_eq!(s.trim(), s, "{what} is not trimmed");
        for c in s.chars() {
            let format = matches!(c as u32, 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x890..=0x891 | 0x8E2 | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0xE0001 | 0xE0020..=0xE007F);
            let bad = (c.is_control() && !(multi && c == '\n')) || format || c == '\u{2028}' || c == '\u{2029}';
            assert!(!bad, "{what} holds U+{:04X}", c as u32);
        }
    }

    fn server_percent(v: &serde_json::Value, what: &str) -> f64 {
        let x = v.as_f64().unwrap_or_else(|| panic!("{what} is not a number"));
        assert!((0.0..=100.0).contains(&x), "{what} = {x}");
        x
    }

    fn server_box(o: &serde_json::Value, what: &str) {
        let (x, y, w, h) = (server_percent(&o["x"], what), server_percent(&o["y"], what), server_percent(&o["w"], what), server_percent(&o["h"], what));
        assert!(w > 0.0 && h > 0.0, "{what}: {w} x {h}");
        assert!(x + w <= 100.01 && y + h <= 100.01, "{what} ends outside the image");
    }

    fn server_link(o: &serde_json::Value, what: &str) {
        server_text(&o["label"], 1, server::BUTTON_LABEL, false, what);
        let url = o["url"].as_str().unwrap();
        assert!(url.starts_with("https://") && url.chars().count() <= server::LINK && !url.chars().any(char::is_whitespace), "{what}: {url}");
    }

    #[test]
    fn a_worst_case_tour_stays_inside_every_server_limit() {
        // Text that tries every rule at once: tabs, CR, LF, separators, controls, bidi, zero-width, emoji, markup.
        let nasty = "\t <b>Hi</b>\r\nthere\u{2028}now\u{2029}\u{202E}rtl\u{200B}\u{0}\u{7F}\u{85}\u{FEFF}😀 ";
        let long = |n: usize| nasty.repeat(n / nasty.chars().count() + 2);
        let (r, ev) = many(200);
        // Step 1 carries 20 notes at full length, a flat bubble (dropped, so not counted) and a spotlight out of range.
        let mut items: Vec<Item> = (0..10).map(|i| text(1.0 + i as f32 * 9.0, 70.0, &long(700), 0)).collect();
        items.extend((0..10).map(|i| pinned(Shape::Bubble { rect: Bounds { x0: 1.0 + i as f32 * 9.0, y0: 1.0, x1: 9.0 + i as f32 * 9.0, y1: 30.0 }, tail: pt(-50.0, 500.0), text: long(700) }, 0)));
        items.push(pinned(Shape::Bubble { rect: Bounds { x0: 99.0, y0: 50.0, x1: 99.0, y1: 60.0 }, tail: pt(1.0, 1.0), text: "flat".into() }, 0));
        items.push(pinned(Shape::Spotlight { rect: Bounds { x0: -40.0, y0: -40.0, x1: 400.0, y1: 400.0 }, dim: 255 }, 0));
        items.push(pinned(Shape::Spotlight { rect: Bounds { x0: 99.9, y0: 79.9, x1: 100.0, y1: 80.0 }, dim: 0 }, 1));
        let mut e = edit(&r);
        for (i, s) in plan_steps(&r, &e, &ev).unwrap().iter().enumerate() {
            let mut c = card(&long(120), &long(400));
            c.next_label = long(40);
            if i % 2 == 0 {
                c.anchor = Anchor::Point;
                c.point = Some(Point { x: if i % 4 == 0 { 99.99 } else { -5.0 }, y: 79.99 });
            }
            e.tour_doc.steps.insert(s.src, StepExtras { card: Some(c), advance: Advance::Anywhere, zoom: Some(true) });
        }
        e.tour_doc.theme.accent = "#GGGGGG".into();
        e.tour_doc.theme.backdrop = "url(x)".into();
        e.tour_doc.zoom.percent = 999;
        e.tour_doc.autoplay = Autoplay { enabled: true, seconds: 0 };
        e.tour_doc.cover = Some(Cover { title: long(120), body: long(300), button: long(40), logo: None });
        e.tour_doc.cta = Some(Link { label: long(40), url: format!(" https://tours.example.com/{} ", "p".repeat(470)) });
        let link = Some(Link { label: long(40), url: "HTTPS://tours.example.com:8443/a?b=c#d".into() });
        e.tour_doc.end = Some(EndCard { text: long(300), primary: link.clone(), secondary: link, replay: true });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &items, &ev, &e, &long(300), &dir, flat, |_| true).unwrap();

        let raw = std::fs::read_to_string(dir.join("tour.json")).unwrap();
        let j: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(serde_json::to_vec(&j).unwrap().len() <= server::MANIFEST_BYTES, "the manifest is over the server's size");
        assert_eq!(j["version"], 3);
        server_text(&j["generator"], 0, server::GENERATOR, false, "generator");
        server_text(&j["title"], 1, server::TITLE, false, "title");
        let dim = j["spotlight_dim"].as_u64().unwrap();
        assert!((10..=90).contains(&dim), "spotlight_dim {dim}");
        let t = &j["theme"];
        assert!(["dark", "light", "compact", "auto"].contains(&t["card_style"].as_str().unwrap()));
        assert!(["pulse", "dot", "none"].contains(&t["hotspot"].as_str().unwrap()));
        assert_eq!((t["accent"].as_str(), t["backdrop"].as_str()), (Some("#5dcaa5"), Some("auto")));
        let zoom = j["zoom"]["percent"].as_u64().unwrap();
        let secs = j["autoplay"]["seconds"].as_u64().unwrap();
        assert!((105..=140).contains(&zoom) && (2..=20).contains(&secs), "zoom {zoom}, autoplay {secs}");
        server_text(&j["cover"]["title"], 1, server::COVER_TITLE, false, "cover.title");
        server_text(&j["cover"]["body"], 0, server::COVER_BODY, true, "cover.body");
        server_text(&j["cover"]["button"], 1, server::COVER_BUTTON, false, "cover.button");
        server_link(&j["cta"], "cta");
        server_text(&j["end"]["text"], 0, server::END_TEXT, true, "end.text");
        server_link(&j["end"]["primary"], "end.primary");
        server_link(&j["end"]["secondary"], "end.secondary");

        let steps = j["steps"].as_array().unwrap();
        assert!(!steps.is_empty() && steps.len() <= server::STEPS, "{} steps", steps.len());
        assert_eq!(steps.len(), 200);
        let mut images = std::collections::HashSet::new();
        for (i, s) in steps.iter().enumerate() {
            let what = |f: &str| format!("steps[{i}].{f}");
            let image = s["image"].as_str().unwrap();
            assert!(image.len() == 12 && image.starts_with("step-") && image[5..8].bytes().all(|b| b.is_ascii_digit()) && (image.ends_with(".png") || image.ends_with(".jpg")), "{image}");
            assert!(images.insert(image.to_string()), "{image} repeats");
            for side in ["width", "height"] {
                assert!((1..=server::IMAGE_SIDE).contains(&s[side].as_u64().unwrap()), "{}", what(side));
            }
            let bytes = s["bytes"].as_u64().unwrap();
            assert!((1..=server::IMAGE_BYTES).contains(&bytes) && bytes == std::fs::metadata(dir.join(image)).unwrap().len(), "{}", what("bytes"));
            let click = &s["click"];
            assert!(!click.is_null(), "every step here has a click");
            server_percent(&click["x"], &what("click.x"));
            server_percent(&click["y"], &what("click.y"));
            if !s["spotlight"].is_null() {
                server_box(&s["spotlight"], &what("spotlight"));
            }
            let notes = s["notes"].as_array().unwrap();
            assert!(notes.len() <= server::NOTES_PER_STEP, "{}", what("notes"));
            for (k, n) in notes.iter().enumerate() {
                let w = what(&format!("notes[{k}]"));
                server_text(&n["text"], 1, server::NOTE_TEXT, true, &w);
                let size = n["size"].as_f64().unwrap();
                assert!(size > 0.0 && size <= 100.0, "{w}.size");
                match n["kind"].as_str().unwrap() {
                    "text" => {
                        server_percent(&n["x"], &w);
                        server_percent(&n["y"], &w);
                    }
                    "bubble" => {
                        server_box(n, &w);
                        server_percent(&n["tail"]["x"], &w);
                        server_percent(&n["tail"]["y"], &w);
                    }
                    other => panic!("{w}: kind {other}"),
                }
            }
            let c = &s["card"];
            server_text(&c["title"], 0, server::CARD_TITLE, false, &what("card.title"));
            server_text(&c["body"], 0, server::CARD_BODY, true, &what("card.body"));
            server_text(&c["next_label"], 1, server::NEXT_LABEL, false, &what("card.next_label"));
            match c["anchor"].as_str().unwrap() {
                "point" => {
                    server_percent(&c["point"]["x"], &what("card.point"));
                    server_percent(&c["point"]["y"], &what("card.point"));
                }
                "hotspot" | "none" => assert!(c["point"].is_null(), "{}", what("card.point")),
                other => panic!("{}: {other}", what("card.anchor")),
            }
            assert_eq!(s["advance"], "anywhere");
        }
        assert_eq!(steps[0]["notes"].as_array().unwrap().len(), 20, "the flat bubble was dropped");
    }

    // Single-file export.

    fn unbase64(s: &str) -> Vec<u8> {
        let val = |c: u8| BASE64.iter().position(|&b| b == c).unwrap_or_else(|| panic!("{:?} is not base64", c as char)) as u32;
        let s = s.as_bytes();
        assert_eq!(s.len() % 4, 0, "base64 length");
        let mut out = Vec::new();
        for q in s.chunks(4) {
            let pad = q.iter().filter(|&&c| c == b'=').count();
            let n = q.iter().take(4 - pad).enumerate().fold(0u32, |n, (i, &c)| n | (val(c) << (18 - 6 * i)));
            out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8][..3 - pad]);
        }
        out
    }

    const BLOCK_START: &str = "<script type=\"application/octet-stream\"";

    /// The single file's picture blocks as (name, type, base64), and the page without them.
    fn image_blocks(html: &str) -> (Vec<(String, String, String)>, String) {
        let (mut blocks, mut page, mut rest) = (Vec::new(), String::new(), html);
        while let Some(i) = rest.find(BLOCK_START) {
            page.push_str(&rest[..i]);
            let (open, more) = rest[i..].split_once('>').unwrap();
            let attr = |k: &str| open.split(&format!("{k}=\"")).nth(1).unwrap().split('"').next().unwrap().to_string();
            let (body, more) = more.split_once(IMAGE_BLOCK_CLOSE).expect("a closed block");
            blocks.push((attr("data-tour-image"), attr("data-type"), body.to_string()));
            rest = more;
        }
        page.push_str(rest);
        (blocks, page)
    }

    /// The single file's pictures as `data:` URLs by name, and its manifest, read back as data.
    fn single_parts(html: &str) -> (serde_json::Map<String, serde_json::Value>, serde_json::Value) {
        let images = image_blocks(html).0.into_iter().map(|(name, mime, b64)| (name, serde_json::Value::String(format!("data:{mime};base64,{b64}")))).collect();
        let manifest = html.split_once("const TOUR = ").unwrap().1.split_once(";\n(function").unwrap().0;
        (images, serde_json::from_str(manifest).unwrap())
    }

    fn csp(html: &str) -> &str {
        html.split("Content-Security-Policy\" content=\"").nth(1).unwrap().split('"').next().unwrap()
    }

    fn noise(w: u32, h: u32) -> RgbaImage {
        let mut s = 3u32;
        RgbaImage::from_fn(w, h, |_, _| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            image::Rgba([(s >> 24) as u8, (s >> 16) as u8, (s >> 8) as u8, 255])
        })
    }

    /// A tour with everything a page can show: cards, a note, a spotlight, a cover with a logo, an end
    /// card, and one busy step that the encoder saves as JPEG.
    fn full_tour(tmp: &Path) -> (Recording, Vec<Item>, Vec<InputEvent>, Edit, impl FnMut(usize) -> Result<RgbaImage, String>) {
        let r = Recording::timing_only(640, 400, &[0, 500, 1000, 1500, 2000], 2500);
        let ev = vec![left(750, 320, 200), left(1750, 64, 100)];
        let items = vec![text(40.0, 40.0, "Unpaid invoices", 1), spot(Bounds { x0: 200.0, y0: 100.0, x1: 440.0, y1: 300.0 }, 1)];
        let mut e = edit(&r);
        e.tour_doc.steps.insert(1, StepExtras { card: Some(card("Open invoices", "One click **away**.")), ..Default::default() });
        e.tour_doc.steps.insert(3, StepExtras { card: Some(card("Then this", "")), advance: Advance::Anywhere, zoom: Some(false) });
        let logo = tmp.join("logo-src.png");
        RgbaImage::from_pixel(64, 32, image::Rgba([20, 120, 220, 255])).save(&logo).unwrap();
        e.tour_doc.cover = Some(Cover { title: "Billing tour".into(), body: "Three steps.".into(), button: "Start".into(), logo: None });
        e.tour_doc.logo_path = Some(logo);
        e.tour_doc.end = Some(EndCard { text: "All done".into(), primary: Some(Link { label: "Book a call".into(), url: "https://tours.example.com".into() }), secondary: None, replay: true });
        let frames = |i: usize| Ok::<_, String>(if i == 3 { noise(640, 400) } else { RgbaImage::from_pixel(640, 400, image::Rgba([230, 232, 236, 255])) });
        (r, items, ev, e, frames)
    }

    #[test]
    fn a_single_file_holds_exactly_the_folder_exports_pictures_and_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, items, ev, e, mut frames) = full_tour(tmp.path());
        let dir = tmp.path().join("folder");
        let folder = write_bundle(&r, &items, &ev, &e, "Billing", &dir, &mut frames, |_| true).unwrap();
        let file = tmp.path().join("billing-tour.html");
        let single = write_single_file(&r, &items, &ev, &e, "Billing", &file, &mut frames, |_| true).unwrap();
        assert_eq!((single.steps, single.squeezed, single.path.as_path()), (folder.steps, folder.squeezed, file.as_path()));
        assert_eq!(single.bytes, std::fs::metadata(&file).unwrap().len(), "the size reported is the size on disk");

        let html = std::fs::read_to_string(&file).unwrap();
        let (images, manifest) = single_parts(&html);
        let json = read_json(&dir);
        assert_eq!(manifest, json, "the embedded manifest is tour.json");
        let mut names: Vec<String> = json["steps"].as_array().unwrap().iter().map(|s| s["image"].as_str().unwrap().to_string()).collect();
        names.push(json["cover"]["logo"].as_str().expect("the logo is in the manifest").to_string());
        assert!(names.iter().any(|n| n.ends_with(".jpg")) && names.iter().any(|n| n.ends_with(".png")), "{names:?}");
        assert_eq!(images.len(), names.len(), "one embedded picture per file, and no others");
        for name in &names {
            let url = images[name].as_str().unwrap();
            let mime = if name.ends_with(".png") { "data:image/png;base64," } else { "data:image/jpeg;base64," };
            let b64 = url.strip_prefix(mime).unwrap_or_else(|| panic!("{name} is not {mime}"));
            assert_eq!(unbase64(b64), std::fs::read(dir.join(name)).unwrap(), "{name}");
        }
        // It is the folder's page: take the pictures out and the image rule back, and the two are the same.
        let (blocks, page) = image_blocks(&html);
        let same = page.replacen(SINGLE_IMG_SRC, FOLDER_IMG_SRC, 1);
        assert_eq!(same, std::fs::read_to_string(dir.join("index.html")).unwrap());
        // Each picture is its own block, so no string in the page holds more than one picture, and the
        // blocks all come before the page's scripts.
        assert_eq!(blocks.len(), names.len());
        let last_block = html.rfind(BLOCK_START).unwrap();
        assert!(last_block < html.find("<script>").unwrap(), "pictures come before the scripts that read them");
        assert!(html.contains("Local preview"));
        // Only the file was written.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 3, "folder, logo source and the tour file");
    }

    #[test]
    fn only_the_single_file_allows_data_images_and_neither_allows_the_network() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let file = tmp.path().join("t.html");
        write_single_file(&r, &[], &clicks(), &edit(&r), "t", &file, flat, |_| true).unwrap();
        let folder_html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        let single_html = std::fs::read_to_string(&file).unwrap();
        let (folder, single) = (csp(&folder_html), csp(&single_html));
        assert!(folder.contains("img-src 'self';") && !folder.contains("data:"), "{folder}");
        assert!(single.contains("img-src data:;") && !single.contains("'self'"), "{single}");
        assert_eq!(single.replacen("img-src data:;", "img-src 'self';", 1), folder, "nothing else differs");
        for part in ["default-src 'none'", "script-src 'unsafe-inline'", "base-uri 'none'", "form-action 'none'"] {
            assert!(single.contains(part), "{part}");
        }
        assert!(!single.contains("http") && !single.contains('*') && !single.contains("connect-src") && !single.contains("blob:"), "{single}");
        assert!(single_html.find("Content-Security-Policy").unwrap() < single_html.find("<script").unwrap());
    }

    #[test]
    fn nothing_in_a_single_file_can_close_its_script_blocks() {
        // Base64 never holds `<`, `"` or `\`, whatever the bytes are.
        let hostile = b"</script><script>alert(1)</script>\"\\";
        let mut out = Vec::new();
        base64_into(&mut out, hostile).unwrap();
        let text64 = String::from_utf8(out).unwrap();
        assert!(text64.bytes().all(|b| BASE64.contains(&b) || b == b'='), "{text64}");
        assert_eq!(unbase64(&text64), hostile);
        for n in 0..=7 {
            let mut out = Vec::new();
            base64_into(&mut out, &hostile[..n]).unwrap();
            assert_eq!(unbase64(std::str::from_utf8(&out).unwrap()), &hostile[..n], "{n} bytes");
        }
        // A picture holding that text after decoding, and a manifest with a hostile title, stay data.
        let mut sink = PageSink { out: Vec::new(), images: 0 };
        sink.image("step-001.png", "png", hostile).unwrap();
        sink.finish(&manifest(HOSTILE.into(), vec![], None, TourDoc::default())).unwrap();
        let html = String::from_utf8(sink.out).unwrap();
        assert_eq!(html.matches("</script").count(), PREVIEW_TEMPLATE.matches("</script").count() + 1, "only the page's own blocks close");
        assert!(!html.contains("</script><img") && !html.contains("alert(1)</script>"));
        let (images, parsed) = single_parts(&html);
        assert_eq!(unbase64(images["step-001.png"].as_str().unwrap().strip_prefix("data:image/png;base64,").unwrap()), hostile);
        assert_eq!(parsed["title"], HOSTILE);
        // A whole export with a hostile title and note.
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("t.html");
        write_single_file(&r, &[text(10.0, 20.0, HOSTILE, 1)], &clicks(), &edit(&r), "</script><b>x", &file, flat, |_| true).unwrap();
        let html = std::fs::read_to_string(&file).unwrap();
        assert!(!html.contains("</script><b>x") && !html.contains("</script><img"));
        assert_eq!(html.matches("</script").count(), PREVIEW_TEMPLATE.matches("</script").count() + 3, "one block per picture and the page's own");
        let (_, parsed) = single_parts(&html);
        assert_eq!((parsed["title"].as_str(), parsed["steps"][0]["notes"][0]["text"].as_str()), (Some("</script><b>x"), Some(HOSTILE)));
        // The picture types follow the encoder; anything else is refused.
        assert_eq!(image_mime("jpg").unwrap(), "image/jpeg");
        assert!(image_mime("gif").is_err());
        // A name that is not plain cannot reach an attribute.
        let mut sink = PageSink { out: Vec::new(), images: 0 };
        assert!(sink.image("step\" onload=\"x.png", "png", b"x").is_err());
        assert!(sink.image("", "png", b"x").is_err());
        assert!(sink.out.is_empty());
    }

    #[test]
    fn a_single_file_over_the_size_cap_is_refused_with_its_size_and_writes_nothing() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("t.html");
        std::fs::write(&file, "old").unwrap();
        let err = write_single_file_within(&r, &[], &clicks(), &edit(&r), "t", &file, flat, |_| true, 10_000).unwrap_err();
        assert!(err.starts_with("As one file this tour would be ") && err.contains(" MB, and a single file can be at most "), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old", "the target is not touched");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1, "no temporary file is left");
        // The size named is the size the file would have had.
        let ok = write_single_file_within(&r, &[], &clicks(), &edit(&r), "t", &file, flat, |_| true, MAX_FILE_BYTES).unwrap();
        assert!(err.contains(&format!("{:.1} MB", ok.bytes as f64 / 1_048_576.0)), "{err} vs {}", ok.bytes);
        assert_eq!(too_big(300 * 1_048_576, MAX_FILE_BYTES), "As one file this tour would be 300.0 MB, and a single file can be at most 200 MB. Export it as a folder instead.");
        assert!(LARGE_FILE_BYTES < MAX_FILE_BYTES);
    }

    #[test]
    fn a_cancel_after_the_last_picture_still_keeps_the_target_unchanged() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("t.html");
        std::fs::write(&file, "old").unwrap();
        assert_eq!(write_single_file(&r, &[], &clicks(), &edit(&r), "t", &file, flat, |f| f < 1.0).unwrap_err(), "cancelled");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
        let dir = tmp.path().join("folder");
        assert_eq!(write_bundle(&r, &[], &clicks(), &edit(&r), "t", &dir, flat, |f| f < 1.0).unwrap_err(), "cancelled");
        assert!(!dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_single_file_is_readable_by_others_like_any_saved_file() {
        use std::os::unix::fs::PermissionsExt;
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("t.html");
        write_single_file(&r, &[], &clicks(), &edit(&r), "t", &file, flat, |_| true).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn a_failed_cancelled_or_refused_single_file_leaves_nothing_and_keeps_what_was_there() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("t.html");
        // Cancel and a frame error: no file, no temporary file.
        assert_eq!(write_single_file(&r, &[], &clicks(), &edit(&r), "t", &file, flat, |_| false).unwrap_err(), "cancelled");
        let mut n = 0;
        let failing = |_: usize| { n += 1; if n == 2 { Err("disk read failed".to_string()) } else { flat(0) } };
        assert_eq!(write_single_file(&r, &[], &clicks(), &edit(&r), "t", &file, failing, |_| true).unwrap_err(), "disk read failed");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "nothing was left behind");
        // An existing file survives a cancel untouched, and is replaced whole by a finished export.
        std::fs::write(&file, "old").unwrap();
        write_single_file(&r, &[], &clicks(), &edit(&r), "t", &file, flat, |_| false).unwrap_err();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        let sum = write_single_file(&r, &[], &clicks(), &edit(&r), "t", &file, flat, |_| true).unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().starts_with("<!doctype html>"));
        assert_eq!(sum.bytes, std::fs::metadata(&file).unwrap().len());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
        // The limits refuse with the folder export's own message and write nothing.
        let (big, ev) = many(201);
        let other = tmp.path().join("big.html");
        let folder_err = write_bundle(&big, &[], &ev, &edit(&big), "t", &tmp.path().join("big"), flat, |_| true).unwrap_err();
        assert_eq!(write_single_file(&big, &[], &ev, &edit(&big), "t", &other, flat, |_| true).unwrap_err(), folder_err);
        let mut e = edit(&r);
        e.tour_removed.extend([1, 3, 4]);
        assert_eq!(write_single_file(&r, &[], &clicks(), &e, "t", &other, flat, |_| true).unwrap_err(), ALL_REMOVED);
        assert_eq!(write_single_file(&r, &[], &[], &edit(&r), "t", &other, flat, |_| true).unwrap_err(), NO_CLICKS);
        // A folder in the way, or a missing parent folder, is an error and is left alone.
        let folder = tmp.path().join("taken.html");
        std::fs::create_dir(&folder).unwrap();
        assert!(write_single_file(&r, &[], &clicks(), &edit(&r), "t", &folder, flat, |_| true).is_err());
        assert!(folder.is_dir() && std::fs::read_dir(&folder).unwrap().count() == 0);
        assert!(write_single_file(&r, &[], &clicks(), &edit(&r), "t", &tmp.path().join("missing").join("t.html"), flat, |_| true).is_err());
        let mut left: Vec<String> = std::fs::read_dir(tmp.path()).unwrap().map(|d| d.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["t.html", "taken.html"]);
    }

    #[test]
    fn a_200_step_single_file_reports_progress_and_its_size() {
        let (r, ev) = many(200);
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("t.html");
        let mut seen = Vec::new();
        let sum = write_single_file(&r, &[], &ev, &edit(&r), "t", &file, flat, |f| { seen.push(f); true }).unwrap();
        assert_eq!(sum.steps, 200);
        assert_eq!((seen.first(), seen.last(), seen.len()), (Some(&0.0), Some(&1.0), 201));
        assert!(seen.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(sum.bytes, std::fs::metadata(&file).unwrap().len());
        assert_eq!(single_parts(&std::fs::read_to_string(&file).unwrap()).0.len(), 200);
    }

    // Site export.

    #[test]
    fn the_preview_page_is_unchanged_by_the_markers() {
        // The preview still carries the label and inline code, and none of the site's parts or markers.
        let html = preview_page("{}");
        assert!(html.contains("Local preview") && html.contains(r#"document.title = TOUR.title + " (local preview)";"#));
        assert!(!html.contains("tour-data") && !html.contains("../../assets/"));
        for m in MARKERS {
            assert!(PREVIEW_TEMPLATE.matches(m).count() == 1, "{m} is in the template once");
            assert!(!html.contains(m) && !TOUR_CORE_JS.contains(m), "{m}");
        }
        // Byte for byte the page written before the markers existed. Change this only together with a
        // reviewed change to the preview page or the shared rules.
        use sha2::Digest;
        let hash: String = sha2::Sha256::digest(html.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hash, "048d477819f6d38db7fc90ee146eb5042cb6730aaa0d0727bbe582f770a970cd");
        // Text in a manifest that looks like a marker stays as it is.
        let page = preview_page(r#"{"title":"/*PLAYER_JS_END*/<!--PLAYER_HTML_BEGIN-->"}"#);
        assert!(page.contains(r#""title":"/*PLAYER_JS_END*/<!--PLAYER_HTML_BEGIN-->""#));
    }

    #[test]
    fn the_site_player_is_cut_from_the_preview_page() {
        // Every part a site's page uses is, word for word, in the preview page, so the two cannot drift.
        let html = preview_page("{}");
        assert!(html.contains(player_css()) && player_css().contains("#card.compact {") && player_css().contains("prefers-reduced-motion"));
        assert!(html.contains(player_html()) && player_html().starts_with("<main>") && player_html().contains(r#"<div id="status""#));
        let js = player_js();
        assert!(js.starts_with("(function () {") && js.trim_end().ends_with("})();"));
        let (before, after) = js.split_once("  \n  $(\"title\")").unwrap();
        assert!(html.contains(before) && html.contains(&format!("  $(\"title\"){after}")));
        assert!(!js.contains("local preview") && !js.contains("Local preview") && !player_html().contains("Local preview"));
        assert!(!player_html().contains("<header") && !player_html().contains("<script"));
    }

    #[test]
    fn a_site_tour_writes_a_thumbnail_and_no_preview() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let a = crate::site::assets();
        let sum = write_tour(&r, &[], &clicks(), &edit(&r), "My tour", &dir, flat, |_| true, Page::Site(&a)).unwrap();
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(html.contains("tour-data") && !html.contains("Local preview"));
        assert!(html.contains(&format!("../../assets/{}", a.js_name)) && html.contains("<title>My tour</title>"));
        let thumb = image::open(dir.join("thumb.jpg")).unwrap();
        assert!(thumb.width() <= 480);
        assert!(dir.join("tour.json").exists() && dir.join("step-001.png").exists());
        // The same pictures and manifest as the folder export.
        let folder = tmp.path().join("folder");
        let plain = write_bundle(&r, &[], &clicks(), &edit(&r), "My tour", &folder, flat, |_| true).unwrap();
        assert_eq!((sum.steps, sum.bytes, sum.squeezed), (plain.steps, plain.bytes, plain.squeezed));
        assert_eq!(std::fs::read(dir.join("tour.json")).unwrap(), std::fs::read(folder.join("tour.json")).unwrap());
        assert!(!folder.join("thumb.jpg").exists());
        // The manifest in the page is the manifest, and nothing in it is markup.
        let data = html.split_once(r#"<script type="application/json" id="tour-data">"#).unwrap().1.split_once("</script>").unwrap().0;
        assert_eq!(serde_json::from_str::<serde_json::Value>(data).unwrap(), read_json(&dir));
    }

    #[test]
    fn a_wide_first_step_gives_a_480_pixel_thumbnail_and_a_failure_leaves_nothing() {
        let r = Recording::timing_only(1600, 1000, &[0, 500], 1000);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let a = crate::site::assets();
        let wide = |_: usize| Ok(RgbaImage::from_pixel(1600, 1000, image::Rgba([10, 200, 30, 255])));
        write_tour(&r, &[], &[left(250, 800, 500)], &edit(&r), "<b>Wide</b>", &dir, wide, |_| true, Page::Site(&a)).unwrap();
        let thumb = image::open(dir.join("thumb.jpg")).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (480, 300));
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(html.contains("<title>&lt;b&gt;Wide&lt;/b&gt;</title>") && !html.contains("<b>Wide"));
        // A cancel removes the folder, thumbnail and all, with the folder export's own message.
        let gone = tmp.path().join("gone");
        assert_eq!(write_tour(&r, &[], &[left(250, 800, 500)], &edit(&r), "t", &gone, wide, |f| f < 1.0, Page::Site(&a)).unwrap_err(), "cancelled");
        assert!(!gone.exists());
    }

    #[test]
    fn preview_handles_broken_tours_and_keeps_controls_reachable() {
        // No steps, a missing picture, and a card whose controls are inert until it is visible.
        assert!(PREVIEW_TEMPLATE.contains("This tour has no steps."));
        assert!(PREVIEW_TEMPLATE.contains(r#"addEventListener("error""#) && PREVIEW_TEMPLATE.contains(r#"<div id="missing" hidden>"#));
        assert!(PREVIEW_TEMPLATE.contains("card.inert = true") && PREVIEW_TEMPLATE.contains("card.inert = false"));
        // Leaving the end card backwards shows the last step, and focus follows the control used.
        assert!(PREVIEW_TEMPLATE.contains("if (d < 0 && done) { done = false; render(); return; }"));
        assert!(PREVIEW_TEMPLATE.contains(r#"go(-1, false, "back")"#) && PREVIEW_TEMPLATE.contains(r#"go(1, false, "next")"#));
        // A hidden tab holds autoplay.
        assert!(PREVIEW_TEMPLATE.contains(r#"addEventListener("visibilitychange""#) && PREVIEW_TEMPLATE.contains("document.hidden"));
    }
}
