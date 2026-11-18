//! The one place that finds and runs ffmpeg.
//!
//! ffmpeg is an external program the user may have installed. It is looked for once, on the
//! user's PATH only (never the current folder, the application's folder or a relative entry),
//! kept by its absolute path, and probed for libx264. It is then run with its arguments as a
//! list (never through a shell), with no console window, below normal priority, and watched:
//! a stall bound, a total bound, a bound on the size of what it writes, and a cancel that is
//! asked every 50 ms. Its own error text is drained so it can never block, capped, and never
//! shown or returned.
//!
//! [`VideoCodec`] is what the rest of the crate builds on, so tests can use a stand-in;
//! [`Ffmpeg`] is the real one: near-lossless H.264 4:4:4, full range, in Matroska, one
//! picture per frame at a nominal constant rate.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The quality of a kept recording. Constants, and part of [`CODEC_ID`]: a change of any of
/// them must change the id, so recordings marked under the old one are tried again.
pub const CRF: u32 = 10;
pub const PRESET: &str = "medium";
/// A key picture every this many frames, so a damaged spot costs at most one group.
pub const GOP: u32 = 30;
/// The rate written into the video. Frame times live in the index, never in the video: picture
/// `i` is frame `i`, proven by counting.
pub const NOMINAL_FPS: u32 = 25;
/// ffmpeg is stopped when nothing moved for this long.
pub const STALL: Duration = Duration::from_secs(120);
/// ffmpeg is stopped after 30 seconds plus this much per frame, whatever it is doing.
pub const PER_FRAME: Duration = Duration::from_secs(5);
/// What [`Ffmpeg`] answers as its [`VideoCodec::id`].
pub const CODEC_ID: &str = "x264-444-crf10-medium-g30";

const BASE_TIME: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// How much of ffmpeg's error text is kept (for a debug build that asks for it); the rest is
/// read and thrown away.
const ERROR_TEXT_KEPT: usize = 8 << 10;
const PROBE_TIME: Duration = Duration::from_secs(10);
const PROBE_KEPT: usize = 1 << 20;

#[cfg(windows)]
const EXE_NAME: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "ffmpeg";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
/// The process creation flags of every ffmpeg this module starts: no console window, and a
/// priority below the recorder's and the user's own work.
const SPAWN_FLAGS: u32 = CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS;

/// An ffmpeg that was found on the PATH and answered the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ffmpeg {
    exe: PathBuf,
    version: String,
}

impl Ffmpeg {
    /// The absolute path of the program.
    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// What it said its version is, for example `7.1-full_build`.
    pub fn version(&self) -> &str {
        &self.version
    }
}

static FOUND: OnceLock<Option<Ffmpeg>> = OnceLock::new();

/// The ffmpeg found by [`find_now`]. None until that ran, and None for good when it found
/// nothing: ffmpeg is looked for once per run. Never starts a program, so it is safe on the
/// UI thread.
pub fn find() -> Option<&'static Ffmpeg> {
    FOUND.get().and_then(Option::as_ref)
}

/// Look for ffmpeg on the PATH and probe it (two short runs of the program), once; later
/// calls answer what the first one found. For a background thread.
pub fn find_now() -> Option<&'static Ffmpeg> {
    FOUND
        .get_or_init(|| {
            let path = std::env::var_os("PATH")?;
            let found = locate(&path, &|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))?;
            // The canonical path, when it is still a plain absolute path to a file.
            let exe = std::fs::canonicalize(&found).ok().filter(|p| p.is_absolute() && p.is_file()).unwrap_or(found);
            let version = probe(&exe)?;
            Some(Ffmpeg { exe, version })
        })
        .as_ref()
}

/// The search itself, with the file system and the program behind two functions: the first
/// absolute entry of `path_var` that holds a file named `ffmpeg.exe` is taken, and it is kept
/// only if `probe` answers its version. Empty and relative entries are never looked into, and
/// no other file name is ever asked about.
pub fn find_in(path_var: &OsStr, is_file: &dyn Fn(&Path) -> bool, probe: &dyn Fn(&Path) -> Option<String>) -> Option<Ffmpeg> {
    let exe = locate(path_var, is_file)?;
    let version = probe(&exe)?;
    Some(Ffmpeg { exe, version })
}

fn locate(path_var: &OsStr, is_file: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    std::env::split_paths(path_var).filter(|dir| !dir.as_os_str().is_empty() && dir.is_absolute()).map(|dir| dir.join(EXE_NAME)).find(|exe| is_file(exe))
}

/// Ask the program for its version and its encoders; its version if it is an ffmpeg with
/// libx264.
fn probe(exe: &Path) -> Option<String> {
    let version = version_of(&String::from_utf8_lossy(&capture(exe, &["-hide_banner", "-version"])?))?;
    let encoders = capture(exe, &["-hide_banner", "-encoders"])?;
    lists_libx264(&String::from_utf8_lossy(&encoders)).then_some(version)
}

/// The version out of the first line of `ffmpeg -version`.
fn version_of(text: &str) -> Option<String> {
    let version = text.lines().next()?.strip_prefix("ffmpeg version ")?.split_whitespace().next()?;
    Some(version.to_string())
}

/// Whether `ffmpeg -encoders` lists the video encoder named exactly `libx264`.
fn lists_libx264(text: &str) -> bool {
    text.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next().is_some_and(|flags| flags.starts_with('V')) && words.next() == Some("libx264")
    })
}

/// What a short run of the program printed, if it ended well within [`PROBE_TIME`].
fn capture(exe: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = command(exe).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || drain(stdout, PROBE_KEPT));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < PROBE_TIME => std::thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text = reader.join().ok()?;
    status.filter(ExitStatus::success).map(|_| text)
}

/// A command for `exe` that opens no console window and runs below normal priority.
fn command(exe: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(SPAWN_FLAGS);
    }
    cmd
}

/// Read to the end, keeping only the first `keep` bytes, so the writer never blocks.
fn drain(mut from: impl Read, keep: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match from.read(&mut chunk) {
            Ok(0) | Err(_) => return kept,
            Ok(n) => {
                let room = keep.saturating_sub(kept.len()).min(n);
                kept.extend_from_slice(&chunk[..room]);
            }
        }
    }
}

/// Why an encode or a decode did not finish. It never carries the program's own words.
#[derive(Debug)]
pub enum CodecError {
    /// `keep_going` answered false.
    Cancelled,
    /// The program ended with an error, or closed its pipe early.
    Failed,
    /// The stall bound or the total bound passed.
    TimedOut,
    /// The output grew past the size it was allowed.
    TooLarge,
    /// The number of frames is not the number asked for: the caller had fewer to give, or the
    /// video held fewer or more (`got` is then one more than asked for).
    WrongFrames { got: usize },
    /// The program could not be started, or its pipe could not be read.
    Io(std::io::Error),
    /// The caller's own function answered an error; its text is the caller's.
    Caller(String),
}

/// Turns frames into a video file and back. One picture per frame, in order; the times are
/// the caller's business.
pub trait VideoCodec: Sync {
    /// What was used, recorded with every video: the codec and its settings.
    fn id(&self) -> &str;

    /// Write `frames` RGBA pictures of `size` to a new file `out`. `next` fills the buffer it
    /// is handed with the next picture (exactly width × height × 4 bytes) and answers
    /// `Ok(true)`, or `Ok(false)` when there are no more. `keep_going` is told how many
    /// frames are done, at least every frame, and stops the work by answering false. On any
    /// error `out` is removed.
    fn encode(&self, size: (u32, u32), frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError>;

    /// Read `video` back: `each` gets every picture in order as RGBA of exactly
    /// width × height × 4 bytes. A video with fewer or more than `frames` pictures is an
    /// error, as is one whose pictures are not of `size`.
    #[allow(clippy::type_complexity)]
    fn decode(&self, video: &Path, size: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError>;
}

impl VideoCodec for Ffmpeg {
    fn id(&self) -> &str {
        CODEC_ID
    }

    fn encode(&self, size: (u32, u32), frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
        let frame_len = frame_len(size)?;
        if frames == 0 {
            return Err(CodecError::Caller("there are no frames to encode".into()));
        }
        let mut cmd = command(&self.exe);
        cmd.args(encode_args(size, out));
        run_encode(cmd, frame_len, frames, next, out, max_bytes, keep_going, &Limits::for_frames(frames))?;
        // Success with nothing written is not a video.
        if std::fs::metadata(out).is_ok_and(|m| m.is_file() && m.len() > 0) {
            Ok(())
        } else {
            let _ = std::fs::remove_file(out);
            Err(CodecError::Failed)
        }
    }

    fn decode(&self, video: &Path, size: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
        let frame_len = frame_len(size)?;
        let mut cmd = command(&self.exe);
        cmd.args(decode_args(video));
        run_decode(cmd, frame_len, frames, each, keep_going, &Limits::for_frames(frames))
    }
}

fn frame_len(size: (u32, u32)) -> Result<usize, CodecError> {
    let bytes = (size.0 as u64).checked_mul(size.1 as u64).and_then(|px| px.checked_mul(4)).filter(|b| *b > 0);
    bytes.and_then(|b| usize::try_from(b).ok()).ok_or_else(|| CodecError::Caller("the picture size is not usable".into()))
}

/// A file name as one argument that ffmpeg cannot take for an option: a relative path is
/// anchored at `.`, so it never starts with a dash.
fn file_arg(path: &Path) -> OsString {
    if path.is_absolute() { path.as_os_str().to_os_string() } else { Path::new(".").join(path).into_os_string() }
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// The arguments that encode raw RGBA from standard input into `out`: H.264 4:4:4 (never
/// 4:2:0), full range, BT.709, a key picture every [`GOP`] frames.
fn encode_args(size: (u32, u32), out: &Path) -> Vec<OsString> {
    let mut args = os(&["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-video_size"]);
    args.push(format!("{}x{}", size.0, size.1).into());
    args.extend(os(&["-framerate", &NOMINAL_FPS.to_string(), "-i", "pipe:0", "-an"]));
    args.extend(os(&["-vf", "scale=out_range=full:out_color_matrix=bt709,format=yuv444p"]));
    args.extend(os(&["-c:v", "libx264", "-crf", &CRF.to_string(), "-preset", PRESET]));
    args.extend(os(&["-g", &GOP.to_string(), "-keyint_min", &GOP.to_string(), "-sc_threshold", "0"]));
    args.extend(os(&["-color_range", "pc", "-colorspace", "bt709", "-f", "matroska"]));
    args.push(file_arg(out));
    args
}

/// The arguments that decode `video` to raw RGBA on standard output. The container is forced
/// and only files and pipes may be opened, so a file put in the video's place cannot make
/// ffmpeg read a playlist or the network.
fn decode_args(video: &Path) -> Vec<OsString> {
    let mut args = os(&["-hide_banner", "-nostdin", "-loglevel", "error", "-protocol_whitelist", "file,pipe", "-f", "matroska", "-i"]);
    args.push(file_arg(video));
    args.extend(os(&["-map", "0:v:0", "-an", "-sn", "-dn"]));
    args.extend(os(&["-vf", "scale=in_range=full:in_color_matrix=bt709,format=rgba"]));
    args.extend(os(&["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"]));
    args
}

/// How long a run may take.
struct Limits {
    /// Nothing moved for this long: no frame through the pipe and, when encoding, no growth
    /// of the output.
    stall: Duration,
    total: Duration,
    poll: Duration,
}

impl Limits {
    fn for_frames(frames: usize) -> Limits {
        let per_frame = PER_FRAME.checked_mul(u32::try_from(frames).unwrap_or(u32::MAX)).unwrap_or(Duration::MAX);
        Limits { stall: STALL, total: BASE_TIME.saturating_add(per_frame), poll: POLL }
    }
}

/// The clock of one run.
struct Watch<'a> {
    limits: &'a Limits,
    started: Instant,
    moved: Instant,
}

impl<'a> Watch<'a> {
    fn start(limits: &'a Limits) -> Self {
        let now = Instant::now();
        Watch { limits, started: now, moved: now }
    }

    fn moved(&mut self) {
        self.moved = Instant::now();
    }

    fn check(&self) -> Result<(), CodecError> {
        if self.started.elapsed() > self.limits.total || self.moved.elapsed() > self.limits.stall { Err(CodecError::TimedOut) } else { Ok(()) }
    }
}

/// Stop the program if the run failed, wait for it, and say how it ended.
fn finish(child: &mut Child, failed: bool) -> Option<ExitStatus> {
    if failed {
        let _ = child.kill();
    }
    child.wait().ok()
}

/// In a debug build with `MYTHICSHOT_FFMPEG_LOG` set, the exit code and the capped error text
/// go to the console. Otherwise the text is dropped: it can hold a folder's name.
fn note_error_text(status: Option<ExitStatus>, text: &[u8]) {
    if cfg!(debug_assertions) && std::env::var_os("MYTHICSHOT_FFMPEG_LOG").is_some() {
        eprintln!("ffmpeg ended with {:?}: {}", status.and_then(|s| s.code()), String::from_utf8_lossy(text).trim());
    }
}

/// Run `cmd` as an encoder: feed it `frames` frames on standard input from a thread of its
/// own, watch it from this one, and remove `out` unless it ended well.
#[allow(clippy::too_many_arguments)]
fn run_encode(cmd: Command, frame_len: usize, frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool, limits: &Limits) -> Result<(), CodecError> {
    let result = feed(cmd, frame_len, frames, next, out, max_bytes, keep_going, limits);
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn feed(mut cmd: Command, frame_len: usize, frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool, limits: &Limits) -> Result<(), CodecError> {
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(CodecError::Io)?;
    let (Some(mut stdin), Some(stderr)) = (child.stdin.take(), child.stderr.take()) else {
        finish(&mut child, true);
        return Err(CodecError::Failed);
    };
    // One buffer goes back and forth: this thread fills it, the writer thread writes it.
    let (to_writer, writer_in) = mpsc::channel::<Vec<u8>>();
    let (writer_out, from_writer) = mpsc::channel::<Option<Vec<u8>>>();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for buf in writer_in {
                let written = stdin.write_all(&buf).is_ok();
                if writer_out.send(written.then_some(buf)).is_err() || !written {
                    break;
                }
            }
            // Dropping the pipe here tells the program its input has ended.
        });
        let errors = scope.spawn(move || drain(stderr, ERROR_TEXT_KEPT));

        let mut to_writer = Some(to_writer);
        let child = &mut child;
        let mut work = || -> Result<(), CodecError> {
            let mut watch = Watch::start(limits);
            let mut size = 0u64;
            let mut done = 0usize;
            // Asked every poll and every frame: cancel, the output's size, the two time bounds.
            let mut look = |done: usize, watch: &mut Watch| -> Result<(), CodecError> {
                if !keep_going(done) {
                    return Err(CodecError::Cancelled);
                }
                let now = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
                if now > max_bytes {
                    return Err(CodecError::TooLarge);
                }
                if now != size {
                    size = now;
                    watch.moved();
                }
                watch.check()
            };
            let mut buf = Vec::with_capacity(frame_len);
            while done < frames {
                buf.clear();
                match next(&mut buf) {
                    Err(text) => return Err(CodecError::Caller(text)),
                    Ok(false) => return Err(CodecError::WrongFrames { got: done }),
                    Ok(true) if buf.len() != frame_len => return Err(CodecError::Caller("a frame has the wrong size".into())),
                    Ok(true) => {}
                }
                let Some(writer) = &to_writer else { return Err(CodecError::Failed) };
                if writer.send(std::mem::take(&mut buf)).is_err() {
                    return Err(CodecError::Failed);
                }
                loop {
                    match from_writer.recv_timeout(limits.poll) {
                        Ok(Some(back)) => {
                            buf = back;
                            done += 1;
                            watch.moved();
                            break;
                        }
                        // The program closed its input before it had every frame.
                        Ok(None) | Err(RecvTimeoutError::Disconnected) => return Err(CodecError::Failed),
                        Err(RecvTimeoutError::Timeout) => look(done, &mut watch)?,
                    }
                }
                look(done, &mut watch)?;
            }
            // End of input; now the program finishes the file.
            to_writer = None;
            loop {
                match child.try_wait().map_err(CodecError::Io)? {
                    Some(status) if status.success() => break,
                    Some(_) => return Err(CodecError::Failed),
                    None => {
                        look(done, &mut watch)?;
                        std::thread::sleep(limits.poll);
                    }
                }
            }
            if std::fs::metadata(out).map(|m| m.len()).unwrap_or(0) > max_bytes { Err(CodecError::TooLarge) } else { Ok(()) }
        };
        let result = work();
        drop(to_writer);
        let status = finish(child, result.is_err());
        let text = errors.join().unwrap_or_default();
        if result.is_err() || !text.is_empty() {
            note_error_text(status, &text);
        }
        result
    })
}

/// What the thread reading the program's output tells the watching thread.
enum Piece {
    Frame(Vec<u8>),
    /// The output ended before every frame was whole.
    Short,
    /// The output ended exactly after the last frame.
    End,
    /// There is output after the last frame.
    Extra,
    Broken(std::io::Error),
}

/// Fill `buf` from `from`; fewer bytes than its length only at the end of the stream.
fn read_full(from: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match from.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// Run `cmd` as a decoder: read exactly `frames` frames of `frame_len` bytes from its standard
/// output on a thread of its own, hand each to `each` on this one, and refuse a stream that
/// is shorter or one byte longer.
#[allow(clippy::type_complexity)]
fn run_decode(mut cmd: Command, frame_len: usize, frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool, limits: &Limits) -> Result<(), CodecError> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(CodecError::Io)?;
    let (Some(mut stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        finish(&mut child, true);
        return Err(CodecError::Failed);
    };
    // One buffer goes back and forth: the reader thread fills it, this thread hands it on.
    let (reader_out, from_reader) = mpsc::channel::<Piece>();
    let (to_reader, reader_in) = mpsc::channel::<Vec<u8>>();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let mut buf = vec![0u8; frame_len];
            for _ in 0..frames {
                buf.resize(frame_len, 0);
                let piece = match read_full(&mut stdout, &mut buf) {
                    Ok(n) if n == frame_len => Piece::Frame(std::mem::take(&mut buf)),
                    Ok(_) => Piece::Short,
                    Err(e) => Piece::Broken(e),
                };
                let whole = matches!(piece, Piece::Frame(_));
                if reader_out.send(piece).is_err() || !whole {
                    return;
                }
                match reader_in.recv() {
                    Ok(back) => buf = back,
                    Err(_) => return,
                }
            }
            let mut one = [0u8; 1];
            let _ = reader_out.send(match read_full(&mut stdout, &mut one) {
                Ok(0) => Piece::End,
                Ok(_) => Piece::Extra,
                Err(e) => Piece::Broken(e),
            });
        });
        let errors = scope.spawn(move || drain(stderr, ERROR_TEXT_KEPT));

        let child = &mut child;
        let mut work = || -> Result<(), CodecError> {
            let mut watch = Watch::start(limits);
            let mut done = 0usize;
            let look = |done: usize, watch: &Watch| -> Result<(), CodecError> { if keep_going(done) { watch.check() } else { Err(CodecError::Cancelled) } };
            // A stream that ends early: an error of the program if it says so, else a count.
            let short = |child: &mut Child, done: usize, watch: &Watch| -> Result<(), CodecError> {
                loop {
                    match child.try_wait().map_err(CodecError::Io)? {
                        Some(status) if status.success() => return Err(CodecError::WrongFrames { got: done }),
                        Some(_) => return Err(CodecError::Failed),
                        None => {
                            look(done, watch)?;
                            std::thread::sleep(limits.poll);
                        }
                    }
                }
            };
            loop {
                match from_reader.recv_timeout(limits.poll) {
                    Ok(Piece::Frame(buf)) => {
                        each(done, &buf).map_err(CodecError::Caller)?;
                        done += 1;
                        // Time the caller took is not the program's stall.
                        watch.moved();
                        look(done, &watch)?;
                        if to_reader.send(buf).is_err() {
                            return Err(CodecError::Failed);
                        }
                    }
                    Ok(Piece::Short) => return short(child, done, &watch),
                    Ok(Piece::Extra) => return Err(CodecError::WrongFrames { got: frames + 1 }),
                    Ok(Piece::Broken(e)) => return Err(CodecError::Io(e)),
                    Ok(Piece::End) => break,
                    Err(RecvTimeoutError::Disconnected) => return Err(CodecError::Failed),
                    Err(RecvTimeoutError::Timeout) => look(done, &watch)?,
                }
            }
            loop {
                match child.try_wait().map_err(CodecError::Io)? {
                    Some(status) if status.success() => return Ok(()),
                    Some(_) => return Err(CodecError::Failed),
                    None => {
                        look(done, &watch)?;
                        std::thread::sleep(limits.poll);
                    }
                }
            }
        };
        let result = work();
        drop(to_reader);
        let status = finish(child, result.is_err());
        let text = errors.join().unwrap_or_default();
        if result.is_err() || !text.is_empty() {
            note_error_text(status, &text);
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Recording;
    use std::cell::{Cell, RefCell};

    const SKIPPED: &str = "skipped: ffmpeg with libx264 was not found on this computer";

    /// An absolute folder that does not have to exist.
    fn folder(name: &str) -> PathBuf {
        std::env::temp_dir().join("mythicshot-find-test").join(name)
    }

    fn path_of(entries: &[&Path]) -> OsString {
        std::env::join_paths(entries).unwrap()
    }

    fn version(_: &Path) -> Option<String> {
        Some("7.1-test".into())
    }

    // ---- the find ----

    #[test]
    fn ffmpeg_is_taken_from_the_first_path_entry_that_holds_it() {
        let (a, b, c) = (folder("a"), folder("b"), folder("c"));
        let path = path_of(&[&a, &b, &c]);
        let holds = |p: &Path| p == b.join(EXE_NAME) || p == c.join(EXE_NAME);
        let found = find_in(&path, &holds, &version).unwrap();
        assert_eq!(found.exe(), b.join(EXE_NAME));
        assert_eq!(found.version(), "7.1-test");
        // The probe is asked about that one file only.
        let probed = RefCell::new(Vec::new());
        let probe = |p: &Path| {
            probed.borrow_mut().push(p.to_path_buf());
            Some("1".to_string())
        };
        find_in(&path, &holds, &probe).unwrap();
        assert_eq!(*probed.borrow(), vec![b.join(EXE_NAME)]);
        // Nothing on the PATH, or no PATH at all: not found.
        assert!(find_in(&path, &|_| false, &version).is_none());
        assert!(find_in(OsStr::new(""), &|_| true, &version).is_none());
    }

    #[test]
    fn relative_and_empty_path_entries_are_never_searched() {
        let real = folder("real");
        let entries = [PathBuf::new(), PathBuf::from("."), PathBuf::from("tools"), PathBuf::from("..").join("bin"), real.clone()];
        let path = path_of(&entries.iter().map(PathBuf::as_path).collect::<Vec<_>>());
        let asked = RefCell::new(Vec::new());
        // Every folder "holds" an ffmpeg; only the absolute one may be looked into.
        let holds = |p: &Path| {
            asked.borrow_mut().push(p.to_path_buf());
            true
        };
        let found = find_in(&path, &holds, &version).unwrap();
        assert_eq!(found.exe(), real.join(EXE_NAME));
        assert_eq!(*asked.borrow(), vec![real.join(EXE_NAME)]);
        // With only relative and empty entries nothing is asked and nothing is found.
        asked.borrow_mut().clear();
        let path = path_of(&entries[..4].iter().map(PathBuf::as_path).collect::<Vec<_>>());
        assert!(find_in(&path, &holds, &version).is_none());
        assert!(asked.borrow().is_empty());
    }

    #[test]
    fn a_batch_file_named_ffmpeg_is_never_taken() {
        let (scripts, programs) = (folder("scripts"), folder("programs"));
        let path = path_of(&[&scripts, &programs]);
        let asked = RefCell::new(Vec::new());
        let batch_only = |p: &Path| {
            asked.borrow_mut().push(p.to_path_buf());
            p == scripts.join("ffmpeg.bat") || p == scripts.join("ffmpeg.cmd") || p == scripts.join("ffmpeg.com") || p == scripts.join("ffmpeg")
        };
        if cfg!(windows) {
            assert!(find_in(&path, &batch_only, &version).is_none());
        }
        // Only the one program name is ever asked about, in any folder.
        find_in(&path, &batch_only, &version);
        assert!(!asked.borrow().is_empty());
        assert!(asked.borrow().iter().all(|p| p.file_name() == Some(OsStr::new(EXE_NAME))), "{:?}", asked.borrow());
        // A batch file earlier on the PATH does not hide the program after it.
        let both = |p: &Path| p == scripts.join("ffmpeg.bat") || p == scripts.join("ffmpeg.cmd") || p == programs.join(EXE_NAME);
        assert_eq!(find_in(&path, &both, &version).unwrap().exe(), programs.join(EXE_NAME));
    }

    #[test]
    fn an_ffmpeg_that_does_not_answer_its_version_or_lacks_libx264_is_not_found() {
        let dir = folder("bin");
        let path = path_of(&[&dir]);
        assert!(find_in(&path, &|_| true, &|_| None).is_none());

        // What the real probe makes of the program's answers.
        assert_eq!(version_of("ffmpeg version 7.1-full_build-www.example Copyright (c) 2000-2024 the developers\nbuilt with gcc\n").as_deref(), Some("7.1-full_build-www.example"));
        assert_eq!(version_of("ffmpeg version n6.0\n").as_deref(), Some("n6.0"));
        assert!(version_of("").is_none());
        assert!(version_of("ffprobe version 7.1\n").is_none());
        assert!(version_of("'ffmpeg' is not recognized as an internal or external command\n").is_none());
        assert!(version_of("ffmpeg version \n").is_none());

        let with = "Encoders:\n V..... = Video\n ------\n V....D libvpx-vp9           libvpx VP9 (codec vp9)\n V....D libx264              libx264 H.264 / AVC (codec h264)\n V....D libx264rgb           libx264 H.264 RGB (codec h264)\n";
        assert!(lists_libx264(with));
        let without = "Encoders:\n ------\n V....D libvpx-vp9           libvpx VP9 (codec vp9)\n V....D libx264rgb           libx264 H.264 RGB (codec h264)\n V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)\n A....D libx264              not a video encoder\n";
        assert!(!lists_libx264(without));
        assert!(!lists_libx264(""));

        // A program that is not there, or is not a program, answers nothing.
        assert!(probe(&folder("nowhere").join(EXE_NAME)).is_none());
    }

    #[test]
    fn the_path_kept_is_absolute() {
        let dir = folder("abs");
        let path = path_of(&[Path::new("relative"), &dir]);
        let found = find_in(&path, &|_| true, &version).unwrap();
        assert!(found.exe().is_absolute());
        assert_eq!(found.exe().file_name(), Some(OsStr::new(EXE_NAME)));
        // And the real one, when this computer has it.
        match find_now() {
            Some(real) => {
                assert!(real.exe().is_absolute(), "{}", real.exe().display());
                assert!(real.exe().is_file());
                assert!(!real.version().is_empty());
                assert_eq!(find(), Some(real));
            }
            None => {
                println!("{SKIPPED}");
                assert!(find().is_none());
            }
        }
    }

    // ---- the argument lists ----

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn value_of<'a>(args: &'a [String], option: &str) -> &'a str {
        let at = args.iter().position(|a| a == option).unwrap_or_else(|| panic!("no {option} in {args:?}"));
        &args[at + 1]
    }

    #[test]
    fn encoding_never_asks_for_4_2_0_and_always_for_full_range_bt709() {
        let out = folder("out").join("frames.mkv");
        let args = strings(&encode_args((2560, 1440), &out));
        assert!(!args.iter().any(|a| a.contains("420")), "{args:?}");
        assert_eq!(value_of(&args, "-vf"), "scale=out_range=full:out_color_matrix=bt709,format=yuv444p");
        assert_eq!(value_of(&args, "-color_range"), "pc");
        assert_eq!(value_of(&args, "-colorspace"), "bt709");
        assert_eq!(value_of(&args, "-c:v"), "libx264");
        assert_eq!(value_of(&args, "-crf"), "10");
        assert_eq!(value_of(&args, "-preset"), "medium");
        assert_eq!(value_of(&args, "-g"), "30");
        assert_eq!(value_of(&args, "-keyint_min"), "30");
        assert_eq!(value_of(&args, "-sc_threshold"), "0");
        assert_eq!(value_of(&args, "-video_size"), "2560x1440");
        assert_eq!(value_of(&args, "-framerate"), "25");
        assert_eq!(value_of(&args, "-i"), "pipe:0");
        assert_eq!(value_of(&args, "-loglevel"), "error");
        assert!(args.contains(&"-nostdin".to_string()) && args.contains(&"-an".to_string()));
        // The whole list, in the planned order.
        let planned = "-hide_banner -nostdin -loglevel error -y -f rawvideo -pix_fmt rgba -video_size 2560x1440 -framerate 25 -i pipe:0 -an -vf scale=out_range=full:out_color_matrix=bt709,format=yuv444p -c:v libx264 -crf 10 -preset medium -g 30 -keyint_min 30 -sc_threshold 0 -color_range pc -colorspace bt709 -f matroska";
        let mut expected: Vec<String> = planned.split(' ').map(String::from).collect();
        expected.push(out.to_string_lossy().into_owned());
        assert_eq!(args, expected);
        // The id names exactly these settings.
        assert_eq!(CODEC_ID, format!("x264-444-crf{CRF}-{PRESET}-g{GOP}"));
        assert_eq!(Ffmpeg { exe: out, version: String::new() }.id(), "x264-444-crf10-medium-g30");
    }

    #[test]
    fn decoding_forces_the_container_and_the_protocols() {
        let video = folder("in").join("frames.mkv");
        let args = strings(&decode_args(&video));
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[input + 1], video.to_string_lossy());
        // Both are input options: they stand before the input they bind.
        let whitelist = args.iter().position(|a| a == "-protocol_whitelist").unwrap();
        assert_eq!(args[whitelist + 1], "file,pipe");
        assert!(whitelist < input);
        let format = args.iter().position(|a| a == "-f").unwrap();
        assert_eq!(args[format + 1], "matroska");
        assert!(format < input);
        assert_eq!(value_of(&args, "-vf"), "scale=in_range=full:in_color_matrix=bt709,format=rgba");
        assert_eq!(value_of(&args, "-map"), "0:v:0");
        assert!(args.contains(&"-nostdin".to_string()));
        let planned = "-map 0:v:0 -an -sn -dn -vf scale=in_range=full:in_color_matrix=bt709,format=rgba -f rawvideo -pix_fmt rgba pipe:1";
        assert_eq!(args[input + 2..].join(" "), planned);
        assert_eq!(args[..input + 1].join(" "), "-hide_banner -nostdin -loglevel error -protocol_whitelist file,pipe -f matroska -i");
    }

    #[test]
    fn a_path_with_spaces_or_a_leading_dash_is_one_argument() {
        let spaced = folder("My Recordings & more").join("frames one.mkv");
        let args = encode_args((4, 4), &spaced);
        assert_eq!(args.last().unwrap(), spaced.as_os_str());
        assert_eq!(args.len(), encode_args((4, 4), Path::new("x.mkv")).len());
        let args = decode_args(&spaced);
        assert_eq!(args.iter().filter(|a| a.as_os_str() == spaced.as_os_str()).count(), 1);
        assert_eq!(args.len(), decode_args(Path::new("x.mkv")).len());

        // A relative name that starts with a dash is anchored, so it cannot be read as an option.
        for dashed in ["-y.mkv", "-i evil.mkv", "--help"] {
            let last = encode_args((4, 4), Path::new(dashed)).last().unwrap().to_string_lossy().into_owned();
            assert!(!last.starts_with('-') && last.ends_with(dashed), "{last}");
            let args = strings(&decode_args(Path::new(dashed)));
            let given = value_of(&args, "-i");
            assert!(!given.starts_with('-') && given.ends_with(dashed), "{given}");
            assert_eq!(args.len(), decode_args(Path::new("x.mkv")).len());
        }
        // A folder that starts with a dash, inside an absolute path, stays as it is.
        let inside = folder("-odd").join("frames.mkv");
        assert_eq!(encode_args((4, 4), &inside).last().unwrap(), inside.as_os_str());
    }

    // ---- the runner, with this test program standing in for ffmpeg ----

    const CHILD_MODE: &str = "MYTHICSHOT_TEST_CHILD";
    const CHILD_FILE: &str = "MYTHICSHOT_TEST_CHILD_FILE";

    /// Not a test of its own: what the stand-in child does when the runner tests start this
    /// test program again with [`CHILD_MODE`] set. Without it, nothing.
    #[test]
    fn stand_in_child() {
        let Ok(mode) = std::env::var(CHILD_MODE) else { return };
        match mode.as_str() {
            // Never reads its input and never ends on its own.
            "deaf" => std::thread::sleep(Duration::from_secs(120)),
            // Never reads its input, and writes its file without end.
            "grow" => {
                let mut file = std::fs::File::create(std::env::var_os(CHILD_FILE).unwrap()).unwrap();
                for _ in 0..12_000 {
                    file.write_all(&[7u8; 32 << 10]).unwrap();
                    file.flush().unwrap();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            // Writes far more error text than a pipe holds, then reads its input to the end.
            "flood" => {
                let mut err = std::io::stderr();
                for _ in 0..(4 << 10) {
                    err.write_all(&[b'e'; 1 << 10]).unwrap();
                }
                let read = std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).unwrap();
                std::fs::write(std::env::var_os(CHILD_FILE).unwrap(), read.to_string()).unwrap();
            }
            other => panic!("unknown stand-in mode {other}"),
        }
        std::process::exit(0);
    }

    fn stand_in(mode: &str, file: &Path) -> Command {
        let mut cmd = command(&std::env::current_exe().unwrap());
        cmd.args(["ffmpeg::tests::stand_in_child", "--exact", "--nocapture", "--test-threads=1"]).env(CHILD_MODE, mode).env(CHILD_FILE, file);
        cmd
    }

    fn limits(stall_ms: u64, total_ms: u64) -> Limits {
        Limits { stall: Duration::from_millis(stall_ms), total: Duration::from_millis(total_ms), poll: Duration::from_millis(20) }
    }

    /// Frames of one megabyte, more than a pipe takes without a reader.
    const BIG: usize = 1 << 20;

    fn big_frames() -> impl FnMut(&mut Vec<u8>) -> Result<bool, String> {
        |buf: &mut Vec<u8>| {
            buf.resize(BIG, 9);
            Ok(true)
        }
    }

    #[test]
    fn a_child_that_stops_reading_is_killed_after_the_stall_bound() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mkv");
        std::fs::write(&out, b"half a video").unwrap();
        let started = Instant::now();
        let asked = Cell::new(0usize);
        let keep_going = |_: usize| {
            asked.set(asked.get() + 1);
            true
        };
        let result = run_encode(stand_in("deaf", &out), BIG, 8, &mut big_frames(), &out, u64::MAX, &keep_going, &limits(400, 60_000));
        assert!(matches!(result, Err(CodecError::TimedOut)), "{result:?}");
        // It waited for the bound, was not left to its 120 seconds, and the output is gone.
        assert!(started.elapsed() >= Duration::from_millis(400), "{:?}", started.elapsed());
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
        assert!(asked.get() > 3, "cancel was asked {} times", asked.get());
        assert!(!out.exists());

        // The total bound stops a child too, also one whose output keeps growing.
        let started = Instant::now();
        let result = run_encode(stand_in("grow", &out), BIG, 8, &mut big_frames(), &out, u64::MAX, &|_| true, &limits(60_000, 500));
        assert!(matches!(result, Err(CodecError::TimedOut)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
        assert!(!out.exists());
    }

    #[test]
    fn cancel_kills_the_child_within_a_second_and_removes_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mkv");
        // Cancel once the child has started writing its output.
        let cancelled: Cell<Option<Instant>> = Cell::new(None);
        let keep_going = |_: usize| {
            if cancelled.get().is_none() && out.exists() {
                cancelled.set(Some(Instant::now()));
            }
            cancelled.get().is_none()
        };
        let result = run_encode(stand_in("grow", &out), BIG, 8, &mut big_frames(), &out, u64::MAX, &keep_going, &limits(60_000, 60_000));
        let took = cancelled.get().expect("the child never wrote its file").elapsed();
        assert!(matches!(result, Err(CodecError::Cancelled)), "{result:?}");
        assert!(took < Duration::from_secs(1), "{took:?}");
        // The child is gone (its file could be removed, and stays removed).
        assert!(!out.exists());
        std::thread::sleep(Duration::from_millis(100));
        assert!(!out.exists());
    }

    #[test]
    fn an_output_that_grows_past_its_bound_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mkv");
        let started = Instant::now();
        let result = run_encode(stand_in("grow", &out), BIG, 8, &mut big_frames(), &out, 200 << 10, &|_| true, &limits(60_000, 60_000));
        assert!(matches!(result, Err(CodecError::TooLarge)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
        assert!(!out.exists());
        std::thread::sleep(Duration::from_millis(100));
        assert!(!out.exists());
    }

    #[test]
    fn a_child_that_floods_its_error_stream_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("count.txt");
        let mut left = 3;
        let mut next = |buf: &mut Vec<u8>| {
            if left == 0 {
                return Ok(false);
            }
            left -= 1;
            buf.resize(1000, 1);
            Ok(true)
        };
        let done = RefCell::new(Vec::new());
        let keep_going = |n: usize| {
            done.borrow_mut().push(n);
            true
        };
        let result = run_encode(stand_in("flood", &out), 1000, 3, &mut next, &out, u64::MAX, &keep_going, &limits(15_000, 30_000));
        assert!(result.is_ok(), "{result:?}");
        // The child got every byte of every frame and ended by itself.
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "3000");
        // Cancel was asked after every frame, with the count so far.
        for n in 1..=3 {
            assert!(done.borrow().contains(&n), "{:?}", done.borrow());
        }
    }

    #[test]
    fn a_caller_with_too_few_frames_or_an_error_stops_the_child_and_removes_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mkv");
        let mut given = 0;
        let mut two = |buf: &mut Vec<u8>| {
            given += 1;
            buf.resize(1000, 1);
            Ok(given <= 2)
        };
        let result = run_encode(stand_in("grow", &out), 1000, 5, &mut two, &out, u64::MAX, &|_| true, &limits(15_000, 30_000));
        assert!(matches!(result, Err(CodecError::WrongFrames { got: 2 })), "{result:?}");
        assert!(!out.exists());
        let result = run_encode(stand_in("grow", &out), 1000, 5, &mut |_| Err("the frames file is damaged".into()), &out, u64::MAX, &|_| true, &limits(15_000, 30_000));
        assert!(matches!(&result, Err(CodecError::Caller(text)) if text == "the frames file is damaged"), "{result:?}");
        assert!(!out.exists());
        let mut wrong = |buf: &mut Vec<u8>| {
            buf.resize(999, 1);
            Ok(true)
        };
        let result = run_encode(stand_in("grow", &out), 1000, 5, &mut wrong, &out, u64::MAX, &|_| true, &limits(15_000, 30_000));
        assert!(matches!(result, Err(CodecError::Caller(_))), "{result:?}");
        assert!(!out.exists());
        // A program that cannot be started is an error of its own, and nothing is left.
        let result = run_encode(command(&dir.path().join("no-such-program.exe")), 1000, 5, &mut big_frames(), &out, u64::MAX, &|_| true, &limits(15_000, 30_000));
        assert!(matches!(result, Err(CodecError::Io(_))), "{result:?}");
    }

    #[test]
    fn the_bounds_follow_the_number_of_frames() {
        let l = Limits::for_frames(248);
        assert_eq!(l.stall, Duration::from_secs(120));
        assert_eq!(l.total, Duration::from_secs(30 + 5 * 248));
        assert_eq!(l.poll, Duration::from_millis(50));
        assert_eq!(Limits::for_frames(0).total, Duration::from_secs(30));
        // No overflow for a count nobody has.
        assert!(Limits::for_frames(usize::MAX).total > Duration::from_secs(1 << 30));
        assert!(matches!(frame_len((0, 10)), Err(CodecError::Caller(_))));
        assert!(matches!(frame_len((u32::MAX, u32::MAX)), Err(CodecError::Caller(_))) || usize::BITS > 64);
        assert_eq!(frame_len((48, 32)).unwrap(), 48 * 32 * 4);
    }

    // ---- the real ffmpeg, when this computer has one ----

    /// Every frame of a small synthetic recording, as RGBA.
    fn synthetic_frames(width: u32, height: u32, count: usize) -> Vec<Vec<u8>> {
        let dir = tempfile::tempdir().unwrap();
        let times: Vec<u64> = (0..count as u64).map(|i| i * 137).collect();
        let rec = Recording::synthetic_in(dir.path(), width, height, &times, count as u64 * 137).unwrap();
        let mut frames = Vec::new();
        rec.for_each_frame(|_, img| {
            frames.push(img.into_raw());
            Ok(())
        })
        .unwrap();
        assert_eq!(frames.len(), count);
        frames
    }

    fn encode_all(ffmpeg: &Ffmpeg, size: (u32, u32), frames: &[Vec<u8>], out: &Path) -> Result<(), CodecError> {
        let mut at = 0;
        let mut next = |buf: &mut Vec<u8>| {
            let Some(frame) = frames.get(at) else { return Ok(false) };
            buf.extend_from_slice(frame);
            at += 1;
            Ok(true)
        };
        ffmpeg.encode(size, frames.len(), &mut next, out, 64 << 20, &|_| true)
    }

    fn decode_all(ffmpeg: &Ffmpeg, video: &Path, size: (u32, u32), frames: usize) -> Result<Vec<Vec<u8>>, CodecError> {
        let mut back = Vec::new();
        let mut each = |i: usize, px: &[u8]| {
            assert_eq!(i, back.len());
            back.push(px.to_vec());
            Ok(())
        };
        ffmpeg.decode(video, size, frames, &mut each, &|_| true).map(|()| back)
    }

    /// The mean difference per colour value between two frames, and whether alpha came back whole.
    fn difference(a: &[u8], b: &[u8]) -> (f64, bool) {
        assert_eq!(a.len(), b.len());
        let mut sum = 0u64;
        let mut opaque = true;
        for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            sum += (0..3).map(|c| x[c].abs_diff(y[c]) as u64).sum::<u64>();
            opaque &= y[3] == 255;
        }
        (sum as f64 / (a.len() / 4 * 3) as f64, opaque)
    }

    fn round_trip(size: (u32, u32), count: usize) {
        let Some(ffmpeg) = find_now() else {
            println!("{SKIPPED}");
            return;
        };
        let frames = synthetic_frames(size.0, size.1, count);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("frames one.mkv");
        encode_all(ffmpeg, size, &frames, &out).unwrap();
        assert!(std::fs::metadata(&out).unwrap().len() > 0);
        let back = decode_all(ffmpeg, &out, size, count).unwrap();
        assert_eq!(back.len(), count);
        for (i, decoded) in back.iter().enumerate() {
            assert_eq!(decoded.len(), (size.0 * size.1 * 4) as usize, "frame {i}");
            // Picture i is frame i: of all the frames that went in, it is closest to its own.
            let nearest = (0..count).min_by(|a, b| difference(&frames[*a], decoded).0.total_cmp(&difference(&frames[*b], decoded).0)).unwrap();
            assert_eq!(nearest, i, "picture {i} is closest to frame {nearest}");
            // And in the right colours: these frames are fine noise, which costs a few levels
            // at this quality; a range or matrix mistake costs far more.
            let (mean, opaque) = difference(&frames[i], decoded);
            assert!(mean < 5.0, "frame {i} differs by {mean} per value");
            assert!(opaque, "frame {i} lost its alpha");
        }
        // Asking for another number of frames is refused, with what was there.
        let fewer = decode_all(ffmpeg, &out, size, count - 1);
        assert!(matches!(fewer, Err(CodecError::WrongFrames { got }) if got == count), "{:?}", fewer.map(|f| f.len()));
        let more = decode_all(ffmpeg, &out, size, count + 1);
        assert!(matches!(more, Err(CodecError::WrongFrames { got }) if got == count), "{:?}", more.map(|f| f.len()));
        // Nothing but the video was written.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_tiny_recording_encodes_and_decodes_to_the_same_number_of_frames() {
        round_trip((48, 32), 40);
    }

    #[test]
    fn odd_picture_sizes_survive() {
        round_trip((47, 33), 40);
    }

    #[test]
    fn the_window_stays_hidden_flag_is_set() {
        // The flags every command of this module is built with.
        assert_eq!(SPAWN_FLAGS & CREATE_NO_WINDOW, CREATE_NO_WINDOW);
        assert_eq!(SPAWN_FLAGS & BELOW_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS);
        let Some(ffmpeg) = find_now() else {
            println!("{SKIPPED}");
            return;
        };
        // And on the built command, as the system reports them for the process it started:
        // an encoder waiting for its first frame.
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::Threading::GetPriorityClass;
            let dir = tempfile::tempdir().unwrap();
            let mut cmd = command(ffmpeg.exe());
            cmd.args(encode_args((48, 32), &dir.path().join("never.mkv")));
            let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
            let class = unsafe { GetPriorityClass(HANDLE(child.as_raw_handle())) };
            let _ = child.kill();
            let _ = child.wait();
            assert_eq!(class, BELOW_NORMAL_PRIORITY_CLASS);
        }
        let _ = ffmpeg;
    }

    #[test]
    fn a_file_that_is_not_a_video_or_a_cancel_fails_cleanly() {
        let Some(ffmpeg) = find_now() else {
            println!("{SKIPPED}");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // Not a Matroska file: the forced container refuses it.
        let fake = dir.path().join("frames.mkv");
        std::fs::write(&fake, b"#EXTM3U\nhttp://example.invalid/a.ts\n").unwrap();
        let result = decode_all(ffmpeg, &fake, (48, 32), 3);
        assert!(matches!(result, Err(CodecError::Failed)), "{:?}", result.map(|f| f.len()));
        assert_eq!(std::fs::read(&fake).unwrap(), b"#EXTM3U\nhttp://example.invalid/a.ts\n");
        // A missing file as well.
        let result = decode_all(ffmpeg, &dir.path().join("gone.mkv"), (48, 32), 3);
        assert!(matches!(result, Err(CodecError::Failed)), "{:?}", result.map(|f| f.len()));
        // A cancel during the encode leaves no file.
        let frames = synthetic_frames(48, 32, 40);
        let out = dir.path().join("out.mkv");
        let mut at = 0;
        let mut next = |buf: &mut Vec<u8>| {
            buf.extend_from_slice(&frames[at]);
            at += 1;
            Ok(true)
        };
        let result = ffmpeg.encode((48, 32), 40, &mut next, &out, 64 << 20, &|done| done < 5);
        assert!(matches!(result, Err(CodecError::Cancelled)), "{result:?}");
        assert!(!out.exists());
        // A video bigger than it may be is refused and removed.
        let result = encode_all(ffmpeg, (48, 32), &frames, &out);
        assert!(result.is_ok());
        let size = std::fs::metadata(&out).unwrap().len();
        let mut at = 0;
        let mut next = |buf: &mut Vec<u8>| {
            buf.extend_from_slice(&frames[at]);
            at += 1;
            Ok(true)
        };
        let result = ffmpeg.encode((48, 32), 40, &mut next, &out, size - 1, &|_| true);
        assert!(matches!(result, Err(CodecError::TooLarge)), "{result:?}");
        assert!(!out.exists());
    }
}
