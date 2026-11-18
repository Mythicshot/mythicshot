//! Screen recording.
//!
//! [`Recorder::start`] runs a capture thread that writes each frame to a temp
//! file as it arrives, so memory stays flat however long the recording runs.
//! Frames are stored as the XOR difference from the previous frame, then
//! zstd-compressed: unchanged pixels become zeros and nearly vanish. Every
//! `KEYFRAME_EVERY`th frame is stored whole so any frame can be decoded
//! without reading from the start. Each frame keeps the active recording time (pauses
//! excluded) and where the mouse pointer was, because the Windows capture path
//! leaves the pointer out; the overlay renderer draws it back in at export.

mod cursor;
pub mod ffmpeg;
mod saved;

pub use cursor::position as pointer_position;
pub mod export;
pub use ffmpeg::{CodecError, Ffmpeg, VideoCodec};
pub use saved::{OpenError, SaveError, SaveStep};

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use image::RgbaImage;
const KEYFRAME_EVERY: usize = 30;
const ZSTD_LEVEL: i32 = 1;
const FRAMES_FILE: &str = "frames.zst";

use pinray::{CaptureEvent, CaptureSession, CursorMode, FrameData, PixelFormat, Rect, SourceId, VideoCaptureTarget};

/// What to record: a rectangle on one display. `x`, `y` are relative to the
/// display's top-left corner; `origin` is that corner in desktop pixels.
#[derive(Debug, Clone)]
pub struct Target {
    /// OS display name, for example `\\.\DISPLAY1` on Windows.
    pub display: String,
    pub origin: (i32, i32),
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct FrameMeta {
    offset: u64,
    len: u32,
    /// Stored whole rather than as a difference from the previous frame.
    key: bool,
    /// Active recording time when the frame arrived.
    pub t_ms: u64,
    /// Pointer position relative to the recorded area, if it was inside.
    pub cursor: Option<(i32, i32)>,
}

/// A finished recording: compressed frames in a folder plus their timing. A recording that
/// was just made owns a temp folder; one opened from a saved folder does not own it.
pub struct Recording {
    dir: PathBuf,
    /// Whether the folder is this recording's to delete.
    owned: bool,
    pub width: u32,
    pub height: u32,
    pub frames: Vec<FrameMeta>,
    /// Active time when recording stopped; the last frame lasts until then.
    pub end_ms: u64,
}

impl Recording {
    fn read_raw(&self, f: &mut File, m: &FrameMeta) -> Result<Vec<u8>, String> {
        f.seek(SeekFrom::Start(m.offset)).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; m.len as usize];
        f.read_exact(&mut buf).map_err(|e| e.to_string())?;
        zstd::bulk::decompress(&buf, (self.width * self.height * 4) as usize).map_err(|e| e.to_string())
    }

    fn to_image(&self, px: Vec<u8>) -> Result<RgbaImage, String> {
        RgbaImage::from_raw(self.width, self.height, px).ok_or_else(|| "frame has the wrong size".to_string())
    }

    /// One frame, decoded from the nearest keyframe before it.
    pub fn frame(&self, i: usize) -> Result<RgbaImage, String> {
        if i >= self.frames.len() {
            return Err("no such frame".into());
        }
        let key = (0..=i).rev().find(|k| self.frames[*k].key).ok_or("no keyframe")?;
        let mut f = File::open(self.dir.join(FRAMES_FILE)).map_err(|e| e.to_string())?;
        let mut px = self.read_raw(&mut f, &self.frames[key])?;
        for m in &self.frames[key + 1..=i] {
            xor_into(&mut px, &self.read_raw(&mut f, m)?);
        }
        self.to_image(px)
    }

    /// Every frame in order, decoding each once.
    pub fn for_each_frame(&self, mut f: impl FnMut(usize, RgbaImage) -> Result<(), String>) -> Result<(), String> {
        let mut file = File::open(self.dir.join(FRAMES_FILE)).map_err(|e| e.to_string())?;
        let mut px: Vec<u8> = Vec::new();
        for (i, m) in self.frames.iter().enumerate() {
            let raw = self.read_raw(&mut file, m)?;
            if m.key {
                px = raw;
            } else {
                xor_into(&mut px, &raw);
            }
            f(i, self.to_image(px.clone())?)?;
        }
        Ok(())
    }

    /// A recording with frame times but no pixels, for tests of timing code.
    #[doc(hidden)]
    pub fn timing_only(width: u32, height: u32, times: &[u64], end_ms: u64) -> Recording {
        let frames = times.iter().map(|t| FrameMeta { offset: 0, len: 0, key: true, t_ms: *t, cursor: None }).collect();
        Recording { dir: PathBuf::new(), owned: false, width, height, frames, end_ms }
    }

    /// A small real recording that owns `dir` (which must exist), as the recorder leaves one:
    /// one frame per entry of `times`, each a little different from the one before. For tests
    /// of code that reads frames or deletes a recording.
    #[doc(hidden)]
    pub fn synthetic_in(dir: &std::path::Path, width: u32, height: u32, times: &[u64], end_ms: u64) -> Result<Recording, String> {
        let mut writer = FrameWriter::create(dir, width, height)?;
        for (i, t_ms) in times.iter().enumerate() {
            let mut px = vec![40u8; (width * height * 4) as usize];
            for (n, p) in px.chunks_exact_mut(4).enumerate() {
                if (n + i * 7) % 13 == 0 {
                    p[0] = (i * 31 % 256) as u8;
                }
                p[3] = 255;
            }
            writer.push(&px, *t_ms, None)?;
        }
        writer.finish(dir.to_path_buf(), end_ms)
    }

    pub fn duration_ms(&self) -> u64 {
        self.end_ms
    }

    /// Whether `other` holds the same recording: the same picture size and end time, and for
    /// every frame the same time, pointer, place and length in the frames file, and kind.
    pub fn same_frames(&self, other: &Recording) -> bool {
        let whole = |m: &FrameMeta| (m.offset, m.len, m.key, m.t_ms, m.cursor);
        (self.width, self.height, self.end_ms, self.frames.len()) == (other.width, other.height, other.end_ms, other.frames.len()) && self.frames.iter().zip(&other.frames).all(|(a, b)| whole(a) == whole(b))
    }

    pub fn delete(self) {
        self.delete_files();
    }

    /// Remove the temp files; for a recording shared through an `Arc`. A recording opened
    /// from a saved folder does not own that folder, and nothing is removed.
    pub fn delete_files(&self) {
        if !self.owned {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Reads frames from a shared recording, keeping the last one decoded so
/// stepping forward costs one small delta instead of a walk from the keyframe.
pub struct FrameReader {
    rec: Arc<Recording>,
    file: File,
    px: Vec<u8>,
    idx: Option<usize>,
}

impl FrameReader {
    pub fn new(rec: Arc<Recording>) -> Result<Self, String> {
        let file = File::open(rec.dir.join(FRAMES_FILE)).map_err(|e| e.to_string())?;
        Ok(Self { rec, file, px: Vec::new(), idx: None })
    }

    pub fn recording(&self) -> &Recording {
        &self.rec
    }

    pub fn get(&mut self, i: usize) -> Result<RgbaImage, String> {
        if i >= self.rec.frames.len() {
            return Err("no such frame".into());
        }
        let start = match self.idx {
            Some(cur) if cur == i => return self.rec.to_image(self.px.clone()),
            // Moving forward within the same keyframe group: apply the deltas in between.
            Some(cur) if cur < i && !(cur + 1..=i).any(|k| self.rec.frames[k].key) => cur + 1,
            _ => {
                let key = (0..=i).rev().find(|k| self.rec.frames[*k].key).ok_or("no keyframe")?;
                self.px = self.rec.read_raw(&mut self.file, &self.rec.frames[key])?;
                key + 1
            }
        };
        for k in start..=i {
            let delta = self.rec.read_raw(&mut self.file, &self.rec.frames[k])?;
            xor_into(&mut self.px, &delta);
        }
        self.idx = Some(i);
        self.rec.to_image(self.px.clone())
    }
}

#[derive(Clone)]
pub struct ClockHandle(Arc<Shared>);

impl ClockHandle {
    pub fn active_ms(&self) -> u64 {
        self.0.active().as_millis() as u64
    }

    pub fn paused(&self) -> bool {
        self.0.paused()
    }
}

/// Live numbers for the control bar.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub frames: u64,
    pub active_ms: u64,
    pub compressed_bytes: u64,
}

/// Active recording time: wall time since the capture started, minus pauses.
/// Shared so key and click events from other threads get the same clock as
/// the frames.
struct Clock {
    started: Instant,
    paused_total: Duration,
    paused_since: Option<Instant>,
}

impl Clock {
    fn active(&self) -> Duration {
        let now = Instant::now();
        let paused_now = self.paused_since.map(|p| now - p).unwrap_or_default();
        (now - self.started).saturating_sub(self.paused_total + paused_now)
    }

    fn set_paused(&mut self, paused: bool) {
        match (paused, self.paused_since) {
            (true, None) => self.paused_since = Some(Instant::now()),
            (false, Some(p)) => {
                self.paused_total += p.elapsed();
                self.paused_since = None;
            }
            _ => {}
        }
    }
}

struct Shared {
    stop: AtomicBool,
    clock: Mutex<Clock>,
    frames: AtomicU64,
    bytes: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn active(&self) -> Duration {
        self.clock.lock().map(|c| c.active()).unwrap_or_default()
    }

    fn paused(&self) -> bool {
        self.clock.lock().map(|c| c.paused_since.is_some()).unwrap_or(false)
    }
}

pub struct Recorder {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<Result<Recording, String>>>,
}

impl Recorder {
    /// Start recording `target` at up to `fps` frames per second.
    pub fn start(target: Target, fps: u32) -> Result<Recorder, String> {
        let dir = std::env::temp_dir().join("Mythicshot").join(format!("rec-{}-{}", std::process::id(), now_nanos()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            clock: Mutex::new(Clock { started: Instant::now(), paused_total: Duration::ZERO, paused_since: None }),
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        // The session is created on the capture thread; report whether it started.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("mythicshot-recorder".into())
            .spawn(move || capture_loop(target, fps, dir, sh, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Recorder { shared, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the capture did not start within 5 seconds".into()),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        if let Ok(mut c) = self.shared.clock.lock() {
            c.set_paused(paused);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused()
    }

    /// The recording clock right now, in ms; frames and input events share it.
    pub fn active_ms(&self) -> u64 {
        self.shared.active().as_millis() as u64
    }

    /// A handle to the recording clock that other threads can keep, so input
    /// events are stamped on the same clock as frames.
    pub fn clock(&self) -> ClockHandle {
        ClockHandle(self.shared.clone())
    }

    pub fn stats(&self) -> Stats {
        Stats {
            frames: self.shared.frames.load(Ordering::Relaxed),
            active_ms: self.active_ms(),
            compressed_bytes: self.shared.bytes.load(Ordering::Relaxed),
        }
    }

    /// An error from the capture thread, if it stopped on its own.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|e| e.clone())
    }

    pub fn stop(mut self) -> Result<Recording, String> {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.thread.take().ok_or("already stopped")?.join().map_err(|_| "the recorder thread panicked".to_string())?
    }

    /// Stop and throw the frames away.
    pub fn discard(self) {
        if let Ok(rec) = self.stop() {
            rec.delete();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.shared.stop.store(true, Ordering::Relaxed);
            if let Ok(Ok(rec)) = t.join() {
                rec.delete();
            }
        }
    }
}

fn xor_into(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d ^= s;
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn capture_loop(target: Target, fps: u32, dir: PathBuf, sh: Arc<Shared>, ready: std::sync::mpsc::Sender<Result<(), String>>) -> Result<Recording, String> {
    let session = CaptureSession::builder()
        .video_target(VideoCaptureTarget::Display(SourceId::new(format!("display:{}", target.display))))
        .pixel_format(PixelFormat::Rgba8888)
        .cursor_mode(CursorMode::Hidden)
        .crop_rect(Some(Rect { x: target.x as i32, y: target.y as i32, width: target.width, height: target.height }))
        .frame_rate(Some(fps.clamp(1, 60)))
        .build();
    let mut session = match session.and_then(|mut s| s.start().map(|_| s)) {
        Ok(s) => {
            if let Ok(mut c) = sh.clock.lock() {
                c.started = Instant::now();
            }
            let _ = ready.send(Ok(()));
            s
        }
        Err(e) => {
            let msg = format!("could not start recording: {e}");
            let _ = ready.send(Err(msg.clone()));
            return Err(msg);
        }
    };

    let (w, h) = (target.width, target.height);
    let row = (w * 4) as usize;
    let mut writer = FrameWriter::create(&dir, w, h)?;
    let mut packed = vec![0u8; row * h as usize];

    let mut failure = None;
    while !sh.stop.load(Ordering::Relaxed) {
        let paused = sh.paused();
        match session.next_event(Some(Duration::from_millis(100))) {
            Ok(CaptureEvent::Video(frame)) => {
                if paused || frame.width != w || frame.height != h {
                    continue;
                }
                let FrameData::Host(data) = &frame.data else { continue };
                for y in 0..h as usize {
                    let src = y * frame.stride as usize;
                    packed[y * row..(y + 1) * row].copy_from_slice(&data[src..src + row]);
                }
                let cursor = cursor::position()
                    .map(|(x, y)| (x - target.origin.0 - target.x as i32, y - target.origin.1 - target.y as i32))
                    .filter(|(x, y)| *x >= -32 && *y >= -32 && *x < w as i32 && *y < h as i32);
                if let Err(e) = writer.push(&packed, sh.active().as_millis() as u64, cursor) {
                    failure = Some(e);
                    break;
                }
                sh.frames.store(writer.frames.len() as u64, Ordering::Relaxed);
                sh.bytes.store(writer.offset, Ordering::Relaxed);
            }
            Ok(_) => {}
            // Timeouts are normal: the screen only sends frames when something changes.
            Err(pinray::PinrayError::Timeout(_)) => {}
            Err(e) => {
                failure = Some(format!("recording stopped: {e}"));
                break;
            }
        }
    }
    let end_ms = sh.active().as_millis() as u64;
    let _ = session.stop();
    let rec = writer.finish(dir.clone(), end_ms)?;
    if let Some(f) = failure {
        if let Ok(mut e) = sh.error.lock() {
            *e = Some(f.clone());
        }
        if rec.frames.is_empty() {
            rec.delete();
            return Err(f);
        }
    }
    Ok(rec)
}

/// Appends frames to the frames file: keyframes whole, the rest as an XOR
/// difference from the previous frame, all zstd-compressed.
struct FrameWriter {
    out: BufWriter<File>,
    width: u32,
    height: u32,
    previous: Vec<u8>,
    delta: Vec<u8>,
    frames: Vec<FrameMeta>,
    offset: u64,
}

impl FrameWriter {
    fn create(dir: &std::path::Path, width: u32, height: u32) -> Result<Self, String> {
        let file = File::create(dir.join(FRAMES_FILE)).map_err(|e| e.to_string())?;
        let size = (width * height * 4) as usize;
        Ok(Self { out: BufWriter::with_capacity(4 << 20, file), width, height, previous: vec![0; size], delta: vec![0; size], frames: Vec::new(), offset: 0 })
    }

    /// Add one RGBA frame of exactly `width * height * 4` bytes.
    fn push(&mut self, px: &[u8], t_ms: u64, cursor: Option<(i32, i32)>) -> Result<(), String> {
        let key = self.frames.len() % KEYFRAME_EVERY == 0;
        let data: &[u8] = if key {
            px
        } else {
            for ((d, a), b) in self.delta.iter_mut().zip(px).zip(&self.previous) {
                *d = a ^ b;
            }
            &self.delta
        };
        let compressed = zstd::bulk::compress(data, ZSTD_LEVEL).map_err(|e| format!("could not compress a frame: {e}"))?;
        self.out.write_all(&compressed).map_err(|e| format!("could not write the recording: {e}"))?;
        self.previous.copy_from_slice(px);
        self.frames.push(FrameMeta { offset: self.offset, len: compressed.len() as u32, key, t_ms, cursor });
        self.offset += compressed.len() as u64;
        Ok(())
    }

    fn finish(mut self, dir: PathBuf, end_ms: u64) -> Result<Recording, String> {
        self.out.flush().map_err(|e| e.to_string())?;
        Ok(Recording { dir, owned: true, width: self.width, height: self.height, frames: self.frames, end_ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame that differs from its neighbours in a few places, like a screen.
    pub(crate) fn synthetic(i: usize, w: u32, h: u32) -> Vec<u8> {
        let mut px = vec![30u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                if (x as usize + i) % 17 == 0 || (y as usize * 3 + i) % 29 == 0 {
                    px[o] = (i * 7 % 256) as u8;
                    px[o + 1] = (x % 256) as u8;
                }
                px[o + 3] = 255;
            }
        }
        px
    }

    #[test]
    fn two_recordings_are_the_same_only_when_every_frame_lies_and_reads_the_same() {
        let frame = |offset, len, key, t_ms, cursor| FrameMeta { offset, len, key, t_ms, cursor };
        let rec = |width, end_ms, frames: Vec<FrameMeta>| Recording { dir: PathBuf::new(), owned: false, width, height: 80, frames, end_ms };
        let base = || vec![frame(0, 10, true, 0, None), frame(10, 12, false, 100, None)];
        assert!(rec(100, 200, base()).same_frames(&rec(100, 200, base())));
        // Equal times and no pointer, and still other frames: a length, where one lies, a whole frame.
        assert!(!rec(100, 200, base()).same_frames(&rec(100, 200, vec![frame(0, 10, true, 0, None), frame(10, 13, false, 100, None)])));
        assert!(!rec(100, 200, base()).same_frames(&rec(100, 200, vec![frame(0, 10, true, 0, None), frame(11, 12, false, 100, None)])));
        assert!(!rec(100, 200, base()).same_frames(&rec(100, 200, vec![frame(0, 10, true, 0, None), frame(10, 12, true, 100, None)])));
        assert!(!rec(100, 200, base()).same_frames(&rec(100, 200, vec![frame(0, 10, true, 0, None), frame(10, 12, false, 101, None)])));
        assert!(!rec(100, 200, base()).same_frames(&rec(100, 200, vec![frame(0, 10, true, 0, Some((1, 2))), frame(10, 12, false, 100, None)])));
        assert!(!rec(100, 200, base()).same_frames(&rec(100, 200, vec![frame(0, 10, true, 0, None)])));
        assert!(!rec(100, 200, base()).same_frames(&rec(101, 200, base())));
        assert!(!rec(100, 200, base()).same_frames(&rec(100, 201, base())));
    }

    #[test]
    fn every_frame_decodes_back_exactly() {
        let dir = std::env::temp_dir().join(format!("mythicshot-rec-test-{}", now_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (48, 32);
        let mut writer = FrameWriter::create(&dir, w, h).unwrap();
        let n = KEYFRAME_EVERY * 2 + 7;
        for i in 0..n {
            writer.push(&synthetic(i, w, h), i as u64 * 66, Some((i as i32, 0))).unwrap();
        }
        let rec = writer.finish(dir, n as u64 * 66).unwrap();
        assert_eq!(rec.frames.iter().filter(|f| f.key).count(), 3);
        // Random access from the nearest keyframe.
        for i in [0, 1, KEYFRAME_EVERY - 1, KEYFRAME_EVERY, KEYFRAME_EVERY + 5, n - 1] {
            assert_eq!(rec.frame(i).unwrap().into_raw(), synthetic(i, w, h), "frame {i}");
        }
        // Sequential decoding matches too.
        let mut seen = 0;
        rec.for_each_frame(|i, img| {
            assert_eq!(img.into_raw(), synthetic(i, w, h), "frame {i}");
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, n);
        rec.delete();
    }
}
