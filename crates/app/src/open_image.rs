//! Opening a picture file in the editor: the tray's "Open image from file…", the Home page's
//! Open image tile, which both ask for a file with the same dialog, and a picture file dropped
//! on the main window.
//!
//! A file opened either way is untrusted: it is read by [`load`] alone, which refuses anything
//! that is not a plain file, a file over [`MAX_FILE_BYTES`] before reading a byte of it, and a
//! picture over [`MAX_SIDE`] or [`MAX_PIXELS`] from its header before decoding it. What it is
//! is decided by its content, never by its name.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, FontId, Rect, Shape, Stroke, pos2, vec2};
use image::error::{ImageFormatHint, LimitError, LimitErrorKind, UnsupportedError, UnsupportedErrorKind};
use image::{DynamicImage, ImageDecoder, ImageError, ImageReader, RgbaImage};

use crate::dialogs;
use crate::icons::{self, Icon};
use crate::theme;

/// The picture files the dialog offers and a drop takes, by extension (in any letter case).
pub const SUPPORTED: [&str; 6] = ["png", "jpg", "jpeg", "bmp", "gif", "webp"];

/// The largest file read at all; a larger one is refused by its size before a byte is read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// The longest side a picture may have: the largest picture the capture path keeps, the limit
/// a saved recording's frames are held to (see `mythicshot_recording::saved`).
pub const MAX_SIDE: u32 = 16_384;
/// The most pixels a picture may have, the same limit as a saved recording's frames: decoded,
/// it takes four bytes a pixel, so at most 256 MB.
pub const MAX_PIXELS: u64 = 64 << 20;

/// What the drop overlay says over a picture file that will open.
pub const DROP_TO_OPEN: &str = "Drop the image to open it in the editor";
/// What it says over anything else: another kind of file, a folder, a shortcut, several files.
pub const ONLY_PICTURES: &str = "Mythicshot opens PNG, JPG, BMP, GIF and WebP pictures";

/// What the main window asks the app to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenImage {
    /// Ask for a file, as the tray's "Open image from file…" does: the Open image tile.
    Pick,
    /// A picture file dropped on the window.
    Dropped(PathBuf),
}

/// Ask for a picture file in `dir`, the captures folder: the dialog the tray's "Open image from
/// file…" opens.
pub fn pick(dir: &Path) -> Option<PathBuf> {
    dialogs::pick_file(rfd::FileDialog::new().add_filter("Images", &SUPPORTED).set_directory(dir), "open image")
}

/// The sentence the app shows when a picture file could not be opened, whichever way it was asked for.
pub fn could_not_open(path: &Path, e: &ImageError) -> String {
    format!("Could not open {}: {e}", path.display())
}

/// Read the picture in the file at `path`, whatever its name says, within the limits above. An
/// animated GIF or WebP gives its first frame. Only a regular file on a disk is read: a folder, a
/// link, a pipe or a device is refused as a file that is not a picture, without being followed.
pub fn load(path: &Path) -> Result<RgbaImage, ImageError> {
    let file = open_disk_file(path)?;
    // The size the file says it has refuses most files at once; the bounded read below holds
    // even when that size is wrong.
    if file.metadata().map_err(ImageError::IoError)?.len() > MAX_FILE_BYTES {
        return Err(too_large_file());
    }
    let bytes = read_bounded(file)?;
    guarded(|| decode(&bytes))
}

/// Read at most [`MAX_FILE_BYTES`] from `reader`; more than that is refused, however much more
/// the reader would give.
pub fn read_bounded(reader: impl Read) -> Result<Vec<u8>, ImageError> {
    let mut bytes = Vec::new();
    reader.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(ImageError::IoError)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large_file());
    }
    Ok(bytes)
}

/// Run a decode, turning a panic in the decoder (a hostile file) into the error for a file that is
/// not a picture, so it never reaches the window.
pub fn guarded(work: impl FnOnce() -> Result<RgbaImage, ImageError>) -> Result<RgbaImage, ImageError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_| Err(not_a_picture()))
}

/// The most memory a decode may need for the picture as stored, before it is turned into four
/// bytes a pixel: so a 16-bit picture may have half the pixels of an 8-bit one.
pub const MAX_DECODED_BYTES: u64 = 256 * 1024 * 1024;

/// Decode the picture in `bytes`, refused after reading only its header when it is empty, over
/// [`MAX_SIDE`] on a side, over [`MAX_PIXELS`] in all, or would take more than
/// [`MAX_DECODED_BYTES`] as stored.
pub fn decode(bytes: &[u8]) -> Result<RgbaImage, ImageError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large_file());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(ImageError::IoError)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) > MAX_PIXELS || decoder.total_bytes() > MAX_DECODED_BYTES {
        return Err(ImageError::Limits(LimitError::from_kind(LimitErrorKind::DimensionError)));
    }
    Ok(DynamicImage::from_decoder(decoder)?.into_rgba8())
}

fn too_large_file() -> ImageError {
    ImageError::Limits(LimitError::from_kind(LimitErrorKind::InsufficientMemory))
}

/// The error for something that is not a picture: the one the decoder gives a file whose content
/// is no picture format it knows.
fn not_a_picture() -> ImageError {
    ImageError::Unsupported(UnsupportedError::from_format_and_kind(ImageFormatHint::Unknown, UnsupportedErrorKind::Format(ImageFormatHint::Unknown)))
}

/// Open `path` for reading only when it is a regular file on a disk, checked on the opened handle
/// itself. On Windows the path is opened without following a reparse point (a link or junction
/// is opened as itself and refused), and a pipe or device is refused by the handle's type rather
/// than read, so it can never block the read.
#[cfg(windows)]
fn open_disk_file(path: &Path) -> Result<File, ImageError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_TYPE_DISK, GetFileType};
    // Backup semantics only lets a folder be opened, so it is refused below like any non-file.
    let file = std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0).open(path).map_err(ImageError::IoError)?;
    // SAFETY: the handle belongs to `file`, which outlives the call.
    let kind = unsafe { GetFileType(HANDLE(file.as_raw_handle())) };
    if kind != FILE_TYPE_DISK {
        return Err(not_a_picture());
    }
    let meta = file.metadata().map_err(ImageError::IoError)?;
    if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(not_a_picture());
    }
    Ok(file)
}

/// Elsewhere: only a regular file, checked on the opened handle.
#[cfg(not(windows))]
fn open_disk_file(path: &Path) -> Result<File, ImageError> {
    let file = File::open(path).map_err(ImageError::IoError)?;
    if !file.metadata().map_err(ImageError::IoError)?.is_file() {
        return Err(not_a_picture());
    }
    Ok(file)
}

/// One picture file read at a time, off the window's thread: a request while one is being read
/// is ignored (the first one opens; the person can ask again once it has).
#[derive(Debug, Default)]
pub struct LoadGate {
    loading: bool,
}

impl LoadGate {
    /// Whether a read may start now; when it may, it counts as started.
    pub fn begin(&mut self) -> bool {
        !std::mem::replace(&mut self.loading, true)
    }

    /// The read has ended, opened or not.
    pub fn end(&mut self) {
        self.loading = false;
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }
}

/// Read the picture at `path` on a thread of its own and hand the result to `done` there. A
/// panic anywhere in the read still ends in a result, so the gate is always let go.
pub fn spawn_load(path: PathBuf, done: impl FnOnce(PathBuf, Result<RgbaImage, ImageError>) + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(&path))).unwrap_or_else(|_| Err(not_a_picture()));
        done(path, result);
    })
}

/// Whether `path` names a plain file, for the overlay: not a folder, and not a link of any kind.
/// The read itself checks again on the opened file ([`load`]).
fn plain_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Whether a file name ends in one of the [`SUPPORTED`] extensions, in any letter case.
pub fn supported_name(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| SUPPORTED.iter().any(|s| s.eq_ignore_ascii_case(e)))
}

/// What a drag over the window, or a drop on it, holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Exactly one plain file whose name is a supported picture's: a drop opens it.
    Accept(PathBuf),
    /// Anything else: a drop does nothing.
    Refuse,
}

/// Judge the files of a drag: one plain file with a picture's name is taken; several files, a
/// folder, a link or shortcut, a file of another kind, or an item without a path are not.
/// Only the name is read and the path looked at, never the file's content.
pub fn judge(paths: &[Option<PathBuf>]) -> Verdict {
    match paths {
        [Some(path)] if supported_name(path) && plain_file(path) => Verdict::Accept(path.clone()),
        _ => Verdict::Refuse,
    }
}

/// The drag over the main window: the verdict on the files being dragged, kept while the same
/// files are over the window so the disk is looked at once a drag, not every frame.
#[derive(Debug, Default)]
pub struct DropState {
    judged: Option<(Vec<Option<PathBuf>>, Verdict)>,
    /// A drop of several files is landing: what else of it arrives is refused too.
    refusing_drop: bool,
    /// A picture of the window with the overlay up, without a drag: the renders.
    #[cfg(test)]
    pub(crate) shown_for_render: Option<bool>,
}

impl DropState {
    /// Called every frame of the main window with what is dragged over it (`hovered`) and what
    /// was dropped on it (`dropped`). While a drag is over the window, paints the overlay over
    /// `area`; returns the picture file to open when one was dropped.
    pub fn frame(&mut self, ctx: &egui::Context, area: Rect, hovered: Vec<Option<PathBuf>>, dropped: Vec<Option<PathBuf>>) -> Option<PathBuf> {
        if !dropped.is_empty() {
            // A drop of several files can arrive a file a frame: the drag before it said how many.
            // The rest of such a drop, in the frames that follow, is refused with it.
            let several = self.judged.take().is_some_and(|(paths, _)| paths.len() > 1);
            if several || self.refusing_drop {
                self.refusing_drop = true;
                return None;
            }
            return match judge(&dropped) {
                Verdict::Accept(path) => Some(path),
                Verdict::Refuse => None,
            };
        }
        self.refusing_drop = false;
        if hovered.is_empty() {
            self.judged = None;
            #[cfg(test)]
            if let Some(accept) = self.shown_for_render {
                paint_overlay(ctx, area, accept);
            }
            return None;
        }
        let verdict = match &self.judged {
            Some((paths, verdict)) if *paths == hovered => verdict.clone(),
            _ => {
                let verdict = judge(&hovered);
                self.judged = Some((hovered, verdict.clone()));
                verdict
            }
        };
        paint_overlay(ctx, area, matches!(verdict, Verdict::Accept(_)));
        None
    }

    /// [`DropState::frame`] with the files egui says are dragged over and dropped on the window.
    pub fn frame_from_input(&mut self, ctx: &egui::Context, area: Rect) -> Option<PathBuf> {
        let (hovered, dropped) = ctx.input(|i| (i.raw.hovered_files.iter().map(|f| f.path.clone()).collect(), i.raw.dropped_files.iter().map(|f| Some(f.path().to_path_buf())).collect()));
        self.frame(ctx, area, hovered, dropped)
    }
}

/// The overlay's see-through tints over the page, from the theme's selection and danger tints:
/// the page stays faintly visible under them.
pub const ACCEPT_TINT: Color32 = Color32::from_rgba_unmultiplied_const(theme::SELECTION.r(), theme::SELECTION.g(), theme::SELECTION.b(), 205);
pub const REFUSE_TINT: Color32 = Color32::from_rgba_unmultiplied_const(theme::DANGER_TINT.r(), theme::DANGER_TINT.g(), theme::DANGER_TINT.b(), 205);

/// The overlay's colours: the see-through tint over the page, the dashed frame, the solid chip
/// the icon and words stand on, and the words. The words are measured against the solid chip,
/// never against the see-through tint, so whatever the page shows through cannot lower their
/// contrast.
pub fn overlay_colours(accept: bool) -> (Color32, Color32, Color32, Color32) {
    if accept { (ACCEPT_TINT, theme::PRIMARY_TEXT, theme::SELECTION, theme::TEXT) } else { (REFUSE_TINT, theme::DANGER_BORDER, theme::DANGER_TINT, theme::DANGER_TEXT) }
}

/// The overlay over `area` (the window below its menu bar) while files are dragged over the
/// window: a see-through tint over the page, a dashed frame, and on a solid chip in the middle
/// the Open image icon and what a drop would do, in the purple selection tint when it would open
/// a picture and in the danger tint when it would not. It is painted on a layer of its own above
/// the page and takes no input, so clicks still reach the page.
fn paint_overlay(ctx: &egui::Context, area: Rect, accept: bool) {
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("open-image-drop")));
    let (tint, edge, chip, ink) = overlay_colours(accept);
    let text = if accept { DROP_TO_OPEN } else { ONLY_PICTURES };
    p.rect_filled(area, 0.0, tint);
    let frame = area.shrink(theme::PAGE_PAD.min(area.width() / 4.0).min(area.height() / 4.0));
    let corners = vec![frame.left_top(), frame.right_top(), frame.right_bottom(), frame.left_bottom(), frame.left_top()];
    p.extend(Shape::dashed_line(&corners, Stroke::new(2.0, edge), 10.0, 6.0));
    let pad = theme::CARD_PAD_X;
    let wrap = (frame.width() - 4.0 * pad).max(40.0);
    // Centred lines: the galley's x of 0 is the middle of each line.
    let mut job = egui::text::LayoutJob::simple(text.to_string(), FontId::proportional(theme::STRONG_SIZE), ink, wrap);
    job.halign = egui::Align::Center;
    let galley = p.layout_job(job);
    let icon = 40.0_f32.min(frame.height() / 4.0);
    let gap = theme::ITEM_GAP * 1.5;
    let block = vec2(galley.size().x.max(icon), icon + gap + galley.size().y);
    let chip_rect = Rect::from_center_size(frame.center(), block + vec2(2.0 * pad, 2.0 * pad));
    p.rect(chip_rect, egui::CornerRadius::same(theme::CARD_RADIUS), chip, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    let top = chip_rect.top() + pad;
    icons::paint(&p, Rect::from_center_size(pos2(frame.center().x, top + icon / 2.0), vec2(icon, icon)), Icon::OpenImage, ink);
    p.galley(pos2(frame.center().x, top + icon + gap), galley, ink);
}

#[cfg(test)]
pub(crate) mod tests;
