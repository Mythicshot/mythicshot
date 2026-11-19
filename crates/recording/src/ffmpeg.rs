//! The one place that finds and runs ffmpeg.
//!
//! ffmpeg is an external program the user may have installed. It is looked for once, on the
//! user's PATH only (never the current folder, the application's folder or a relative entry),
//! kept by its absolute path, and probed for its version and for libx264. It is then run with
//! its arguments as a list (never through a shell), with no console window, below normal
//! priority, and watched: a stall bound, a total bound, a bound on the size of what it
//! writes, and a cancel that is asked every 50 ms. Its own error text is drained so it can
//! never block, capped, and never shown or returned.
//!
//! Every pipe is read or written on a thread of its own that owns its end and is never
//! waited for without a bound: a program that leaves a child of its own holding the pipes
//! cannot hang the caller. The started program and its output file belong to a guard that
//! stops the program and removes the file on every way out, a panic of the caller's own
//! function included, unless the run ended well.
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
///
/// The crf is 8, not the 10 first planned: at 10 the key pictures of a screen full of small
/// sharp text came back at 39 dB, under the 40 dB the verification asks of every frame; at 8
/// they measured 44 dB, for 5 to 7 percent more bytes (the ignored test
/// `measure_encoder_candidates` in `video.rs` measures the alternatives again).
pub const CRF: u32 = 8;
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
pub const CODEC_ID: &str = "x264-444-crf8-medium-g30";

const BASE_TIME: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// How much of ffmpeg's error text is kept (for a debug build that asks for it); the rest is
/// read and thrown away.
const ERROR_TEXT_KEPT: usize = 8 << 10;
const PROBE_TIME: Duration = Duration::from_secs(10);
const PROBE_KEPT: usize = 1 << 20;
/// How long the pipe threads of a stopped program are waited for before they are left alone.
const COLLECT: Duration = Duration::from_secs(2);
/// How long a program that closed its pipe early is given to end by itself, so its exit code
/// can be told.
const GRACE: Duration = Duration::from_secs(1);

#[cfg(windows)]
const EXE_NAME: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "ffmpeg";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
/// The process creation flags of every ffmpeg this module starts: no console window, and a
/// priority below the recorder's and the user's own work.
const SPAWN_FLAGS: u32 = CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS;

/// An ffmpeg that was found on the PATH and answered its version. It can always be used for
/// the MP4 and WebM exports; it can archive recordings only when [`Ffmpeg::can_archive`].
#[derive(Clone, PartialEq, Eq)]
pub struct Ffmpeg {
    exe: PathBuf,
    version: String,
    has_libx264: bool,
}

impl Ffmpeg {
    /// The absolute path of the program. It can hold the user's name: never log or show it.
    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// What it said its version is, for example `7.1-full_build`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Whether this build has libx264, which archiving a recording as video needs. Without it
    /// [`VideoCodec::encode`] and [`VideoCodec::decode`] refuse.
    pub fn can_archive(&self) -> bool {
        self.has_libx264
    }
}

/// Never the path: it can hold a folder's or the user's name.
impl std::fmt::Debug for Ffmpeg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ffmpeg").field("version", &self.version).field("can_archive", &self.has_libx264).finish_non_exhaustive()
    }
}

/// What a probe of the program answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    pub version: String,
    pub has_libx264: bool,
}

/// How the one search of this run ended, as a word and never a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// [`find_now`] has not finished yet.
    NotYet,
    /// There is no ffmpeg on the PATH, or the first one there did not answer its version.
    None,
    /// There is an ffmpeg, but without libx264: the exports work, archiving does not.
    Unusable,
    /// There is an ffmpeg that can archive.
    Ok,
}

impl Found {
    /// The word for the result of a finished search.
    pub fn of(found: Option<&Ffmpeg>) -> Found {
        match found {
            None => Found::None,
            Some(f) if f.can_archive() => Found::Ok,
            Some(_) => Found::Unusable,
        }
    }
}

static FOUND: OnceLock<Option<Ffmpeg>> = OnceLock::new();

/// The ffmpeg found by [`find_now`]. None until that ran, and None for good when it found
/// nothing: ffmpeg is looked for once per run. Never starts a program, so it is safe on the
/// UI thread. What is found may lack libx264; ask [`Ffmpeg::can_archive`] before archiving.
pub fn find() -> Option<&'static Ffmpeg> {
    FOUND.get().and_then(Option::as_ref)
}

/// How the search stands, without starting anything: also tells "not looked yet" from
/// "nothing found", and "found, but it cannot archive" from "found".
pub fn found() -> Found {
    match FOUND.get() {
        None => Found::NotYet,
        Some(f) => Found::of(f.as_ref()),
    }
}

/// Look for ffmpeg on the PATH and probe it (two short runs of the program), once; later
/// calls answer what the first one found. For a background thread.
pub fn find_now() -> Option<&'static Ffmpeg> {
    FOUND.get_or_init(|| find_on(&std::env::var_os("PATH")?)).as_ref()
}

/// The search [`find_now`] does, on a PATH that is handed in: the real file system and the
/// real program. Runs the program it finds (bounded), so not for the UI thread.
pub fn find_on(path_var: &OsStr) -> Option<Ffmpeg> {
    find_on_with(path_var, &probe)
}

/// [`find_on`] with the probe handed in. The first absolute entry that holds a file named
/// `ffmpeg.exe` decides: its canonical path is kept, and only if that still ends in
/// `ffmpeg.exe` (a link to a script must not bring a shell back) and the probe answers.
/// Later entries are never tried.
fn find_on_with(path_var: &OsStr, probe: &dyn Fn(&Path) -> Option<Probed>) -> Option<Ffmpeg> {
    let listed = locate(path_var, &|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))?;
    let exe = std::fs::canonicalize(&listed).ok()?;
    if !is_the_program(&exe) || !exe.is_absolute() || !exe.is_file() {
        return None;
    }
    let probed = probe(&exe)?;
    Some(Ffmpeg { exe, version: probed.version, has_libx264: probed.has_libx264 })
}

/// Whether a path is still named `ffmpeg.exe` (in any ASCII case) once links are resolved.
fn is_the_program(canonical: &Path) -> bool {
    canonical.file_name().and_then(OsStr::to_str).is_some_and(|name| name.eq_ignore_ascii_case(EXE_NAME))
}

/// The search itself, with the file system and the program behind two functions: the first
/// absolute entry of `path_var` that holds a file named `ffmpeg.exe` is taken, and it is kept
/// only if `probe` answers. Empty and relative entries are never looked into, no other file
/// name is ever asked about, and a later entry is never tried when the first does not answer.
pub fn find_in(path_var: &OsStr, is_file: &dyn Fn(&Path) -> bool, probe: &dyn Fn(&Path) -> Option<Probed>) -> Option<Ffmpeg> {
    let exe = locate(path_var, is_file)?;
    let probed = probe(&exe)?;
    Some(Ffmpeg { exe, version: probed.version, has_libx264: probed.has_libx264 })
}

fn locate(path_var: &OsStr, is_file: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    std::env::split_paths(path_var).filter(|dir| !dir.as_os_str().is_empty() && dir.is_absolute()).map(|dir| dir.join(EXE_NAME)).find(|exe| is_file(exe))
}

/// Ask the program for its version and its encoders. None when it does not answer its
/// version as an ffmpeg does.
fn probe(exe: &Path) -> Option<Probed> {
    let version = version_of(&String::from_utf8_lossy(&capture(exe, &["-hide_banner", "-version"])?))?;
    let has_libx264 = capture(exe, &["-hide_banner", "-encoders"]).is_some_and(|text| lists_libx264(&String::from_utf8_lossy(&text)));
    Some(Probed { version, has_libx264 })
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
    let mut cmd = command(exe);
    cmd.args(args);
    capture_cmd(cmd, PROBE_TIME)
}

/// What `cmd` printed, if it ended well within `within` and its output ended with it.
fn capture_cmd(mut cmd: Command, within: Duration) -> Option<Vec<u8>> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut run = Run::start(cmd, None).ok()?;
    let stdout = run.child.stdout.take()?;
    let (text_out, text) = mpsc::channel();
    run.detached(move || {
        let _ = text_out.send(drain(stdout, PROBE_KEPT));
    })
    .ok()?;
    let started = Instant::now();
    let status = loop {
        match run.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < within => std::thread::sleep(POLL),
            _ => return None,
        }
    };
    if !status.success() {
        return None;
    }
    text.recv_timeout(COLLECT).ok()
}

/// A command for `exe` that opens no console window, runs below normal priority, and is not
/// told by the environment to write a report file.
fn command(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    cmd.env_remove("FFREPORT");
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

/// Which time bound stopped the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// Nothing moved for [`STALL`].
    Stall,
    /// The whole run took longer than 30 seconds plus [`PER_FRAME`] per frame.
    Total,
}

/// Why an encode or a decode did not finish. Numbers and words of this crate only: it never
/// carries the program's own text.
#[derive(Debug)]
pub enum CodecError {
    /// `keep_going` answered false.
    Cancelled,
    /// The program ended with an error (`exit` is its exit code), or it could not do the
    /// work and was stopped or never started (`exit` is None): it closed its pipe early, or
    /// this build cannot archive.
    Failed { exit: Option<i32> },
    /// A time bound passed and the program was stopped.
    TimedOut { bound: Bound },
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

    /// Write `frames` RGBA pictures of `size` to a new file `out`; what was at `out` before
    /// is removed first. `next` fills the buffer it is handed with the next picture (exactly
    /// width × height × 4 bytes) and answers `Ok(true)`, or `Ok(false)` when there are no
    /// more. `keep_going` is told how many frames are done, at least every frame, and stops
    /// the work by answering false. On any error, a refusal before the work began included,
    /// nothing is left at `out`.
    fn encode(&self, size: (u32, u32), frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError>;

    /// Read `video` back: `each` gets every picture in order as RGBA of exactly
    /// width × height × 4 bytes. A video with fewer or more than `frames` pictures is an
    /// error. What is checked is the number of bytes, nothing else: a video whose pictures
    /// have another size is refused only when its total does not come to
    /// `frames × width × height × 4` bytes. One whose total happens to match (4×2 for 2×4,
    /// or twice the frames at half the size) is handed on as if it were right, so the caller
    /// must compare the pictures with what it expects before it trusts them.
    #[allow(clippy::type_complexity)]
    fn decode(&self, video: &Path, size: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError>;
}

impl VideoCodec for Ffmpeg {
    fn id(&self) -> &str {
        CODEC_ID
    }

    fn encode(&self, size: (u32, u32), frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
        let result = (|| {
            if !self.has_libx264 {
                return Err(CodecError::Failed { exit: None });
            }
            let frame_len = frame_len(size)?;
            if frames == 0 {
                return Err(CodecError::Caller("there are no frames to encode".into()));
            }
            let mut cmd = command(&self.exe);
            cmd.args(encode_args(size, out));
            run_encode(cmd, frame_len, frames, next, out, max_bytes, keep_going, &Limits::for_frames(frames))?;
            // Success with nothing written is not a video.
            if std::fs::metadata(out).is_ok_and(|m| m.is_file() && m.len() > 0) { Ok(()) } else { Err(CodecError::Failed { exit: Some(0) }) }
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(out);
        }
        result
    }

    fn decode(&self, video: &Path, size: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
        if !self.has_libx264 {
            return Err(CodecError::Failed { exit: None });
        }
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
    /// of the output. Time the caller's own functions take does not count.
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
        if self.started.elapsed() > self.limits.total {
            Err(CodecError::TimedOut { bound: Bound::Total })
        } else if self.moved.elapsed() > self.limits.stall {
            Err(CodecError::TimedOut { bound: Bound::Stall })
        } else {
            Ok(())
        }
    }
}

/// A started program, and what must not outlive a failed run: the program itself, the file
/// it writes, and the threads on its pipes. Dropping it, on any way out, stops the program
/// if it still runs, waits for it, gives the pipe threads [`COLLECT`] to end and then leaves
/// them alone, and removes the output unless [`Run::keep`] was called.
struct Run {
    child: Child,
    /// The output to remove; None once the run ended well, or when there is none.
    out: Option<PathBuf>,
    /// The buffer's way to the pipe thread; dropped first, so that thread sees the end.
    pipe: Option<mpsc::Sender<Vec<u8>>>,
    /// One per pipe thread: disconnected when the thread has ended.
    threads: Vec<mpsc::Receiver<()>>,
    /// The capped error text, when its thread has read to the end.
    errors: Option<mpsc::Receiver<Vec<u8>>>,
}

impl Run {
    /// Start `cmd`, whose pipes the caller has set up. With an output, what is there now is
    /// removed first, so nothing stale can be taken for the program's work.
    fn start(mut cmd: Command, out: Option<&Path>) -> Result<Run, CodecError> {
        if let Some(out) = out {
            match std::fs::remove_file(out) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(CodecError::Io(e)),
                _ => {}
            }
        }
        let child = cmd.spawn().map_err(CodecError::Io)?;
        let mut run = Run { child, out: out.map(Path::to_path_buf), pipe: None, threads: Vec::new(), errors: None };
        if let Some(stderr) = run.child.stderr.take() {
            let (text_out, text) = mpsc::channel();
            run.errors = Some(text);
            run.detached(move || {
                let _ = text_out.send(drain(stderr, ERROR_TEXT_KEPT));
            })?;
        }
        Ok(run)
    }

    /// Run `work` on a thread of its own that this run never waits for without a bound.
    fn detached(&mut self, work: impl FnOnce() + Send + 'static) -> Result<(), CodecError> {
        let (ended, end) = mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("mythicshot-ffmpeg-pipe".into())
            .spawn(move || {
                let _ended = ended;
                work();
            })
            .map_err(CodecError::Io)?;
        self.threads.push(end);
        Ok(())
    }

    /// The run ended well: the output stays.
    fn keep(&mut self) {
        self.out = None;
    }

    /// The error for a program that closed its pipe before the work was done: with its exit
    /// code if it ends by itself within [`GRACE`].
    fn gave_up(&mut self) -> CodecError {
        let until = Instant::now() + GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return CodecError::Failed { exit: status.code() },
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                _ => return CodecError::Failed { exit: None },
            }
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.pipe = None;
        let ended = matches!(self.child.try_wait(), Ok(Some(_)));
        if !ended {
            let _ = self.child.kill();
        }
        let status = self.child.wait().ok();
        // The pipe threads end when the program's ends of the pipes are closed. A child of
        // the program can still hold them; then the threads are left behind.
        let until = Instant::now() + COLLECT;
        for thread in &self.threads {
            while let Ok(()) | Err(RecvTimeoutError::Timeout) = thread.recv_timeout(until.saturating_duration_since(Instant::now())) {
                if Instant::now() >= until {
                    break;
                }
            }
        }
        if let Some(text) = self.errors.take().and_then(|e| e.recv_timeout(until.saturating_duration_since(Instant::now())).ok()) {
            note_error_text(status, ended, &text);
        }
        if let Some(out) = self.out.take() {
            let _ = std::fs::remove_file(out);
        }
    }
}

/// In a debug build with `MYTHICSHOT_FFMPEG_LOG` set, the exit code and the capped error text
/// go to the console. Otherwise the text is dropped: it can hold a folder's name.
fn note_error_text(status: Option<ExitStatus>, by_itself: bool, text: &[u8]) {
    if cfg!(debug_assertions) && !text.is_empty() && std::env::var_os("MYTHICSHOT_FFMPEG_LOG").is_some() {
        let code = status.filter(|_| by_itself).and_then(|s| s.code());
        eprintln!("ffmpeg ended with {code:?}: {}", String::from_utf8_lossy(text).trim());
    }
}

/// Run `cmd` as an encoder: feed it `frames` frames on standard input through a thread of
/// its own, watch it from this one, and leave `out` only if it ended well.
#[allow(clippy::too_many_arguments)]
fn run_encode(mut cmd: Command, frame_len: usize, frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool, limits: &Limits) -> Result<(), CodecError> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    // From here on every way out, a panic of `next` or `keep_going` included, goes through
    // the drop of `run`.
    let mut run = Run::start(cmd, Some(out))?;
    let Some(mut stdin) = run.child.stdin.take() else { return Err(CodecError::Failed { exit: None }) };
    // One buffer goes back and forth: this thread fills it, the pipe thread writes it.
    let (to_writer, writer_in) = mpsc::channel::<Vec<u8>>();
    let (writer_out, from_writer) = mpsc::channel::<Option<Vec<u8>>>();
    run.pipe = Some(to_writer);
    run.detached(move || {
        for buf in writer_in {
            let written = stdin.write_all(&buf).is_ok();
            if writer_out.send(written.then_some(buf)).is_err() || !written {
                break;
            }
        }
        // Dropping the pipe here tells the program its input has ended.
    })?;

    let mut watch = Watch::start(limits);
    let mut size = 0u64;
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
    let mut done = 0usize;
    while done < frames {
        buf.clear();
        match next(&mut buf) {
            Err(text) => return Err(CodecError::Caller(text)),
            Ok(false) => return Err(CodecError::WrongFrames { got: done }),
            Ok(true) if buf.len() != frame_len => return Err(CodecError::Caller("a frame has the wrong size".into())),
            Ok(true) => {}
        }
        // Time the caller took is not the program's stall.
        watch.moved();
        let sent = run.pipe.as_ref().is_some_and(|pipe| pipe.send(std::mem::take(&mut buf)).is_ok());
        if !sent {
            return Err(run.gave_up());
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
                Ok(None) | Err(RecvTimeoutError::Disconnected) => return Err(run.gave_up()),
                Err(RecvTimeoutError::Timeout) => look(done, &mut watch)?,
            }
        }
        look(done, &mut watch)?;
    }
    // End of input; now the program finishes the file.
    run.pipe = None;
    loop {
        match run.child.try_wait().map_err(CodecError::Io)? {
            Some(status) if status.success() => break,
            Some(status) => return Err(CodecError::Failed { exit: status.code() }),
            None => {
                look(done, &mut watch)?;
                std::thread::sleep(limits.poll);
            }
        }
    }
    if std::fs::metadata(out).map(|m| m.len()).unwrap_or(0) > max_bytes {
        return Err(CodecError::TooLarge);
    }
    run.keep();
    Ok(())
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
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // From here on every way out, a panic of `each` or `keep_going` included, goes through
    // the drop of `run`.
    let mut run = Run::start(cmd, None)?;
    let Some(mut stdout) = run.child.stdout.take() else { return Err(CodecError::Failed { exit: None }) };
    // One buffer goes back and forth: the pipe thread fills it, this thread hands it on.
    let (reader_out, from_reader) = mpsc::channel::<Piece>();
    let (to_reader, reader_in) = mpsc::channel::<Vec<u8>>();
    run.pipe = Some(to_reader);
    run.detached(move || {
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
    })?;

    let mut watch = Watch::start(limits);
    let mut done = 0usize;
    let look = |done: usize, watch: &Watch| -> Result<(), CodecError> { if keep_going(done) { watch.check() } else { Err(CodecError::Cancelled) } };
    // Whether the stream ended early (a count, unless the program says it failed) or whole.
    let mut short = false;
    loop {
        match from_reader.recv_timeout(limits.poll) {
            Ok(Piece::Frame(buf)) => {
                each(done, &buf).map_err(CodecError::Caller)?;
                done += 1;
                // Time the caller took is not the program's stall.
                watch.moved();
                look(done, &watch)?;
                if run.pipe.as_ref().is_none_or(|pipe| pipe.send(buf).is_err()) {
                    return Err(run.gave_up());
                }
            }
            Ok(Piece::Short) => {
                short = true;
                break;
            }
            Ok(Piece::End) => break,
            Ok(Piece::Extra) => return Err(CodecError::WrongFrames { got: frames + 1 }),
            Ok(Piece::Broken(e)) => return Err(CodecError::Io(e)),
            Err(RecvTimeoutError::Disconnected) => return Err(run.gave_up()),
            Err(RecvTimeoutError::Timeout) => look(done, &watch)?,
        }
    }
    loop {
        match run.child.try_wait().map_err(CodecError::Io)? {
            Some(status) if !status.success() => return Err(CodecError::Failed { exit: status.code() }),
            Some(_) if short => return Err(CodecError::WrongFrames { got: done }),
            Some(_) => return Ok(()),
            None => {
                look(done, &watch)?;
                std::thread::sleep(limits.poll);
            }
        }
    }
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

    fn version(_: &Path) -> Option<Probed> {
        Some(Probed { version: "7.1-test".into(), has_libx264: true })
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
            version(p)
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

    /// Since the review: an ffmpeg without libx264 is found (the exports can use it), and
    /// only archiving refuses it; one that does not answer its version is not found.
    #[test]
    fn an_ffmpeg_that_does_not_answer_its_version_or_lacks_libx264_is_not_found() {
        let dir = folder("bin");
        let path = path_of(&[&dir]);
        assert!(find_in(&path, &|_| true, &|_| None).is_none());
        assert_eq!(Found::of(None), Found::None);

        let plain = find_in(&path, &|_| true, &|_| Some(Probed { version: "6.0".into(), has_libx264: false })).unwrap();
        assert!(!plain.can_archive());
        assert_eq!(plain.version(), "6.0");
        assert_eq!(Found::of(Some(&plain)), Found::Unusable);
        let whole = find_in(&path, &|_| true, &version).unwrap();
        assert!(whole.can_archive());
        assert_eq!(Found::of(Some(&whole)), Found::Ok);
        // Archiving with it is refused before any program is started (this one does not
        // exist, so a start would be another error), and what stood at the output is removed.
        let out_dir = tempfile::tempdir().unwrap();
        let out = out_dir.path().join("out.mkv");
        std::fs::write(&out, b"older").unwrap();
        let result = plain.encode((4, 4), 1, &mut |_| Ok(false), &out, 1 << 20, &|_| true);
        assert!(matches!(result, Err(CodecError::Failed { exit: None })), "{result:?}");
        assert!(!out.exists());
        let result = plain.decode(&out, (4, 4), 1, &mut |_, _| Ok(()), &|_| true);
        assert!(matches!(result, Err(CodecError::Failed { exit: None })), "{result:?}");
        // The other early refusals of an encode remove the output as well.
        for (size, frames) in [((0, 4), 1), ((4, 4), 0)] {
            std::fs::write(&out, b"older").unwrap();
            let result = whole.encode(size, frames, &mut |_| Ok(false), &out, 1 << 20, &|_| true);
            assert!(matches!(result, Err(CodecError::Caller(_))), "{result:?}");
            assert!(!out.exists());
        }
        // What the one find of this run says, in a word and without a path.
        match find_now() {
            Some(real) => assert_eq!(found(), if real.can_archive() { Found::Ok } else { Found::Unusable }),
            None => assert_eq!(found(), Found::None),
        }

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

    #[cfg(windows)]
    fn link(to: &Path, at: &Path) -> bool {
        std::os::windows::fs::symlink_file(to, at).is_ok()
    }
    #[cfg(not(windows))]
    fn link(to: &Path, at: &Path) -> bool {
        std::os::unix::fs::symlink(to, at).is_ok()
    }

    #[test]
    fn the_search_on_a_real_folder_takes_only_a_real_program_file_under_its_own_name() {
        let root = tempfile::tempdir().unwrap();
        let dir = |name: &str| {
            let d = root.path().join(name);
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        let (folder_named, real, empty, other) = (dir("folder named"), dir("real"), dir("empty"), dir("other"));
        std::fs::create_dir(folder_named.join(EXE_NAME)).unwrap();
        std::fs::write(real.join(EXE_NAME), b"a program").unwrap();
        std::fs::write(empty.join(EXE_NAME), b"").unwrap();
        let probed = RefCell::new(Vec::new());
        let probe = |p: &Path| {
            probed.borrow_mut().push(p.to_path_buf());
            version(p)
        };
        // A folder with the program's name is passed over; the file after it is taken, by
        // its canonical absolute path.
        let found = find_on_with(&path_of(&[Path::new("relative"), &folder_named, &real, &empty]), &probe).unwrap();
        assert_eq!(found.exe(), std::fs::canonicalize(real.join(EXE_NAME)).unwrap());
        assert!(found.exe().is_absolute());
        assert_eq!(*probed.borrow(), vec![found.exe().to_path_buf()]);
        // An empty file is a file: it is the first, it decides, and it is no ffmpeg. The
        // real one after it is not tried.
        assert!(find_on(&path_of(&[&folder_named, &empty, &real])).is_none());
        // Nothing there at all.
        assert!(find_on(&path_of(&[&folder_named, &other])).is_none());

        // The rule for where a link may lead, by itself (a link cannot be made everywhere).
        assert!(is_the_program(&real.join(EXE_NAME)));
        assert!(is_the_program(&real.join(EXE_NAME.to_ascii_uppercase())) || !cfg!(windows));
        for other_name in ["run.cmd", "ffmpeg.bat", "ffmpeg.cmd", "ffmpeg.exe.cmd", "cmd.exe", "ffmpeg2.exe"] {
            assert!(!is_the_program(&real.join(other_name)), "{other_name}");
        }
        assert!(!is_the_program(&real));

        // A link named like the program that leads to a script is never run.
        let (links, scripts) = (dir("links"), dir("scripts"));
        std::fs::write(scripts.join("run.cmd"), b"@echo ffmpeg version 1.0").unwrap();
        if !link(&scripts.join("run.cmd"), &links.join(EXE_NAME)) {
            println!("skipped: a symbolic link could not be made on this computer");
            return;
        }
        probed.borrow_mut().clear();
        assert!(find_on_with(&path_of(&[&links, &real]), &probe).is_none());
        assert!(probed.borrow().is_empty(), "{:?}", probed.borrow());
        // A link to a program of the same name (in any case) is followed, and the path kept
        // is where the program really is.
        let upper = if cfg!(windows) { EXE_NAME.to_ascii_uppercase() } else { EXE_NAME.to_string() };
        std::fs::write(other.join(&upper), b"a program").unwrap();
        let links2 = dir("links2");
        assert!(link(&other.join(&upper), &links2.join(EXE_NAME)));
        let found = find_on_with(&path_of(&[&links2]), &probe).unwrap();
        assert_eq!(found.exe(), std::fs::canonicalize(other.join(&upper)).unwrap());
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
        assert_eq!(value_of(&args, "-crf"), "8");
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
        let planned = "-hide_banner -nostdin -loglevel error -y -f rawvideo -pix_fmt rgba -video_size 2560x1440 -framerate 25 -i pipe:0 -an -vf scale=out_range=full:out_color_matrix=bt709,format=yuv444p -c:v libx264 -crf 8 -preset medium -g 30 -keyint_min 30 -sc_threshold 0 -color_range pc -colorspace bt709 -f matroska";
        let mut expected: Vec<String> = planned.split(' ').map(String::from).collect();
        expected.push(out.to_string_lossy().into_owned());
        assert_eq!(args, expected);
        // The id names exactly these settings.
        assert_eq!(CODEC_ID, format!("x264-444-crf{CRF}-{PRESET}-g{GOP}"));
        assert_eq!(Ffmpeg { exe: out, version: String::new(), has_libx264: true }.id(), "x264-444-crf8-medium-g30");
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
    const CHILD_BYTES: &str = "MYTHICSHOT_TEST_CHILD_BYTES";

    /// Not a test of its own: what the stand-in child does when the runner tests start this
    /// test program again with [`CHILD_MODE`] set. Without it, nothing.
    #[test]
    fn stand_in_child() {
        let Ok(mode) = std::env::var(CHILD_MODE) else { return };
        match mode.as_str() {
            // Never reads its input and never ends on its own.
            "deaf" => std::thread::sleep(Duration::from_secs(120)),
            // Never reads its input, and writes its file again and again (for about 20 seconds):
            // while it lives, a removed file comes back.
            "grow" => grow(),
            // Starts a child of its own that inherits this one's pipes and outlives it, then grows.
            "family" => {
                grandchild();
                grow();
            }
            // Starts such a child and ends at once, well.
            "orphan" => grandchild(),
            // The child of a stand-in: holds the pipes it inherited for a while, and does nothing.
            "nap" => std::thread::sleep(Duration::from_secs(15)),
            // Writes far more error text than a pipe holds, then reads its input to the end.
            "flood" => {
                let mut err = std::io::stderr();
                for _ in 0..(4 << 10) {
                    err.write_all(&[b'e'; 1 << 10]).unwrap();
                }
                let read = std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).unwrap();
                std::fs::write(std::env::var_os(CHILD_FILE).unwrap(), read.to_string()).unwrap();
            }
            // Prints nothing of its own.
            "quiet" => {}
            // Ends at once with an error of its own.
            "fail" => std::process::exit(3),
            // Writes so many bytes to its output and ends well; "emit-hang" then stays.
            "emit" | "emit-hang" => {
                let bytes: usize = std::env::var(CHILD_BYTES).unwrap().parse().unwrap();
                let mut o = std::io::stdout();
                o.write_all(&vec![0x5a; bytes]).unwrap();
                o.flush().unwrap();
                if mode == "emit-hang" {
                    std::thread::sleep(Duration::from_secs(15));
                }
            }
            // Writes to its output until nobody listens.
            "endless" => {
                let mut o = std::io::stdout();
                while o.write_all(&[0x5a; 4096]).is_ok() {}
            }
            other => panic!("unknown stand-in mode {other}"),
        }
        std::process::exit(0);
    }

    fn grow() {
        let file = std::env::var_os(CHILD_FILE).unwrap();
        for _ in 0..2000 {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&file).unwrap();
            f.write_all(&[7u8; 32 << 10]).unwrap();
            drop(f);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn grandchild() {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["ffmpeg::tests::stand_in_child", "--exact", "--nocapture", "--test-threads=1"]).env(CHILD_MODE, "nap");
        std::mem::forget(cmd.spawn().unwrap());
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
        assert!(matches!(result, Err(CodecError::TimedOut { bound: Bound::Stall })), "{result:?}");
        // It waited for the bound, was not left to its 120 seconds, and the output is gone.
        assert!(started.elapsed() >= Duration::from_millis(400), "{:?}", started.elapsed());
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
        assert!(asked.get() > 3, "cancel was asked {} times", asked.get());
        assert!(!out.exists());

        // The total bound stops a child too, also one whose output keeps growing.
        let started = Instant::now();
        let result = run_encode(stand_in("grow", &out), BIG, 8, &mut big_frames(), &out, u64::MAX, &|_| true, &limits(60_000, 500));
        assert!(matches!(result, Err(CodecError::TimedOut { bound: Bound::Total })), "{result:?}");
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

    // ---- a child whose own child keeps the pipes open, and a caller that panics ----

    #[test]
    fn a_cancel_returns_and_removes_the_output_when_a_grandchild_still_holds_the_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mkv");
        let cancelled: Cell<Option<Instant>> = Cell::new(None);
        let keep_going = |_: usize| {
            if cancelled.get().is_none() && out.exists() {
                cancelled.set(Some(Instant::now()));
            }
            cancelled.get().is_none()
        };
        let result = run_encode(stand_in("family", &out), BIG, 8, &mut big_frames(), &out, u64::MAX, &keep_going, &limits(60_000, 60_000));
        let took = cancelled.get().expect("the child never wrote its file").elapsed();
        assert!(matches!(result, Err(CodecError::Cancelled)), "{result:?}");
        assert!(took < Duration::from_secs(6), "{took:?}");
        assert!(!out.exists());
        std::thread::sleep(Duration::from_millis(200));
        assert!(!out.exists());
    }

    #[test]
    fn a_time_bound_returns_and_removes_the_output_when_a_grandchild_still_holds_the_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mkv");
        let started = Instant::now();
        let result = run_encode(stand_in("family", &out), BIG, 8, &mut big_frames(), &out, u64::MAX, &|_| true, &limits(60_000, 500));
        assert!(matches!(result, Err(CodecError::TimedOut { bound: Bound::Total })), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
        assert!(!out.exists());
        std::thread::sleep(Duration::from_millis(200));
        assert!(!out.exists());
    }

    #[test]
    fn a_panic_in_the_callers_function_stops_the_child_and_removes_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mkv");
        let started = Instant::now();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut given = 0;
            let mut next = |buf: &mut Vec<u8>| {
                given += 1;
                // Wait until the child is writing, then fail badly.
                while given == 2 && !out.exists() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(given < 2, "the caller's function panics");
                buf.resize(1000, 9);
                Ok(true)
            };
            run_encode(stand_in("grow", &out), 1000, 8, &mut next, &out, u64::MAX, &|_| true, &limits(60_000, 60_000))
        }));
        assert!(caught.is_err());
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
        assert!(!out.exists());
        std::thread::sleep(Duration::from_millis(200));
        assert!(!out.exists(), "the child is still writing");
        // The same when it is the cancel question that panics.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let keep_going = |_: usize| -> bool {
                if out.exists() {
                    panic!("the cancel question panics");
                }
                true
            };
            run_encode(stand_in("grow", &out), BIG, 8, &mut big_frames(), &out, u64::MAX, &keep_going, &limits(60_000, 60_000))
        }));
        assert!(caught.is_err());
        std::thread::sleep(Duration::from_millis(200));
        assert!(!out.exists(), "the child is still writing");
    }

    #[test]
    fn what_was_at_the_output_before_is_removed_before_the_program_starts() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count.txt");
        let stale = dir.path().join("out.mkv");
        std::fs::write(&stale, b"an older video").unwrap();
        // This child never writes the output; a run that ends well leaves none.
        let mut left = 2;
        let mut next = |buf: &mut Vec<u8>| {
            if left == 0 {
                return Ok(false);
            }
            left -= 1;
            buf.resize(1000, 1);
            Ok(true)
        };
        let result = run_encode(stand_in("flood", &count), 1000, 2, &mut next, &stale, u64::MAX, &|_| true, &limits(15_000, 30_000));
        assert!(result.is_ok(), "{result:?}");
        assert!(!stale.exists());
    }

    #[test]
    fn every_command_is_built_without_a_report_file_setting() {
        let cmd = command(Path::new("ffmpeg.exe"));
        assert!(cmd.get_envs().any(|(name, value)| name == "FFREPORT" && value.is_none()));
    }

    #[test]
    fn the_error_text_kept_is_capped_at_8_kb() {
        let text = vec![b'x'; 100 << 10];
        assert_eq!(drain(&text[..], ERROR_TEXT_KEPT).len(), 8 << 10);
        assert_eq!(drain(&text[..100], ERROR_TEXT_KEPT).len(), 100);
        assert_eq!(ERROR_TEXT_KEPT, 8 << 10);
    }

    #[test]
    fn the_debug_text_of_a_found_ffmpeg_holds_no_path() {
        let found = find_in(&path_of(&[&folder("secret-folder-name")]), &|_| true, &version).unwrap();
        let text = format!("{found:?}");
        assert!(!text.contains("secret-folder-name"), "{text}");
        assert!(text.contains("7.1-test"), "{text}");
    }

    #[test]
    fn the_probe_returns_when_a_grandchild_still_holds_the_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("unused");
        // The program ended well, and its output never ends.
        let started = Instant::now();
        assert!(capture_cmd(stand_in("orphan", &file), Duration::from_secs(10)).is_none());
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
        // The program never ends: stopped at the bound, and its output never ends either.
        let started = Instant::now();
        assert!(capture_cmd(stand_in("family", &file), Duration::from_millis(300)).is_none());
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
        // A program that ends well by itself is read whole.
        assert!(capture_cmd(stand_in("quiet", &file), Duration::from_secs(10)).is_some());
        assert!(capture_cmd(stand_in("fail", &file), Duration::from_secs(10)).is_none());
    }

    #[test]
    fn a_program_that_ends_with_an_error_is_reported_with_its_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mkv");
        let mut next = |buf: &mut Vec<u8>| {
            buf.resize(1000, 1);
            Ok(true)
        };
        let result = run_encode(stand_in("fail", &out), 1000, 3, &mut next, &out, u64::MAX, &|_| true, &limits(15_000, 30_000));
        assert!(matches!(result, Err(CodecError::Failed { exit: Some(3) })), "{result:?}");
        let result = run_decode(stand_in("fail", &out), 1000, 3, &mut |_, _| Ok(()), &|_| true, &limits(15_000, 30_000));
        assert!(matches!(result, Err(CodecError::Failed { exit: Some(3) })), "{result:?}");
    }

    #[test]
    fn a_slow_caller_is_not_the_programs_stall() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count.txt");
        let out = dir.path().join("out.mkv");
        let mut left = 3;
        // Every frame takes the caller longer than the stall bound.
        let mut next = |buf: &mut Vec<u8>| {
            if left == 0 {
                return Ok(false);
            }
            left -= 1;
            std::thread::sleep(Duration::from_millis(700));
            buf.resize(BIG, 1);
            Ok(true)
        };
        let result = run_encode(stand_in("flood", &count), BIG, 3, &mut next, &out, u64::MAX, &|_| true, &limits(600, 30_000));
        assert!(result.is_ok(), "{result:?}");
    }

    // ---- the decode runner, with the stand-in ----

    /// What the test program prints by itself before a test's own output; the stand-in's
    /// bytes come after it, so the tests count it in.
    fn preamble() -> usize {
        let dir = tempfile::tempdir().unwrap();
        let bytes = capture_cmd(stand_in("quiet", &dir.path().join("unused")), Duration::from_secs(20)).unwrap().len();
        assert!(bytes < FRAME, "the test program prints {bytes} bytes by itself");
        bytes
    }

    const FRAME: usize = 1000;

    /// A stand-in whose whole output is `bytes` long.
    fn emitting(mode: &str, bytes: usize) -> Command {
        let mut cmd = stand_in(mode, Path::new("unused"));
        cmd.env(CHILD_BYTES, (bytes - preamble()).to_string());
        cmd
    }

    fn decode_from(cmd: Command, frames: usize, limits: &Limits, keep_going: &dyn Fn(usize) -> bool) -> (Result<(), CodecError>, Vec<usize>) {
        let mut seen = Vec::new();
        let mut each = |i: usize, px: &[u8]| {
            assert_eq!(px.len(), FRAME);
            seen.push(i);
            Ok(())
        };
        let result = run_decode(cmd, FRAME, frames, &mut each, keep_going, limits);
        (result, seen)
    }

    #[test]
    fn a_stream_of_exactly_the_frames_asked_for_is_read_whole() {
        let (result, seen) = decode_from(emitting("emit", 5 * FRAME), 5, &limits(15_000, 30_000), &|_| true);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_stream_one_byte_short_or_one_byte_long_is_refused_with_its_count() {
        let (result, seen) = decode_from(emitting("emit", 5 * FRAME - 1), 5, &limits(15_000, 30_000), &|_| true);
        assert!(matches!(result, Err(CodecError::WrongFrames { got: 4 })), "{result:?}");
        assert_eq!(seen, vec![0, 1, 2, 3]);
        let (result, seen) = decode_from(emitting("emit", 5 * FRAME + 1), 5, &limits(15_000, 30_000), &|_| true);
        assert!(matches!(result, Err(CodecError::WrongFrames { got: 6 })), "{result:?}");
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_stream_without_end_is_stopped_after_the_last_frame() {
        let started = Instant::now();
        let (result, seen) = decode_from(stand_in("endless", Path::new("unused")), 5, &limits(15_000, 30_000), &|_| true);
        assert!(matches!(result, Err(CodecError::WrongFrames { got: 6 })), "{result:?}");
        assert_eq!(seen.len(), 5);
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    }

    #[test]
    fn a_stream_that_stops_short_and_hangs_is_stopped_at_the_stall_bound() {
        let started = Instant::now();
        let (result, seen) = decode_from(emitting("emit-hang", 4 * FRAME + 500), 5, &limits(400, 30_000), &|_| true);
        assert!(matches!(result, Err(CodecError::TimedOut { bound: Bound::Stall })), "{result:?}");
        assert_eq!(seen, vec![0, 1, 2, 3]);
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
        // The same with the caller's own error, and with a panic in it: the child is stopped.
        let started = Instant::now();
        let result = run_decode(emitting("emit-hang", 4 * FRAME), FRAME, 5, &mut |_, _| Err("no room".into()), &|_| true, &limits(15_000, 30_000));
        assert!(matches!(&result, Err(CodecError::Caller(text)) if text == "no room"), "{result:?}");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_decode(emitting("emit-hang", 4 * FRAME), FRAME, 5, &mut |_, _| panic!("the caller panics"), &|_| true, &limits(15_000, 30_000))));
        assert!(caught.is_err());
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    }

    #[test]
    fn a_cancel_stops_a_decode_within_a_second() {
        let cancelled: Cell<Option<Instant>> = Cell::new(None);
        let keep_going = |done: usize| {
            if done == 4 && cancelled.get().is_none() {
                cancelled.set(Some(Instant::now()));
            }
            cancelled.get().is_none()
        };
        let (result, seen) = decode_from(emitting("emit-hang", 4 * FRAME + 500), 5, &limits(60_000, 60_000), &keep_going);
        let took = cancelled.get().unwrap().elapsed();
        assert!(matches!(result, Err(CodecError::Cancelled)), "{result:?}");
        assert_eq!(seen.len(), 4);
        assert!(took < Duration::from_secs(1), "{took:?}");
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
        assert!(matches!(result, Err(CodecError::Failed { exit: Some(code) }) if code != 0), "{:?}", result.map(|f| f.len()));
        assert_eq!(std::fs::read(&fake).unwrap(), b"#EXTM3U\nhttp://example.invalid/a.ts\n");
        // A missing file as well.
        let result = decode_all(ffmpeg, &dir.path().join("gone.mkv"), (48, 32), 3);
        assert!(matches!(result, Err(CodecError::Failed { exit: Some(code) }) if code != 0), "{:?}", result.map(|f| f.len()));
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
