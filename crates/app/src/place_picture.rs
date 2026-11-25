//! Placing a picture file on a capture: a file dropped on an editor window, or chosen with the
//! rail's Picture tool. The file is untrusted: it is read only by [`open_image::load`], within its
//! limits and its panic guard, on a thread of its own, and only the newest request of an editor
//! is placed ([`open_image::ImageLoads`], one per editor, so two editors never cancel each other,
//! sharing the app-wide cap on reads in flight, [`open_image::MAX_READS_ALL`]).
//!
//! The picture becomes the editor's existing placed picture (`Shape::Image`): at its natural size
//! when it fits inside the capture, else scaled down, keeping its proportions, to fit inside the
//! capture less a margin of [`MARGIN`] on each side. A picture larger than it will be shown is
//! resampled down to that size on the loader's thread ([`resample`]), so its pixels never bloat
//! a saved project and the window's thread never works on a large picture.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;
use image::imageops::{self, FilterType};
use image::{ImageError, RgbaImage};
use mythicshot_editor::{Bounds, ImageData, Pt, pt};

use crate::open_image::{self, ImageLoads, ReadJob, Settled};

/// What the drop overlay says over an editor's canvas when the file dragged there is a picture
/// a drop would place. Approved wording.
pub const PLACE_HERE: &str = "Drop the picture to place it on this capture";

/// What it says over a picture while nothing may be placed (the Tour view with no step to pin
/// to, or a dialog open). Approved wording.
pub const CANNOT_PLACE_NOW: &str = "A picture cannot be placed here right now";

/// The room a picture too large for the capture keeps from each edge once scaled down, as a
/// share of the capture's width (left and right) and height (top and bottom): 5%, the margin a
/// pasted picture keeps.
pub const MARGIN: f32 = 0.05;

/// The most memory the resample's filtering pass may take: the bound a decode is held to
/// ([`open_image::MAX_DECODED_BYTES`]), so placing a picture never needs more than opening one.
/// The filter works in floats (16 bytes a pixel) on the picture's width times the new height;
/// when that would be more, the picture is averaged down in blocks instead, which takes no memory
/// beyond the picture it makes.
pub const RESAMPLE_BYTES: u64 = open_image::MAX_DECODED_BYTES;

/// The size, in picture pixels of the capture, a picture of `picture` pixels is placed at on a
/// capture of `capture` pixels: its own size when it fits inside the capture, else scaled down,
/// keeping its proportions, to fit inside the capture less [`MARGIN`] on each side. Whole pixels,
/// at least one each way, so the picture is shown pixel for pixel at 100%.
pub fn placed_size(picture: (u32, u32), capture: (u32, u32)) -> (u32, u32) {
    let (pw, ph) = (picture.0.max(1), picture.1.max(1));
    let (cw, ch) = (capture.0.max(1), capture.1.max(1));
    if pw <= cw && ph <= ch {
        return (pw, ph);
    }
    let room = 1.0 - 2.0 * f64::from(MARGIN);
    let scale = (f64::from(cw) * room / f64::from(pw)).min(f64::from(ch) * room / f64::from(ph)).min(1.0);
    let w = ((f64::from(pw) * scale).round() as u32).clamp(1, pw);
    let h = ((f64::from(ph) * scale).round() as u32).clamp(1, ph);
    (w, h)
}

/// Where a picture of `size` (capture pixels) goes when it is placed centred on `centre`: the
/// centre is moved only as far as needed for the picture to lie inside the capture.
pub fn placed_rect(size: (f32, f32), centre: Pt, capture: (u32, u32)) -> Bounds {
    let (w, h) = size;
    let (cw, ch) = (capture.0 as f32, capture.1 as f32);
    let fit = |c: f32, side: f32, room: f32| (c - side / 2.0).clamp(0.0, (room - side).max(0.0));
    let (x0, y0) = (fit(centre.x, w, cw), fit(centre.y, h, ch));
    Bounds::new(pt(x0, y0), pt(x0 + w, y0 + h))
}

/// `img` resampled down to `w` by `h`, or `img` itself when it is no larger: a picture is never
/// enlarged. The filter is Lanczos (three lobes), on premultiplied alpha so see-through edges
/// keep their colour. Its memory is bounded: a picture more than twice the size asked for is
/// first averaged down in blocks to twice the size (which allocates only the picture it makes),
/// and the filter runs only when its float pass fits in [`RESAMPLE_BYTES`]; otherwise block
/// averaging makes the final size too.
pub fn resample(img: RgbaImage, w: u32, h: u32) -> RgbaImage {
    let (w, h) = (w.max(1), h.max(1));
    if img.width() <= w && img.height() <= h {
        return img;
    }
    let mut img = premultiply(img);
    if img.width() > 2 * w || img.height() > 2 * h {
        img = imageops::thumbnail(&img, img.width().min(2 * w), img.height().min(2 * h));
    }
    let out = if filter_fits(img.width(), h) { imageops::resize(&img, w, h, FilterType::Lanczos3) } else { imageops::thumbnail(&img, w, h) };
    unpremultiply(out)
}

/// Whether the filter's float pass over a picture `width` wide, to `h` high, fits in
/// [`RESAMPLE_BYTES`]: it takes 16 bytes for each pixel of `width` by `h`.
pub fn filter_fits(width: u32, h: u32) -> bool {
    u64::from(width) * u64::from(h) * 16 <= RESAMPLE_BYTES
}

fn premultiply(mut img: RgbaImage) -> RgbaImage {
    for p in img.pixels_mut() {
        let a = u16::from(p.0[3]);
        for c in &mut p.0[..3] {
            *c = ((u16::from(*c) * a + 127) / 255) as u8;
        }
    }
    img
}

fn unpremultiply(mut img: RgbaImage) -> RgbaImage {
    for p in img.pixels_mut() {
        let a = u16::from(p.0[3]);
        for c in &mut p.0[..3] {
            // The filter can ring a colour past its alpha: it is cut there.
            *c = if a == 0 { 0 } else { ((u16::from(*c).min(a) * 255 + a / 2) / a) as u8 };
        }
    }
    img
}

/// The picture read for a capture of `capture` pixels, made ready to place: resampled to the
/// size it will be placed at ([`placed_size`]) and turned into the editor's picture. Runs on the
/// loader's thread.
pub fn prepare(img: RgbaImage, capture: (u32, u32)) -> ImageData {
    let (w, h) = placed_size(img.dimensions(), capture);
    ImageData::from_rgba(&resample(img, w, h))
}

/// What a picture is placed on: the capture's size, and in a recording editor the view and the
/// step shown. A picture that arrives once any of it has changed (the capture was cropped or
/// resized, another view or step is shown) is dropped without a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub capture: (u32, u32),
    /// For a recording: whether the Tour view is shown, and the step's frame there.
    pub view: Option<(bool, Option<usize>)>,
}

/// A picture that has arrived, or the file that could not be read, for the newest request.
#[derive(Debug)]
pub enum Placed {
    /// Place this picture centred on `centre` (capture pixels), if the editor still shows `target`.
    Picture { image: ImageData, centre: Pt, target: Target },
    /// Say this sentence (`Could not open {path}: {e}`), if the editor still shows `target`.
    Failed { sentence: String, target: Target },
}

/// What a read sends back: its number, its file, and the picture made ready to place.
type Arrived = (u64, PathBuf, Result<ImageData, ImageError>);

/// The newest request: its number, where its picture goes and what it goes on.
#[derive(Debug, Clone, Copy)]
struct Request {
    generation: u64,
    centre: Pt,
    target: Target,
}

/// One editor's pictures being read to be placed, the newest request winning.
pub struct Placing {
    loads: ImageLoads,
    newest: Option<Request>,
    tx: Sender<Arrived>,
    rx: Receiver<Arrived>,
    /// Wakes the editor's window when a read ends: its context and viewport, from its last frame.
    wake: Option<(egui::Context, egui::ViewportId)>,
}

impl Default for Placing {
    fn default() -> Self {
        Self::with_loads(ImageLoads::default())
    }
}

impl Placing {
    /// Reads made through `loads` (its pool of app-wide slots).
    pub fn with_loads(loads: ImageLoads) -> Self {
        let (tx, rx) = mpsc::channel();
        Self { loads, newest: None, tx, rx, wake: None }
    }

    /// The window to wake when a read ends: the one whose frame is being drawn.
    pub fn set_wake(&mut self, ctx: &egui::Context) {
        if self.wake.is_none() {
            let (wake, viewport) = (ctx.clone(), ctx.viewport_id());
            // A read waiting for an app-wide slot starts on the frame after one comes free.
            self.loads.set_wake(move || wake.request_repaint_of(viewport));
        }
        self.wake = Some((ctx.clone(), ctx.viewport_id()));
    }

    /// Read the picture at `path` in the background, to be placed centred on `centre` on `target`.
    pub fn request(&mut self, path: PathBuf, centre: Pt, target: Target) {
        let job = self.loads.request(path);
        self.newest = Some(Request { generation: self.loads.newest(), centre, target });
        if let Some(job) = job {
            self.start(job);
        }
    }

    /// Start a read. A read that starts later (it waited for a free slot) is always the newest
    /// request's, so it is made ready for the newest request's capture.
    fn start(&self, job: ReadJob) {
        let Some(capture) = self.newest.map(|r| r.target.capture) else { return };
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        job.spawn(
            move |img| prepare(img, capture),
            move |generation, path, result| {
                // The editor may be gone: then nobody waits for this picture, and it is dropped.
                if tx.send((generation, path, result)).is_ok()
                    && let Some((ctx, viewport)) = wake
                {
                    ctx.request_repaint_of(viewport);
                }
            },
        );
    }

    /// The reads that have ended since the last call: the newest request's picture or failure,
    /// if it has arrived. A waiting read starts as one ends; an older read's result is dropped.
    pub fn poll(&mut self) -> Vec<Placed> {
        let mut placed = Vec::new();
        if let Some(job) = self.loads.retry() {
            self.start(job);
        }
        while let Ok((generation, path, result)) = self.rx.try_recv() {
            let (settled, next) = self.loads.finished(generation, &path, result);
            if let Some(job) = next {
                self.start(job);
            }
            let Some(request) = self.newest.filter(|r| r.generation == generation) else { continue };
            match settled {
                Settled::Open(image, _) => placed.push(Placed::Picture { image, centre: request.centre, target: request.target }),
                Settled::Failed(sentence) => placed.push(Placed::Failed { sentence, target: request.target }),
                Settled::Superseded => {}
            }
        }
        placed
    }

    /// Whether the newest request's picture is still being read: an update waits for it.
    pub fn pending(&self) -> bool {
        self.loads.pending()
    }
}

#[cfg(test)]
pub(crate) mod tests;
