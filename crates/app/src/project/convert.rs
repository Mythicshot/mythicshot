//! The conversion queue: saved recordings are stored again as video, one at a time, in the
//! background.
//!
//! [`run`] is the loop the start-up housekeeping thread enters once the folder of saved
//! recordings is tidied and ffmpeg was looked for. It asks the store for the next project
//! ([`Store::next_candidate`], oldest first), and [`convert_one`] archives its recording into
//! the conversion's own folder, has every frame verified, and swaps the video in or notes why
//! the frames are kept. Between projects and before every frame the [`Gate`] is asked: the
//! switch in Preferences, a recording session, and the end of the app each stop the work.
//!
//! Without a codec (no ffmpeg that can archive) or with the switch off nothing here ever
//! touches a project.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use mythicshot_recording::ffmpeg::{self, Ffmpeg, Found};
use mythicshot_recording::{ArchiveError, Bound, CodecError, Mismatch, Recording, Stage, TOLERANCE, Tolerance, VideoCodec};

use super::state::ProjectKey;
use super::store::{Candidate, MARK_FAILED, MARK_NOT_SMALLER, Store, set_current_codec};

/// A recording whose frames file is smaller than this is left as it is.
pub const MIN_BYTES: u64 = 8 << 20;
/// How long after the start of the app the first conversion may begin.
pub const START_AFTER: Duration = Duration::from_secs(60);
/// How long after a save, the end of a recording session or the switch being turned on the
/// queue looks for work again.
pub const AFTER_SAVE: Duration = Duration::from_secs(10);
/// How long after a conversion gave way to an editor or a save the queue looks again, also
/// when nothing nudges it: an editor that is closed without saving tells nobody.
pub const AFTER_CANCEL: Duration = Duration::from_secs(5 * 60);
/// After this many failures of the codec itself in a row the queue stops until the next start.
const CODEC_FAILURES: u32 = 3;

/// How the room left on a folder's disk is read: the bytes free there, or None when that
/// cannot be told.
type Free<'a> = &'a (dyn Fn(&Path) -> Option<u64> + Sync);

/// The room left for this user on the disk `folder` is on. None when the folder is not
/// there, the system does not say, or on a system this is not written for.
#[cfg(windows)]
pub(crate) fn free_bytes(folder: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let name: Vec<u16> = folder.as_os_str().encode_wide().collect();
    if name.is_empty() || name.contains(&0) {
        return None;
    }
    let name: Vec<u16> = name.into_iter().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `name` is a wide string that ends in a zero and outlives the call, and `free`
    // is a u64 of this function's own that the call writes once.
    unsafe { GetDiskFreeSpaceExW(windows::core::PCWSTR(name.as_ptr()), Some(&mut free), None, None) }.ok().map(|()| free)
}

#[cfg(not(windows))]
pub(crate) fn free_bytes(_: &Path) -> Option<u64> {
    None
}

/// What Preferences says about ffmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegState {
    /// The one search of this run has not finished.
    Checking,
    /// There is an ffmpeg that can store recordings as video.
    Found,
    /// There is none, or the one there cannot (no libx264).
    NotFound,
}

/// How the search for ffmpeg stands. Starts nothing; safe on the UI thread.
pub fn ffmpeg_state() -> FfmpegState {
    state_of(ffmpeg::found())
}

fn state_of(found: Found) -> FfmpegState {
    match found {
        Found::NotYet => FfmpegState::Checking,
        Found::Ok => FfmpegState::Found,
        Found::None | Found::Unusable => FfmpegState::NotFound,
    }
}

/// How the conversion of one project ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The recording is kept as a video now: the size of its frames, and of the video.
    Swapped { before: u64, after: u64 },
    /// The frames are kept and the project file says why ([`MARK_FAILED`] or
    /// [`MARK_NOT_SMALLER`]): it is not tried again by this codec and version.
    Marked(&'static str),
    /// Nothing was changed or noted, and this run does not try the project again: the codec
    /// itself failed (three such failures in a row end the queue for this run), the recording
    /// could not be read, or the project could not be rewritten.
    GaveUp,
    /// The work was told to stop. Nothing was changed or noted.
    Cancelled,
    /// Nothing was changed or noted, and the project is tried again later: no room (the disk
    /// was full, or the codec failed while the disk had less room left than the frames take),
    /// a time bound, a file in use, or a project that changed meanwhile.
    Later,
}

/// What the queue asks before it starts on a project and before every frame, and what wakes it.
pub struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
    start_after: Duration,
    after_save: Duration,
    after_cancel: Duration,
    min_bytes: u64,
}

struct GateState {
    enabled: bool,
    recording: bool,
    quitting: bool,
    /// Something happened that may have left work: a save, the end of a recording session, the
    /// switch turned on.
    nudged: bool,
    /// A conversion is under way.
    converting: bool,
    /// The queue waits: for the switch, for a session to end, or for a nudge.
    parked: bool,
    /// How often the queue looked for work.
    #[cfg(test)]
    looks: u32,
}

impl Gate {
    /// A gate with the switch off, and the four numbers the app's own gate takes from
    /// [`START_AFTER`], [`AFTER_SAVE`], [`AFTER_CANCEL`] and [`MIN_BYTES`].
    pub const fn new(start_after: Duration, after_save: Duration, after_cancel: Duration, min_bytes: u64) -> Gate {
        Gate { state: Mutex::new(GateState { enabled: false, recording: false, quitting: false, nudged: false, converting: false, parked: false, #[cfg(test)] looks: 0 }), changed: Condvar::new(), start_after, after_save, after_cancel, min_bytes }
    }

    fn state(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn change(&self, change: impl FnOnce(&mut GateState)) {
        change(&mut self.state());
        self.changed.notify_all();
    }

    /// The switch "store kept recordings as video". Turned on, the queue looks for work.
    pub fn set_enabled(&self, on: bool) {
        self.change(|s| {
            s.nudged |= on && !s.enabled;
            s.enabled = on;
        });
    }

    /// A recording session started or ended. While one runs nothing is converted, and a
    /// conversion that is under way stops at its next frame; when it ends, the queue looks for
    /// work again.
    pub fn set_recording(&self, active: bool) {
        self.change(|s| {
            s.nudged |= s.recording && !active;
            s.recording = active;
        });
    }

    /// A recording was saved: the queue looks for work again.
    pub fn nudge(&self) {
        self.change(|s| s.nudged = true);
    }

    /// The app is closing: the queue ends for good, a conversion that is under way stops at
    /// its next frame, and this waits up to `wait` for it to have let go of everything (its
    /// ffmpeg ended, its folder removed).
    pub fn shutdown(&self, wait: Duration) {
        let mut s = self.state();
        s.quitting = true;
        self.changed.notify_all();
        let until = Instant::now() + wait;
        while s.converting {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            s = self.changed.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Whether a conversion may run, or run on.
    fn allowed(&self) -> bool {
        self.state().may_run()
    }

    /// Wait while `waiting` says so, and say whether the app is still running.
    fn park(&self, waiting: impl Fn(&GateState) -> bool) -> bool {
        let mut s = self.state();
        s.parked = true;
        while !s.quitting && waiting(&s) {
            s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        s.parked = false;
        !s.quitting
    }

    /// Let `pause` go by; false when the app is closing.
    fn rest(&self, pause: Duration) -> bool {
        let until = Instant::now() + pause;
        let mut s = self.state();
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if s.quitting || left.is_zero() {
                return !s.quitting;
            }
            s = self.changed.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Wait until a conversion may run: `Some(true)` when that took a wait, None when the app
    /// is closing.
    fn wait_allowed(&self) -> Option<bool> {
        let waited = !self.allowed();
        self.park(|s| !s.may_run()).then_some(waited)
    }

    /// Wait for the next nudge, for `at_most` if that is given: `Some(true)` when there was
    /// one, `Some(false)` when the time went by without, None when the app is closing.
    fn wait_nudge(&self, at_most: Option<Duration>) -> Option<bool> {
        let until = at_most.map(|time| Instant::now() + time);
        let mut s = self.state();
        s.parked = true;
        while !s.quitting && !s.nudged {
            s = match until.map(|until| until.saturating_duration_since(Instant::now())) {
                None => self.changed.wait(s).unwrap_or_else(|e| e.into_inner()),
                Some(left) if left.is_zero() => break,
                Some(left) => self.changed.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0,
            };
        }
        s.parked = false;
        (!s.quitting).then_some(s.nudged)
    }

    /// The queue is about to look for work: what happens from now on is a new nudge.
    fn looking(&self) {
        let mut s = self.state();
        s.nudged = false;
        #[cfg(test)]
        {
            s.looks += 1;
        }
    }

    /// Notes that a conversion is under way until the answer is dropped; None, and nothing
    /// noted, when none may run. Asked and noted in one step, so that [`Gate::shutdown`]
    /// either sees the conversion and waits for it, or the conversion never starts.
    fn converting(&self) -> Option<Busy<'_>> {
        let mut s = self.state();
        if !s.may_run() {
            return None;
        }
        s.converting = true;
        Some(Busy(self))
    }

    #[cfg(test)]
    fn parked(&self) -> bool {
        self.state().parked
    }

    #[cfg(test)]
    fn looks(&self) -> u32 {
        self.state().looks
    }
}

impl GateState {
    fn may_run(&self) -> bool {
        self.enabled && !self.recording && !self.quitting
    }
}

/// A conversion under way, as [`Gate::shutdown`] waits for it.
struct Busy<'a>(&'a Gate);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.change(|s| s.converting = false);
    }
}

static GATE: Gate = Gate::new(START_AFTER, AFTER_SAVE, AFTER_CANCEL, MIN_BYTES);

/// The gate of the app's own queue.
pub fn gate() -> &'static Gate {
    &GATE
}

/// [`Gate::set_enabled`] for the app's own queue: at start-up and whenever the settings change.
pub fn set_enabled(on: bool) {
    GATE.set_enabled(on);
}

/// [`Gate::set_recording`] for the app's own queue.
pub fn set_recording(active: bool) {
    GATE.set_recording(active);
}

/// [`Gate::nudge`] for the app's own queue: after every recording that was kept.
pub fn nudge() {
    #[cfg(test)]
    tests::NUDGES.with(|count| count.set(count.get() + 1));
    GATE.nudge();
}

/// How often this thread called [`nudge`].
#[cfg(test)]
pub fn nudges_here() -> u32 {
    tests::NUDGES.with(|count| count.get())
}

/// [`Gate::shutdown`] for the app's own queue: when the app closes.
pub fn shutdown(wait: Duration) {
    GATE.shutdown(wait);
}

/// What the start-up housekeeping thread does once the folder of saved recordings is tidied
/// and ffmpeg was looked for (`found` is what the search answered): tell every store which
/// codec stores recordings as video now, or that none does, and enter the queue. Returns when
/// the queue does; without a folder for saved recordings, or an ffmpeg that can archive, at
/// once.
pub fn start(store: Option<&Store>, found: Option<&Ffmpeg>, gate: &Gate) {
    let codec = found.filter(|ffmpeg| ffmpeg.can_archive());
    set_current_codec(codec.map(|_| ffmpeg::CODEC_ID));
    if let Some(store) = store {
        run(store, codec.map(|ffmpeg| ffmpeg as &dyn VideoCodec), gate);
    }
}

/// The queue: entered by the start-up housekeeping thread once the folder of saved recordings
/// is tidied, and left only when the app closes, when there is no codec, or when the codec
/// failed [`CODEC_FAILURES`] times in a row.
///
/// One project at a time, the one saved longest ago first. The first look is `start_after`
/// into the run; after that the queue rests until it is nudged (a save, the end of a recording
/// session, the switch turned on) and looks `after_save` later. A conversion that has to wait
/// (no room, a time bound) or gave way to an editor or a save is tried again after the next
/// nudge, and the queue goes on with the next oldest meanwhile; one the gate stopped ends the
/// pass. A project that gave way, and one that was open in an editor when the queue looked, is
/// looked for again every `after_cancel` as well, until its conversion has ended in some other
/// way or it is no longer there to be stored: an editor closed without saving nudges nobody.
/// A conversion that panics gives its project up for this run, and the queue lives on.
///
/// A project this run gave up on (see [`Outcome::GaveUp`]) is not tried again in this run, and
/// the queue goes on with the next oldest. A failure of the codec itself gives its project up
/// too; [`CODEC_FAILURES`] of them in a row, on whatever projects, end the queue.
pub fn run(store: &Store, codec: Option<&dyn VideoCodec>, gate: &Gate) {
    run_with(store, codec, gate, &free_bytes);
}

/// [`run`] with the read of the room left on the disk handed in.
fn run_with(store: &Store, codec: Option<&dyn VideoCodec>, gate: &Gate, free: Free<'_>) {
    let Some(codec) = codec else { return };
    let mut failures = 0;
    // By the time of saving too: a project that is saved again is another recording.
    let mut given_up: HashSet<(ProjectKey, i64)> = HashSet::new();
    // Not tried again before the queue has rested: the ones that have to wait, and the ones
    // that gave way to an editor or a save.
    let mut not_now: HashSet<(ProjectKey, i64)> = HashSet::new();
    // The projects whose conversion gave way and has not ended in another way since, and the
    // ones a look found open in an editor: while there is one, the queue looks again without
    // a nudge.
    let mut gave_way: HashSet<ProjectKey> = HashSet::new();
    let mut pause = gate.start_after;
    loop {
        if !gate.rest(pause) {
            return;
        }
        not_now.clear();
        loop {
            match gate.wait_allowed() {
                None => return,
                // The switch was just turned on, or a recording session just ended.
                Some(true) if !gate.rest(gate.after_save) => return,
                Some(true) => continue,
                Some(false) => {}
            }
            gate.looking();
            let skip = |key: &ProjectKey, saved_at: i64| {
                let project = (key.clone(), saved_at);
                given_up.contains(&project) || not_now.contains(&project)
            };
            let look = store.look(codec.id(), gate.min_bytes, &skip);
            let Some(candidate) = look.next else {
                // Nothing is left for this pass. What the queue goes on looking for without a
                // nudge: a project that gave way in this pass, and one an editor has open. One
                // that gave way earlier and is neither is no longer there to be stored (it was
                // removed, or stored or noted meanwhile), and is forgotten.
                gave_way.retain(|key| not_now.iter().any(|(waiting, _)| waiting == key));
                gave_way.extend(look.open);
                break;
            };
            let end = {
                // Not noted when the gate has closed meanwhile: a quit must not wait for a
                // conversion that will not start.
                let Some(_busy) = gate.converting() else { continue };
                let work = std::panic::AssertUnwindSafe(|| convert(store, codec, &candidate, &TOLERANCE, free, &|| gate.allowed()));
                // A panic has let go of the project and removed the conversion's folder on its way out.
                std::panic::catch_unwind(work).unwrap_or_else(|_| {
                    say("Saved recordings: storing a recording as video broke off because of an internal error; it is not tried again until Mythicshot starts again.".to_string());
                    End::NotThisRun
                })
            };
            let project = (candidate.key, candidate.saved_at);
            if end != End::Displaced {
                gave_way.remove(&project.0);
            }
            match end {
                End::Swapped { .. } | End::Marked(_) => failures = 0,
                End::CodecFailed => {
                    given_up.insert(project);
                    failures += 1;
                    if failures >= CODEC_FAILURES {
                        say(format!("Saved recordings: ffmpeg failed {failures} times in a row, so no more recordings are stored as video until Mythicshot starts again."));
                        return;
                    }
                }
                End::NotThisRun => {
                    given_up.insert(project);
                }
                // The gate closed: nothing else may run either.
                End::Cancelled => break,
                // The others need not wait for this one.
                End::Displaced => {
                    gave_way.insert(project.0.clone());
                    not_now.insert(project);
                }
                End::Later => {
                    not_now.insert(project);
                }
            }
        }
        pause = match gate.wait_nudge((!gave_way.is_empty()).then_some(gate.after_cancel)) {
            None => return,
            Some(true) => gate.after_save,
            // The rest after a conversion gave way is over: look at once.
            Some(false) => Duration::ZERO,
        };
    }
}

/// Store the recording of one candidate as video: archive it into the conversion's own folder
/// with every frame verified, then swap the video in, or note in the project file why the
/// frames are kept. `keep_going` is asked before every frame of both passes and once more
/// before the project is touched; after a no, or once the store told the conversion to stop
/// (the project was opened, saved over or removed), nothing is swapped and nothing is noted.
///
/// Whatever the outcome, the conversion's folder is gone and the project is let go of when
/// this returns, and one line is logged: fixed words with sizes and counts, never a path and
/// never the codec's own words.
// The queue itself asks `convert`, which also tells the two kinds of giving up apart.
#[cfg_attr(not(test), allow(dead_code))]
pub fn convert_one(store: &Store, codec: &dyn VideoCodec, candidate: &Candidate, keep_going: &dyn Fn() -> bool) -> Outcome {
    convert(store, codec, candidate, &TOLERANCE, &free_bytes, keep_going).outcome()
}

#[cfg(test)]
fn convert_with(store: &Store, codec: &dyn VideoCodec, candidate: &Candidate, tol: &Tolerance, keep_going: &dyn Fn() -> bool) -> Outcome {
    convert(store, codec, candidate, tol, &free_bytes, keep_going).outcome()
}

/// [`Outcome`], with the two kinds of [`Outcome::GaveUp`] told apart for the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Swapped { before: u64, after: u64 },
    Marked(&'static str),
    /// The codec itself failed: counted, and the project is not tried again in this run.
    CodecFailed,
    /// This project is not tried again in this run.
    NotThisRun,
    /// Told to stop by the caller: the switch, a recording session, the end of the app.
    Cancelled,
    /// Told to stop by the store: the project was opened, saved over or removed.
    Displaced,
    Later,
}

impl End {
    #[cfg_attr(not(test), allow(dead_code))]
    fn outcome(self) -> Outcome {
        match self {
            End::Swapped { before, after } => Outcome::Swapped { before, after },
            End::Marked(state) => Outcome::Marked(state),
            End::CodecFailed | End::NotThisRun => Outcome::GaveUp,
            End::Cancelled | End::Displaced => Outcome::Cancelled,
            End::Later => Outcome::Later,
        }
    }
}

fn convert(store: &Store, codec: &dyn VideoCodec, c: &Candidate, tol: &Tolerance, free: Free<'_>, keep_going: &dyn Fn() -> bool) -> End {
    let before = c.frames_bytes;
    let Some(conv) = store.begin_conversion(c) else {
        say("Saved recordings: a recording could not be started on now (it changed, is open, or another is being stored); it is tried again later.".to_string());
        return End::Later;
    };
    let stopped = || conv.cancelled() || !keep_going();
    // The recording, and with it everything that reads the project's frames, is dropped
    // before the conversion is: whoever waits for the conversion to let go takes that for the
    // frames file being closed.
    let (frames, archived) = match Recording::open_saved(&c.dir) {
        Ok(rec) => (rec.frames.len(), rec.archive_video(codec, &conv.video_path(), &[], tol, &|_: Stage, _, _| !stopped())),
        Err(_) => (0, Err(ArchiveError::Unusable("the recording could not be opened"))),
    };
    let size = format!("{frames} frames, {before} bytes of frames");
    let halted = || if conv.cancelled() { End::Displaced } else { End::Cancelled };
    let end = match archived {
        Err(ArchiveError::Cancelled | ArchiveError::Codec(CodecError::Cancelled)) => halted(),
        // Stopped: whatever the work came to is nobody's business any more.
        _ if stopped() => halted(),
        Ok(archived) => match store.swap_to_video(c, &conv, archived.bytes) {
            Ok(true) => {
                say(format!("Saved recordings: a recording is stored as video now ({size}, {} bytes of video, {} frames compared).", archived.bytes, archived.checked));
                End::Swapped { before, after: archived.bytes }
            }
            Ok(false) => {
                say(format!("Saved recordings: a recording changed while its video was made, so its frames are kept ({size})."));
                End::NotThisRun
            }
            Err(why) => {
                say(format!("Saved recordings: a recording keeps its frames ({size}): {why}"));
                End::NotThisRun
            }
        },
        Err(ArchiveError::Verify(mismatch)) => {
            let found = match mismatch {
                Mismatch::Count { got } => format!("it held {got} pictures"),
                Mismatch::Alpha { frame } => format!("picture {frame} was not opaque"),
                Mismatch::Frame { frame, db } => format!("picture {frame} measured {db:.1} dB"),
                Mismatch::Block { frame, db } => format!("a block of picture {frame} measured {db:.1} dB"),
                Mismatch::Shift { frame } => format!("picture {frame} had a colour cast"),
            };
            say(format!("Saved recordings: the video of a recording did not match its frames, so its frames are kept ({found}; {size})."));
            mark(store, codec, c, MARK_FAILED)
        }
        Err(ArchiveError::NotSmaller) => {
            say(format!("Saved recordings: a recording is not enough smaller as a video, so its frames are kept ({size})."));
            mark(store, codec, c, MARK_NOT_SMALLER)
        }
        Err(ArchiveError::Unusable(why)) => {
            say(format!("Saved recordings: a recording cannot be stored as video ({size}): {why}."));
            End::NotThisRun
        }
        Err(ArchiveError::Codec(e)) => {
            let (found, end) = match e {
                CodecError::Io(e) => (format!("a file or the program could not be used ({:?})", e.kind()), End::Later),
                CodecError::TimedOut { bound: Bound::Stall } => ("ffmpeg was stopped at its stall bound".to_string(), End::Later),
                CodecError::TimedOut { bound: Bound::Total } => ("ffmpeg was stopped at its total bound".to_string(), End::Later),
                CodecError::TooLarge => ("the video grew past its bound".to_string(), End::Later),
                CodecError::Failed { exit } => {
                    let how = exit.map_or("ffmpeg could not do the work and gave no exit code".to_string(), |code| format!("ffmpeg ended with exit code {code}"));
                    // The program ends like this when the disk fills, too. A video never needs
                    // more room than the frames it is made of: with less than that left, the
                    // disk is taken for the reason. When the room cannot be told, the program is.
                    if free(store.root()).is_some_and(|room| room < before) { (format!("{how}, and the disk has less room than the recording takes"), End::Later) } else { (how, End::CodecFailed) }
                }
                CodecError::WrongFrames { got } => (format!("ffmpeg handled {got} frames"), End::CodecFailed),
                // The text is the caller's and can hold anything: never logged.
                CodecError::Caller(_) => ("the frames could not be handed over".to_string(), End::CodecFailed),
                CodecError::Cancelled => ("it was stopped".to_string(), halted()),
            };
            let then = if end == End::Later { "it is tried again later" } else { "its frames are kept" };
            say(format!("Saved recordings: a recording was not stored as video ({found}; {size}); {then}."));
            end
        }
    };
    if matches!(end, End::Cancelled | End::Displaced) {
        say(format!("Saved recordings: storing a recording as video was stopped ({size}); it is tried again later."));
    }
    drop(conv);
    end
}

/// Note in the project file why the frames are kept. A project that changed meanwhile is not
/// noted and comes up again by itself; one whose file cannot be written is given up for this
/// run, or it would be encoded again and again.
fn mark(store: &Store, codec: &dyn VideoCodec, c: &Candidate, state: &'static str) -> End {
    match store.mark_conversion(c, state, codec.id()) {
        Ok(true) => End::Marked(state),
        Ok(false) => End::Later,
        Err(why) => {
            say(format!("Saved recordings: how storing a recording as video ended could not be noted: {why}"));
            End::NotThisRun
        }
    }
}

/// One line for the log.
fn say(line: String) {
    #[cfg(test)]
    tests::LINES.lock().unwrap_or_else(|e| e.into_inner()).push(line.clone());
    eprintln!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{ConvertState, Expand, Form, Loaded, ProjectInfo, Snapshot, cancel_conversion_of, converting_now, epoch, set_current_codec};
    use crate::video::Edit;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Every line the queue logged in this process.
    pub(super) static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    thread_local! {
        /// How often this thread nudged the app's own queue.
        pub(super) static NUDGES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    // ---- One test, one process ----
    //
    // The store has one conversion, one codec in use and one change counter per process, and the
    // store's own tests count on having them to themselves. Every test here that starts a
    // conversion therefore runs in a process of its own: the test binary run again for exactly
    // that test.

    const ALONE: &str = "MYTHICSHOT_TEST_ONE_CONVERSION";

    fn alone(name: &str, test: impl FnOnce()) {
        if std::env::var_os(ALONE).is_some() {
            return test();
        }
        let path = format!("{}::{name}", module_path!().split_once("::").map_or(module_path!(), |(_, rest)| rest));
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([path.as_str(), "--exact", "--nocapture", "--test-threads=1"]).env(ALONE, "1");
        let (status, stdout, stderr) = within(cmd, ALONE_AT_MOST).unwrap_or_else(|(stdout, stderr)| panic!("{path} on its own did not end within {ALONE_AT_MOST:?} and was stopped:\n{stdout}\n{stderr}"));
        // What a test that could not run said, wherever on its line the test runner put it.
        for said in stdout.lines().filter_map(|line| line.find("skipped:").map(|at| &line[at..])) {
            println!("{said}");
        }
        assert!(status.success() && stdout.contains("1 passed"), "{path} on its own:\n{stdout}\n{stderr}");
    }

    /// The longest one test may take in its own process before it is stopped and has failed.
    const ALONE_AT_MOST: Duration = Duration::from_secs(120);

    /// Run `cmd` to its end and answer how it ended and what it printed; when it has not
    /// ended within `at_most`, stop it and answer only what it printed until then.
    fn within(mut cmd: std::process::Command, at_most: Duration) -> Result<(std::process::ExitStatus, String, String), (String, String)> {
        use std::io::Read;
        use std::process::Stdio;
        let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        // Read on threads of their own, so that a child that prints much never waits for this one.
        let read = |mut from: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut text = Vec::new();
                let _ = from.read_to_end(&mut text);
                String::from_utf8_lossy(&text).to_string()
            })
        };
        let (out, err) = (read(Box::new(child.stdout.take().unwrap())), read(Box::new(child.stderr.take().unwrap())));
        let started = Instant::now();
        let status = loop {
            match child.try_wait().unwrap() {
                Some(status) => break Some(status),
                None if started.elapsed() >= at_most => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let (stdout, stderr) = (out.join().unwrap(), err.join().unwrap());
        status.map(|status| (status, stdout.clone(), stderr.clone())).ok_or((stdout, stderr))
    }

    #[test]
    fn a_test_on_its_own_that_hangs_is_stopped_and_has_failed() {
        // This test binary, asked for a test there is none of, ends at once and well.
        let mut quick = std::process::Command::new(std::env::current_exe().unwrap());
        quick.args(["no test has this name", "--exact"]);
        let (status, stdout, _) = within(quick, LONG).expect("it ended in time");
        assert!(status.success() && stdout.contains("0 passed"), "{stdout}");
        // And asked for the stand-in below, which never ends, is stopped.
        let mut hangs = std::process::Command::new(std::env::current_exe().unwrap());
        hangs.args([format!("{}::hang", module_path!().split_once("::").unwrap().1).as_str(), "--exact", "--ignored", "--nocapture"]).env(ALONE, "hang");
        let started = Instant::now();
        let (stdout, _) = within(hangs, Duration::from_millis(1500)).expect_err("it was stopped");
        assert!(started.elapsed() < LONG / 2 && stdout.contains("running 1 test"), "{stdout}");
    }

    /// The stand-in for a test that hangs. Does nothing unless the test above asks for it.
    #[test]
    #[ignore = "a stand-in another test runs"]
    fn hang() {
        if std::env::var_os(ALONE).is_some_and(|how| how == "hang") {
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }

    // ---- A store, and a codec that does what it is told ----

    const SITE: &str = "https://tours.example.com";
    const TOUR: &str = "tour_0123456789abcdef01234567";
    const CH_A: &str = "ch_0123456789abcdef0123456a";
    const CH_B: &str = "ch_0123456789abcdef0123456b";
    const CH_C: &str = "ch_0123456789abcdef0123456c";
    const FRAMES: usize = 12;
    const FAKE_ID: &str = "a-codec-of-the-queue-tests";
    const LONG: Duration = Duration::from_secs(20);

    fn key(chapter: &str) -> ProjectKey {
        ProjectKey { server: SITE.into(), tour_id: TOUR.into(), chapter_id: chapter.into() }
    }

    struct World {
        tmp: tempfile::TempDir,
        store: Store,
    }

    impl World {
        fn new() -> World {
            set_current_codec(Some(FAKE_ID));
            let tmp = tempfile::tempdir().unwrap();
            let store = Store::at(tmp.path().join("projects"));
            World { tmp, store }
        }

        /// Save a small real recording as the project of `chapter`.
        fn save(&self, chapter: &str) -> ProjectInfo {
            let dir = self.tmp.path().join(format!("recorded {chapter}"));
            std::fs::create_dir_all(&dir).unwrap();
            let times: Vec<u64> = (0..FRAMES as u64).map(|i| i * 100).collect();
            self.save_recording(chapter, &Recording::synthetic_in(&dir, 64, 48, &times, FRAMES as u64 * 100).unwrap())
        }

        fn save_recording(&self, chapter: &str, rec: &Recording) -> ProjectInfo {
            let edit = Edit::new(rec, Default::default());
            let saved = self.store.save(&key(chapter), &Snapshot { rec, title: "Chapter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] }).unwrap();
            // The time of saving is in milliseconds: the next save is a later one.
            std::thread::sleep(Duration::from_millis(3));
            saved.info
        }

        fn next(&self) -> Option<Candidate> {
            self.store.next_candidate(FAKE_ID, 0)
        }

        fn info(&self, chapter: &str) -> ProjectInfo {
            self.store.list().into_iter().find(|p| p.key == key(chapter)).expect("the project is listed")
        }

        fn converting_folders(&self) -> usize {
            std::fs::read_dir(self.store.root()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".converting-")).count()
        }
    }

    /// Every file of a folder with its bytes, sorted by name.
    fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    out.push((path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"), std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    fn names(dir: &Path) -> Vec<String> {
        files(dir).into_iter().map(|(name, _)| name).collect()
    }

    fn pictures(rec: &Recording) -> Vec<Vec<u8>> {
        (0..rec.frames.len()).map(|i| rec.frame(i).unwrap().into_raw()).collect()
    }

    fn until(what: &str, done: impl Fn() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < LONG, "waited too long for: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[derive(Default, Clone, Copy)]
    enum How {
        /// Every picture comes back as it went in.
        #[default]
        Faithful,
        /// Every picture comes back with more green.
        Shifted,
        /// The video is larger than it may be.
        Grown,
        /// Encoding ends with this error.
        Fails(fn() -> CodecError),
        /// Encoding ends with the error for the encodes (counted from 0) the first answers true for.
        FailsWhen(fn(usize) -> bool, fn() -> CodecError),
    }

    /// Keeps the pictures it is given in memory, by the bytes of the small file it writes as
    /// the video, and hands them back for that file.
    #[derive(Default)]
    struct Fake<'a> {
        kept: Mutex<HashMap<Vec<u8>, Vec<Vec<u8>>>>,
        encodes: AtomicUsize,
        /// The project each encode was for, as the store names the one being converted.
        tried: Mutex<Vec<std::path::PathBuf>>,
        /// The frames gone through so far, encoding and decoding counted together.
        done: AtomicUsize,
        how: How,
        /// Called before every frame of an encode, with the frame's number.
        during: Option<Box<dyn Fn(usize) + Sync + 'a>>,
        /// Called when the one who asked for a decode refused a picture.
        refused: Option<Box<dyn Fn() + Sync + 'a>>,
    }

    impl<'a> Fake<'a> {
        fn that(how: How) -> Fake<'a> {
            Fake { how, ..Fake::default() }
        }

        fn during(hook: impl Fn(usize) + Sync + 'a) -> Fake<'a> {
            Fake { during: Some(Box::new(hook)), ..Fake::default() }
        }

        fn encodes(&self) -> usize {
            self.encodes.load(Ordering::SeqCst)
        }

        fn done(&self) -> usize {
            self.done.load(Ordering::SeqCst)
        }
    }

    impl VideoCodec for Fake<'_> {
        fn id(&self) -> &str {
            FAKE_ID
        }

        fn encode(&self, _: (u32, u32), frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, max_bytes: u64, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            let nth = self.encodes.fetch_add(1, Ordering::SeqCst);
            let _ = std::fs::remove_file(out);
            self.tried.lock().unwrap().extend(converting_now());
            match self.how {
                How::Fails(why) => return Err(why()),
                How::FailsWhen(when, why) if when(nth) => return Err(why()),
                _ => {}
            }
            let (mut pictures, mut buf) = (Vec::new(), Vec::new());
            for i in 0..frames {
                if !keep_going(i) {
                    return Err(CodecError::Cancelled);
                }
                if let Some(during) = &self.during {
                    during(i);
                }
                match next(&mut buf) {
                    Ok(true) => pictures.push(buf.clone()),
                    Ok(false) => break,
                    Err(why) => return Err(CodecError::Caller(why)),
                }
                self.done.fetch_add(1, Ordering::SeqCst);
            }
            let bytes = match self.how {
                How::Grown => vec![7u8; max_bytes as usize + 1],
                // An index refuses a video of less than a byte per picture.
                _ => format!("video {nth:058}").into_bytes(),
            };
            std::fs::write(out, &bytes).map_err(CodecError::Io)?;
            self.kept.lock().unwrap().insert(bytes, pictures);
            Ok(())
        }

        fn decode(&self, video: &Path, _: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            let bytes = std::fs::read(video).map_err(CodecError::Io)?;
            let pictures = self.kept.lock().unwrap().get(&bytes).cloned().ok_or(CodecError::Failed { exit: None })?;
            let got = pictures.len();
            for (i, mut picture) in pictures.into_iter().enumerate() {
                if !keep_going(i) {
                    return Err(CodecError::Cancelled);
                }
                if matches!(self.how, How::Shifted) {
                    picture.chunks_exact_mut(4).for_each(|px| px[1] = px[1].saturating_add(60));
                }
                if let Err(why) = each(i, &picture) {
                    if let Some(refused) = &self.refused {
                        refused();
                    }
                    return Err(CodecError::Caller(why));
                }
                self.done.fetch_add(1, Ordering::SeqCst);
            }
            if got == frames { Ok(()) } else { Err(CodecError::WrongFrames { got }) }
        }
    }

    fn go() -> bool {
        true
    }

    fn open(w: &World, chapter: &str, codec: &dyn VideoCodec, temp: &str) -> Loaded {
        match w.store.load_with(&key(chapter), &Expand { codec: Some(codec), temp: &w.tmp.path().join(temp), keep_going: &|_, _| true }) {
            Ok(loaded) => loaded,
            Err(e) => panic!("the project did not open: {e:?}"),
        }
    }

    /// The queue on a thread of its own for as long as `test` runs; then it is told to quit.
    fn queue(w: &World, codec: &dyn VideoCodec, gate: &Gate, test: impl FnOnce()) {
        queue_with(w, codec, gate, &free_bytes, test);
    }

    fn queue_with(w: &World, codec: &dyn VideoCodec, gate: &Gate, free: Free<'_>, test: impl FnOnce()) {
        struct Quit<'a>(&'a Gate);
        impl Drop for Quit<'_> {
            fn drop(&mut self) {
                self.0.shutdown(Duration::ZERO);
            }
        }
        std::thread::scope(|scope| {
            let _quit = Quit(gate);
            scope.spawn(|| run_with(&w.store, Some(codec), gate, free));
            test();
        });
    }

    /// A gate that waits for nothing, takes every size, and has the switch on. After a
    /// conversion gave way to an editor it waits for a nudge, as good as for ever.
    fn open_gate() -> Gate {
        open_gate_looking_again_after(NEVER)
    }

    fn open_gate_looking_again_after(after_cancel: Duration) -> Gate {
        let gate = Gate::new(Duration::ZERO, Duration::ZERO, after_cancel, 0);
        gate.set_enabled(true);
        gate
    }

    const NEVER: Duration = Duration::from_secs(24 * 60 * 60);

    // ---- One conversion ----

    #[test]
    fn a_saved_project_is_converted_swapped_and_listed_as_video() {
        alone("a_saved_project_is_converted_swapped_and_listed_as_video", || {
            let w = World::new();
            let saved = w.save(CH_A);
            assert_eq!((saved.form, saved.convert), (Form::Frames, ConvertState::NotTried));
            let before = pictures(&Recording::open_saved(&saved.dir).unwrap());
            let c = w.next().unwrap();
            let fake = Fake::default();

            let outcome = convert_one(&w.store, &fake, &c, &go);
            let video = std::fs::metadata(saved.dir.join("frames.mkv")).unwrap().len();
            assert_eq!((outcome, video), (Outcome::Swapped { before: c.frames_bytes, after: 64 }, 64));
            assert!(c.frames_bytes > video);

            let info = w.info(CH_A);
            assert_eq!((info.form, info.convert, info.problem), (Form::Video, ConvertState::Done, None));
            assert_eq!(names(&saved.dir), ["frames.mkv", "project.json", "recording.json"]);
            assert_eq!((w.converting_folders(), converting_now(), w.next()), (0, None, None));
            // It opens again, through the codec, with every frame it had.
            let loaded = open(&w, CH_A, &fake, "edit");
            assert_eq!(pictures(&loaded.rec), before);
        });
    }

    #[test]
    fn a_project_whose_video_fails_verification_keeps_its_frames_and_is_not_tried_again() {
        alone("a_project_whose_video_fails_verification_keeps_its_frames_and_is_not_tried_again", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let fake = Fake::that(How::Shifted);
            assert_eq!(convert_one(&w.store, &fake, &w.next().unwrap(), &go), Outcome::Marked(MARK_FAILED));
            let info = w.info(CH_A);
            assert_eq!((info.form, info.convert, info.problem), (Form::Frames, ConvertState::Failed, None));
            // Only the project file changed, and the project still opens as it was saved.
            let now = files(&saved.dir);
            assert_eq!(names(&saved.dir), ["frames.zst", "project.json", "recording.json"]);
            assert!(now.iter().zip(&whole).all(|(a, b)| a.0 == "project.json" || a == b));
            assert_eq!((w.next(), w.converting_folders(), fake.encodes()), (None, 0, 1));
            assert!(w.store.load(&key(CH_A)).is_ok());
        });
    }

    #[test]
    fn a_project_whose_video_is_not_smaller_keeps_its_frames_and_is_not_tried_again() {
        alone("a_project_whose_video_is_not_smaller_keeps_its_frames_and_is_not_tried_again", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let fake = Fake::that(How::Grown);
            assert_eq!(convert_one(&w.store, &fake, &w.next().unwrap(), &go), Outcome::Marked(MARK_NOT_SMALLER));
            let info = w.info(CH_A);
            assert_eq!((info.form, info.convert, info.problem), (Form::Frames, ConvertState::NotSmaller, None));
            assert_eq!(names(&saved.dir), ["frames.zst", "project.json", "recording.json"]);
            assert_eq!((w.next(), w.converting_folders()), (None, 0));
        });
    }

    #[test]
    fn a_cancelled_conversion_leaves_the_project_and_no_converting_folder_and_is_tried_again() {
        alone("a_cancelled_conversion_leaves_the_project_and_no_converting_folder_and_is_tried_again", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let c = w.next().unwrap();
            // Told to stop while the frames are fed, while the video is read back, and at the very end.
            for stop_at in [3, FRAMES + 3, 2 * FRAMES] {
                let fake = Fake::default();
                let outcome = convert_one(&w.store, &fake, &c, &|| fake.done() < stop_at);
                assert_eq!(outcome, Outcome::Cancelled, "stopped at {stop_at}");
                assert_eq!(fake.done(), stop_at, "no frame goes through after the no");
                assert_eq!(files(&saved.dir), whole);
                assert_eq!((w.converting_folders(), converting_now(), w.next()), (0, None, Some(c.clone())));
            }
            // A video that is refused is not noted either, once the work was told to stop.
            let stopped = AtomicBool::new(false);
            let fake = Fake { how: How::Shifted, refused: Some(Box::new(|| stopped.store(true, Ordering::SeqCst))), ..Fake::default() };
            let outcome = convert_one(&w.store, &fake, &c, &|| !stopped.load(Ordering::SeqCst));
            assert_eq!((outcome, stopped.load(Ordering::SeqCst), fake.done()), (Outcome::Cancelled, true, FRAMES));
            assert_eq!((files(&saved.dir), w.next()), (whole, Some(c.clone())));
            // And then it is converted.
            assert!(matches!(convert_one(&w.store, &Fake::default(), &c, &go), Outcome::Swapped { .. }));
        });
    }

    fn no_room() -> CodecError {
        CodecError::Io(std::io::Error::from_raw_os_error(if cfg!(windows) { 112 } else { 28 }))
    }

    fn stalled() -> CodecError {
        CodecError::TimedOut { bound: Bound::Stall }
    }

    fn exit_1() -> CodecError {
        CodecError::Failed { exit: Some(1) }
    }

    #[test]
    fn a_conversion_that_runs_out_of_room_leaves_no_mark() {
        alone("a_conversion_that_runs_out_of_room_leaves_no_mark", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let c = w.next().unwrap();
            for why in [no_room as fn() -> CodecError, stalled] {
                assert_eq!(convert_one(&w.store, &Fake::that(How::Fails(why)), &c, &go), Outcome::Later);
                assert_eq!(files(&saved.dir), whole);
                assert_eq!((w.info(CH_A).convert, w.converting_folders(), w.next()), (ConvertState::NotTried, 0, Some(c.clone())));
            }
        });
    }

    #[test]
    fn a_codec_that_fails_when_the_disk_has_no_room_for_the_recording_is_tried_again_later() {
        alone("a_codec_that_fails_when_the_disk_has_no_room_for_the_recording_is_tried_again_later", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let c = w.next().unwrap();
            let asked = Mutex::new(Vec::new());
            let ended = |why: fn() -> CodecError, room: Option<u64>| {
                let free = |folder: &Path| {
                    asked.lock().unwrap().push(folder.to_path_buf());
                    room
                };
                convert(&w.store, &Fake::that(How::Fails(why)), &c, &TOLERANCE, &free, &go).outcome()
            };
            // Less room than the frames take: the program most likely failed for that. A video
            // never needs more than the frames, so with that much room it failed by itself.
            for why in [exit_1 as fn() -> CodecError, no_exit] {
                assert_eq!(ended(why, Some(c.frames_bytes - 1)), Outcome::Later);
                assert_eq!(ended(why, Some(0)), Outcome::Later);
                assert_eq!(ended(why, Some(c.frames_bytes)), Outcome::GaveUp);
                // The room cannot be told: as a failure of the program.
                assert_eq!(ended(why, None), Outcome::GaveUp);
            }
            // It is the room of the projects folder that is asked for, and only after such a failure.
            assert!(asked.lock().unwrap().iter().all(|folder| folder == w.store.root()) && asked.lock().unwrap().len() == 8);
            assert_eq!(ended(caller, Some(0)), Outcome::GaveUp);
            assert_eq!(ended(stalled, Some(0)), Outcome::Later);
            assert_eq!(asked.lock().unwrap().len(), 8);
            assert_eq!((files(&saved.dir) == whole, w.info(CH_A).convert, w.converting_folders()), (true, ConvertState::NotTried, 0));
            let lines = lines_without_names(&w);
            assert!(lines.iter().any(|line| line.contains("exit code 1") && line.contains("room") && line.ends_with("tried again later.")), "{lines:#?}");

            // The real read: this disk has room for a few kilobytes, so the failure is the program's.
            if cfg!(windows) {
                let room = free_bytes(w.store.root()).expect("the room on the disk of the temporary folder can be read");
                assert!(room > c.frames_bytes, "{room}");
                assert_eq!(free_bytes(&w.store.root().join("no such folder")), None);
            }
            assert_eq!(convert_one(&w.store, &Fake::that(How::Fails(exit_1)), &c, &go), Outcome::GaveUp);

            // In the queue: neither counted nor given up. Every project is tried once in a
            // pass, the oldest does not hold the others back, and a nudge tries all three again.
            let w = World::new();
            let dirs = [CH_A, CH_B, CH_C].map(|chapter| w.save(chapter).dir);
            let (fake, gate) = (Fake::that(How::Fails(exit_1)), open_gate());
            queue_with(&w, &fake, &gate, &|_| Some(0), || {
                until("the queue to have tried all three", || fake.encodes() == 3 && gate.parked());
                gate.nudge();
                until("the queue to have tried all three again", || fake.encodes() == 6 && gate.parked());
                assert_eq!(*fake.tried.lock().unwrap(), [&dirs[..], &dirs[..]].concat());
            });
        });
    }

    // ---- The queue ----

    #[test]
    fn the_switch_turned_off_stops_a_conversion_and_turned_on_again_starts_it_again() {
        alone("the_switch_turned_off_stops_a_conversion_and_turned_on_again_starts_it_again", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let gate = open_gate();
            let met = AtomicBool::new(false);
            let fake = Fake::during(|i| {
                if i == 2 && !met.swap(true, Ordering::SeqCst) {
                    gate.set_enabled(false);
                }
            });
            queue(&w, &fake, &gate, || {
                until("the conversion to be stopped by the switch", || fake.encodes() == 1 && gate.parked());
                assert_eq!((files(&saved.dir) == whole, w.converting_folders(), converting_now(), w.info(CH_A).convert), (true, 0, None, ConvertState::NotTried));
                // Nothing but the switch wakes the queue here: no save, no session.
                gate.set_enabled(true);
                until("the recording to be stored as video", || w.info(CH_A).form == Form::Video);
                assert_eq!(fake.encodes(), 2);
            });
        });
    }

    #[test]
    fn a_save_over_a_project_whose_frames_the_queue_is_reading_goes_through() {
        alone("a_save_over_a_project_whose_frames_the_queue_is_reading_goes_through", || {
            let w = World::new();
            let saved = w.save(CH_A);
            // The conversion stands at its third frame, its frames file open, until the save is on its way.
            let (reached, saving, once) = (AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false));
            let fake = Fake::during(|i| {
                if i == 2 && !once.swap(true, Ordering::SeqCst) {
                    reached.store(true, Ordering::SeqCst);
                    until("the save", || saving.load(Ordering::SeqCst));
                    std::thread::sleep(Duration::from_millis(150));
                }
            });
            let gate = open_gate();
            queue(&w, &fake, &gate, || {
                until("the conversion to be under way", || reached.load(Ordering::SeqCst));
                assert_eq!(converting_now(), Some(saved.dir.clone()));
                // Another recording for the same chapter, saved from this thread: the whole project is replaced.
                let dir = w.tmp.path().join("recorded again");
                std::fs::create_dir_all(&dir).unwrap();
                let rec = Recording::synthetic_in(&dir, 64, 48, &[0, 100, 200, 300, 400], 500).unwrap();
                saving.store(true, Ordering::SeqCst);
                let again = w.save_recording(CH_A, &rec);
                assert_eq!((again.form, again.problem, again.dir.clone()), (Form::Frames, None, saved.dir.clone()));
                assert_eq!(Recording::open_saved(&saved.dir).unwrap().frames.len(), 5, "the project is the one just saved");
                until("the conversion to have let go", || converting_now().is_none());
                // What a publish does after it kept its recording: the new one is converted.
                gate.nudge();
                until("the new recording to be stored as video", || w.info(CH_A).form == Form::Video);
                let loaded = open(&w, CH_A, &fake, "edit");
                assert_eq!(pictures(&loaded.rec), pictures(&rec));
            });
            assert_eq!((fake.encodes(), w.converting_folders(), w.store.list().len()), (2, 0, 1));
        });
    }

    #[test]
    fn a_conversion_that_breaks_off_gives_its_project_up_and_the_queue_lives_on() {
        alone("a_conversion_that_breaks_off_gives_its_project_up_and_the_queue_lives_on", || {
            let w = World::new();
            let (old, young) = (w.save(CH_B), w.save(CH_A));
            let whole = files(&old.dir);
            let fake = Fake::during(|i| {
                if i == 1 && converting_now().as_ref() == Some(&old.dir) {
                    panic!("the codec of the tests broke off, as this test asks it to");
                }
            });
            let gate = open_gate();
            queue(&w, &fake, &gate, || {
                until("the younger recording to be stored as video", || w.info(CH_A).form == Form::Video && gate.parked());
                gate.nudge();
                std::thread::sleep(Duration::from_millis(30));
                until("the queue to rest", || gate.parked());
                assert_eq!(*fake.tried.lock().unwrap(), [old.dir.clone(), young.dir.clone()]);
                // A quit does not wait for a conversion that is long over.
                let started = Instant::now();
                gate.shutdown(LONG);
                assert!(started.elapsed() < LONG / 2);
            });
            assert_eq!((files(&old.dir) == whole, w.info(CH_B).convert, w.converting_folders(), converting_now()), (true, ConvertState::NotTried, 0, None));
            let lines = lines_without_names(&w);
            assert!(lines.iter().any(|line| line.contains("internal error")), "{lines:#?}");
        });
    }

    #[test]
    fn a_younger_project_is_not_held_back_by_an_older_one_that_has_to_wait() {
        alone("a_younger_project_is_not_held_back_by_an_older_one_that_has_to_wait", || {
            let w = World::new();
            let (old, young) = (w.save(CH_B), w.save(CH_A));
            // The first encode of every pass meets a full disk; the others work.
            let fake = Fake::that(How::FailsWhen(|nth| nth == 0, no_room));
            let gate = open_gate();
            queue(&w, &fake, &gate, || {
                until("the younger recording to be stored as video", || w.info(CH_A).form == Form::Video && gate.parked());
                assert_eq!((w.info(CH_B).form, w.info(CH_B).convert), (Form::Frames, ConvertState::NotTried));
                assert_eq!(*fake.tried.lock().unwrap(), [old.dir.clone(), young.dir.clone()]);
                // The next nudge tries the older one again.
                gate.nudge();
                until("the older recording to be stored as video", || w.info(CH_B).form == Form::Video);
            });
            assert_eq!(fake.encodes(), 3);
        });
    }

    #[test]
    fn while_an_editor_has_one_project_the_others_are_converted() {
        alone("while_an_editor_has_one_project_the_others_are_converted", || {
            let w = World::new();
            let (old, young) = (w.save(CH_B), w.save(CH_A));
            let (opened, once): (Mutex<Option<Loaded>>, AtomicBool) = (Mutex::new(None), AtomicBool::new(false));
            let fake = Fake::during(|i| {
                if i == 1 && !once.swap(true, Ordering::SeqCst) {
                    *opened.lock().unwrap() = Some(w.store.load(&key(CH_B)).unwrap_or_else(|e| panic!("{e:?}")));
                }
            });
            let gate = open_gate();
            queue(&w, &fake, &gate, || {
                until("the younger recording to be stored as video", || w.info(CH_A).form == Form::Video && gate.parked());
                assert_eq!(*fake.tried.lock().unwrap(), [old.dir.clone(), young.dir.clone()]);
                assert_eq!(w.info(CH_B).form, Form::Frames);
            });
        });
    }

    #[test]
    fn with_the_switch_off_or_without_a_codec_nothing_is_converted() {
        alone("with_the_switch_off_or_without_a_codec_nothing_is_converted", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(w.store.root());
            // Without a codec the queue ends at once, whatever the switch says.
            run(&w.store, None, &open_gate());
            assert_eq!(files(w.store.root()), whole);

            // With the switch off it waits, and touches nothing.
            let gate = Gate::new(Duration::ZERO, Duration::ZERO, NEVER, 0);
            let fake = Fake::default();
            queue(&w, &fake, &gate, || {
                until("the queue to wait for the switch", || gate.parked());
                gate.nudge();
                std::thread::sleep(Duration::from_millis(50));
                assert_eq!((fake.encodes(), files(w.store.root()) == whole, gate.parked()), (0, true, true));
                // Turned on, it converts; turned off again, the video stays a video.
                gate.set_enabled(true);
                until("the recording to be stored as video", || w.info(CH_A).form == Form::Video);
                gate.set_enabled(false);
                until("the queue to wait again", || gate.parked());
                assert_eq!((fake.encodes(), names(&saved.dir)), (1, vec!["frames.mkv".to_string(), "project.json".into(), "recording.json".into()]));
            });
        });
    }

    #[test]
    fn a_recording_smaller_than_the_least_size_is_left_as_it_is() {
        alone("a_recording_smaller_than_the_least_size_is_left_as_it_is", || {
            let w = World::new();
            w.save(CH_A);
            let whole = files(w.store.root());
            let gate = Gate::new(Duration::ZERO, Duration::ZERO, NEVER, MIN_BYTES);
            gate.set_enabled(true);
            let fake = Fake::default();
            queue(&w, &fake, &gate, || {
                until("the queue to find nothing", || gate.parked());
                assert_eq!((fake.encodes(), files(w.store.root()) == whole), (0, true));
            });
        });
    }

    #[test]
    fn nothing_is_converted_while_a_recording_session_runs_and_a_running_conversion_stops() {
        alone("nothing_is_converted_while_a_recording_session_runs_and_a_running_conversion_stops", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let gate = open_gate();
            gate.set_recording(true);
            // The first conversion is met by a recording session at its third frame.
            let met = AtomicBool::new(false);
            let fake = Fake::during(|i| {
                if i == 2 && !met.swap(true, Ordering::SeqCst) {
                    gate.set_recording(true);
                }
            });
            queue(&w, &fake, &gate, || {
                until("the queue to wait for the session to end", || gate.parked());
                std::thread::sleep(Duration::from_millis(50));
                assert_eq!((fake.encodes(), files(&saved.dir) == whole), (0, true));

                gate.set_recording(false);
                until("the conversion to be stopped by the next session", || fake.encodes() == 1 && gate.parked());
                assert_eq!((files(&saved.dir) == whole, w.converting_folders(), converting_now()), (true, 0, None));
                assert_eq!(w.info(CH_A).convert, ConvertState::NotTried);
                std::thread::sleep(Duration::from_millis(50));
                assert_eq!(fake.encodes(), 1, "nothing starts while the session runs");

                // The session ends: the project is queued again by that alone.
                gate.set_recording(false);
                until("the recording to be stored as video", || w.info(CH_A).form == Form::Video);
                assert_eq!(fake.encodes(), 2);
            });
        });
    }

    #[test]
    fn one_conversion_runs_at_a_time_oldest_first() {
        alone("one_conversion_runs_at_a_time_oldest_first", || {
            let w = World::new();
            // Saved in an order that is not the order of their folders' names.
            let order = [CH_C, CH_A, CH_B].map(|chapter| w.save(chapter).dir);
            let seen = Mutex::new(Vec::new());
            let fake = Fake::during(|i| {
                let now = converting_now().expect("a conversion is registered while it runs");
                if i == 0 {
                    seen.lock().unwrap().push(now);
                } else {
                    assert_eq!(seen.lock().unwrap().last(), Some(&now), "another project while this one is converted");
                }
            });
            let gate = open_gate();
            queue(&w, &fake, &gate, || {
                until("every recording to be stored as video", || w.store.list().iter().all(|p| p.form == Form::Video));
                until("the queue to rest", || gate.parked());
            });
            assert_eq!(*seen.lock().unwrap(), order);
            assert_eq!((fake.encodes(), w.converting_folders(), converting_now()), (3, 0, None));
        });
    }

    // ---- The editor, and the queue's own state ----

    #[test]
    fn a_project_opened_in_an_editor_during_its_conversion_is_not_swapped() {
        alone("a_project_opened_in_an_editor_during_its_conversion_is_not_swapped", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let c = w.next().unwrap();
            for at in [1, FRAMES - 1] {
                let opened: Mutex<Option<Loaded>> = Mutex::new(None);
                let fake = Fake::during(|i| {
                    if i == at {
                        *opened.lock().unwrap() = Some(w.store.load(&key(CH_A)).unwrap_or_else(|e| panic!("{e:?}")));
                    }
                });
                assert_eq!(convert_one(&w.store, &fake, &c, &go), Outcome::Cancelled, "opened at frame {at}");
                assert_eq!((files(&saved.dir) == whole, w.converting_folders(), converting_now()), (true, 0, None));
                // The editor has its frames, and while it is open the project is nobody's candidate.
                let loaded = opened.lock().unwrap().take().unwrap();
                assert_eq!(pictures(&loaded.rec).len(), FRAMES);
                assert_eq!(w.next(), None);
                assert_eq!(convert_one(&w.store, &Fake::default(), &c, &go), Outcome::Later, "an open project is not started on");
                assert_eq!(files(&saved.dir), whole);
                drop(loaded);
            }
            assert!(matches!(convert_one(&w.store, &Fake::default(), &w.next().unwrap(), &go), Outcome::Swapped { .. }));
        });
    }

    #[test]
    fn edit_cancels_the_conversion_of_that_project() {
        alone("edit_cancels_the_conversion_of_that_project", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let (opened, once): (Mutex<Option<Loaded>>, AtomicBool) = (Mutex::new(None), AtomicBool::new(false));
            let fake = Fake::during(|i| {
                if i == 1 && !once.swap(true, Ordering::SeqCst) {
                    *opened.lock().unwrap() = Some(w.store.load(&key(CH_A)).unwrap_or_else(|e| panic!("{e:?}")));
                }
            });
            let gate = open_gate();
            queue(&w, &fake, &gate, || {
                until("the conversion to give way to the editor", || fake.encodes() == 1 && gate.parked());
                assert_eq!((files(&saved.dir) == whole, w.converting_folders(), converting_now()), (true, 0, None));
                assert_eq!(w.info(CH_A).convert, ConvertState::NotTried);
                // Nothing is started while the editor has it, whatever wakes the queue.
                gate.nudge();
                std::thread::sleep(Duration::from_millis(50));
                until("the queue to rest", || gate.parked());
                assert_eq!(fake.encodes(), 1);
                // The editor closes, and the next save anywhere queues the project again.
                let loaded = opened.lock().unwrap().take().unwrap();
                assert_eq!(pictures(&loaded.rec).len(), FRAMES);
                drop(loaded);
                gate.nudge();
                until("the recording to be stored as video", || w.info(CH_A).form == Form::Video);
            });
            // Asked by name for a project that is not being converted: nothing happens.
            cancel_conversion_of(&saved.dir, Duration::ZERO);
            assert_eq!(w.info(CH_A).form, Form::Video);
        });
    }

    #[test]
    fn a_conversion_interrupted_by_a_quit_is_picked_up_at_the_next_start() {
        alone("a_conversion_interrupted_by_a_quit_is_picked_up_at_the_next_start", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let gate = open_gate();
            let reached = AtomicBool::new(false);
            // The conversion stands at its third frame until the app is told to quit.
            let fake = Fake::during(|i| {
                if i == 2 {
                    reached.store(true, Ordering::SeqCst);
                    until("the quit", || !gate.allowed());
                }
            });
            queue(&w, &fake, &gate, || {
                until("the conversion to be under way", || reached.load(Ordering::SeqCst));
                assert_eq!(converting_now(), Some(saved.dir.clone()));
                let started = Instant::now();
                gate.shutdown(LONG);
                // The quit waited for the conversion to let go, and for no longer.
                assert!(started.elapsed() < LONG / 2, "{:?}", started.elapsed());
                assert_eq!((converting_now(), w.converting_folders(), files(&saved.dir) == whole), (None, 0, true));
            });
            assert_eq!(w.info(CH_A).convert, ConvertState::NotTried);

            // A quit that could not wait leaves the conversion's folder behind; the next start
            // sweeps it once it is an hour old, and converts the project.
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let folder = |minutes: u128| w.store.root().join(format!(".converting-{}-0123456789abcdef", now - minutes * 60 * 1000));
            let (left, fresh) = (folder(120), folder(30));
            for dir in [&left, &fresh] {
                std::fs::create_dir_all(dir).unwrap();
                std::fs::write(dir.join("frames.mkv"), b"half a video").unwrap();
            }
            // And a quit in the middle of a swap leaves a video the project file does not name.
            std::fs::write(saved.dir.join("frames.mkv"), b"a video nobody names").unwrap();
            crate::publish_ui::tidy_saved_recordings(&w.store);
            assert!(!left.exists() && fresh.exists(), "one of another Mythicshot that runs now is not an hour old");
            assert_eq!(files(&saved.dir), whole);
            std::fs::remove_dir_all(&fresh).unwrap();
            let (gate, fake) = (open_gate(), Fake::default());
            queue(&w, &fake, &gate, || until("the recording to be stored as video", || w.info(CH_A).form == Form::Video));
        });
    }

    /// An ffmpeg as the search answers one, at a path where there is none.
    fn found(has_libx264: bool) -> Ffmpeg {
        let exe = std::env::temp_dir().join("no such folder of the queue tests").join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" });
        let path = std::env::join_paths([exe.parent().unwrap()]).unwrap();
        ffmpeg::find_in(&path, &|_| true, &|_| Some(ffmpeg::Probed { version: "7.1".into(), has_libx264 })).unwrap()
    }

    #[test]
    fn the_start_names_the_codec_only_when_ffmpeg_can_archive_and_touches_nothing_without_one() {
        alone("the_start_names_the_codec_only_when_ffmpeg_can_archive_and_touches_nothing_without_one", || {
            let w = World::new();
            let saved = w.save(CH_A);
            // A try the real codec made, as its mark says: read only while that codec is the one in use.
            assert_eq!(w.store.mark_conversion(&w.next().unwrap(), MARK_FAILED, ffmpeg::CODEC_ID), Ok(true));
            let whole = files(&saved.dir);
            let read = || w.info(CH_A).convert;
            set_current_codec(None);
            assert_eq!(read(), ConvertState::NotTried);

            // The queue is not entered here: the app is closing.
            let closing = Gate::new(Duration::ZERO, Duration::ZERO, NEVER, 0);
            closing.shutdown(Duration::ZERO);
            start(Some(&w.store), Some(&found(true)), &closing);
            assert_eq!(read(), ConvertState::Failed, "the codec in use is the real one's");
            // No ffmpeg, or one that cannot archive: no codec is in use, and the queue ends at
            // once although the switch is on and a project waits.
            w.save(CH_B);
            let whole_now = files(w.store.root());
            for none in [None, Some(found(false))] {
                set_current_codec(Some(ffmpeg::CODEC_ID));
                start(Some(&w.store), none.as_ref(), &open_gate());
                assert_eq!(read(), ConvertState::NotTried);
                assert_eq!(files(w.store.root()), whole_now);
            }
            // Without a folder for saved recordings the codec is told all the same.
            start(None, Some(&found(true)), &open_gate());
            assert_eq!(read(), ConvertState::Failed);
            assert_eq!(files(&saved.dir), whole);
        });
    }

    #[test]
    fn three_failures_of_the_codec_stop_the_queue_for_this_run() {
        alone("three_failures_of_the_codec_stop_the_queue_for_this_run", || {
            const MORE: [&str; 3] = ["ch_0123456789abcdef0123456d", "ch_0123456789abcdef0123456e", "ch_0123456789abcdef0123456f"];
            let w = World::new();
            let dirs = [CH_A, CH_B, CH_C, MORE[0]].map(|chapter| w.save(chapter).dir);
            let whole = files(w.store.root());
            let fake = Fake::that(How::Fails(exit_1));
            // The queue ends by itself: nothing here tells it to. Each failure was another
            // project's, the oldest three, and the fourth was never started on.
            run(&w.store, Some(&fake), &open_gate());
            assert_eq!(*fake.tried.lock().unwrap(), dirs[..3]);
            assert_eq!((files(w.store.root()) == whole, w.converting_folders(), converting_now()), (true, 0, None));
            assert!(w.store.list().iter().all(|p| p.convert == ConvertState::NotTried));

            // Two failures are not three: the queue lives on, and tries neither project again.
            let w = World::new();
            let dirs = [CH_A, CH_B].map(|chapter| w.save(chapter).dir);
            let (fake, gate) = (Fake::that(How::Fails(exit_1)), open_gate());
            queue(&w, &fake, &gate, || {
                until("the queue to have tried both", || fake.encodes() == 2 && gate.parked());
                for _ in 0..3 {
                    gate.nudge();
                    std::thread::sleep(Duration::from_millis(30));
                    until("the queue to rest", || gate.parked());
                }
                assert_eq!(*fake.tried.lock().unwrap(), dirs);
            });

            // A conversion that works in between starts the count again: two failures, one
            // that works, two failures, and the queue lives on.
            let w = World::new();
            let dirs = [CH_A, CH_B, CH_C, MORE[0], MORE[1]].map(|chapter| w.save(chapter).dir);
            let (fake, gate) = (Fake::that(How::FailsWhen(|nth| nth != 2, exit_1)), open_gate());
            queue(&w, &fake, &gate, || {
                until("the queue to have tried all five", || fake.encodes() == 5 && gate.parked());
                gate.nudge();
                std::thread::sleep(Duration::from_millis(30));
                until("the queue to rest", || gate.parked());
                assert_eq!(*fake.tried.lock().unwrap(), dirs);
            });
            assert_eq!(w.store.list().iter().filter(|p| p.form == Form::Video).map(|p| p.dir.clone()).collect::<Vec<_>>(), [dirs[2].clone()]);
            let _ = MORE[2];
        });
    }

    #[test]
    fn an_unreadable_oldest_project_does_not_hold_back_a_younger_one() {
        alone("an_unreadable_oldest_project_does_not_hold_back_a_younger_one", || {
            let w = World::new();
            let (old, young) = (w.save(CH_B), w.save(CH_A));
            // The oldest one's frames file is as long as it was, and holds something else.
            let frames = old.dir.join("frames.zst");
            std::fs::write(&frames, vec![0u8; std::fs::metadata(&frames).unwrap().len() as usize]).unwrap();
            let whole = files(&old.dir);
            let (fake, gate) = (Fake::default(), open_gate());
            queue(&w, &fake, &gate, || {
                until("the younger recording to be stored as video", || w.info(CH_A).form == Form::Video && gate.parked());
                // Woken again, the queue starts on neither.
                for _ in 0..3 {
                    gate.nudge();
                    std::thread::sleep(Duration::from_millis(30));
                    until("the queue to rest", || gate.parked());
                }
                assert_eq!(*fake.tried.lock().unwrap(), [old.dir.clone(), young.dir.clone()]);
            });
            assert_eq!((files(&old.dir) == whole, w.info(CH_B).form, w.info(CH_B).convert, w.converting_folders()), (true, Form::Frames, ConvertState::NotTried, 0));
        });
    }

    #[test]
    fn a_conversion_that_gave_way_to_an_editor_is_looked_at_again_without_a_nudge() {
        alone("a_conversion_that_gave_way_to_an_editor_is_looked_at_again_without_a_nudge", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            let (opened, once): (Mutex<Option<Loaded>>, AtomicBool) = (Mutex::new(None), AtomicBool::new(false));
            let fake = Fake::during(|i| {
                if i == 1 && !once.swap(true, Ordering::SeqCst) {
                    *opened.lock().unwrap() = Some(w.store.load(&key(CH_A)).unwrap_or_else(|e| panic!("{e:?}")));
                }
            });
            let gate = open_gate_looking_again_after(Duration::from_millis(40));
            queue(&w, &fake, &gate, || {
                until("the conversion to give way to the editor", || fake.encodes() == 1 && converting_now().is_none());
                // The queue looks again and again; while the editor has the project, it finds nothing.
                std::thread::sleep(Duration::from_millis(250));
                assert_eq!((fake.encodes(), files(&saved.dir) == whole), (1, true));
                // The editor is closed without saving: nobody nudges, and the recording is converted all the same.
                drop(opened.lock().unwrap().take());
                until("the recording to be stored as video", || w.info(CH_A).form == Form::Video);
                assert_eq!(fake.encodes(), 2);
            });
        });
    }

    #[test]
    fn a_project_that_was_open_when_the_queue_looked_is_looked_at_again_without_a_nudge() {
        alone("a_project_that_was_open_when_the_queue_looked_is_looked_at_again_without_a_nudge", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let whole = files(&saved.dir);
            // The editor has the project before the queue ever looks: no conversion gives way.
            let loaded = w.store.load(&key(CH_A)).unwrap_or_else(|e| panic!("{e:?}"));
            let (fake, gate) = (Fake::default(), open_gate_looking_again_after(Duration::from_millis(40)));
            queue(&w, &fake, &gate, || {
                until("the queue to have looked", || gate.looks() >= 2);
                assert_eq!((fake.encodes(), files(&saved.dir) == whole), (0, true));
                // The editor is closed without saving: nobody nudges, and the recording is converted all the same.
                drop(loaded);
                until("the recording to be stored as video", || w.info(CH_A).form == Form::Video);
                assert_eq!(fake.encodes(), 1);
                // Nothing waits any more: the queue rests until it is nudged.
                rests(&gate);
            });
        });
    }

    /// The queue has stopped looking by itself: it rests, and stays at rest for many times
    /// the wait after which it would look again.
    fn rests(gate: &Gate) {
        std::thread::sleep(Duration::from_millis(150));
        until("the queue to rest", || gate.parked());
        let looks = gate.looks();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!((gate.looks(), gate.parked()), (looks, true), "the queue looked again with nothing to wait for");
    }

    #[test]
    fn a_project_that_gave_way_and_is_no_longer_there_is_forgotten() {
        alone("a_project_that_gave_way_and_is_no_longer_there_is_forgotten", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let (opened, once): (Mutex<Option<Loaded>>, AtomicBool) = (Mutex::new(None), AtomicBool::new(false));
            let fake = Fake::during(|i| {
                if i == 1 && !once.swap(true, Ordering::SeqCst) {
                    *opened.lock().unwrap() = Some(w.store.load(&key(CH_A)).unwrap_or_else(|e| panic!("{e:?}")));
                }
            });
            let gate = open_gate_looking_again_after(Duration::from_millis(40));
            queue(&w, &fake, &gate, || {
                until("the conversion to give way to the editor", || fake.encodes() == 1 && converting_now().is_none());
                // While the editor has it, the queue looks again and again.
                let looks = gate.looks();
                until("the queue to look again", || gate.looks() >= looks + 2);
                // The recording is removed while its editor is open: it will never be converted.
                assert_eq!(w.store.remove_project(&saved.dir), Ok(crate::project::Removed::WhenItsEditorCloses));
                rests(&gate);
                drop(opened.lock().unwrap().take());
                assert_eq!((fake.encodes(), w.store.list().len()), (1, 0));
            });
        });
    }

    #[test]
    fn a_project_that_cannot_be_rewritten_is_converted_once_and_not_again_in_this_run() {
        alone("a_project_that_cannot_be_rewritten_is_converted_once_and_not_again_in_this_run", || {
            let read_only = |path: &Path, on: bool| {
                let mut permissions = std::fs::metadata(path).unwrap().permissions();
                permissions.set_readonly(on);
                std::fs::set_permissions(path, permissions).unwrap();
            };
            for how in [How::Faithful, How::Shifted] {
                let w = World::new();
                let saved = w.save(CH_A);
                let project_file = saved.dir.join("project.json");
                read_only(&project_file, true);
                let whole = files(&saved.dir);
                let fake = Fake::that(how);
                let gate = open_gate();
                queue(&w, &fake, &gate, || {
                    until("the queue to give the project up", || fake.encodes() == 1 && gate.parked());
                    // Woken again and again, it does not encode the same recording again.
                    for _ in 0..3 {
                        gate.nudge();
                        std::thread::sleep(Duration::from_millis(30));
                        until("the queue to rest", || gate.parked());
                    }
                    assert_eq!(fake.encodes(), 1);
                });
                read_only(&project_file, false);
                assert_eq!((files(&saved.dir) == whole, w.converting_folders(), w.info(CH_A).form), (true, 0, Form::Frames));
                // Why it was given up is logged, in the store's words, without a name.
                let lines = lines_without_names(&w);
                assert!(lines.iter().any(|line| line.contains("keeps its frames") || line.contains("could not be noted")), "{lines:#?}");
            }
        });
    }

    #[test]
    fn the_epoch_moves_when_a_conversion_starts_and_when_it_ends() {
        alone("the_epoch_moves_when_a_conversion_starts_and_when_it_ends", || {
            let w = World::new();
            w.save(CH_A);
            let c = w.next().unwrap();
            // A conversion that is stopped, and one that goes through.
            for stop_at in [4, usize::MAX] {
                let during = AtomicUsize::new(0);
                let fake = Fake::during(|i| {
                    if i == 0 {
                        during.store(epoch() as usize, Ordering::SeqCst);
                    }
                });
                let asked = AtomicUsize::new(0);
                let before = epoch() as usize;
                let outcome = convert_one(&w.store, &fake, &c, &|| asked.fetch_add(1, Ordering::SeqCst) < stop_at);
                let (during, after) = (during.load(Ordering::SeqCst), epoch() as usize);
                assert!(before < during && during < after, "{outcome:?}: {before} {during} {after}");
            }
            assert_eq!(w.info(CH_A).form, Form::Video);
        });
    }

    /// Every line logged in this process so far, each checked: a fixed sentence, with no part
    /// of a path, no id of a tour or chapter, no address, and nothing a codec or the system said.
    fn lines_without_names(w: &World) -> Vec<String> {
        let lines = LINES.lock().unwrap().clone();
        let folder = w.tmp.path().file_name().unwrap().to_string_lossy().to_string();
        let hidden: Vec<String> = [folder.as_str(), "projects", TOUR, CH_A, CH_B, "tours.example.com", "somebody", "the caller's own words", "Users", "frames.", "project.json", ".converting-", "os error"].map(String::from).into_iter().chain(w.tmp.path().components().filter_map(|part| part.as_os_str().to_str().map(String::from)).filter(|part| part.len() > 3)).collect();
        for line in &lines {
            assert!(line.starts_with("Saved recordings: ") && line.ends_with('.') && !line.contains('\\') && !line.contains('/'), "{line}");
            for word in &hidden {
                assert!(!line.contains(word.as_str()), "{line:?} holds {word:?}");
            }
        }
        lines
    }

    fn caller() -> CodecError {
        CodecError::Caller(r"C:\Users\somebody\the caller's own words".into())
    }

    fn total() -> CodecError {
        CodecError::TimedOut { bound: Bound::Total }
    }

    fn no_exit() -> CodecError {
        CodecError::Failed { exit: None }
    }

    #[test]
    fn no_log_line_holds_a_folder_name() {
        alone("no_log_line_holds_a_folder_name", || {
            let w = World::new();
            let saved = w.save(CH_A);
            let c = w.next().unwrap();
            let mut outcomes = Vec::new();
            // Every way a conversion ends without changing the project, then the two marks, then the swap.
            for why in [no_room as fn() -> CodecError, stalled, total, exit_1, no_exit, caller] {
                outcomes.push(convert_one(&w.store, &Fake::that(How::Fails(why)), &c, &go));
            }
            outcomes.push(convert_one(&w.store, &Fake::default(), &c, &|| false));
            let loaded = w.store.load(&key(CH_A)).unwrap_or_else(|e| panic!("{e:?}"));
            outcomes.push(convert_one(&w.store, &Fake::default(), &c, &go));
            drop(loaded);
            // The frames cannot be read: the file is as long as it was, and holds something else.
            let frames = saved.dir.join("frames.zst");
            let real = std::fs::read(&frames).unwrap();
            std::fs::write(&frames, vec![0u8; real.len()]).unwrap();
            outcomes.push(convert_one(&w.store, &Fake::default(), &c, &go));
            std::fs::write(&frames, &real).unwrap();
            for how in [How::Shifted, How::Grown] {
                let other = World::new();
                other.save(CH_B);
                outcomes.push(convert_one(&other.store, &Fake::that(how), &other.next().unwrap(), &go));
            }
            outcomes.push(convert_one(&w.store, &Fake::default(), &c, &go));
            use Outcome::*;
            assert_eq!(outcomes[..11], [Later, Later, Later, GaveUp, GaveUp, GaveUp, Cancelled, Later, GaveUp, Marked(MARK_FAILED), Marked(MARK_NOT_SMALLER)]);
            assert!(matches!(outcomes[11], Swapped { .. }));

            let lines = lines_without_names(&w);
            assert!(lines.len() >= outcomes.len(), "{lines:#?}");
            // What they do hold: sizes, counts, the exit code, and which bound stopped the program.
            let any = |part: &str| lines.iter().any(|line| line.contains(part));
            assert!(any("exit code 1") && any("stall") && any("total") && any(&format!("{} bytes", c.frames_bytes)) && any(&format!("{FRAMES} frames")), "{lines:#?}");
        });
    }

    #[test]
    fn the_ffmpeg_line_follows_the_search() {
        assert_eq!([Found::NotYet, Found::Ok, Found::None, Found::Unusable].map(state_of), [FfmpegState::Checking, FfmpegState::Found, FfmpegState::NotFound, FfmpegState::NotFound]);
        // The app's own numbers.
        assert_eq!((MIN_BYTES, START_AFTER, AFTER_SAVE), (8 * 1024 * 1024, Duration::from_secs(60), Duration::from_secs(10)));
        assert_eq!((gate().start_after, gate().after_save, gate().after_cancel, gate().min_bytes), (START_AFTER, AFTER_SAVE, AFTER_CANCEL, MIN_BYTES));
        assert_eq!(AFTER_CANCEL, Duration::from_secs(300));
    }

    #[test]
    fn a_conversion_is_noted_as_under_way_only_while_the_gate_lets_one_run() {
        let under_way = |gate: &Gate| gate.state().converting;
        let gate = Gate::new(Duration::ZERO, Duration::ZERO, NEVER, 0);
        // The switch is off, a session runs, the app is closing: none is noted, so a quit waits for nothing.
        assert!(gate.converting().is_none() && !under_way(&gate));
        gate.set_enabled(true);
        gate.set_recording(true);
        assert!(gate.converting().is_none() && !under_way(&gate));
        gate.set_recording(false);
        let busy = gate.converting().expect("the gate lets one run");
        assert!(under_way(&gate));
        drop(busy);
        assert!(!under_way(&gate));
        gate.shutdown(Duration::ZERO);
        assert!(gate.converting().is_none() && !under_way(&gate));
        let started = Instant::now();
        gate.shutdown(LONG);
        assert!(started.elapsed() < LONG / 2);
    }

    #[test]
    fn a_gate_starts_closed_and_a_quit_is_for_good() {
        let gate = Gate::new(Duration::ZERO, Duration::ZERO, NEVER, 0);
        assert!(!gate.allowed());
        gate.set_enabled(true);
        assert!(gate.allowed());
        gate.set_recording(true);
        assert!(!gate.allowed());
        gate.set_recording(false);
        assert!(gate.allowed());
        gate.set_enabled(false);
        assert!(!gate.allowed());
        gate.set_enabled(true);
        // Nothing runs: a quit does not wait.
        let started = Instant::now();
        gate.shutdown(LONG);
        assert!(started.elapsed() < LONG / 2 && !gate.allowed());
        gate.set_enabled(true);
        gate.nudge();
        assert!(!gate.allowed());
    }

    // ---- The real program ----

    /// Frame `i` of something like a screen: a title bar, a dark side panel, a light page with
    /// lines of dark marks whose lengths change from frame to frame, and a block the size of a
    /// pointer that moves. Flat panels and sharp edges, as a recording has them: frames of
    /// scattered single pixels do not come back within the tolerance, and no screen has them.
    fn screen(i: usize, w: usize, h: usize) -> Vec<u8> {
        let mut px = vec![255u8; w * h * 4];
        let mut put = |x: usize, y: usize, c: [u8; 3]| {
            if x < w && y < h {
                px[(y * w + x) * 4..][..3].copy_from_slice(&c);
            }
        };
        let side = w / 5;
        for y in 0..h {
            for x in 0..w {
                put(x, y, if y < 6 { [0, 120, 215] } else if x < side { [32, 34, 40] } else { [245, 245, 245] });
            }
        }
        for (line, top) in (12..h.saturating_sub(8)).step_by(16).enumerate() {
            for col in 0..2 + (line * 3 + i) % 5 {
                for y in 0..5 {
                    for x in 0..3 {
                        put(side + 4 + col * 5 + x, top + y, [20, 20, 20]);
                    }
                }
            }
        }
        let (left, top) = (side + 2 + (i * 3) % (w - side - 8), 8 + (i * 2) % (h - 18));
        for y in 0..8 {
            for x in 0..5 {
                put(left + x, top + y, if x == 0 || y == 0 || x == 4 || y == 7 { [0, 0, 0] } else { [255, 255, 255] });
            }
        }
        px
    }

    /// Hands out the pictures it holds, whatever file it is asked to decode.
    struct Drawn(Vec<Vec<u8>>);

    impl VideoCodec for Drawn {
        fn id(&self) -> &str {
            "drawn"
        }

        fn encode(&self, _: (u32, u32), _: usize, _: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, _: &Path, _: u64, _: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            unreachable!("nothing is encoded to draw a recording")
        }

        fn decode(&self, _: &Path, _: (u32, u32), _: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, _: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            self.0.iter().enumerate().try_for_each(|(i, picture)| each(i, picture).map_err(CodecError::Caller))
        }
    }

    /// A recording of `n` screen-like frames in `dir/frames`, written by the recording crate's
    /// own writer: the one way this crate has to make frames of pictures of its choosing.
    fn drawn_recording(dir: &Path, w: u32, h: u32, n: usize) -> Recording {
        let into = dir.join("frames");
        std::fs::create_dir_all(&into).unwrap();
        let stand_in = dir.join("stand-in");
        std::fs::write(&stand_in, vec![0u8; n]).unwrap();
        let index = mythicshot_recording::VideoIndex { width: w, height: h, end_ms: n as u64 * 100, times: (0..n as u64).map(|i| (i * 100, None)).collect(), video_bytes: None };
        let drawn = Drawn((0..n).map(|i| screen(i, w as usize, h as usize)).collect());
        let rec = mythicshot_recording::expand_video(&index, &stand_in, &into, &drawn, &|_, _| true).unwrap();
        // With the file it was "expanded from" gone, it is kept as the frames it is.
        std::fs::remove_file(&stand_in).unwrap();
        rec
    }

    #[test]
    fn a_synthetic_project_is_converted_and_opens_again_through_expansion() {
        alone("a_synthetic_project_is_converted_and_opens_again_through_expansion", || {
            let Some(ffmpeg) = ffmpeg::find_now().filter(|found| found.can_archive()) else {
                println!("skipped: ffmpeg with libx264 was not found on this computer");
                return;
            };
            let w = World::new();
            set_current_codec(Some(ffmpeg::CODEC_ID));
            let (width, height, n) = (160, 120, 70);
            let saved = w.save_recording(CH_A, &drawn_recording(&w.tmp.path().join("a"), width, height, n));
            assert_eq!((saved.form, names(&saved.dir)), (Form::Frames, vec!["frames.zst".to_string(), "project.json".into(), "recording.json".into()]));
            let original = Recording::open_saved(&saved.dir).unwrap();
            let before = pictures(&original);
            assert_eq!(before, (0..n).map(|i| screen(i, width as usize, height as usize)).collect::<Vec<_>>());
            let times = |rec: &Recording| rec.frames.iter().map(|m| m.t_ms).collect::<Vec<_>>();
            let c = w.store.next_candidate(ffmpeg::CODEC_ID, 0).unwrap();

            // The frames of so small a recording compress to less than a video's own headers:
            // every measure is the one that ships, but for the size.
            let any_size = Tolerance { smaller_by: -1.0e12, ..TOLERANCE };
            let outcome = convert_with(&w.store, ffmpeg, &c, &any_size, &go);
            let video = std::fs::metadata(saved.dir.join("frames.mkv")).map(|m| m.len()).unwrap_or(0);
            assert_eq!(outcome, Outcome::Swapped { before: c.frames_bytes, after: video });
            let info = w.info(CH_A);
            assert_eq!((info.form, info.convert, info.problem), (Form::Video, ConvertState::Done, None));
            assert_eq!((names(&saved.dir), w.converting_folders()), (vec!["frames.mkv".to_string(), "project.json".into(), "recording.json".into()], 0));

            // It opens again through the real program, with its times and every frame within the tolerance.
            let loaded = open(&w, CH_A, ffmpeg, "edit");
            assert_eq!((times(&loaded.rec), loaded.rec.end_ms), (times(&original), original.end_ms));
            let after = pictures(&loaded.rec);
            assert_eq!(after.len(), n);
            for (i, (a, b)) in before.iter().zip(&after).enumerate() {
                let db = mythicshot_recording::video::psnr_rgb(a, b);
                assert!(db >= TOLERANCE.frame_db, "frame {i}: {db} dB");
            }
            drop(loaded);

            // With the size that ships, the same recording is stored as video only if that is
            // a fifth smaller, and is noted as not smaller otherwise.
            let other = w.save_recording(CH_B, &drawn_recording(&w.tmp.path().join("b"), width, height, n));
            let c = w.store.next_candidate(ffmpeg::CODEC_ID, 0).unwrap();
            assert_eq!(c.dir, other.dir);
            let info = |w: &World| (w.info(CH_B).form, w.info(CH_B).convert);
            match convert_one(&w.store, ffmpeg, &c, &go) {
                Outcome::Swapped { before, after } => assert!(after * 5 <= before * 4 && info(&w) == (Form::Video, ConvertState::Done), "{before} {after}"),
                Outcome::Marked(MARK_NOT_SMALLER) => assert_eq!(info(&w), (Form::Frames, ConvertState::NotSmaller)),
                other => panic!("{other:?}"),
            }
            assert_eq!(w.store.next_candidate(ffmpeg::CODEC_ID, 0), None);
        });
    }
}
