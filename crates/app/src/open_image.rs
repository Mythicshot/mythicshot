//! Opening a picture file in the editor: the tray's "Open image from file…", the Home page's
//! Open image tile, which both ask for a file with the same dialog, and a picture file dropped
//! on the main window.
//!
//! A file opened either way is untrusted: it is read by [`load`] alone, which refuses anything
//! that is not a plain file, a file over [`MAX_FILE_BYTES`] before reading a byte of it, and a
//! picture over [`MAX_SIDE`] or [`MAX_PIXELS`] from its header before decoding it. What it is
//! is decided by its content, never by its name.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek};
use std::path::{Path, PathBuf};

use eframe::egui::{self, FontId, Rect, Shape, Stroke, pos2, vec2};
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
/// animated GIF or WebP gives its first frame. A path that is not a plain file (a folder, a
/// link) is refused as a file that is not a picture, without being followed.
pub fn load(path: &Path) -> Result<RgbaImage, ImageError> {
    // A file that is gone or cannot be looked at says why, as the open always did.
    if !std::fs::symlink_metadata(path).map_err(ImageError::IoError)?.file_type().is_file() {
        return Err(not_a_picture());
    }
    let file = File::open(path).map_err(ImageError::IoError)?;
    let len = file.metadata().map_err(ImageError::IoError)?.len();
    decode(BufReader::new(file), len)
}

/// Decode the picture `reader` holds, `len` bytes long: refused before reading when `len` is over
/// [`MAX_FILE_BYTES`], and after reading only its header when the picture is over [`MAX_SIDE`]
/// on a side or [`MAX_PIXELS`] in all.
pub fn decode<R: BufRead + Seek>(reader: R, len: u64) -> Result<RgbaImage, ImageError> {
    if len > MAX_FILE_BYTES {
        return Err(ImageError::Limits(LimitError::from_kind(LimitErrorKind::InsufficientMemory)));
    }
    let mut reader = ImageReader::new(reader).with_guessed_format().map_err(ImageError::IoError)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    if w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(ImageError::Limits(LimitError::from_kind(LimitErrorKind::DimensionError)));
    }
    Ok(DynamicImage::from_decoder(decoder)?.into_rgba8())
}

/// The error for something that is not a picture: the one the decoder gives a file whose content
/// is no picture format it knows.
fn not_a_picture() -> ImageError {
    ImageError::Unsupported(UnsupportedError::from_format_and_kind(ImageFormatHint::Unknown, UnsupportedErrorKind::Format(ImageFormatHint::Unknown)))
}

/// Whether `path` names a plain file: not a folder, and not a link of any kind (which is not followed).
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
}

impl DropState {
    /// Called every frame of the main window with what is dragged over it (`hovered`) and what
    /// was dropped on it (`dropped`). While a drag is over the window, paints the overlay over
    /// `area`; returns the picture file to open when one was dropped.
    pub fn frame(&mut self, ctx: &egui::Context, area: Rect, hovered: Vec<Option<PathBuf>>, dropped: Vec<Option<PathBuf>>) -> Option<PathBuf> {
        if !dropped.is_empty() {
            self.judged = None;
            return match judge(&dropped) {
                Verdict::Accept(path) => Some(path),
                Verdict::Refuse => None,
            };
        }
        if hovered.is_empty() {
            self.judged = None;
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

/// The overlay over `area` while files are dragged over the window: a dashed frame with the
/// Open image icon and what a drop would do, in the purple selection tint when it would open a
/// picture and in the danger tint when it would not. It is painted on a layer of its own above
/// the page and takes no input, so clicks still reach the page.
pub(crate) fn paint_overlay(ctx: &egui::Context, area: Rect, accept: bool) {
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("open-image-drop")));
    let (fill, edge, ink, text) = if accept { (theme::SELECTION, theme::PRIMARY_TEXT, theme::TEXT, DROP_TO_OPEN) } else { (theme::DANGER_TINT, theme::DANGER_BORDER, theme::DANGER_TEXT, ONLY_PICTURES) };
    p.rect_filled(area, 0.0, theme::SCRIM_STRONG);
    let frame = area.shrink(theme::PAGE_PAD.min(area.width() / 4.0).min(area.height() / 4.0));
    p.rect_filled(frame, 0.0, fill);
    let corners = vec![frame.left_top(), frame.right_top(), frame.right_bottom(), frame.left_bottom(), frame.left_top()];
    p.extend(Shape::dashed_line(&corners, Stroke::new(2.0, edge), 10.0, 6.0));
    let wrap = (frame.width() - 2.0 * theme::PAGE_PAD).max(40.0);
    // Centred lines: the galley's x of 0 is the middle of each line.
    let mut job = egui::text::LayoutJob::simple(text.to_string(), FontId::proportional(theme::STRONG_SIZE), ink, wrap);
    job.halign = egui::Align::Center;
    let galley = p.layout_job(job);
    let icon = 40.0_f32.min(frame.height() / 3.0);
    let gap = theme::ITEM_GAP * 1.5;
    let top = frame.center().y - (icon + gap + galley.size().y) / 2.0;
    icons::paint(&p, Rect::from_center_size(pos2(frame.center().x, top + icon / 2.0), vec2(icon, icon)), Icon::OpenImage, ink);
    p.galley(pos2(frame.center().x, top + icon + gap), galley, ink);
}

#[cfg(test)]
pub(crate) mod tests;
