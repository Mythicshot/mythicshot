//! Archive a recording as a video, and prove the video before anybody trusts it.
//!
//! [`Recording::archive_video`] feeds every frame to a [`VideoCodec`], one at a time, and then
//! reads the video back and walks the recording again in step: the video must hold exactly as
//! many pictures as the recording has frames, and the sampled pictures must be the frames they
//! stand for, within a [`Tolerance`]. Only then is the video left at its place; on any failure
//! or a cancel nothing is left there. Nothing else is ever written.
//!
//! The measures are pure functions of two RGBA pictures: the PSNR of the whole frame over red,
//! green and blue, the PSNR of the worst block (a frame measure alone hides one broken block),
//! and the mean signed difference per channel (a colour cast). Alpha is not measured: it must
//! come back 255.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::ffmpeg::{CodecError, GOP, VideoCodec};
use crate::{FRAMES_FILE, Recording, xor_into};

/// What a video must keep of the recording to replace its frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tolerance {
    /// The least PSNR of a whole frame, in dB.
    pub frame_db: f64,
    /// The least PSNR of any one block of a frame, in dB.
    pub block_db: f64,
    /// The side of a block, in pixels.
    pub block: u32,
    /// The most a channel's mean may move, in levels, up or down.
    pub mean_shift: f64,
    /// How much smaller than the frames file the video must be, as a part of it.
    pub smaller_by: f64,
}

/// The tolerance of a kept recording.
///
/// `frame_db` stands on the owner's real recording (46.7 dB mean, 43.7 dB worst frame at this
/// quality; a range or matrix mistake lands near 25).
///
/// `mean_shift` is 1.5 and PROVISIONAL, pending the owner's confirmation: the plan proposed
/// 0.5, but with the planned ffmpeg arguments a flat pure red and a flat pure blue come back
/// exactly one level off on that channel (the rounding of 8-bit YUV; range and matrix are
/// right), which is a mean shift of 1.0. A range or matrix mistake moves a channel by many
/// levels and still fails.
///
/// `block_db` is proposed too, and is confirmed or corrected by the ignored test
/// `measure_a_real_recording` on the owner's recording.
pub const TOLERANCE: Tolerance = Tolerance { frame_db: 40.0, block_db: 30.0, block: 64, mean_shift: 1.5, smaller_by: 0.20 };

/// A recording of up to this many frames is checked whole.
const CHECK_ALL_UP_TO: usize = 600;

/// Which pass of [`Recording::archive_video`] is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Encoding,
    Verifying,
}

/// A video that was archived and verified.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Archived {
    /// The size of the video file.
    pub bytes: u64,
    /// How many frames were compared with their pictures.
    pub checked: usize,
    /// The lowest frame PSNR among them, in dB; infinite when every one came back exactly.
    pub worst_frame_db: f64,
    /// The lowest block PSNR among them, in dB.
    pub worst_block_db: f64,
}

/// What the verification found wrong with the video.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mismatch {
    /// The video does not hold one picture of the right size per frame; `got` is how many
    /// whole ones were counted (one more than the frames when there are too many).
    Count { got: usize },
    /// A picture came back with an alpha that is not 255.
    Alpha { frame: usize },
    /// A picture is too far from its frame as a whole.
    Frame { frame: usize, db: f64 },
    /// One block of a picture is too far from its frame.
    Block { frame: usize, db: f64 },
    /// A picture has a colour cast.
    Shift { frame: usize },
}

/// Why a recording was not archived. Nothing is left at the output in any of these cases.
#[derive(Debug)]
pub enum ArchiveError {
    /// `keep_going` answered false.
    Cancelled,
    /// The video is not smaller than the frames by the part the tolerance asks for.
    NotSmaller,
    /// The video is not the recording.
    Verify(Mismatch),
    /// The codec could not do the work.
    Codec(CodecError),
    /// The recording itself cannot be archived; the words are this crate's own.
    Unusable(&'static str),
}

/// The PSNR, in dB, of a squared error summed over `values` colour values.
fn db(squared: u64, values: u64) -> f64 {
    if values == 0 {
        0.0
    } else if squared == 0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 * values as f64 / squared as f64).log10()
    }
}

/// Whether the two are RGBA pictures of the same, whole number of pixels, and not of none.
fn fit(a: &[u8], b: &[u8]) -> bool {
    !a.is_empty() && a.len() == b.len() && a.len().is_multiple_of(4)
}

/// The PSNR of `b` against `a` over red, green and blue, in dB. Both are RGBA; alpha is not
/// measured. Infinite when the colours are the same; 0 when the two do not fit each other.
pub fn psnr_rgb(a: &[u8], b: &[u8]) -> f64 {
    if !fit(a, b) {
        return 0.0;
    }
    let mut squared = 0u64;
    for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        squared += (0..3).map(|c| (x[c].abs_diff(y[c]) as u64).pow(2)).sum::<u64>();
    }
    db(squared, (a.len() / 4 * 3) as u64)
}

/// The PSNR of the worst block of `b` against `a`, in dB: the picture is cut into squares of
/// `block` pixels from its top-left corner, and the ones at the right and bottom edges are as
/// small as what is left. 0 when the pictures do not fit the size.
pub fn worst_block_psnr(a: &[u8], b: &[u8], width: u32, height: u32, block: u32) -> f64 {
    measure(a, b, width, height, block).map_or(0.0, |m| m.block_db)
}

/// The mean of `b - a` for red, green and blue, in levels: a colour cast, with its direction.
/// 255 on every channel when the two do not fit each other.
pub fn mean_shift(a: &[u8], b: &[u8]) -> [f64; 3] {
    if !fit(a, b) {
        return [255.0; 3];
    }
    let mut sums = [0i64; 3];
    for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            sums[c] += y[c] as i64 - x[c] as i64;
        }
    }
    sums.map(|sum| sum as f64 / (a.len() / 4) as f64)
}

/// Which frames of a recording of `frames` frames are compared with their pictures, in
/// order: every frame of a short recording; of a long one the first, the last, the last of
/// every group of pictures (where drift is largest) and every frame in `must` that exists.
pub fn sample(frames: usize, must: &[usize]) -> Vec<usize> {
    if frames <= CHECK_ALL_UP_TO {
        return (0..frames).collect();
    }
    let group = GOP as usize;
    let mut picked: Vec<usize> = (group - 1..frames).step_by(group).collect();
    picked.extend([0, frames - 1]);
    picked.extend(must.iter().copied().filter(|i| *i < frames));
    picked.sort_unstable();
    picked.dedup();
    picked
}

/// The most a video of a recording whose frames take `lossless` bytes may take.
fn allowed_bytes(lossless: u64, smaller_by: f64) -> u64 {
    // A float that is not a number, or a part of more than the whole, leaves nothing; the
    // cast cuts off below nothing and above the largest size.
    (lossless as f64 * (1.0 - smaller_by)).floor() as u64
}

/// What one picture measures against its frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Measured {
    frame_db: f64,
    block_db: f64,
    shift: [f64; 3],
    opaque: bool,
}

/// Every measure of `b` against `a` in one walk. None when the pictures do not fit the size.
fn measure(a: &[u8], b: &[u8], width: u32, height: u32, block: u32) -> Option<Measured> {
    let (w, h) = (width as usize, height as usize);
    let len = w.checked_mul(h)?.checked_mul(4)?;
    if len == 0 || a.len() != len || b.len() != len {
        return None;
    }
    let block = (block as usize).max(1);
    // The squared error of each block of the row of blocks being walked.
    let mut row = vec![0u64; w.div_ceil(block)];
    let (mut total, mut worst) = (0u64, f64::INFINITY);
    let mut sums = [0i64; 3];
    let mut opaque = true;
    for y in 0..h {
        let (line_a, line_b) = (&a[y * w * 4..][..w * 4], &b[y * w * 4..][..w * 4]);
        for ((cell_a, cell_b), squared) in line_a.chunks(block * 4).zip(line_b.chunks(block * 4)).zip(&mut row) {
            for (p, q) in cell_a.chunks_exact(4).zip(cell_b.chunks_exact(4)) {
                for c in 0..3 {
                    let d = q[c] as i64 - p[c] as i64;
                    sums[c] += d;
                    *squared += (d * d) as u64;
                }
                opaque &= q[3] == 255;
            }
        }
        if y % block == block - 1 || y == h - 1 {
            let lines = y % block + 1;
            for (i, squared) in row.iter_mut().enumerate() {
                let across = block.min(w - i * block);
                worst = worst.min(db(*squared, (across * lines * 3) as u64));
                total += *squared;
                *squared = 0;
            }
        }
    }
    Some(Measured { frame_db: db(total, (w * h * 3) as u64), block_db: worst, shift: sums.map(|sum| sum as f64 / (w * h) as f64), opaque })
}

/// Whether what was measured is within the tolerance, and if not, what is wrong first.
fn check(frame: usize, m: &Measured, tol: &Tolerance) -> Result<(), Mismatch> {
    if !m.opaque {
        Err(Mismatch::Alpha { frame })
    } else if m.frame_db < tol.frame_db {
        Err(Mismatch::Frame { frame, db: m.frame_db })
    } else if m.block_db < tol.block_db {
        Err(Mismatch::Block { frame, db: m.block_db })
    } else if m.shift.iter().any(|s| s.abs() > tol.mean_shift) {
        Err(Mismatch::Shift { frame })
    } else {
        Ok(())
    }
}

impl Recording {
    /// Write this recording to `out` as a video and verify it: one picture per frame, in
    /// order, and the frames in [`sample`]`(frames, must)` compared with their pictures
    /// against `tol`. `keep_going` is told the pass and how many frames of it are done, at
    /// least before every frame, and stops the work by answering false.
    ///
    /// One frame is held at a time in both passes. On any error, a cancel included, nothing
    /// is left at `out` (what stood there before is gone too), and nothing but `out` is ever
    /// written. `out` must not be the recording's own frames file: that is refused, and the
    /// frames stay.
    pub fn archive_video(&self, codec: &dyn VideoCodec, out: &Path, must: &[usize], tol: &Tolerance, keep_going: &dyn Fn(Stage, usize, usize) -> bool) -> Result<Archived, ArchiveError> {
        archive(self, codec, out, must, tol, keep_going).map(|(archived, _)| archived)
    }
}

/// [`Recording::archive_video`], and with it the largest mean shift of any channel of any
/// frame that was compared.
fn archive(rec: &Recording, codec: &dyn VideoCodec, out: &Path, must: &[usize], tol: &Tolerance, keep_going: &dyn Fn(Stage, usize, usize) -> bool) -> Result<(Archived, f64), ArchiveError> {
    // The one file that must never be written over or removed here: the frames themselves.
    let frames_file = std::fs::canonicalize(rec.dir.join(FRAMES_FILE));
    if frames_file.is_ok_and(|frames| std::fs::canonicalize(out).is_ok_and(|out| out == frames)) {
        return Err(ArchiveError::Unusable("the video cannot take the place of the frames"));
    }
    let result = encode_and_verify(rec, codec, out, must, tol, keep_going);
    if result.is_err() {
        // Whatever the codec left, or whatever stood here before: it is not this recording.
        let _ = std::fs::remove_file(out);
    }
    result
}

const FRAMES_UNREADABLE: &str = "the frames could not be read";

fn encode_and_verify(rec: &Recording, codec: &dyn VideoCodec, out: &Path, must: &[usize], tol: &Tolerance, keep_going: &dyn Fn(Stage, usize, usize) -> bool) -> Result<(Archived, f64), ArchiveError> {
    let n = rec.frames.len();
    if n == 0 {
        return Err(ArchiveError::Unusable("the recording has no frames"));
    }
    let (w, h) = (rec.width, rec.height);
    let len = (w as u64).checked_mul(h as u64).and_then(|px| px.checked_mul(4)).and_then(|b| usize::try_from(b).ok()).filter(|b| *b > 0).ok_or(ArchiveError::Unusable("the picture size is not usable"))?;
    let lossless = std::fs::metadata(rec.dir.join(FRAMES_FILE)).ok().filter(|m| m.is_file()).map(|m| m.len()).ok_or(ArchiveError::Unusable(FRAMES_UNREADABLE))?;
    let max_bytes = allowed_bytes(lossless, tol.smaller_by);

    // The first pass: every frame, one at a time, into the codec.
    {
        let mut frames = Frames::open(rec, len).map_err(ArchiveError::Unusable)?;
        let mut stopped = None;
        let mut given = 0usize;
        let mut next = |buf: &mut Vec<u8>| -> Result<bool, String> {
            if given == n {
                return Ok(false);
            }
            if !keep_going(Stage::Encoding, given, n) {
                stopped = Some(ArchiveError::Cancelled);
                return Err(String::new());
            }
            match frames.next() {
                Ok(px) => {
                    buf.clear();
                    buf.extend_from_slice(px);
                    given += 1;
                    Ok(true)
                }
                Err(why) => {
                    stopped = Some(ArchiveError::Unusable(why));
                    Err(String::new())
                }
            }
        };
        let result = codec.encode((w, h), n, &mut next, out, max_bytes, &|done| keep_going(Stage::Encoding, done, n));
        // Why this side stopped comes before what the codec made of it.
        if let Some(why) = stopped {
            return Err(why);
        }
        match result {
            Ok(()) if given == n => {}
            Ok(()) => return Err(ArchiveError::Codec(CodecError::WrongFrames { got: given })),
            Err(CodecError::Cancelled) => return Err(ArchiveError::Cancelled),
            Err(CodecError::TooLarge) => return Err(ArchiveError::NotSmaller),
            Err(e) => return Err(ArchiveError::Codec(e)),
        }
    }
    // The codec's word is not taken for the file or for its size.
    let bytes = std::fs::metadata(out).ok().filter(|m| m.is_file() && m.len() > 0).map(|m| m.len()).ok_or(ArchiveError::Codec(CodecError::Failed { exit: None }))?;
    if bytes > max_bytes {
        return Err(ArchiveError::NotSmaller);
    }

    // The second pass: the video read back, and the frames walked again in step.
    let picked = sample(n, must);
    let mut frames = Frames::open(rec, len).map_err(ArchiveError::Unusable)?;
    let mut stopped = None;
    let (mut seen, mut at, mut checked) = (0usize, 0usize, 0usize);
    let (mut worst_frame_db, mut worst_block_db, mut worst_shift) = (f64::INFINITY, f64::INFINITY, 0.0f64);
    let mut each = |i: usize, picture: &[u8]| -> Result<(), String> {
        let mut verdict = || -> Result<(), ArchiveError> {
            if i != seen || i >= n {
                // A picture too many, or pictures out of order.
                return Err(ArchiveError::Verify(Mismatch::Count { got: seen + 1 }));
            }
            if picture.len() != len {
                return Err(ArchiveError::Verify(Mismatch::Count { got: seen }));
            }
            if !keep_going(Stage::Verifying, i, n) {
                return Err(ArchiveError::Cancelled);
            }
            let frame = frames.next().map_err(ArchiveError::Unusable)?;
            seen += 1;
            if picked.get(at) != Some(&i) {
                return Ok(());
            }
            at += 1;
            let m = measure(frame, picture, w, h, tol.block).ok_or(ArchiveError::Unusable("the picture size is not usable"))?;
            checked += 1;
            worst_frame_db = worst_frame_db.min(m.frame_db);
            worst_block_db = worst_block_db.min(m.block_db);
            worst_shift = m.shift.iter().fold(worst_shift, |worst, s| worst.max(s.abs()));
            check(i, &m, tol).map_err(ArchiveError::Verify)
        };
        verdict().map_err(|why| {
            stopped = Some(why);
            String::new()
        })
    };
    if !keep_going(Stage::Verifying, 0, n) {
        return Err(ArchiveError::Cancelled);
    }
    let result = codec.decode(out, (w, h), n, &mut each, &|done| keep_going(Stage::Verifying, done, n));
    if let Some(why) = stopped {
        return Err(why);
    }
    match result {
        Ok(()) if seen == n => {}
        Ok(()) => return Err(ArchiveError::Verify(Mismatch::Count { got: seen })),
        Err(CodecError::Cancelled) => return Err(ArchiveError::Cancelled),
        Err(CodecError::WrongFrames { got }) => return Err(ArchiveError::Verify(Mismatch::Count { got })),
        Err(e) => return Err(ArchiveError::Codec(e)),
    }
    Ok((Archived { bytes, checked, worst_frame_db, worst_block_db }, worst_shift))
}

/// The frames of a recording, one after the other: the chain of differences walked once,
/// with one frame, one difference and one compressed frame held, however long it is.
struct Frames<'a> {
    rec: &'a Recording,
    file: File,
    file_len: u64,
    /// The frame that was read last.
    px: Buf,
    /// The difference that made it out of the one before.
    delta: Buf,
    packed: Vec<u8>,
    at: usize,
}

impl<'a> Frames<'a> {
    fn open(rec: &'a Recording, len: usize) -> Result<Frames<'a>, &'static str> {
        let file = File::open(rec.dir.join(FRAMES_FILE)).map_err(|_| FRAMES_UNREADABLE)?;
        let file_len = file.metadata().map_err(|_| FRAMES_UNREADABLE)?.len();
        Ok(Frames { rec, file, file_len, px: Buf::new(len), delta: Buf::new(len), packed: Vec::new(), at: 0 })
    }

    /// The next frame, as RGBA. An error when there is none, or it cannot be read whole.
    fn next(&mut self) -> Result<&[u8], &'static str> {
        let m = self.rec.frames.get(self.at).ok_or("the recording has no such frame")?;
        // An index that points past the file is not followed into a huge buffer.
        if m.offset.checked_add(m.len as u64).is_none_or(|end| end > self.file_len) {
            return Err(FRAMES_UNREADABLE);
        }
        self.packed.resize(m.len as usize, 0);
        self.file.seek(SeekFrom::Start(m.offset)).and_then(|_| self.file.read_exact(&mut self.packed)).map_err(|_| FRAMES_UNREADABLE)?;
        let whole = |written: std::io::Result<usize>, len: usize| if written.is_ok_and(|n| n == len) { Ok(()) } else { Err(FRAMES_UNREADABLE) };
        if m.key {
            whole(zstd::bulk::decompress_to_buffer(&self.packed, &mut self.px.0[..]), self.px.0.len())?;
        } else if self.at == 0 {
            return Err("the first frame is not a whole one");
        } else {
            whole(zstd::bulk::decompress_to_buffer(&self.packed, &mut self.delta.0[..]), self.delta.0.len())?;
            xor_into(&mut self.px.0, &self.delta.0);
        }
        self.at += 1;
        Ok(&self.px.0)
    }
}

/// A buffer the size of one frame. Under test every one that is alive is counted.
struct Buf(Vec<u8>);

impl Buf {
    fn new(len: usize) -> Buf {
        #[cfg(test)]
        live::born();
        Buf(vec![0; len])
    }
}

#[cfg(test)]
impl Drop for Buf {
    fn drop(&mut self) {
        live::gone();
    }
}

/// How many frame buffers of this module are alive on this thread, and the most there were.
#[cfg(test)]
mod live {
    use std::cell::Cell;

    thread_local! {
        static NOW: Cell<usize> = const { Cell::new(0) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }

    pub fn born() {
        let now = NOW.get() + 1;
        NOW.set(now);
        PEAK.set(PEAK.get().max(now));
    }

    pub fn gone() {
        NOW.set(NOW.get() - 1);
    }

    pub fn now() -> usize {
        NOW.get()
    }

    pub fn peak() -> usize {
        PEAK.get()
    }

    pub fn reset() {
        PEAK.set(NOW.get());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FrameWriter;
    use crate::ffmpeg::{Ffmpeg, find_now};
    use std::io::Write;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SKIPPED: &str = "skipped: ffmpeg with libx264 was not found on this computer";

    /// Any size is small enough: for tiny test recordings, whose frames compress to almost
    /// nothing, and for the fake codec, which stores frames raw.
    const ANY_SIZE: Tolerance = Tolerance { smaller_by: -1.0e12, ..TOLERANCE };
    /// Nothing but the count and alpha is refused: for measuring.
    const MEASURE_ONLY: Tolerance = Tolerance { frame_db: 0.0, block_db: 0.0, block: 64, mean_shift: 255.0, smaller_by: -1.0e12 };

    // ---- pictures ----

    fn flat(c: [u8; 3], w: u32, h: u32) -> Vec<u8> {
        [c[0], c[1], c[2], 255].repeat((w * h) as usize)
    }

    fn put(px: &mut [u8], w: usize, x: usize, y: usize, c: [u8; 3]) {
        if x < w && (y * w + x) * 4 + 4 <= px.len() {
            px[(y * w + x) * 4..][..3].copy_from_slice(&c);
        }
    }

    fn bit(a: usize, b: usize, c: usize, d: usize) -> bool {
        let n = a.wrapping_mul(73_856_093) ^ b.wrapping_mul(19_349_663) ^ c.wrapping_mul(83_492_791) ^ d.wrapping_mul(2_654_435_761);
        (n >> 7) & 1 == 1
    }

    /// Frame `i` of something like a screen: a flat title bar, a dark side panel and a light
    /// page (flat panels), lines of dark glyph-sized marks whose lengths change from frame to
    /// frame, one line every `pitch` pixels (text-like sharp edges), six squares of saturated
    /// flat colour, and a block the
    /// size of a pointer, white with a black outline, that moves every frame. With `striped`,
    /// also a patch of one-pixel red, green and blue stripes.
    fn screen_with(i: usize, w: u32, h: u32, pitch: usize, striped: bool) -> Vec<u8> {
        let (w, h) = (w as usize, h as usize);
        let mut px = vec![255u8; w * h * 4];
        let side = w / 5;
        for y in 0..h {
            for x in 0..w {
                let c = if y < 12 {
                    [0, 120, 215]
                } else if x < side {
                    [32, 34, 40]
                } else {
                    [245, 245, 245]
                };
                put(&mut px, w, x, y, c);
            }
        }
        let cells = (w - side).saturating_sub(10) / 6;
        let text_end = h.saturating_sub(36);
        for (line, top) in (18..text_end.saturating_sub(8)).step_by(pitch).enumerate() {
            let length = if cells == 0 { 0 } else { 3 + (line * 7 + i) % cells };
            for col in 0..length.min(cells) {
                // A mark of up to seven strokes, one pixel wide, as the segments of a digit.
                let strokes: [(std::ops::Range<usize>, std::ops::Range<usize>); 7] = [(0..5, 0..1), (0..5, 3..4), (0..5, 6..7), (0..1, 0..4), (4..5, 0..4), (0..1, 3..7), (4..5, 3..7)];
                for (k, (xs, ys)) in strokes.into_iter().enumerate() {
                    if k == (line + col) % 7 || bit(line, col, k, 1) {
                        for gy in ys {
                            for gx in xs.clone() {
                                put(&mut px, w, side + 6 + col * 6 + gx, top + gy, [20, 20, 20]);
                            }
                        }
                    }
                }
            }
        }
        let saturated = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [255, 0, 255], [0, 255, 255]];
        for (k, c) in saturated.iter().enumerate() {
            for y in 0..12 {
                for x in 0..12 {
                    put(&mut px, w, side + 6 + k * 16 + x, h.saturating_sub(32) + y, *c);
                }
            }
        }
        if striped {
            for y in 0..10 {
                for x in 0..30 {
                    put(&mut px, w, side + 6 + x, h.saturating_sub(14) + y, saturated[x % 3]);
                }
            }
        }
        let (px0, py0) = (side + 4 + (i * 5) % (w - side).saturating_sub(16).max(1), 16 + (i * 3) % h.saturating_sub(40).max(1));
        for y in 0..14 {
            for x in 0..9 {
                let edge = x == 0 || y == 0 || x == 8 || y == 13;
                put(&mut px, w, px0 + x, py0 + y, if edge { [0, 0, 0] } else { [255, 255, 255] });
            }
        }
        px
    }

    fn screen(i: usize, w: u32, h: u32) -> Vec<u8> {
        screen_with(i, w, h, 22, false)
    }

    /// A whole picture of one-pixel red, green and blue columns.
    fn stripes(w: u32, h: u32) -> Vec<u8> {
        let mut px = flat([0, 0, 0], w, h);
        for y in 0..h as usize {
            for x in 0..w as usize {
                put(&mut px, w as usize, x, y, [[255, 0, 0], [0, 255, 0], [0, 0, 255]][x % 3]);
            }
        }
        px
    }

    /// Four saturated colours that meet in sharp edges.
    fn quarters(w: u32, h: u32) -> Vec<u8> {
        let mut px = flat([0, 0, 0], w, h);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let c = match (x < w as usize / 2, y < h as usize / 2) {
                    (true, true) => [255, 0, 0],
                    (false, true) => [0, 0, 255],
                    (true, false) => [0, 255, 0],
                    (false, false) => [255, 0, 255],
                };
                put(&mut px, w as usize, x, y, c);
            }
        }
        px
    }

    /// A recording of these pictures, as the recorder leaves one, owning `dir`.
    fn recording_of(dir: &Path, w: u32, h: u32, pictures: &[Vec<u8>]) -> Recording {
        let mut writer = FrameWriter::create(dir, w, h).unwrap();
        for (i, px) in pictures.iter().enumerate() {
            writer.push(px, i as u64 * 131, None).unwrap();
        }
        writer.finish(dir.to_path_buf(), pictures.len() as u64 * 131).unwrap()
    }

    fn screens(w: u32, h: u32, count: usize) -> Vec<Vec<u8>> {
        (0..count).map(|i| screen(i, w, h)).collect()
    }

    fn verdict(a: &[u8], b: &[u8], w: u32, h: u32, tol: &Tolerance) -> Result<(), Mismatch> {
        check(7, &measure(a, b, w, h, tol.block).expect("the pictures fit"), tol)
    }

    // ---- the measures ----

    #[test]
    fn identical_frames_measure_as_infinite_and_pass() {
        let (w, h) = (96, 80);
        let a = screen(3, w, h);
        assert_eq!(psnr_rgb(&a, &a), f64::INFINITY);
        assert_eq!(worst_block_psnr(&a, &a, w, h, 64), f64::INFINITY);
        assert_eq!(mean_shift(&a, &a), [0.0; 3]);
        let m = measure(&a, &a, w, h, 64).unwrap();
        assert_eq!(m, Measured { frame_db: f64::INFINITY, block_db: f64::INFINITY, shift: [0.0; 3], opaque: true });
        assert_eq!(check(0, &m, &TOLERANCE), Ok(()));
        // The planned numbers, and the one that was measured since.
        assert_eq!((TOLERANCE.frame_db, TOLERANCE.block_db, TOLERANCE.block, TOLERANCE.smaller_by), (40.0, 30.0, 64, 0.20));
        assert_eq!(TOLERANCE.mean_shift, 1.5);
    }

    /// The plan's tolerance for the mean shift was 0.5; one level of 8-bit rounding on a
    /// saturated colour made it 1.5 (see [`TOLERANCE`]). So a plus one on every channel fails
    /// only the mean shift under the planned 0.5, and it takes a plus two to fail (and again
    /// only the mean shift) under the tolerance that ships.
    #[test]
    fn a_plus_one_on_every_channel_is_about_48_db_and_fails_only_the_mean_shift() {
        let (w, h) = (96, 80);
        let a = flat([100, 150, 200], w, h);
        let plus = |n: u8| flat([100 + n, 150 + n, 200 + n], w, h);
        let one = plus(1);
        assert!((psnr_rgb(&a, &one) - 48.13).abs() < 0.01, "{}", psnr_rgb(&a, &one));
        assert!((worst_block_psnr(&a, &one, w, h, 64) - 48.13).abs() < 0.01);
        assert_eq!(mean_shift(&a, &one), [1.0; 3]);
        assert_eq!(mean_shift(&one, &a), [-1.0; 3]);
        let planned = Tolerance { mean_shift: 0.5, ..TOLERANCE };
        assert_eq!(verdict(&a, &one, w, h, &planned), Err(Mismatch::Shift { frame: 7 }));
        assert_eq!(verdict(&one, &a, w, h, &planned), Err(Mismatch::Shift { frame: 7 }));
        assert_eq!(verdict(&a, &one, w, h, &TOLERANCE), Ok(()));

        let two = plus(2);
        assert!((psnr_rgb(&a, &two) - 42.11).abs() < 0.01, "{}", psnr_rgb(&a, &two));
        assert_eq!(mean_shift(&a, &two), [2.0; 3]);
        assert_eq!(verdict(&a, &two, w, h, &TOLERANCE), Err(Mismatch::Shift { frame: 7 }));
        assert_eq!(verdict(&two, &a, w, h, &TOLERANCE), Err(Mismatch::Shift { frame: 7 }));
        // Without the mean shift it would pass: the other two measures do not see it.
        assert_eq!(verdict(&a, &two, w, h, &Tolerance { mean_shift: 2.0, ..TOLERANCE }), Ok(()));
    }

    #[test]
    fn a_one_level_rounding_on_a_saturated_colour_passes_and_a_three_level_cast_fails() {
        let (w, h) = (96, 80);
        // What the real codec does to flat pure red and pure blue: one level off, on that channel.
        for (sent, back) in [([255, 0, 0], [254, 0, 0]), ([0, 0, 255], [0, 0, 254]), ([255, 0, 0], [255, 1, 0]), ([0, 0, 0], [1, 1, 1])] {
            let (a, b) = (flat(sent, w, h), flat(back, w, h));
            assert_eq!(verdict(&a, &b, w, h, &TOLERANCE), Ok(()), "{sent:?} came back {back:?}");
            assert!(mean_shift(&a, &b).iter().all(|s| s.abs() <= 1.0));
        }
        // A cast of three levels on one channel is far above 40 dB as a frame and in every
        // block, and is still refused, in either direction and on any channel.
        for channel in 0..3 {
            for by in [3i16, -3] {
                let a = flat([120, 120, 120], w, h);
                let mut c = [120u8; 3];
                c[channel] = (120 + by) as u8;
                let b = flat(c, w, h);
                assert!(psnr_rgb(&a, &b) > 43.0 && worst_block_psnr(&a, &b, w, h, 64) > 43.0);
                assert_eq!(mean_shift(&a, &b)[channel], by as f64);
                assert_eq!(verdict(&a, &b, w, h, &TOLERANCE), Err(Mismatch::Shift { frame: 7 }), "channel {channel} by {by}");
            }
        }
        // Two levels fail, one and a half is the edge.
        let a = flat([120, 120, 120], w, h);
        assert_eq!(verdict(&a, &flat([120, 122, 120], w, h), w, h, &TOLERANCE), Err(Mismatch::Shift { frame: 7 }));
        assert_eq!(verdict(&a, &flat([120, 121, 120], w, h), w, h, &TOLERANCE), Ok(()));
    }

    #[test]
    fn one_broken_block_fails_the_block_measure_and_not_the_frame_measure() {
        let (w, h) = (640u32, 480u32);
        let a = screen(0, w, h);
        let mut b = a.clone();
        // 64 badly wrong pixels in 307200, as many up as down so the mean does not move.
        for y in 200..208usize {
            for x in 300..308usize {
                let o = (y * w as usize + x) * 4;
                for c in 0..3 {
                    b[o + c] = if (x + y) % 2 == 0 { a[o + c].wrapping_add(128) } else { a[o + c].wrapping_sub(128) };
                }
            }
        }
        let frame = psnr_rgb(&a, &b);
        let block = worst_block_psnr(&a, &b, w, h, 64);
        assert!(frame > TOLERANCE.frame_db, "{frame}");
        assert!(block < TOLERANCE.block_db, "{block}");
        assert!((block - 24.05).abs() < 0.05, "{block}");
        assert!(mean_shift(&a, &b).iter().all(|s| s.abs() < 0.5), "{:?}", mean_shift(&a, &b));
        assert_eq!(verdict(&a, &b, w, h, &TOLERANCE), Err(Mismatch::Block { frame: 7, db: block }));
        // With nothing but the frame measure it would have passed.
        assert_eq!(verdict(&a, &b, w, h, &Tolerance { block_db: 0.0, ..TOLERANCE }), Ok(()));
        // The block that holds the damage is found wherever it lies, the smaller ones at the
        // right and bottom edges included (640 × 480 leaves a last row of 32).
        let mut edge = a.clone();
        let o = ((h as usize - 1) * w as usize + w as usize - 1) * 4;
        edge[o] ^= 0x80;
        let at_edge = worst_block_psnr(&a, &edge, w, h, 64);
        let expected = 10.0 * (255.0f64 * 255.0 / (128.0 * 128.0 / (64.0 * 32.0 * 3.0))).log10();
        assert!((at_edge - expected).abs() < 1e-9, "{at_edge} {expected}");
        // A block as large as the picture is the frame measure.
        assert_eq!(worst_block_psnr(&a, &b, w, h, 4096), frame);
    }

    #[test]
    fn a_limited_range_round_trip_fails_the_frame_measure() {
        let (w, h) = (256, 192);
        let a = screen(5, w, h);
        // What a range mistake does: full range squeezed into 16..235 and never widened again.
        let mut b = a.clone();
        for p in b.chunks_exact_mut(4) {
            for v in &mut p[..3] {
                *v = (16.0 + *v as f64 * 219.0 / 255.0).round() as u8;
            }
        }
        let frame = psnr_rgb(&a, &b);
        assert!(frame < 30.0, "{frame}");
        assert_eq!(verdict(&a, &b, w, h, &TOLERANCE), Err(Mismatch::Frame { frame: 7, db: frame }));
        // The other way round (limited range widened twice) fails it as well.
        let mut c = a.clone();
        for p in c.chunks_exact_mut(4) {
            for v in &mut p[..3] {
                *v = ((*v as f64 - 16.0) * 255.0 / 219.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        assert!(matches!(verdict(&a, &c, w, h, &TOLERANCE), Err(Mismatch::Frame { frame: 7, db }) if db < 30.0));
        // And so does the other matrix (BT.601 written, BT.709 read): on saturated colour.
        let a = quarters(w, h);
        let mut d = a.clone();
        for p in d.chunks_exact_mut(4) {
            let (r, g, b) = (p[0] as f64, p[1] as f64, p[2] as f64);
            let y = 0.299 * r + 0.587 * g + 0.114 * b;
            let (cb, cr) = ((b - y) / 1.772, (r - y) / 1.402);
            let back = [y + 1.5748 * cr, y - 0.1873 * cb - 0.4681 * cr, y + 1.8556 * cb];
            for (v, n) in p[..3].iter_mut().zip(back) {
                *v = n.round().clamp(0.0, 255.0) as u8;
            }
        }
        assert!(matches!(verdict(&a, &d, w, h, &TOLERANCE), Err(Mismatch::Frame { frame: 7, db }) if db < 30.0), "{:?}", verdict(&a, &d, w, h, &TOLERANCE));
    }

    #[test]
    fn alpha_is_not_measured_and_must_be_255() {
        let (w, h) = (96, 80);
        let a = screen(1, w, h);
        let mut b = a.clone();
        b[4 * 500 + 3] = 254;
        // No measure sees it.
        assert_eq!(psnr_rgb(&a, &b), f64::INFINITY);
        assert_eq!(worst_block_psnr(&a, &b, w, h, 64), f64::INFINITY);
        assert_eq!(mean_shift(&a, &b), [0.0; 3]);
        // And it is refused all the same, before anything else.
        assert_eq!(verdict(&a, &b, w, h, &TOLERANCE), Err(Mismatch::Alpha { frame: 7 }));
        let mut worse = b.clone();
        worse[0] ^= 0xff;
        assert_eq!(verdict(&a, &worse, w, h, &MEASURE_ONLY), Err(Mismatch::Alpha { frame: 7 }));
        // The alpha of the frame itself does not matter: the picture's must be 255.
        let mut clear = a.clone();
        clear[3] = 0;
        assert_eq!(verdict(&clear, &a, w, h, &TOLERANCE), Ok(()));
        // A whole picture of alpha 0.
        let gone: Vec<u8> = a.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2], 0]).collect();
        assert_eq!(verdict(&a, &gone, w, h, &TOLERANCE), Err(Mismatch::Alpha { frame: 7 }));
    }

    #[test]
    fn pictures_that_do_not_fit_measure_as_nothing_alike() {
        let a = flat([1, 2, 3], 8, 8);
        assert_eq!(psnr_rgb(&a, &a[..a.len() - 4]), 0.0);
        assert_eq!(psnr_rgb(&[], &[]), 0.0);
        assert_eq!(psnr_rgb(&a[..7], &a[..7]), 0.0);
        assert_eq!(mean_shift(&a, &a[..a.len() - 4]), [255.0; 3]);
        assert_eq!(mean_shift(&[], &[]), [255.0; 3]);
        assert_eq!(worst_block_psnr(&a, &a, 8, 7, 64), 0.0);
        assert_eq!(worst_block_psnr(&a, &a, 0, 8, 64), 0.0);
        assert_eq!(worst_block_psnr(&a, &a, u32::MAX, u32::MAX, 64), 0.0);
        assert!(measure(&a, &a, 4, 8, 64).is_none());
        assert!(measure(&a, &a[..8], 8, 8, 64).is_none());
        // A block of nothing is taken as one pixel, not as a division by nothing.
        assert_eq!(worst_block_psnr(&a, &a, 8, 8, 0), f64::INFINITY);
        // The one walk agrees with the three measures.
        let (w, h) = (100, 70);
        let (a, b) = (screen(2, w, h), screen(9, w, h));
        let m = measure(&a, &b, w, h, 64).unwrap();
        assert_eq!(m.frame_db, psnr_rgb(&a, &b));
        assert_eq!(m.block_db, worst_block_psnr(&a, &b, w, h, 64));
        assert_eq!(m.shift, mean_shift(&a, &b));
        assert!(m.block_db <= m.frame_db && m.frame_db.is_finite());
    }

    // ---- the sample ----

    #[test]
    fn a_short_recording_is_checked_whole() {
        assert_eq!(sample(0, &[]), Vec::<usize>::new());
        assert_eq!(sample(0, &[0, 5]), Vec::<usize>::new());
        assert_eq!(sample(1, &[]), vec![0]);
        assert_eq!(sample(40, &[3, 3, 99]), (0..40).collect::<Vec<_>>());
        assert_eq!(sample(600, &[]), (0..600).collect::<Vec<_>>());
        assert!(sample(601, &[]).len() < 601);
    }

    #[test]
    fn a_long_recording_checks_first_last_one_per_group_and_every_step_frame() {
        let picked = sample(1000, &[500, 7, 7, 31, 998]);
        let mut expected: Vec<usize> = (0..1000).filter(|i| i % 30 == 29).collect();
        expected.extend([0, 999, 500, 7, 31, 998]);
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(picked, expected);
        // In order, each once: 33 groups of 30 fit in 1000, plus the first, the last and four steps.
        assert!(picked.windows(2).all(|p| p[0] < p[1]));
        assert_eq!(picked.len(), 33 + 2 + 4);
        // A step frame that is already picked is not picked twice.
        assert_eq!(sample(1000, &[0, 29, 999]), sample(1000, &[]));
        // The group is the encoder's group of pictures.
        assert_eq!(GOP, 30);
        // 601 frames: the last frame is the first of its group, and is still checked.
        let picked = sample(601, &[]);
        assert_eq!((picked[0], picked[1], *picked.last().unwrap(), picked.len()), (0, 29, 600, 22));
    }

    #[test]
    fn a_step_frame_beyond_the_recording_is_ignored() {
        let picked = sample(700, &[700, 701, 9999, usize::MAX, 699, 12]);
        assert!(picked.iter().all(|i| *i < 700), "{picked:?}");
        assert!(picked.contains(&12) && picked.contains(&699));
        assert_eq!(picked, sample(700, &[12]));
        assert_eq!(sample(3, &[3, 10]), vec![0, 1, 2]);
    }

    #[test]
    fn the_video_may_take_what_the_tolerance_leaves_of_the_frames() {
        assert_eq!(allowed_bytes(1000, 0.20), 800);
        assert_eq!(allowed_bytes(1001, 0.20), 800);
        assert_eq!(allowed_bytes(172_000_000, 0.20), 137_600_000);
        assert_eq!(allowed_bytes(0, 0.20), 0);
        assert_eq!(allowed_bytes(1000, 0.0), 1000);
        assert_eq!(allowed_bytes(1000, 1.0), 0);
        assert_eq!(allowed_bytes(1000, 7.0), 0);
        assert_eq!(allowed_bytes(1000, f64::NAN), 0);
        assert_eq!(allowed_bytes(1000, -1.0), 2000);
        assert_eq!(allowed_bytes(1000, f64::NEG_INFINITY), u64::MAX);
        assert!(allowed_bytes(1, ANY_SIZE.smaller_by) > 1 << 39);
        assert_eq!(allowed_bytes(u64::MAX, 0.0), u64::MAX);
    }

    // ---- a codec that is not ffmpeg ----

    /// What the fake codec does wrong.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Fault {
        None,
        /// Hands out one picture fewer and says so, as a real decoder does.
        Drop,
        /// Hands out one picture fewer and says all is well.
        DropQuietly,
        /// Says there is one picture more than asked for.
        Extra,
        /// Hands out one picture more and says all is well.
        ExtraQuietly,
        /// Hands out a picture of another size.
        Short,
        /// Adds this much to one channel of every picture.
        Shift { channel: usize, by: u8 },
        /// Breaks 8 × 8 pixels of the second picture.
        BreakBlock,
        /// Hands out one pixel of the second picture with alpha 0.
        AlphaZero,
        /// Writes this many bytes more than it was allowed, and says all is well.
        Grow(u64),
        /// Says all is well and writes nothing.
        Nothing,
        /// Cannot read its own video back.
        Unreadable,
    }

    /// A codec that stores the frames raw, one after the other, in the file it is given.
    struct Fake {
        fault: Fault,
        /// Whether it asks `keep_going` itself, as the trait says it must.
        asks: bool,
        /// Whether it removes its file when it fails, as the trait says it must.
        tidy: bool,
        /// The most frames it held itself at once.
        held: AtomicUsize,
        /// The most frame buffers the archive held while this codec was at work.
        seen_alive: AtomicUsize,
        /// Every file it was asked to write or read.
        touched: Mutex<Vec<std::path::PathBuf>>,
    }

    impl Fake {
        fn new(fault: Fault) -> Fake {
            Fake { fault, asks: true, tidy: true, held: AtomicUsize::new(0), seen_alive: AtomicUsize::new(0), touched: Mutex::new(Vec::new()) }
        }

        fn look(&self) {
            self.held.fetch_max(1, Ordering::Relaxed);
            self.seen_alive.fetch_max(live::now(), Ordering::Relaxed);
        }

        fn fail(&self, out: &Path, error: CodecError) -> Result<(), CodecError> {
            if self.tidy {
                let _ = std::fs::remove_file(out);
            }
            Err(error)
        }
    }

    impl VideoCodec for Fake {
        fn id(&self) -> &str {
            "fake-raw"
        }

        fn encode(&self, _size: (u32, u32), frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            self.touched.lock().unwrap().push(out.to_path_buf());
            let _ = std::fs::remove_file(out);
            let mut file = File::create(out).map_err(CodecError::Io)?;
            let mut buf = Vec::new();
            let mut total = 0u64;
            for i in 0..frames {
                buf.clear();
                match next(&mut buf) {
                    Err(text) => return self.fail(out, CodecError::Caller(text)),
                    Ok(false) => return self.fail(out, CodecError::WrongFrames { got: i }),
                    Ok(true) => {}
                }
                self.look();
                if self.fault != Fault::Nothing {
                    file.write_all(&buf).map_err(CodecError::Io)?;
                }
                total += buf.len() as u64;
                if total > max_bytes {
                    return self.fail(out, CodecError::TooLarge);
                }
                if self.asks && !keep_going(i + 1) {
                    return self.fail(out, CodecError::Cancelled);
                }
            }
            if let Fault::Grow(more) = self.fault {
                file.write_all(&vec![0u8; (max_bytes - total + more) as usize]).map_err(CodecError::Io)?;
            }
            Ok(())
        }

        fn decode(&self, video: &Path, size: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            self.touched.lock().unwrap().push(video.to_path_buf());
            if self.fault == Fault::Unreadable {
                return Err(CodecError::Failed { exit: Some(1) });
            }
            let mut file = File::open(video).map_err(CodecError::Io)?;
            let len = (size.0 * size.1 * 4) as usize;
            let w = size.0 as usize;
            let mut frame = vec![0u8; len];
            let give = match self.fault {
                Fault::Drop | Fault::DropQuietly => frames - 1,
                _ => frames,
            };
            for i in 0..give {
                file.read_exact(&mut frame).map_err(CodecError::Io)?;
                match self.fault {
                    Fault::Shift { channel, by } => frame.chunks_exact_mut(4).for_each(|p| p[channel] = p[channel].saturating_add(by)),
                    Fault::BreakBlock if i == 1 => {
                        for y in 64..72 {
                            for x in 64..72 {
                                let o = (y * w + x) * 4;
                                for v in &mut frame[o..o + 3] {
                                    *v = if (x + y) % 2 == 0 { v.wrapping_add(128) } else { v.wrapping_sub(128) };
                                }
                            }
                        }
                    }
                    Fault::AlphaZero if i == 1 => frame[4 * 17 + 3] = 0,
                    _ => {}
                }
                self.look();
                let handed = if self.fault == Fault::Short && i == 1 { &frame[..len - 4] } else { &frame[..] };
                each(i, handed).map_err(CodecError::Caller)?;
                if self.asks && !keep_going(i + 1) {
                    return Err(CodecError::Cancelled);
                }
            }
            match self.fault {
                Fault::Drop => Err(CodecError::WrongFrames { got: frames - 1 }),
                Fault::Extra => Err(CodecError::WrongFrames { got: frames + 1 }),
                Fault::ExtraQuietly => each(frames, &frame).map_err(CodecError::Caller),
                _ => Ok(()),
            }
        }
    }

    /// A recording of `count` screen-like frames in a folder of its own, and an empty folder
    /// for the video.
    struct Bench {
        rec: Recording,
        pictures: Vec<Vec<u8>>,
        _frames_dir: tempfile::TempDir,
        out_dir: tempfile::TempDir,
        out: std::path::PathBuf,
    }

    fn bench_of(w: u32, h: u32, pictures: Vec<Vec<u8>>) -> Bench {
        let frames_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let mut rec = recording_of(frames_dir.path(), w, h, &pictures);
        // The folder is the bench's to remove, with its tempdir.
        rec.owned = false;
        let out = out_dir.path().join("frames.mkv");
        Bench { rec, pictures, _frames_dir: frames_dir, out_dir, out }
    }

    fn bench(w: u32, h: u32, count: usize) -> Bench {
        bench_of(w, h, screens(w, h, count))
    }

    impl Bench {
        fn archive(&self, codec: &dyn VideoCodec, tol: &Tolerance) -> Result<Archived, ArchiveError> {
            self.rec.archive_video(codec, &self.out, &[], tol, &|_, _, _| true)
        }

        /// Every file in the two folders, by name.
        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = [self._frames_dir.path(), self.out_dir.path()].iter().flat_map(|d| std::fs::read_dir(d).unwrap()).map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            names
        }

        fn frames_bytes(&self) -> Vec<u8> {
            std::fs::read(self._frames_dir.path().join(FRAMES_FILE)).unwrap()
        }
    }

    #[test]
    fn a_faithful_video_is_archived_and_reported_with_its_size() {
        let (w, h, n) = (96, 80, 37);
        let b = bench(w, h, n);
        let before = b.frames_bytes();
        let codec = Fake::new(Fault::None);
        let progress = Mutex::new(Vec::new());
        let archived = b
            .rec
            .archive_video(&codec, &b.out, &[], &ANY_SIZE, &|stage, done, total| {
                progress.lock().unwrap().push((stage, done, total));
                true
            })
            .unwrap();
        let raw = (w * h * 4) as u64 * n as u64;
        assert_eq!(archived, Archived { bytes: raw, checked: n, worst_frame_db: f64::INFINITY, worst_block_db: f64::INFINITY });
        assert_eq!(std::fs::metadata(&b.out).unwrap().len(), raw);
        // The video holds the frames, in order.
        let video = std::fs::read(&b.out).unwrap();
        for (i, px) in b.pictures.iter().enumerate() {
            assert!(video[i * px.len()..][..px.len()] == px[..], "frame {i}");
        }
        // Nothing but the video was written, and the frames are untouched.
        assert_eq!(b.files(), vec!["frames.mkv".to_string(), FRAMES_FILE.to_string()]);
        assert_eq!(b.frames_bytes(), before);
        assert!(codec.touched.lock().unwrap().iter().all(|p| *p == b.out));
        // Progress: the encoding first, then the verifying, each to its end, always of n.
        let progress = progress.into_inner().unwrap();
        assert!(progress.iter().all(|(_, done, total)| *total == n && *done <= n));
        let turn = progress.iter().position(|(stage, _, _)| *stage == Stage::Verifying).unwrap();
        assert!(progress[..turn].iter().all(|p| p.0 == Stage::Encoding) && progress[turn..].iter().all(|p| p.0 == Stage::Verifying));
        assert!(progress.contains(&(Stage::Encoding, n, n)) && progress.contains(&(Stage::Verifying, n, n)));
        // A video that was there before is replaced, not appended to.
        let again = b.archive(&Fake::new(Fault::None), &ANY_SIZE).unwrap();
        assert_eq!(again.bytes, raw);

        // What is reported is the worst of what was compared: a codec that adds one level to
        // green on every picture measures 10·log10(255² ÷ ⅓) dB everywhere it is not already
        // at the top.
        let b = bench_of(w, h, (0..5u8).map(|i| flat([10 * i, 100, 200], w, h)).collect());
        let (archived, shift) = archive(&b.rec, &Fake::new(Fault::Shift { channel: 1, by: 1 }), &b.out, &[], &ANY_SIZE, &|_, _, _| true).unwrap();
        let expected = 10.0 * (255.0f64 * 255.0 * 3.0).log10();
        assert!((archived.worst_frame_db - expected).abs() < 1e-9 && (archived.worst_block_db - expected).abs() < 1e-9, "{archived:?}");
        assert_eq!((archived.checked, shift), (5, 1.0));
    }

    #[test]
    fn a_video_with_one_frame_missing_or_one_too_many_is_refused() {
        let b = bench(96, 80, 12);
        for (fault, got) in [(Fault::Drop, 11), (Fault::DropQuietly, 11), (Fault::Extra, 13), (Fault::ExtraQuietly, 13)] {
            let result = b.archive(&Fake::new(fault), &ANY_SIZE);
            assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Count { got: g })) if g == got), "{fault:?}: {result:?}");
            assert!(!b.out.exists(), "{fault:?}");
        }
        // A picture of another size is no picture: the count stops before it.
        let result = b.archive(&Fake::new(Fault::Short), &ANY_SIZE);
        assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Count { got: 1 }))), "{result:?}");
        assert!(!b.out.exists());
        // A codec that says all is well and wrote nothing, and one that cannot read its video.
        let result = b.archive(&Fake::new(Fault::Nothing), &ANY_SIZE);
        assert!(matches!(result, Err(ArchiveError::Codec(CodecError::Failed { exit: None }))), "{result:?}");
        assert!(!b.out.exists());
        let result = b.archive(&Fake::new(Fault::Unreadable), &ANY_SIZE);
        assert!(matches!(result, Err(ArchiveError::Codec(CodecError::Failed { exit: Some(1) }))), "{result:?}");
        assert!(!b.out.exists());
    }

    #[test]
    fn a_video_that_shifts_colour_is_refused() {
        let b = bench(96, 80, 8);
        let result = b.archive(&Fake::new(Fault::Shift { channel: 1, by: 3 }), &ANY_SIZE);
        assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Shift { frame: 0 }))), "{result:?}");
        assert!(!b.out.exists());
        // One level, as the rounding of a saturated colour, is not a cast.
        assert!(b.archive(&Fake::new(Fault::Shift { channel: 2, by: 1 }), &ANY_SIZE).is_ok());
        // A range mistake is refused by the frame measure.
        let result = b.archive(&Fake::new(Fault::Shift { channel: 0, by: 16 }), &ANY_SIZE);
        assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Frame { frame: 0, db })) if db < 40.0), "{result:?}");
        assert!(!b.out.exists());

        // One broken block in the second picture, and an alpha that is not 255.
        let b = bench(640, 480, 3);
        let result = b.archive(&Fake::new(Fault::BreakBlock), &ANY_SIZE);
        assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Block { frame: 1, db })) if db < 30.0), "{result:?}");
        assert!(!b.out.exists());
        let result = b.archive(&Fake::new(Fault::AlphaZero), &ANY_SIZE);
        assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Alpha { frame: 1 }))), "{result:?}");
        assert!(!b.out.exists());
    }

    /// Of a long recording only the sample is compared, and the frames named in `must` are
    /// part of it.
    #[test]
    fn a_fault_outside_the_sample_of_a_long_recording_is_seen_only_when_its_frame_must_be_checked() {
        let (w, h, n) = (16, 8, 640);
        let b = bench_of(w, h, (0..n).map(|i| flat([(i % 200) as u8, 90, 40], w, h)).collect());
        // The fake breaks the second picture's alpha; frame 1 is not in the sample of 640.
        let archived = b.rec.archive_video(&Fake::new(Fault::AlphaZero), &b.out, &[], &ANY_SIZE, &|_, _, _| true).unwrap();
        assert_eq!(archived.checked, sample(n, &[]).len());
        assert_eq!(archived.checked, 23);
        let result = b.rec.archive_video(&Fake::new(Fault::AlphaZero), &b.out, &[1, 9999], &ANY_SIZE, &|_, _, _| true);
        assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Alpha { frame: 1 }))), "{result:?}");
        assert!(!b.out.exists());
        // The count is of every picture, sampled or not.
        let result = b.archive(&Fake::new(Fault::DropQuietly), &ANY_SIZE);
        assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Count { got: 639 }))), "{result:?}");
    }

    #[test]
    fn a_video_no_smaller_than_the_frames_is_refused_as_not_smaller() {
        let b = bench(96, 80, 12);
        let lossless = std::fs::metadata(b._frames_dir.path().join(FRAMES_FILE)).unwrap().len();
        let raw = 96 * 80 * 4 * 12u64;
        assert!(raw > lossless);
        // The fake's raw frames are far larger than the compressed ones: stopped while encoding.
        let result = b.archive(&Fake::new(Fault::None), &TOLERANCE);
        assert!(matches!(result, Err(ArchiveError::NotSmaller)), "{result:?}");
        assert!(!b.out.exists());
        // Exactly at the edge: the video may take what the tolerance leaves, and not a byte more.
        let part = |allowed: u64| Tolerance { smaller_by: 1.0 - allowed as f64 / lossless as f64, ..TOLERANCE };
        let fits = part(raw);
        assert_eq!(allowed_bytes(lossless, fits.smaller_by), raw);
        assert_eq!(b.archive(&Fake::new(Fault::None), &fits).unwrap().bytes, raw);
        let tight = part(raw - 1);
        assert_eq!(allowed_bytes(lossless, tight.smaller_by), raw - 1);
        let result = b.archive(&Fake::new(Fault::None), &tight);
        assert!(matches!(result, Err(ArchiveError::NotSmaller)), "{result:?}");
        assert!(!b.out.exists());
        // A codec that does not keep to its bound and says all is well is caught by the
        // file's own size, and its file is removed.
        let result = b.archive(&Fake::new(Fault::Grow(1)), &fits);
        assert!(matches!(result, Err(ArchiveError::NotSmaller)), "{result:?}");
        assert!(!b.out.exists());
        assert_eq!(b.files(), vec![FRAMES_FILE.to_string()]);
    }

    #[test]
    fn a_refused_or_cancelled_archive_leaves_no_file() {
        let (w, h, n) = (96, 80, 12);
        let b = bench(w, h, n);
        let before = b.frames_bytes();
        // Every refusal, with a codec that tidies up and with one that leaves its file.
        for tidy in [true, false] {
            for fault in [Fault::Drop, Fault::DropQuietly, Fault::Extra, Fault::ExtraQuietly, Fault::Short, Fault::Shift { channel: 0, by: 9 }, Fault::AlphaZero, Fault::Unreadable] {
                let codec = Fake { tidy, ..Fake::new(fault) };
                assert!(b.archive(&codec, &ANY_SIZE).is_err(), "{fault:?}");
                assert_eq!(b.files(), vec![FRAMES_FILE.to_string()], "{fault:?}, tidy {tidy}");
            }
            let codec = Fake { tidy, ..Fake::new(Fault::None) };
            assert!(matches!(b.archive(&codec, &TOLERANCE), Err(ArchiveError::NotSmaller)));
            assert_eq!(b.files(), vec![FRAMES_FILE.to_string()], "not smaller, tidy {tidy}");
            // A cancel at every point of both passes.
            for stage in [Stage::Encoding, Stage::Verifying] {
                for at in [0, 1, n / 2, n - 1, n] {
                    let codec = Fake { tidy, ..Fake::new(Fault::None) };
                    let result = b.rec.archive_video(&codec, &b.out, &[], &ANY_SIZE, &|s, done, _| !(s == stage && done >= at));
                    assert!(matches!(result, Err(ArchiveError::Cancelled)), "{stage:?} at {at}: {result:?}");
                    assert_eq!(b.files(), vec![FRAMES_FILE.to_string()], "cancelled {stage:?} at {at}, tidy {tidy}");
                }
            }
        }
        // What stood at the output before a refused archive is gone too: it is not the video.
        std::fs::write(&b.out, b"an older video").unwrap();
        assert!(b.archive(&Fake::new(Fault::Drop), &ANY_SIZE).is_err());
        assert!(!b.out.exists());
        // A recording that cannot be archived at all: no frames, a size of nothing, frames
        // that are gone or cut short. Nothing is left, and nothing was asked of the codec.
        let codec = Fake::new(Fault::None);
        let empty = Recording { dir: b.rec.dir.clone(), owned: false, width: w, height: h, frames: Vec::new(), end_ms: 0 };
        let flat = Recording { dir: b.rec.dir.clone(), owned: false, width: 0, height: h, frames: b.rec.frames.clone(), end_ms: 0 };
        let gone = Recording { dir: b.out_dir.path().join("nowhere"), owned: false, width: w, height: h, frames: b.rec.frames.clone(), end_ms: 0 };
        for unusable in [&empty, &flat, &gone] {
            std::fs::write(&b.out, b"an older video").unwrap();
            let result = unusable.archive_video(&codec, &b.out, &[], &ANY_SIZE, &|_, _, _| true);
            assert!(matches!(result, Err(ArchiveError::Unusable(_))), "{result:?}");
            assert!(!b.out.exists());
        }
        assert!(codec.touched.lock().unwrap().is_empty());
        // Frames cut short are found while encoding.
        let cut_dir = tempfile::tempdir().unwrap();
        std::fs::write(cut_dir.path().join(FRAMES_FILE), &before[..before.len() / 2]).unwrap();
        let cut = Recording { dir: cut_dir.path().to_path_buf(), owned: false, width: w, height: h, frames: b.rec.frames.clone(), end_ms: 0 };
        for tidy in [true, false] {
            let result = cut.archive_video(&Fake { tidy, ..Fake::new(Fault::None) }, &b.out, &[], &ANY_SIZE, &|_, _, _| true);
            assert!(matches!(result, Err(ArchiveError::Unusable(_))), "{result:?}");
            assert!(!b.out.exists());
        }
        // Another picture size than the frames were written with: refused, not misread.
        let other = Recording { dir: b.rec.dir.clone(), owned: false, width: w + 1, height: h, frames: b.rec.frames.clone(), end_ms: 0 };
        let result = other.archive_video(&Fake::new(Fault::None), &b.out, &[], &ANY_SIZE, &|_, _, _| true);
        assert!(matches!(result, Err(ArchiveError::Unusable(_))), "{result:?}");
        assert!(!b.out.exists());
        // The frames file itself as the output: refused, and neither written over nor removed.
        let codec = Fake::new(Fault::None);
        for own in [b.rec.dir.join(FRAMES_FILE), b.rec.dir.join(".").join(FRAMES_FILE.to_uppercase())] {
            let result = b.rec.archive_video(&codec, &own, &[], &ANY_SIZE, &|_, _, _| true);
            assert!(matches!(result, Err(ArchiveError::Unusable(_))), "{result:?}");
        }
        assert!(codec.touched.lock().unwrap().is_empty());
        assert_eq!(b.frames_bytes(), before);
    }

    #[test]
    fn cancel_is_asked_every_frame_in_both_passes() {
        let n = 41;
        let b = bench(64, 48, n);
        // With a codec that asks as it must, and with one that never asks by itself.
        for asks in [true, false] {
            let asked = Mutex::new(Vec::new());
            let codec = Fake { asks, ..Fake::new(Fault::None) };
            b.rec
                .archive_video(&codec, &b.out, &[], &ANY_SIZE, &|stage, done, total| {
                    asked.lock().unwrap().push((stage, done, total));
                    true
                })
                .unwrap();
            let asked = asked.into_inner().unwrap();
            for stage in [Stage::Encoding, Stage::Verifying] {
                // Before every frame of the pass, with how many are done and of how many.
                for done in 0..n {
                    assert!(asked.contains(&(stage, done, n)), "{stage:?} was not asked at {done} (codec asks: {asks})");
                }
            }
            assert!(asked.len() >= 2 * n);
        }
        // An answer of no stops the pass at that frame: the codec is handed no further one.
        for (stage, at) in [(Stage::Encoding, 17), (Stage::Verifying, 23)] {
            let codec = Fake { asks: false, ..Fake::new(Fault::None) };
            let last = Mutex::new((Stage::Encoding, 0));
            let result = b.rec.archive_video(&codec, &b.out, &[], &ANY_SIZE, &|s, done, _| {
                *last.lock().unwrap() = (s, done);
                !(s == stage && done == at)
            });
            assert!(matches!(result, Err(ArchiveError::Cancelled)), "{result:?}");
            assert_eq!(*last.lock().unwrap(), (stage, at));
            assert!(!b.out.exists());
        }
    }

    #[test]
    fn only_one_frame_is_held_at_a_time() {
        for n in [3usize, 95] {
            let b = bench(64, 48, n);
            assert_eq!(live::now(), 0);
            live::reset();
            let codec = Fake::new(Fault::None);
            let archived = b.archive(&codec, &ANY_SIZE).unwrap();
            assert_eq!(archived.checked, n);
            // The frame being walked and the difference that makes the next one out of it:
            // two buffers, however many frames there are, and none once it is over.
            assert_eq!(live::peak(), 2, "{n} frames");
            assert_eq!(codec.seen_alive.load(Ordering::Relaxed), 2, "{n} frames");
            assert_eq!(codec.held.load(Ordering::Relaxed), 1);
            assert_eq!(live::now(), 0);
            // The same when it ends badly.
            live::reset();
            assert!(b.archive(&Fake::new(Fault::Shift { channel: 0, by: 40 }), &ANY_SIZE).is_err());
            assert!(b.rec.archive_video(&Fake::new(Fault::None), &b.out, &[], &ANY_SIZE, &|s, done, _| !(s == Stage::Verifying && done == 2)).is_err());
            assert_eq!((live::peak(), live::now()), (2, 0));
        }
    }

    // ---- the real ffmpeg, when this computer has one ----

    fn real() -> Option<&'static Ffmpeg> {
        match find_now() {
            Some(found) if found.can_archive() => Some(found),
            _ => {
                println!("{SKIPPED}");
                None
            }
        }
    }

    /// Archive with nothing refused but the count and alpha, so that everything is measured,
    /// and print what was measured.
    fn measured(name: &str, b: &Bench, codec: &dyn VideoCodec) -> (Archived, f64) {
        let (archived, shift) = archive(&b.rec, codec, &b.out, &[], &MEASURE_ONLY, &|_, _, _| true).unwrap();
        let lossless = std::fs::metadata(b.rec.dir.join(FRAMES_FILE)).unwrap().len();
        println!("measured, {name}: worst frame {:.2} dB, worst block {:.2} dB, worst mean shift {:.3} levels; {} frames checked; video {} bytes, frames {} bytes", archived.worst_frame_db, archived.worst_block_db, shift, archived.checked, archived.bytes, lossless);
        (archived, shift)
    }

    #[test]
    fn a_synthetic_recording_archives_and_verifies() {
        let Some(ffmpeg) = real() else { return };
        // Two groups of pictures and a part of a third.
        let (w, h, n) = (256, 192, 70);
        let b = bench(w, h, n);
        let before = b.frames_bytes();
        let (measured, shift) = measured("screen-like frames", &b, ffmpeg);
        assert!(measured.worst_frame_db >= TOLERANCE.frame_db && measured.worst_block_db >= TOLERANCE.block_db && shift <= TOLERANCE.mean_shift, "{measured:?} {shift}");
        // And through the front door, with the tolerance that ships (but for the size: the
        // frames of so small a recording compress to less than a video's own headers).
        let archived = b.rec.archive_video(ffmpeg, &b.out, &[3, 69, 500], &ANY_SIZE, &|_, _, _| true).unwrap();
        assert_eq!(archived, measured);
        assert_eq!(archived.checked, n);
        assert_eq!(archived.bytes, std::fs::metadata(&b.out).unwrap().len());
        assert_eq!(b.files(), vec!["frames.mkv".to_string(), FRAMES_FILE.to_string()]);
        assert_eq!(b.frames_bytes(), before);
        // A cancel in either pass leaves nothing.
        for stage in [Stage::Encoding, Stage::Verifying] {
            let result = b.rec.archive_video(ffmpeg, &b.out, &[], &ANY_SIZE, &|s, done, _| !(s == stage && done >= 31));
            assert!(matches!(result, Err(ArchiveError::Cancelled)), "{result:?}");
            assert_eq!(b.files(), vec![FRAMES_FILE.to_string()]);
        }
        // With the size the tolerance asks for, this tiny recording is not smaller as a video.
        let result = b.archive(ffmpeg, &TOLERANCE);
        assert!(matches!(result, Err(ArchiveError::NotSmaller)), "{result:?}");
        assert_eq!(b.files(), vec![FRAMES_FILE.to_string()]);
    }

    #[test]
    fn flat_colours_and_saturated_edges_come_back_within_the_tolerance() {
        let Some(ffmpeg) = real() else { return };
        let (w, h) = (256, 192);
        // Pure red, green, blue, white, black and mid grey, each for a few frames and then
        // cut to the next, and saturated colours meeting in sharp edges.
        let colours = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255], [0, 0, 0], [128, 128, 128], [255, 255, 0], [0, 255, 255], [255, 0, 255]];
        let mut pictures: Vec<Vec<u8>> = colours.iter().flat_map(|c| vec![flat(*c, w, h); 3]).collect();
        pictures.extend(vec![quarters(w, h); 3]);
        let b = bench_of(w, h, pictures);
        let (flat_measured, shift) = measured("flat colours and saturated edges", &b, ffmpeg);
        assert!(flat_measured.worst_frame_db >= TOLERANCE.frame_db && flat_measured.worst_block_db >= TOLERANCE.block_db && shift <= TOLERANCE.mean_shift, "{flat_measured:?} {shift}");
        assert!(b.archive(ffmpeg, &ANY_SIZE).is_ok());

        // One-pixel coloured stripes: a patch of them on a screen, and a whole picture.
        let b = bench_of(w, h, (0..35).map(|i| screen_with(i, w, h, 22, true)).collect());
        let (patch, patch_shift) = measured("a patch of one-pixel coloured stripes on a screen", &b, ffmpeg);
        let b = bench_of(w, h, vec![stripes(w, h); 6]);
        let (whole, whole_shift) = measured("a whole picture of one-pixel coloured stripes", &b, ffmpeg);
        assert!(patch.worst_frame_db >= TOLERANCE.frame_db && patch.worst_block_db >= TOLERANCE.block_db && patch_shift <= TOLERANCE.mean_shift, "{patch:?} {patch_shift}");
        assert!(whole.worst_frame_db >= TOLERANCE.frame_db && whole.worst_block_db >= TOLERANCE.block_db && whole_shift <= TOLERANCE.mean_shift, "{whole:?} {whole_shift}");
    }

    #[test]
    fn the_worst_measures_are_reported() {
        let Some(ffmpeg) = real() else { return };
        let (w, h, n) = (256, 192, 40);
        let b = bench(w, h, n);
        let (archived, shift) = measured("the worst measures", &b, ffmpeg);
        // Read the video back by hand and measure every picture against its frame.
        let (mut frame_db, mut block_db, mut cast) = (f64::INFINITY, f64::INFINITY, 0.0f64);
        let mut count = 0;
        let mut each = |i: usize, px: &[u8]| {
            frame_db = frame_db.min(psnr_rgb(&b.pictures[i], px));
            block_db = block_db.min(worst_block_psnr(&b.pictures[i], px, w, h, TOLERANCE.block));
            cast = mean_shift(&b.pictures[i], px).iter().fold(cast, |m, s| m.max(s.abs()));
            count += 1;
            Ok(())
        };
        ffmpeg.decode(&b.out, (w, h), n, &mut each, &|_| true).unwrap();
        assert_eq!(count, n);
        assert_eq!((archived.worst_frame_db, archived.worst_block_db, shift, archived.checked), (frame_db, block_db, cast, n));
        // Near-lossless is not lossless: the numbers are real ones.
        assert!(archived.worst_frame_db.is_finite() && archived.worst_block_db <= archived.worst_frame_db);
        assert_eq!(archived.bytes, std::fs::metadata(&b.out).unwrap().len());
    }

    /// MEASURED, not chosen: a screen filled wall to wall with small one-pixel marks (a line
    /// of them every 11 pixels, where the screen-like frames above have one every 22) came
    /// back at 39.07 dB on its worst frame with ffmpeg 9.0.2 (worst block 34.70 dB, mean
    /// shift 0.228), which is under the 40 dB a frame must reach. The weak frames are the
    /// key pictures (the first of each group of 30, and most of all the one that opens a
    /// short last group); the frames between them measured 44 to 52 dB. The threshold is not
    /// lowered for it: such a recording is refused by the frame measure and keeps its
    /// lossless frames. This test pins that the verdict follows the measure, whichever side
    /// of 40 dB another ffmpeg lands on, and prints what was measured.
    #[test]
    fn a_screen_full_of_small_sharp_text_is_judged_by_what_it_measures() {
        let Some(ffmpeg) = real() else { return };
        let (w, h, n) = (256, 192, 35);
        let b = bench_of(w, h, (0..n).map(|i| screen_with(i, w, h, 11, true)).collect());
        let (dense, shift) = measured("a screen full of small one-pixel marks (dense text)", &b, ffmpeg);
        // What was measured, with room for another version of the encoder.
        assert!(dense.worst_frame_db > 36.0 && dense.worst_frame_db < 44.0, "{dense:?}");
        assert!(dense.worst_block_db >= TOLERANCE.block_db && shift <= TOLERANCE.mean_shift, "{dense:?} {shift}");
        let result = b.archive(ffmpeg, &ANY_SIZE);
        if dense.worst_frame_db >= TOLERANCE.frame_db {
            assert_eq!(result.unwrap(), dense);
        } else {
            // Refused for a key picture, with the number that was measured, and nothing left.
            assert!(matches!(result, Err(ArchiveError::Verify(Mismatch::Frame { frame, db })) if frame % 30 == 0 && db < TOLERANCE.frame_db), "{result:?}");
            assert_eq!(b.files(), vec![FRAMES_FILE.to_string()]);
        }
    }

    /// Not run by itself: `MYTHICSHOT_MEASURE_DIR=<a saved recording's folder> cargo test -p
    /// mythicshot-recording measure_a_real_recording -- --ignored --nocapture` archives that
    /// recording into a temporary folder (the recording's own folder is only read), compares
    /// every frame, and prints the worst frame, block and mean shift, so that the proposed
    /// thresholds can be confirmed on a real recording.
    #[test]
    #[ignore = "needs MYTHICSHOT_MEASURE_DIR and ffmpeg; it measures, it does not test"]
    fn measure_a_real_recording() {
        let Some(dir) = std::env::var_os("MYTHICSHOT_MEASURE_DIR") else {
            println!("skipped: MYTHICSHOT_MEASURE_DIR is not set");
            return;
        };
        let Some(ffmpeg) = real() else { return };
        let rec = Recording::open_saved(Path::new(&dir)).expect("the folder holds a saved recording");
        let n = rec.frames.len();
        let lossless = std::fs::metadata(rec.dir.join(FRAMES_FILE)).unwrap().len();
        let out_dir = tempfile::tempdir().unwrap();
        let out = out_dir.path().join("frames.mkv");
        let every: Vec<usize> = (0..n).collect();
        let started = std::time::Instant::now();
        let (archived, shift) = archive(&rec, ffmpeg, &out, &every, &MEASURE_ONLY, &|_, _, _| true).expect("the recording archives");
        println!("{} × {}, {n} frames, {} ms: frames {lossless} bytes, video {} bytes ({:.1} times smaller), {:.1} s for both passes", rec.width, rec.height, rec.end_ms, archived.bytes, lossless as f64 / archived.bytes as f64, started.elapsed().as_secs_f64());
        println!("worst frame {:.2} dB (at least {} asked), worst block of {} {:.2} dB (at least {} asked), worst mean shift {:.3} levels (at most {} asked), {} frames compared", archived.worst_frame_db, TOLERANCE.frame_db, TOLERANCE.block, archived.worst_block_db, TOLERANCE.block_db, shift, TOLERANCE.mean_shift, archived.checked);
        let passes = archived.worst_frame_db >= TOLERANCE.frame_db && archived.worst_block_db >= TOLERANCE.block_db && shift <= TOLERANCE.mean_shift && archived.bytes <= allowed_bytes(lossless, TOLERANCE.smaller_by);
        println!("with the tolerance that ships this recording {}", if passes { "is archived" } else { "is NOT archived" });
    }
}
