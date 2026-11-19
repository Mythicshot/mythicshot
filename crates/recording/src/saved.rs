//! Keeping a recording, and opening a kept one again.
//!
//! [`Recording::save_into`] puts the frames file and an index (`recording.json`) into a
//! folder the caller made. The frames get there by hard link, so nothing is copied and the
//! data stays when the recording's own temp folder is deleted; a copy is made only when a
//! link cannot be. [`Recording::open_saved`] reads such a folder back into a recording that
//! does not own it, so `delete` and `delete_files` never remove it.
//!
//! A folder can also keep a recording as a video ([`VIDEO_FILE`]) beside an index of format 2
//! that holds every frame's time and pointer and no place in a frames file. [`form_of`] says
//! which of the two a folder holds, [`open_video_index`] reads the times of either, and
//! [`expand_video`] turns the video back into a frames file in a folder of its own. A
//! recording made that way remembers its video, and keeping it again keeps the video.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ffmpeg::{CODEC_ID, CodecError, NOMINAL_FPS, VideoCodec};
use crate::{FRAMES_FILE, FrameMeta, FrameWriter, Recording, now_nanos};

/// The format number of an index that describes a frames file.
const FORMAT: u32 = 1;
/// The format number of an index that describes a video. A higher one was written by a newer version.
const FORMAT_VIDEO: u32 = 2;
const INDEX_FILE: &str = "recording.json";

/// The name of the video in a folder that keeps a recording in its video form.
pub const VIDEO_FILE: &str = "frames.mkv";

/// The largest picture an index may name. A frame is decoded into `width * height * 4`
/// bytes, so a damaged index must not be able to ask for more memory than a screen needs.
const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 64 << 20;

/// The longest recording an index may name, in ms (about 49 days), so time arithmetic on a
/// damaged index cannot overflow.
const MAX_MS: u64 = u32::MAX as u64;

/// The most frames an index may name: the recorder's highest rate (60 a second) for the
/// longest recording. A frames index is bounded further by its file, where every frame takes
/// at least a byte; a video index by the video's size in the same way.
const MAX_FRAMES: usize = (MAX_MS / 1000 * 60) as usize;

/// How far left of or above the recorded area the recorder still notes the pointer.
const POINTER_MARGIN: i32 = 32;

/// How often a rename is tried while another program (a scanner, a sync) has the file open.
const RENAME_TRIES: u32 = 5;
const RENAME_WAIT: std::time::Duration = std::time::Duration::from_millis(120);

/// One frame in the index: offset, length, keyframe, time, pointer x, pointer y.
type Row = (u64, u32, bool, u64, Option<i32>, Option<i32>);

#[derive(Serialize, Deserialize)]
struct Index {
    format: u32,
    width: u32,
    height: u32,
    end_ms: u64,
    frames: Vec<Row>,
}

/// One frame in a video index: time, pointer x, pointer y. Picture i of the video is frame i.
type VideoRow = (u64, Option<i32>, Option<i32>);

/// The index of the video form.
#[derive(Serialize, Deserialize)]
struct VideoFile {
    format: u32,
    width: u32,
    height: u32,
    end_ms: u64,
    video: VideoPart,
    frames: Vec<VideoRow>,
}

/// What a video index notes about its video. Only the size is read back; the rate and the
/// codec are there for whoever looks at the file.
#[derive(Serialize, Deserialize)]
struct VideoPart {
    bytes: u64,
    #[serde(default)]
    fps: u32,
    #[serde(default)]
    codec: String,
}

/// Read before anything else, so an index from a newer version is never taken for a damaged one.
#[derive(Deserialize)]
struct Head {
    format: u32,
}

/// Why a kept recording could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The folder, its index or its frames file is not there.
    Missing,
    /// The index cannot be read, or does not describe the frames file beside it.
    Damaged(String),
    /// The index was written by a newer version, with this format number.
    Newer(u32),
    /// The folder keeps the recording as a video, which [`Recording::open_saved`] does not
    /// read: [`open_video_index`] and [`expand_video`] do.
    Video,
}

/// How a folder keeps a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// The lossless frames file and an index of format 1.
    Frames,
    /// A video ([`VIDEO_FILE`]) and an index of format 2.
    Video,
}

/// What a save kept: the form, and the size in bytes of the frames file or of the video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedAs {
    pub form: Form,
    pub bytes: u64,
}

/// What an index says about a recording without its frames file: the picture size, the end,
/// and every frame's time and pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoIndex {
    pub width: u32,
    pub height: u32,
    pub end_ms: u64,
    /// One entry per frame, in order: its time in ms and where the pointer was.
    pub times: Vec<(u64, Option<(i32, i32)>)>,
    /// The size of the video as the index notes it; None for an index of the frames form.
    pub video_bytes: Option<u64>,
}

/// Why a video could not be expanded into frames.
#[derive(Debug)]
pub enum ExpandError {
    /// The caller's check said to stop.
    Cancelled,
    /// The disk the frames are written to is full.
    NoRoom,
    /// The video or the index does not hold the recording; the words say what was found.
    /// They are this crate's own and never the codec's.
    Damaged(&'static str),
    /// The codec could not do the work for a reason that says nothing about the video: a
    /// time bound, a program that could not be started or cannot do this.
    Codec(CodecError),
    /// The folder to expand into cannot be used, or the frames could not be written.
    Io(io::Error),
}

/// Why a recording could not be kept.
#[derive(Debug)]
pub enum SaveError {
    /// The caller's check said to stop while the frames were copied. What the call wrote is removed.
    Cancelled,
    /// The recording or the folder cannot be used for this; the words say why.
    Unusable(&'static str),
    /// A file could not be linked, copied, written or put in place: which one, what was
    /// being done, and the system's error.
    Io { step: SaveStep, context: &'static str, error: io::Error },
}

/// The file a save was working on when it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStep {
    Frames,
    Index,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Cancelled => f.write_str("keeping the recording was cancelled"),
            SaveError::Unusable(why) => f.write_str(why),
            SaveError::Io { context, error, .. } => write!(f, "{context}: {error}"),
        }
    }
}

const KEEP_FRAMES: &str = "could not keep the frames";
const WRITE_INDEX: &str = "could not write the recording's index";

fn frames_failed(error: io::Error) -> SaveError {
    SaveError::Io { step: SaveStep::Frames, context: KEEP_FRAMES, error }
}

fn index_failed(error: io::Error) -> SaveError {
    SaveError::Io { step: SaveStep::Index, context: WRITE_INDEX, error }
}

/// How much of the frames file a copy writes between two looks at the caller's check.
const COPY_CHUNK: usize = 1 << 20;

impl Recording {
    /// Keep this recording in `dir`, which must exist: the frames file by hard link, or by
    /// copy when a link cannot be made, then the index. Returns what was kept and its size.
    /// A failure removes what this call wrote and nothing else. Saving into the folder the
    /// recording already lives in only rewrites the index.
    ///
    /// A recording expanded from a video ([`expand_video`]) whose video is still a file of
    /// the size it had is kept as that video, linked or copied the same way, with the index
    /// of the video form: the answer says [`Form::Video`], and the frames are not kept and
    /// never encoded a second time. If the video is gone or has another size, the expanded
    /// frames are kept like any recording's. A file of the other form that was already in
    /// `dir` is not this call's to remove and stays; the index says which one counts.
    pub fn save_into(&self, dir: &Path) -> Result<SavedAs, String> {
        self.save_with(dir, |from, to| std::fs::hard_link(from, to), copy_whole)
    }

    /// [`Recording::save_into`] for a caller that may have to stop, and that wants to know
    /// what went wrong: when the frames have to be copied, `keep_going` is asked before every
    /// megabyte, and a no ends the save as [`SaveError::Cancelled`] with nothing left in `dir`
    /// of what this call wrote. A link is made at once and is never cancelled.
    pub fn save_into_with(&self, dir: &Path, keep_going: &dyn Fn() -> bool) -> Result<SavedAs, SaveError> {
        self.save_while(dir, |from, to| std::fs::hard_link(from, to), COPY_CHUNK, keep_going)
    }

    /// [`Recording::save_into_with`] with the link and the copy's chunk size handed in, for a test.
    fn save_while(&self, dir: &Path, link: impl FnOnce(&Path, &Path) -> io::Result<()>, chunk: usize, keep_going: &dyn Fn() -> bool) -> Result<SavedAs, SaveError> {
        let stopped = std::cell::Cell::new(false);
        let go = || {
            let go = keep_going();
            stopped.set(!go);
            go
        };
        match self.save_typed(dir, link, |from, to| copy_while(from, to, chunk, &go)) {
            Err(_) if stopped.get() => Err(SaveError::Cancelled),
            other => other,
        }
    }

    /// [`Recording::save_into`] with the link and the copy handed in, so a test can make
    /// either fail. Both put the file `from` at the new path `to`.
    fn save_with(&self, dir: &Path, link: impl FnOnce(&Path, &Path) -> io::Result<()>, copy: impl FnOnce(&Path, &Path) -> io::Result<()>) -> Result<SavedAs, String> {
        self.save_typed(dir, link, copy).map_err(|e| e.to_string())
    }

    fn save_typed(&self, dir: &Path, link: impl FnOnce(&Path, &Path) -> io::Result<()>, copy: impl FnOnce(&Path, &Path) -> io::Result<()>) -> Result<SavedAs, SaveError> {
        if !dir.is_dir() {
            return Err(SaveError::Unusable("the folder to keep the recording in does not exist"));
        }
        let expected = self.frames_end().filter(|end| *end > 0).ok_or(SaveError::Unusable("the recording has no frames to keep"))?;
        let dest = dir.join(FRAMES_FILE);
        if same_folder(&self.dir, dir) {
            let size = std::fs::metadata(&dest).map_err(|error| SaveError::Io { step: SaveStep::Frames, context: "could not read the frames file", error })?.len();
            if size != expected {
                return Err(SaveError::Unusable("the frames file is not the one this recording was read from"));
            }
            self.write_index(dir)?;
            return Ok(SavedAs { form: Form::Frames, bytes: size });
        }

        let (staged, kept) = match self.video_to_keep() {
            Some((video, bytes)) => (self.stage_as(dir, video, VIDEO_FILE, bytes, link, copy, |to| self.write_video_index_to(to, bytes))?, SavedAs { form: Form::Video, bytes }),
            None => (self.stage(dir, expected, link, copy)?, SavedAs { form: Form::Frames, bytes: expected }),
        };
        if let Err(e) = staged.swap_frames() {
            staged.undo(false);
            return Err(frames_failed(e));
        }
        if let Err(e) = staged.swap_index() {
            staged.undo(true);
            return Err(index_failed(e));
        }
        Ok(kept)
    }

    /// The video this recording was expanded from, when it is still a file of the size it
    /// had then: what a save keeps instead of the frames.
    fn video_to_keep(&self) -> Option<(&Path, u64)> {
        self.expanded_from().filter(|(video, bytes)| std::fs::metadata(video).is_ok_and(|m| m.is_file() && m.len() == *bytes))
    }

    /// The first half of a save: the frames file and the index, each whole and on the disk
    /// under a temporary name in `dir`. Nothing that was in the folder has been touched yet,
    /// and a failure removes both temporary files.
    fn stage(&self, dir: &Path, expected: u64, link: impl FnOnce(&Path, &Path) -> io::Result<()>, copy: impl FnOnce(&Path, &Path) -> io::Result<()>) -> Result<Staged, SaveError> {
        self.stage_as(dir, &self.dir.join(FRAMES_FILE), FRAMES_FILE, expected, link, copy, |to| self.write_index_to(to))
    }

    /// [`Recording::stage`] for either form: the file `src` of `expected` bytes under the
    /// name `name`, and the index that `write_index` writes.
    #[allow(clippy::too_many_arguments)]
    fn stage_as(&self, dir: &Path, src: &Path, name: &str, expected: u64, link: impl FnOnce(&Path, &Path) -> io::Result<()>, copy: impl FnOnce(&Path, &Path) -> io::Result<()>, write_index: impl FnOnce(&Path) -> io::Result<()>) -> Result<Staged, SaveError> {
        let (frames, index) = (dir.join(name), dir.join(INDEX_FILE));
        let staged = Staged { frames_tmp: temp_beside(&frames), index_tmp: temp_beside(&index), replaces: frames.exists(), frames, index };
        let tmp = &staged.frames_tmp;
        // A link is the same data under a second name: make sure that data is on the disk.
        let result = link(src, tmp)
            .and_then(|()| sync(tmp))
            .or_else(|_| {
                let _ = std::fs::remove_file(tmp);
                copy(src, tmp)
            })
            .and_then(|()| match std::fs::metadata(tmp)?.len() {
                size if size == expected => Ok(()),
                _ => Err(io::Error::other("the frames file is not complete")),
            })
            .map_err(frames_failed)
            .and_then(|()| write_index(&staged.index_tmp).map_err(index_failed));
        match result {
            Ok(()) => Ok(staged),
            Err(e) => {
                staged.undo(false);
                Err(e)
            }
        }
    }

    /// Where the frames file ends by the index: the last frame's offset plus its length.
    fn frames_end(&self) -> Option<u64> {
        self.frames.last().and_then(|m| m.offset.checked_add(m.len as u64))
    }

    /// Write the index, whole and on the disk, to the new file `to`.
    fn write_index_to(&self, to: &Path) -> io::Result<()> {
        let index = Index {
            format: FORMAT,
            width: self.width,
            height: self.height,
            end_ms: self.end_ms,
            frames: self.frames.iter().map(|m| (m.offset, m.len, m.key, m.t_ms, m.cursor.map(|c| c.0), m.cursor.map(|c| c.1))).collect(),
        };
        let bytes = serde_json::to_vec(&index).map_err(io::Error::other)?;
        let mut f = File::create(to)?;
        f.write_all(&bytes)?;
        f.sync_all()
    }

    /// Write `recording.json` through a temporary file and one rename, so the folder never
    /// holds half an index.
    fn write_index(&self, dir: &Path) -> Result<(), SaveError> {
        let path = dir.join(INDEX_FILE);
        let tmp = temp_beside(&path);
        self.write_index_to(&tmp).and_then(|()| patiently(|| std::fs::rename(&tmp, &path))).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            index_failed(e)
        })
    }

    /// Open a recording kept by [`Recording::save_into`]. The folder is not this recording's
    /// to delete: `delete` and `delete_files` leave it alone. Nothing is changed or removed,
    /// whatever is found. Only the frames form is opened: a folder whose index is of the
    /// video form answers [`OpenError::Video`], whatever else lies in it.
    pub fn open_saved(dir: &Path) -> Result<Recording, OpenError> {
        let text = std::fs::read(dir.join(INDEX_FILE)).map_err(not_readable)?;
        let head: Head = serde_json::from_slice(&text).map_err(|_| damaged("the index cannot be read"))?;
        if form_of_head(&head)? == Form::Video {
            return Err(OpenError::Video);
        }
        let index: Index = serde_json::from_slice(&text).map_err(|_| damaged("the index cannot be read"))?;
        let pixels = index.width as u64 * index.height as u64;
        if index.width == 0 || index.height == 0 || index.width > MAX_SIDE || index.height > MAX_SIDE || pixels > MAX_PIXELS {
            return Err(damaged("the picture size in the index is not possible"));
        }
        let meta = std::fs::metadata(dir.join(FRAMES_FILE)).map_err(not_readable)?;
        if !meta.is_file() {
            return Err(damaged("the frames file is not a file"));
        }

        if !index.frames.first().is_some_and(|row| row.2) {
            return Err(damaged("the first frame is not a whole frame"));
        }
        let mut frames = Vec::with_capacity(index.frames.len());
        let (mut end, mut last_ms) = (0u64, 0u64);
        for (offset, len, key, t_ms, x, y) in index.frames {
            // Frames lie one after the other from the start of the file, in the order of their times.
            if offset != end || len == 0 || t_ms < last_ms {
                return Err(damaged("the index does not describe a recording"));
            }
            end = offset.checked_add(len as u64).ok_or_else(|| damaged("the index does not describe a recording"))?;
            last_ms = t_ms;
            frames.push(FrameMeta { offset, len, key, t_ms, cursor: x.zip(y) });
        }
        // No pointer position but those the recorder writes: a little outside the top-left, or inside.
        let pointer_fits = |c: &(i32, i32)| (-POINTER_MARGIN..index.width as i32).contains(&c.0) && (-POINTER_MARGIN..index.height as i32).contains(&c.1);
        if index.end_ms > MAX_MS || !frames.iter().filter_map(|m| m.cursor.as_ref()).all(pointer_fits) {
            return Err(damaged("the index does not describe a recording"));
        }
        if end != meta.len() {
            return Err(damaged("the frames file is not as long as the index says"));
        }
        if index.end_ms < last_ms {
            return Err(damaged("the index does not describe a recording"));
        }
        Ok(Recording { dir: dir.to_path_buf(), owned: false, width: index.width, height: index.height, frames, end_ms: index.end_ms, expanded_from: None })
    }

    /// Write the index of the video form into `dir`, for a video of `video_bytes` bytes that
    /// holds one picture per frame of this recording, in order: every frame's time and
    /// pointer, the end, and the video's size. Through a temporary file and one rename, so
    /// the folder never holds half an index; an index that was there is replaced. Nothing
    /// else in `dir` is touched, and the video itself is not looked at.
    pub fn write_video_index(&self, dir: &Path, video_bytes: u64) -> Result<(), SaveError> {
        if self.frames.is_empty() {
            return Err(SaveError::Unusable("the recording has no frames to keep"));
        }
        if video_bytes == 0 {
            return Err(SaveError::Unusable("the video has no size"));
        }
        let path = dir.join(INDEX_FILE);
        let tmp = temp_beside(&path);
        self.write_video_index_to(&tmp, video_bytes).and_then(|()| patiently(|| std::fs::rename(&tmp, &path))).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            index_failed(e)
        })
    }

    /// Write the index of the video form, whole and on the disk, to the new file `to`.
    fn write_video_index_to(&self, to: &Path, video_bytes: u64) -> io::Result<()> {
        let index = VideoFile {
            format: FORMAT_VIDEO,
            width: self.width,
            height: self.height,
            end_ms: self.end_ms,
            video: VideoPart { bytes: video_bytes, fps: NOMINAL_FPS, codec: CODEC_ID.to_string() },
            frames: self.frames.iter().map(|m| (m.t_ms, m.cursor.map(|c| c.0), m.cursor.map(|c| c.1))).collect(),
        };
        let bytes = serde_json::to_vec(&index).map_err(io::Error::other)?;
        let mut f = File::create(to)?;
        f.write_all(&bytes)?;
        f.sync_all()
    }

    /// Whether `other` describes this recording's timing: the same picture size and end
    /// time, the same number of frames, and for every frame the same time and pointer.
    /// Where the frames lie in a file, and the video's size, are not timing.
    pub fn same_timing(&self, other: &VideoIndex) -> bool {
        (self.width, self.height, self.end_ms, self.frames.len()) == (other.width, other.height, other.end_ms, other.times.len()) && self.frames.iter().zip(&other.times).all(|(m, at)| (m.t_ms, m.cursor) == *at)
    }

    /// The video this recording was expanded from by [`expand_video`], and the size it had
    /// then. None for every other recording.
    pub fn expanded_from(&self) -> Option<(&Path, u64)> {
        self.expanded_from.as_ref().map(|(video, bytes)| (video.as_path(), *bytes))
    }
}

/// Which form the folder `dir` keeps a recording in, by its index's format number alone:
/// neither the rest of the index nor the frames file or the video is read, so this says
/// nothing about whether they are whole. An index from a newer version is
/// [`OpenError::Newer`], one without a known format number is damaged.
pub fn form_of(dir: &Path) -> Result<Form, OpenError> {
    form_in(&std::fs::read(dir.join(INDEX_FILE)).map_err(not_readable)?)
}

/// [`form_of`] on the index's text.
fn form_in(text: &[u8]) -> Result<Form, OpenError> {
    let head: Head = serde_json::from_slice(text).map_err(|_| damaged("the index cannot be read"))?;
    form_of_head(&head)
}

fn form_of_head(head: &Head) -> Result<Form, OpenError> {
    match head.format {
        FORMAT => Ok(Form::Frames),
        FORMAT_VIDEO => Ok(Form::Video),
        newer if newer > FORMAT_VIDEO => Err(OpenError::Newer(newer)),
        _ => Err(damaged("the index has no known format")),
    }
}

/// Read the times, pointers, end and picture size from the index in `dir`: an index of the
/// video form, or one of the frames form (which holds the same times; it is what a
/// conversion cut off between two steps leaves beside a video). Neither the frames file nor
/// the video is looked at, and nothing is changed or removed, whatever is found.
///
/// The limits are those of [`Recording::open_saved`]: a picture no larger than a frame may
/// be, times in order and no later than the end, an end no later than any recording, and
/// pointers where the recorder notes them. On top, there is at least one frame and no more
/// than a recording can hold, and a video index names a video of at least a byte per frame.
/// All of it is checked here, before a caller sizes anything by these numbers.
pub fn open_video_index(dir: &Path) -> Result<VideoIndex, OpenError> {
    read_video_index(&std::fs::read(dir.join(INDEX_FILE)).map_err(not_readable)?)
}

/// [`open_video_index`] on the index's text.
fn read_video_index(text: &[u8]) -> Result<VideoIndex, OpenError> {
    let unreadable = |_| damaged("the index cannot be read");
    let index = match form_in(text)? {
        Form::Video => {
            let file: VideoFile = serde_json::from_slice(text).map_err(unreadable)?;
            VideoIndex { width: file.width, height: file.height, end_ms: file.end_ms, times: file.frames.into_iter().map(|(t_ms, x, y)| (t_ms, x.zip(y))).collect(), video_bytes: Some(file.video.bytes) }
        }
        Form::Frames => {
            let file: Index = serde_json::from_slice(text).map_err(unreadable)?;
            VideoIndex { width: file.width, height: file.height, end_ms: file.end_ms, times: file.frames.into_iter().map(|(_, _, _, t_ms, x, y)| (t_ms, x.zip(y))).collect(), video_bytes: None }
        }
    };
    index.check().map_err(damaged)?;
    Ok(index)
}

const NOT_A_RECORDING: &str = "the index does not describe a recording";

impl VideoIndex {
    /// Whether these numbers can be a recording; the words say what is wrong. Nothing is
    /// sized by them before this passed.
    fn check(&self) -> Result<(), &'static str> {
        let pixels = self.width as u64 * self.height as u64;
        if self.width == 0 || self.height == 0 || self.width > MAX_SIDE || self.height > MAX_SIDE || pixels > MAX_PIXELS {
            return Err("the picture size in the index is not possible");
        }
        let frames = self.times.len();
        if frames == 0 || frames > MAX_FRAMES || self.video_bytes.is_some_and(|bytes| bytes < frames as u64) {
            return Err(NOT_A_RECORDING);
        }
        let in_order = self.times.windows(2).all(|pair| pair[0].0 <= pair[1].0);
        let last_ms = self.times.last().map_or(0, |at| at.0);
        // No pointer position but those the recorder writes: a little outside the top-left, or inside.
        let pointer_fits = |c: &(i32, i32)| (-POINTER_MARGIN..self.width as i32).contains(&c.0) && (-POINTER_MARGIN..self.height as i32).contains(&c.1);
        if !in_order || self.end_ms > MAX_MS || self.end_ms < last_ms || !self.times.iter().filter_map(|at| at.1.as_ref()).all(pointer_fits) {
            return Err(NOT_A_RECORDING);
        }
        Ok(())
    }
}

const VIDEO_NOT_THERE: &str = "the video is not there";
const VIDEO_NOT_A_FILE: &str = "the video is not a file";
const VIDEO_WRONG_SIZE: &str = "the video is not as long as the index says";
const VIDEO_WRONG_COUNT: &str = "the video does not hold the frames the index names";
const VIDEO_UNREADABLE: &str = "the video cannot be read";

/// Turn the video `video` back into a frames file in the folder `into`, which must exist and
/// be empty, and answer a recording that owns `into` (its `delete` removes that folder and
/// never the video). The recording has the index's picture size, end, times and pointers,
/// whole frames where the recorder puts them, and remembers the video and its size
/// ([`Recording::expanded_from`]). The video is only read.
///
/// `keep_going` is asked before every frame with the frames written so far and their total,
/// and a no ends the work as [`ExpandError::Cancelled`]; the codec asks it too, while it
/// waits. One frame is held at a time, beside the writer's previous frame and difference.
/// On any way out but success (an error, a cancel, a panic of the codec or of `keep_going`)
/// `into` is empty again.
///
/// The index is checked against its limits before anything is made or sized by it. A video
/// that is missing, not the size its index notes, or that does not decode to exactly the
/// index's number of pictures of the index's size is [`ExpandError::Damaged`], in this
/// crate's own words; a full disk is [`ExpandError::NoRoom`].
///
/// What "exactly" can mean here is the codec's limit: a decode counts bytes and nothing
/// else. A video whose pictures have another size, and whose total still comes to
/// frames × width × height × 4 bytes, is not noticed and expands into wrong pictures. What
/// guards the content is the comparison made when the video was written, before it replaced
/// the frames; this function trusts a video that was verified then and only proves the count.
pub fn expand_video(index: &VideoIndex, video: &Path, into: &Path, codec: &dyn VideoCodec, keep_going: &dyn Fn(usize, usize) -> bool) -> Result<Recording, ExpandError> {
    expand_with(index, video, into, codec, keep_going, |path| File::create(path))
}

/// [`expand_video`] with the making of the frames file handed in, so a test can let the disk
/// run full. `open` is given the frames file's path in `into`.
fn expand_with<W: Write>(index: &VideoIndex, video: &Path, into: &Path, codec: &dyn VideoCodec, keep_going: &dyn Fn(usize, usize) -> bool, open: impl FnOnce(&Path) -> io::Result<W>) -> Result<Recording, ExpandError> {
    index.check().map_err(ExpandError::Damaged)?;
    let total = index.times.len();
    let size = match std::fs::metadata(video) {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(_) => return Err(ExpandError::Damaged(VIDEO_NOT_A_FILE)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ExpandError::Damaged(VIDEO_NOT_THERE)),
        Err(e) => return Err(ExpandError::Io(e)),
    };
    // Not the file the index was written for, or too short to hold a byte per picture.
    if index.video_bytes.is_some_and(|noted| noted != size) || size < total as u64 {
        return Err(ExpandError::Damaged(VIDEO_WRONG_SIZE));
    }
    if std::fs::read_dir(into).map_err(ExpandError::Io)?.next().is_some() {
        return Err(ExpandError::Io(io::Error::other("the folder to expand into is not empty")));
    }

    // From here on every way out but the last line removes the frames file. The writer is
    // made after the guard, so it lets go of the file first, also when a panic unwinds.
    let mut guard = Emptied { file: into.join(FRAMES_FILE), keep: false };
    let out = open(&guard.file).map_err(write_failed)?;
    let mut writer = FrameWriter::over(out, index.width, index.height);
    let frame_len = index.width as usize * index.height as usize * 4;
    // Why this function's own part stopped the decode, which the codec only knows as "the caller".
    let mut stopped: Option<ExpandError> = None;
    let mut each = |i: usize, px: &[u8]| -> Result<(), String> {
        let step = match index.times.get(i) {
            _ if !keep_going(i, total) => Err(ExpandError::Cancelled),
            Some((t_ms, cursor)) if px.len() == frame_len => writer.push_io(px, *t_ms, *cursor).map_err(|(_, e)| write_failed(e)),
            _ => Err(ExpandError::Damaged(VIDEO_WRONG_COUNT)),
        };
        step.map_err(|why| {
            stopped = Some(why);
            String::new()
        })
    };
    let decoded = codec.decode(video, (index.width, index.height), total, &mut each, &|done| keep_going(done, total));
    if let Some(why) = stopped {
        return Err(why);
    }
    decoded.map_err(|e| match e {
        CodecError::Cancelled => ExpandError::Cancelled,
        // The video held another number of pictures, or the program could not read it.
        CodecError::WrongFrames { .. } => ExpandError::Damaged(VIDEO_WRONG_COUNT),
        CodecError::Failed { exit: Some(_) } => ExpandError::Damaged(VIDEO_UNREADABLE),
        other => ExpandError::Codec(other),
    })?;
    if writer.frames.len() != total {
        return Err(ExpandError::Damaged(VIDEO_WRONG_COUNT));
    }
    let frames = writer.into_frames().map_err(write_failed)?;
    guard.keep = true;
    Ok(Recording { dir: into.to_path_buf(), owned: true, width: index.width, height: index.height, frames, end_ms: index.end_ms, expanded_from: Some((video.to_path_buf(), size)) })
}

/// Removes the frames file an expansion was writing, unless the expansion finished.
struct Emptied {
    file: PathBuf,
    keep: bool,
}

impl Drop for Emptied {
    fn drop(&mut self) {
        if !self.keep {
            let _ = patiently(|| std::fs::remove_file(&self.file));
        }
    }
}

/// A write that failed because the disk is full or a quota is used up is no room; any other is itself.
fn write_failed(e: io::Error) -> ExpandError {
    if matches!(e.kind(), io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded) { ExpandError::NoRoom } else { ExpandError::Io(e) }
}

fn damaged(why: &str) -> OpenError {
    OpenError::Damaged(why.to_string())
}

/// A file that is not there is missing; one that is there and cannot be read is damaged.
fn not_readable(e: io::Error) -> OpenError {
    match e.kind() {
        io::ErrorKind::NotFound => OpenError::Missing,
        _ => OpenError::Damaged(e.to_string()),
    }
}

/// Whether two paths name one folder, however each is spelled.
fn same_folder(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// A name of this call's own beside `path`, for a file that is renamed to `path` when whole.
fn temp_beside(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{}.tmp", std::process::id(), now_nanos()));
    path.with_file_name(name)
}

/// Make sure what was written to the file at `path` is on the disk.
fn sync(path: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new().write(true).open(path)?.sync_all()
}

/// A save whose new files are whole under temporary names, ready to take their places.
struct Staged {
    frames_tmp: PathBuf,
    frames: PathBuf,
    index_tmp: PathBuf,
    index: PathBuf,
    /// The folder already held a frames file, which the new one replaces.
    replaces: bool,
}

impl Staged {
    /// Put the new frames in place. An index that described the frames being replaced goes
    /// first, so if the save stops right here the folder holds frames without an index and
    /// never an index beside frames it does not describe.
    fn swap_frames(&self) -> io::Result<()> {
        if self.replaces {
            match patiently(|| std::fs::remove_file(&self.index)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        patiently(|| std::fs::rename(&self.frames_tmp, &self.frames))
    }

    fn swap_index(&self) -> io::Result<()> {
        patiently(|| std::fs::rename(&self.index_tmp, &self.index))
    }

    /// Remove what this save wrote: its temporary files, and the frames file if it put one
    /// where there was none.
    fn undo(&self, frames_placed: bool) {
        let _ = std::fs::remove_file(&self.frames_tmp);
        let _ = std::fs::remove_file(&self.index_tmp);
        if frames_placed && !self.replaces {
            let _ = std::fs::remove_file(&self.frames);
        }
    }
}

/// Try a rename or a removal again for half a second while another program has the file open.
fn patiently(mut step: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut tried = 1;
    loop {
        match step() {
            Err(e) if tried < RENAME_TRIES && (e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32))) => {
                tried += 1;
                std::thread::sleep(RENAME_WAIT);
            }
            other => return other,
        }
    }
}

/// Copy `from` to the new file `to` and make sure it is on the disk.
fn copy_whole(from: &Path, to: &Path) -> io::Result<()> {
    let mut src = File::open(from)?;
    let mut out = File::create(to)?;
    io::copy(&mut src, &mut out)?;
    out.flush()?;
    out.sync_all()
}

/// [`copy_whole`] in pieces of `chunk` bytes, asking `keep_going` before each: a no stops the
/// copy with an error, and the caller removes the half-written file.
fn copy_while(from: &Path, to: &Path, chunk: usize, keep_going: &dyn Fn() -> bool) -> io::Result<()> {
    use std::io::Read;
    let mut src = File::open(from)?;
    let mut out = File::create(to)?;
    let mut buf = vec![0u8; chunk.max(1)];
    loop {
        if !keep_going() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "the copy was cancelled"));
        }
        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    out.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::synthetic;
    use crate::ffmpeg::Bound;
    use crate::{FrameReader, KEYFRAME_EVERY};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const W: u32 = 48;
    const H: u32 = 32;
    const N: usize = KEYFRAME_EVERY + 9;

    /// A real recording of `N` frames in `<root>/rec`, as the recorder leaves one.
    fn recorded(root: &Path) -> Recording {
        recorded_as(root, "rec", N)
    }

    /// A real recording of the first `n` of those frames in `<root>/<name>`.
    fn recorded_as(root: &Path, name: &str, n: usize) -> Recording {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut writer = FrameWriter::create(&dir, W, H).unwrap();
        for i in 0..n {
            writer.push(&synthetic(i, W, H), i as u64 * 66, (i % 3 != 0).then_some((i as i32 - 5, 7))).unwrap();
        }
        writer.finish(dir, n as u64 * 66).unwrap()
    }

    fn folder(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn refused(dir: &Path) -> OpenError {
        match Recording::open_saved(dir) {
            Ok(_) => panic!("the recording opened"),
            Err(e) => e,
        }
    }

    fn opened(dir: &Path) -> Recording {
        match Recording::open_saved(dir) {
            Ok(rec) => rec,
            Err(e) => panic!("the recording did not open: {e:?}"),
        }
    }

    fn assert_every_frame(rec: &Recording) {
        let mut seen = 0;
        rec.for_each_frame(|i, img| {
            assert_eq!(img.into_raw(), synthetic(i, W, H), "frame {i}");
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, N);
    }

    fn no_link(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::other("no links on this disk"))
    }

    fn no_copy(_: &Path, _: &Path) -> io::Result<()> {
        panic!("a copy was made")
    }

    /// What a save answers when it kept the frames file, of `bytes` bytes.
    fn as_frames(bytes: u64) -> SavedAs {
        SavedAs { form: Form::Frames, bytes }
    }

    #[test]
    fn a_saved_recording_opens_again_and_every_frame_decodes_the_same() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        let size = rec.save_into(&kept).unwrap().bytes;
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        assert_eq!(size, std::fs::metadata(kept.join(FRAMES_FILE)).unwrap().len());
        assert!(size > 0);

        let again = opened(&kept);
        assert_eq!((again.width, again.height, again.end_ms), (W, H, N as u64 * 66));
        assert_eq!(again.frames.len(), N);
        for (a, b) in again.frames.iter().zip(&rec.frames) {
            assert_eq!((a.offset, a.len, a.key, a.t_ms, a.cursor), (b.offset, b.len, b.key, b.t_ms, b.cursor));
        }
        assert_every_frame(&again);
        // Random access and the editor's reader work on it too.
        assert_eq!(again.frame(N - 1).unwrap().into_raw(), synthetic(N - 1, W, H));
        let mut reader = FrameReader::new(Arc::new(again)).unwrap();
        assert_eq!(reader.get(KEYFRAME_EVERY + 2).unwrap().into_raw(), synthetic(KEYFRAME_EVERY + 2, W, H));

        // The index is the documented shape: one row of six per frame, null for no pointer.
        let index: serde_json::Value = serde_json::from_slice(&std::fs::read(kept.join(INDEX_FILE)).unwrap()).unwrap();
        assert_eq!(index["format"], 1);
        assert_eq!((index["width"].as_u64(), index["height"].as_u64(), index["end_ms"].as_u64()), (Some(W as u64), Some(H as u64), Some(N as u64 * 66)));
        assert_eq!(index["frames"].as_array().unwrap().len(), N);
        assert_eq!(index["frames"][0], serde_json::json!([0, rec.frames[0].len, true, 0, null, null]));
        assert_eq!(index["frames"][1], serde_json::json!([rec.frames[1].offset, rec.frames[1].len, false, 66, -4, 7]));
    }

    #[test]
    fn saving_links_the_frames_file_and_deleting_the_original_keeps_the_saved_one() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        // No copy is made: the link alone puts the frames there.
        rec.save_with(&kept, |from, to| std::fs::hard_link(from, to), no_copy).unwrap();
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        // The recording's own folder goes, as when the editor closes.
        rec.delete();
        assert!(!tmp.path().join("rec").exists());
        assert_every_frame(&opened(&kept));
    }

    #[test]
    fn a_copy_is_made_when_a_link_cannot_be() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        let size = rec.save_with(&kept, no_link, copy_whole).unwrap().bytes;
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        assert_eq!(size, std::fs::metadata(tmp.path().join("rec").join(FRAMES_FILE)).unwrap().len());
        rec.delete();
        assert_every_frame(&opened(&kept));
    }

    #[test]
    fn a_copy_that_fails_half_way_leaves_no_frames_file_and_no_index() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        std::fs::write(kept.join("notes.txt"), "not this call's").unwrap();
        // The disk fills: half the bytes are written, then the copy fails.
        let half = |from: &Path, to: &Path| {
            let bytes = std::fs::read(from)?;
            std::fs::write(to, &bytes[..bytes.len() / 2])?;
            Err(io::Error::other("the disk is full"))
        };
        let err = rec.save_with(&kept, no_link, half).unwrap_err();
        assert!(err.contains("the disk is full"), "{err}");
        assert_eq!(names(&kept), ["notes.txt"]);
        assert_eq!(refused(&kept), OpenError::Missing);
        // A copy that ends without an error but short is not kept either.
        let short = |from: &Path, to: &Path| {
            let bytes = std::fs::read(from)?;
            std::fs::write(to, &bytes[..bytes.len() - 1])
        };
        assert!(rec.save_with(&kept, no_link, short).is_err());
        assert_eq!(names(&kept), ["notes.txt"]);
        // The recording itself is untouched.
        assert_every_frame(&rec);
    }

    #[test]
    fn a_frames_file_that_is_shorter_than_the_index_says_is_refused_as_damaged() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        let size = rec.save_with(&kept, no_link, copy_whole).unwrap().bytes;
        let frames = kept.join(FRAMES_FILE);
        std::fs::OpenOptions::new().write(true).open(&frames).unwrap().set_len(size - 1).unwrap();
        assert!(matches!(refused(&kept), OpenError::Damaged(_)));
        // Longer than the index says is not the file that was saved either.
        std::fs::OpenOptions::new().write(true).open(&frames).unwrap().set_len(size + 1).unwrap();
        assert!(matches!(refused(&kept), OpenError::Damaged(_)));
        std::fs::OpenOptions::new().write(true).open(&frames).unwrap().set_len(0).unwrap();
        assert!(matches!(refused(&kept), OpenError::Damaged(_)));
        // Nothing was removed for being damaged.
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
    }

    #[test]
    fn a_missing_frames_file_or_index_is_reported_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let (no_frames, no_index) = (folder(tmp.path(), "no-frames"), folder(tmp.path(), "no-index"));
        rec.save_into(&no_frames).unwrap();
        rec.save_into(&no_index).unwrap();
        std::fs::remove_file(no_frames.join(FRAMES_FILE)).unwrap();
        std::fs::remove_file(no_index.join(INDEX_FILE)).unwrap();
        assert_eq!(refused(&no_frames), OpenError::Missing);
        assert_eq!(refused(&no_index), OpenError::Missing);
        assert_eq!(refused(&tmp.path().join("never-made")), OpenError::Missing);
        assert_eq!(refused(&folder(tmp.path(), "empty")), OpenError::Missing);
        // What is left stays where it is.
        assert_eq!(names(&no_frames), [INDEX_FILE]);
        assert_eq!(names(&no_index), [FRAMES_FILE]);
    }

    #[test]
    fn an_index_from_a_newer_format_is_refused_and_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        rec.save_into(&kept).unwrap();
        // A newer version may have changed everything but the format number.
        let newer = r#"{"format": 3, "picture": {"w": 48}, "frames": "elsewhere"}"#;
        std::fs::write(kept.join(INDEX_FILE), newer).unwrap();
        assert_eq!(refused(&kept), OpenError::Newer(3));
        assert_eq!(std::fs::read_to_string(kept.join(INDEX_FILE)).unwrap(), newer);
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        // Newer is said even when the frames file is not where this version looks for it.
        std::fs::remove_file(kept.join(FRAMES_FILE)).unwrap();
        assert_eq!(refused(&kept), OpenError::Newer(3));
    }

    #[test]
    fn an_index_that_is_cut_off_or_does_not_fit_is_damaged_and_never_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        let size = rec.save_into(&kept).unwrap().bytes;
        let good = std::fs::read_to_string(kept.join(INDEX_FILE)).unwrap();
        let len0 = rec.frames[0].len;
        let rest = size - len0 as u64;
        let one = |w: u64, h: u64, end: u64, frames: &str| format!(r#"{{"format":1,"width":{w},"height":{h},"end_ms":{end},"frames":{frames}}}"#);
        let cases = [
            ("cut off", good[..good.len() / 2].to_string()),
            ("empty", String::new()),
            ("not json", "frames".to_string()),
            ("no format", r#"{"width":48,"height":32,"end_ms":0,"frames":[]}"#.to_string()),
            ("format 0", good.replacen("\"format\":1", "\"format\":0", 1)),
            ("no frames", one(48, 32, 0, "[]")),
            ("no width", one(0, 32, 10, &format!("[[0,{size},true,0,null,null]]"))),
            ("a picture too large to decode", one(70_000, 70_000, 10, &format!("[[0,{size},true,0,null,null]]"))),
            ("no keyframe first", one(48, 32, 10, &format!("[[0,{size},false,0,null,null]]"))),
            ("a gap between frames", one(48, 32, 10, &format!("[[0,{len0},true,0,null,null],[{},{},false,5,null,null]]", len0 + 1, rest - 1))),
            ("an offset that overflows", one(48, 32, 10, &format!("[[0,{len0},true,0,null,null],[{},4000000000,false,5,null,null]]", u64::MAX - 5))),
            ("an empty frame", one(48, 32, 10, &format!("[[0,{size},true,0,null,null],[{size},0,false,5,null,null]]"))),
            ("time running backwards", one(48, 32, 10, &format!("[[0,{len0},true,7,null,null],[{len0},{rest},false,5,null,null]]"))),
            ("an end before the last frame", one(48, 32, 4, &format!("[[0,{len0},true,0,null,null],[{len0},{rest},false,5,null,null]]"))),
            ("an end later than any recording", one(48, 32, u32::MAX as u64 + 1, &format!("[[0,{size},true,0,null,null]]"))),
            ("a frame later than any recording", one(48, 32, u64::MAX, &format!("[[0,{size},true,{},null,null]]", u32::MAX as u64 + 1))),
            ("a pointer right of the picture", one(48, 32, 10, &format!("[[0,{size},true,0,48,4]]"))),
            ("a pointer below the picture", one(48, 32, 10, &format!("[[0,{size},true,0,3,32]]"))),
            ("a pointer too far left", one(48, 32, 10, &format!("[[0,{size},true,0,-33,4]]"))),
            ("a pointer too far up", one(48, 32, 10, &format!("[[0,{size},true,0,3,-33]]"))),
            ("a pointer far outside", one(48, 32, 10, &format!("[[0,{size},true,0,{},{}]]", i32::MAX, i32::MIN))),
        ];
        for (what, text) in cases {
            std::fs::write(kept.join(INDEX_FILE), text).unwrap();
            assert!(matches!(refused(&kept), OpenError::Damaged(_)), "{what}");
        }
        // The same checks pass an index that does fit.
        std::fs::write(kept.join(INDEX_FILE), one(48, 32, 10, &format!("[[0,{len0},true,0,null,null],[{len0},{rest},false,5,3,4]]"))).unwrap();
        assert_eq!(opened(&kept).frames.len(), 2);
        // The longest time and the outermost pointer positions the recorder writes are fine.
        let edge = one(48, 32, u32::MAX as u64, &format!("[[0,{len0},true,0,-32,-32],[{len0},{rest},false,{},47,31]]", u32::MAX));
        std::fs::write(kept.join(INDEX_FILE), edge).unwrap();
        let rec = opened(&kept);
        assert_eq!((rec.frames[0].cursor, rec.frames[1].cursor, rec.end_ms), (Some((-32, -32)), Some((47, 31)), u32::MAX as u64));
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
    }

    #[test]
    fn an_index_that_cannot_be_put_in_a_new_folder_takes_the_new_frames_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        // Something that is not a file has the index's name, so the rename onto it fails.
        std::fs::create_dir(kept.join(INDEX_FILE)).unwrap();
        let err = rec.save_into(&kept).unwrap_err();
        assert!(err.contains("index"), "{err}");
        assert_eq!(names(&kept), [INDEX_FILE]);
        assert!(kept.join(INDEX_FILE).is_dir());
        assert_every_frame(&rec);
    }

    #[cfg(windows)]
    #[test]
    fn an_index_that_cannot_be_replaced_leaves_the_recording_that_was_there_whole() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let first = recorded(tmp.path());
        let other = recorded_as(tmp.path(), "other", 5);
        let kept = folder(tmp.path(), "kept");
        first.save_into(&kept).unwrap();
        let (frames, index) = (std::fs::read(kept.join(FRAMES_FILE)).unwrap(), std::fs::read(kept.join(INDEX_FILE)).unwrap());
        // Another program holds the index and shares it with nobody.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(kept.join(INDEX_FILE)).unwrap();
        assert!(other.save_into(&kept).is_err());
        drop(held);
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        assert_eq!(std::fs::read(kept.join(FRAMES_FILE)).unwrap(), frames);
        assert_eq!(std::fs::read(kept.join(INDEX_FILE)).unwrap(), index);
        assert_every_frame(&opened(&kept));
        // Once the index is free the other recording takes the folder.
        other.save_into(&kept).unwrap();
        assert_eq!(opened(&kept).frames.len(), 5);
    }

    #[test]
    fn a_save_cut_off_between_the_frames_and_the_index_is_never_taken_for_a_recording() {
        let tmp = tempfile::tempdir().unwrap();
        let first = recorded(tmp.path());
        let other = recorded_as(tmp.path(), "other", 5);
        let kept = folder(tmp.path(), "kept");
        first.save_into(&kept).unwrap();
        // The save of another recording gets as far as its frames, and stops there.
        let expected = other.frames_end().unwrap();
        let staged = other.stage(&kept, expected, no_link, copy_whole).unwrap();
        assert_every_frame(&opened(&kept));
        staged.swap_frames().unwrap();
        // The first recording's index is not left beside frames it does not describe,
        // whatever their size: there is no index at all.
        assert!(!kept.join(INDEX_FILE).exists());
        assert_eq!(refused(&kept), OpenError::Missing);
        // The second half makes it the other recording, whole.
        staged.swap_index().unwrap();
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        let again = opened(&kept);
        assert_eq!(again.frames.len(), 5);
        assert_eq!(again.frame(4).unwrap().into_raw(), synthetic(4, W, H));
    }

    #[test]
    fn a_recording_open_in_a_reader_is_kept_and_outlives_its_temp_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = Arc::new(recorded(tmp.path()));
        // The editor holds the frames file open the whole time.
        let mut reader = FrameReader::new(rec.clone()).unwrap();
        assert_eq!(reader.get(3).unwrap().into_raw(), synthetic(3, W, H));
        let kept = folder(tmp.path(), "kept");
        rec.save_with(&kept, |from, to| std::fs::hard_link(from, to), no_copy).unwrap();
        // The editor closes: the temp files go while the reader still has its handle.
        rec.delete_files();
        let again = opened(&kept);
        assert_eq!(again.frame(N - 1).unwrap().into_raw(), synthetic(N - 1, W, H));
        assert_every_frame(&again);
        // The reader's own handle still reads what it had open.
        assert_eq!(reader.get(4).unwrap().into_raw(), synthetic(4, W, H));
    }

    #[test]
    fn deleting_an_opened_recording_never_removes_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        rec.save_into(&kept).unwrap();
        rec.delete();
        let shared = Arc::new(opened(&kept));
        shared.delete_files();
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        opened(&kept).delete();
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        assert_every_frame(&shared);
    }

    #[test]
    fn saving_into_its_own_folder_only_rewrites_the_index() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        let size = rec.save_into(&kept).unwrap().bytes;
        rec.delete();
        let again = opened(&kept);
        let index = std::fs::read(kept.join(INDEX_FILE)).unwrap();
        std::fs::remove_file(kept.join(INDEX_FILE)).unwrap();
        // Neither a link nor a copy is asked for; the frames file stays as it is.
        let no_link = |_: &Path, _: &Path| -> io::Result<()> { panic!("a link was made") };
        assert_eq!(again.save_with(&kept, no_link, no_copy), Ok(as_frames(size)));
        assert_eq!(std::fs::read(kept.join(INDEX_FILE)).unwrap(), index);
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        assert_every_frame(&opened(&kept));
        // The folder named another way is still the same folder.
        assert_eq!(again.save_with(&kept.join("..").join("kept"), no_link, no_copy), Ok(as_frames(size)));
        // Into a second folder, an opened recording is kept like any other.
        let second = folder(tmp.path(), "second");
        assert_eq!(again.save_into(&second), Ok(as_frames(size)));
        assert_every_frame(&opened(&second));
    }

    #[test]
    fn saving_needs_a_folder_that_exists_and_a_recording_with_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        assert!(rec.save_into(&tmp.path().join("never-made")).is_err());
        assert!(!tmp.path().join("never-made").exists());
        let kept = folder(tmp.path(), "kept");
        assert!(Recording::timing_only(W, H, &[0, 100], 200).save_into(&kept).is_err());
        assert!(Recording::timing_only(W, H, &[], 0).save_into(&kept).is_err());
        assert!(names(&kept).is_empty());
    }

    #[test]
    fn saving_over_frames_that_are_already_there_replaces_them_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        rec.save_into(&kept).unwrap();
        // The same recording saved again from its temp folder, by link and by copy.
        let size = rec.save_into(&kept).unwrap().bytes;
        assert_eq!(rec.save_with(&kept, no_link, copy_whole), Ok(as_frames(size)));
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        // A save that fails leaves the frames that were there before it.
        assert!(rec.save_with(&kept, no_link, |_, _| Err(io::Error::other("the disk is full"))).is_err());
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        assert_every_frame(&opened(&kept));
    }

    #[test]
    fn a_copy_can_be_stopped_part_way_and_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        let no_link = |_: &Path, _: &Path| -> io::Result<()> { Err(io::Error::other("links are not supported here")) };
        // Asked before every 64 bytes; the third answer is no.
        let asked = std::cell::Cell::new(0);
        let stop_at_three = || {
            asked.set(asked.get() + 1);
            asked.get() < 3
        };
        let err = rec.save_while(&kept, no_link, 64, &stop_at_three).unwrap_err();
        assert!(matches!(err, SaveError::Cancelled), "{err:?}");
        assert_eq!(asked.get(), 3, "the copy stopped where it was told to");
        assert_eq!(names(&kept), Vec::<String>::new());
        // Never told to stop: the same copy, piece by piece, is whole.
        let size = rec.save_while(&kept, no_link, 64, &|| true).unwrap().bytes;
        assert_eq!(std::fs::read(kept.join(FRAMES_FILE)).unwrap().len() as u64, size);
        let again = Recording::open_saved(&kept).unwrap();
        assert_eq!(again.frame(N - 1).unwrap(), rec.frame(N - 1).unwrap());
    }

    #[test]
    fn a_link_is_never_cancelled_and_a_real_failure_says_which_file_and_what_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        // A link is made at once: the check is not even asked.
        let kept = folder(tmp.path(), "kept");
        assert!(rec.save_into_with(&kept, &|| panic!("a link needs no check")).is_ok());
        // The recording's own frames file is gone: neither a link nor a copy can be made.
        let gone = recorded_as(tmp.path(), "gone", 4);
        std::fs::remove_file(tmp.path().join("gone").join(FRAMES_FILE)).unwrap();
        let other = folder(tmp.path(), "other");
        match gone.save_into_with(&other, &|| true) {
            Err(SaveError::Io { step: SaveStep::Frames, error, .. }) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
            other => panic!("{other:?}"),
        }
        assert_eq!(names(&other), Vec::<String>::new());
        // What `save_into` says is what it always said.
        let said = gone.save_into(&other).unwrap_err();
        assert!(said.starts_with("could not keep the frames: "), "{said}");
        assert!(matches!(rec.save_into_with(&tmp.path().join("never-made"), &|| true), Err(SaveError::Unusable("the folder to keep the recording in does not exist"))));
    }

    // ---- the video form: its index, expanding it, and keeping an expanded recording ----

    const SKIPPED: &str = "skipped: ffmpeg with libx264 was not found on this computer";

    /// A codec for these tests: its "video" is the pictures' bytes one after the other. Like
    /// the real one it hands on the pictures it finds before it knows whether their number
    /// is right, and it answers the real one's errors.
    struct Fake {
        /// What a decode answers once it has handed on the pictures, instead of success.
        fails: Option<fn() -> CodecError>,
        /// Never asks `keep_going` itself.
        quiet: bool,
        decodes: AtomicUsize,
    }

    impl Fake {
        fn new() -> Fake {
            Fake { fails: None, quiet: false, decodes: AtomicUsize::new(0) }
        }

        fn quiet() -> Fake {
            Fake { quiet: true, ..Fake::new() }
        }

        fn failing(with: fn() -> CodecError) -> Fake {
            Fake { fails: Some(with), ..Fake::new() }
        }

        fn decodes(&self) -> usize {
            self.decodes.load(Ordering::Relaxed)
        }
    }

    impl VideoCodec for Fake {
        fn id(&self) -> &str {
            "raw-for-tests"
        }

        fn encode(&self, size: (u32, u32), frames: usize, next: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, out: &Path, _max_bytes: u64, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            let mut all = Vec::new();
            for done in 0..frames {
                let mut buf = Vec::new();
                if !next(&mut buf).map_err(CodecError::Caller)? {
                    return Err(CodecError::WrongFrames { got: done });
                }
                assert_eq!(buf.len(), (size.0 * size.1 * 4) as usize);
                all.extend_from_slice(&buf);
                if !keep_going(done + 1) {
                    return Err(CodecError::Cancelled);
                }
            }
            std::fs::write(out, all).map_err(CodecError::Io)
        }

        fn decode(&self, video: &Path, size: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            self.decodes.fetch_add(1, Ordering::Relaxed);
            let bytes = std::fs::read(video).map_err(|_| CodecError::Failed { exit: Some(1) })?;
            let len = (size.0 * size.1 * 4) as usize;
            let whole = bytes.len() / len;
            for i in 0..frames.min(whole) {
                each(i, &bytes[i * len..(i + 1) * len]).map_err(CodecError::Caller)?;
                if !self.quiet && !keep_going(i + 1) {
                    return Err(CodecError::Cancelled);
                }
            }
            if let Some(fails) = self.fails {
                return Err(fails());
            }
            match bytes.len() {
                n if n < frames * len => Err(CodecError::WrongFrames { got: whole }),
                n if n > frames * len => Err(CodecError::WrongFrames { got: frames + 1 }),
                _ => Ok(()),
            }
        }
    }

    /// The fake codec's video of `rec` with `pictures` pictures (the last frame again for any
    /// past the end), at `to`. Returns its size.
    fn raw_video(rec: &Recording, to: &Path, pictures: usize) -> u64 {
        let mut bytes = Vec::new();
        for i in 0..pictures {
            bytes.extend_from_slice(rec.frame(i.min(rec.frames.len() - 1)).unwrap().as_raw());
        }
        std::fs::write(to, &bytes).unwrap();
        bytes.len() as u64
    }

    /// What the index of `rec` holds, read straight from the recording.
    fn index_of(rec: &Recording, video_bytes: Option<u64>) -> VideoIndex {
        VideoIndex { width: rec.width, height: rec.height, end_ms: rec.end_ms, times: rec.frames.iter().map(|m| (m.t_ms, m.cursor)).collect(), video_bytes }
    }

    /// A recording, its fake video `<root>/frames.mkv` and that video's size.
    fn with_video(root: &Path) -> (Recording, PathBuf, u64) {
        let rec = recorded(root);
        let video = root.join(VIDEO_FILE);
        let bytes = raw_video(&rec, &video, N);
        (rec, video, bytes)
    }

    fn always(_: usize, _: usize) -> bool {
        true
    }

    fn no_room_error() -> io::Error {
        io::Error::from_raw_os_error(if cfg!(windows) { 112 } else { 28 })
    }

    #[test]
    fn a_video_index_round_trips_every_time_pointer_and_the_end() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        rec.write_video_index(&kept, 4321).unwrap();
        // The index alone, and no temporary file beside it.
        assert_eq!(names(&kept), [INDEX_FILE]);
        let index = open_video_index(&kept).unwrap();
        assert_eq!(index, index_of(&rec, Some(4321)));
        assert_eq!((index.width, index.height, index.end_ms, index.times.len()), (W, H, N as u64 * 66, N));
        assert_eq!((index.times[0], index.times[1]), ((0, None), (66, Some((-4, 7)))));

        // The documented shape: the video's size, rate and codec, and one row of three per frame.
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(kept.join(INDEX_FILE)).unwrap()).unwrap();
        assert_eq!(json["format"], 2);
        assert_eq!((json["width"].as_u64(), json["height"].as_u64(), json["end_ms"].as_u64()), (Some(W as u64), Some(H as u64), Some(N as u64 * 66)));
        assert_eq!(json["video"], serde_json::json!({"bytes": 4321, "fps": 25, "codec": "x264-444-crf10-medium-g30"}));
        assert_eq!(json["frames"].as_array().unwrap().len(), N);
        assert_eq!(json["frames"][0], serde_json::json!([0, null, null]));
        assert_eq!(json["frames"][1], serde_json::json!([66, -4, 7]));

        // Written again it is replaced whole.
        rec.write_video_index(&kept, 5000).unwrap();
        assert_eq!(open_video_index(&kept).unwrap(), index_of(&rec, Some(5000)));
        assert_eq!(names(&kept), [INDEX_FILE]);

        // It needs a folder, frames, and a video that has a size.
        assert!(matches!(rec.write_video_index(&tmp.path().join("never-made"), 1), Err(SaveError::Io { step: SaveStep::Index, .. })));
        assert!(!tmp.path().join("never-made").exists());
        assert!(matches!(Recording::timing_only(W, H, &[], 0).write_video_index(&kept, 1), Err(SaveError::Unusable(_))));
        assert!(matches!(rec.write_video_index(&kept, 0), Err(SaveError::Unusable(_))));
        assert_eq!(open_video_index(&kept).unwrap(), index_of(&rec, Some(5000)));
    }

    #[test]
    fn a_format_1_index_reads_as_a_video_index_with_the_same_times() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        rec.save_into(&kept).unwrap();
        let before = std::fs::read(kept.join(INDEX_FILE)).unwrap();
        let index = open_video_index(&kept).unwrap();
        // The same times, pointers and end, and no video size: the index names none.
        assert_eq!(index, index_of(&rec, None));
        assert!(rec.same_timing(&index));
        // The frames file is not read: what a swap cut off between two steps leaves still reads.
        std::fs::remove_file(kept.join(FRAMES_FILE)).unwrap();
        assert_eq!(open_video_index(&kept).unwrap(), index_of(&rec, None));
        assert_eq!(std::fs::read(kept.join(INDEX_FILE)).unwrap(), before);
        assert_eq!(names(&kept), [INDEX_FILE]);
    }

    #[test]
    fn the_timing_is_the_same_only_when_the_size_the_end_and_every_time_and_pointer_are() {
        let rec = Recording::timing_only(100, 80, &[0, 100, 250], 400);
        let same = || VideoIndex { width: 100, height: 80, end_ms: 400, times: vec![(0, None), (100, None), (250, None)], video_bytes: Some(9) };
        assert!(rec.same_timing(&same()));
        // The video's size is not timing.
        assert!(rec.same_timing(&VideoIndex { video_bytes: None, ..same() }));
        assert!(!rec.same_timing(&VideoIndex { width: 101, ..same() }));
        assert!(!rec.same_timing(&VideoIndex { height: 81, ..same() }));
        assert!(!rec.same_timing(&VideoIndex { end_ms: 401, ..same() }));
        assert!(!rec.same_timing(&VideoIndex { times: vec![(0, None), (100, None)], ..same() }));
        assert!(!rec.same_timing(&VideoIndex { times: vec![(0, None), (100, None), (250, None), (300, None)], ..same() }));
        assert!(!rec.same_timing(&VideoIndex { times: vec![(0, None), (101, None), (250, None)], ..same() }));
        assert!(!rec.same_timing(&VideoIndex { times: vec![(0, None), (100, Some((1, 2))), (250, None)], ..same() }));
    }

    #[test]
    fn a_video_index_from_a_newer_format_is_refused_and_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        rec.write_video_index(&kept, 4321).unwrap();
        // A newer version may have changed everything but the format number.
        let newer = r#"{"format": 3, "picture": {"w": 48}, "video": "elsewhere", "frames": 7}"#;
        std::fs::write(kept.join(INDEX_FILE), newer).unwrap();
        assert_eq!(open_video_index(&kept), Err(OpenError::Newer(3)));
        assert_eq!(form_of(&kept), Err(OpenError::Newer(3)));
        assert_eq!(refused(&kept), OpenError::Newer(3));
        let far = r#"{"format": 4000000000}"#;
        std::fs::write(kept.join(INDEX_FILE), far).unwrap();
        assert_eq!(open_video_index(&kept), Err(OpenError::Newer(4_000_000_000)));
        assert_eq!(std::fs::read_to_string(kept.join(INDEX_FILE)).unwrap(), far);
        assert_eq!(names(&kept), [INDEX_FILE]);
    }

    #[test]
    fn a_damaged_video_index_is_refused_with_the_same_limits_as_a_frames_index() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        rec.write_video_index(&kept, 4321).unwrap();
        let good = std::fs::read_to_string(kept.join(INDEX_FILE)).unwrap();
        let two = |w: u64, h: u64, end: u64, bytes: &str, frames: &str| format!(r#"{{"format":2,"width":{w},"height":{h},"end_ms":{end},"video":{{"bytes":{bytes},"fps":25,"codec":"x"}},"frames":{frames}}}"#);
        let one = |w: u64, h: u64, end: u64, frames: &str| format!(r#"{{"format":1,"width":{w},"height":{h},"end_ms":{end},"frames":{frames}}}"#);
        let late = u32::MAX as u64 + 1;
        let cases = [
            ("cut off", good[..good.len() / 2].to_string()),
            ("empty", String::new()),
            ("not json", "frames".to_string()),
            ("no format", r#"{"width":48,"height":32,"end_ms":0,"video":{"bytes":9},"frames":[[0,null,null]]}"#.to_string()),
            ("format 0", good.replacen("\"format\":2", "\"format\":0", 1)),
            ("no frames", two(48, 32, 0, "9", "[]")),
            ("no width", two(0, 32, 10, "9", "[[0,null,null]]")),
            ("no height", two(48, 0, 10, "9", "[[0,null,null]]")),
            ("a picture too large to decode", two(70_000, 70_000, 10, "9", "[[0,null,null]]")),
            ("a side too long", two(16_385, 1, 10, "9", "[[0,null,null]]")),
            ("too many pixels on sides that fit", two(16_384, 16_384, 10, "9", "[[0,null,null]]")),
            ("time running backwards", two(48, 32, 10, "9", "[[7,null,null],[5,null,null]]")),
            ("an end before the last frame", two(48, 32, 4, "9", "[[0,null,null],[5,null,null]]")),
            ("an end later than any recording", two(48, 32, late, "9", "[[0,null,null]]")),
            ("a frame later than any recording", two(48, 32, u64::MAX, "9", &format!("[[{late},null,null]]"))),
            ("a pointer right of the picture", two(48, 32, 10, "9", "[[0,48,4]]")),
            ("a pointer below the picture", two(48, 32, 10, "9", "[[0,3,32]]")),
            ("a pointer too far left", two(48, 32, 10, "9", "[[0,-33,4]]")),
            ("a pointer too far up", two(48, 32, 10, "9", "[[0,3,-33]]")),
            ("a pointer far outside", two(48, 32, 10, "9", &format!("[[0,{},{}]]", i32::MAX, i32::MIN))),
            ("no video", r#"{"format":2,"width":48,"height":32,"end_ms":10,"frames":[[0,null,null]]}"#.to_string()),
            ("a video without a size", r#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"fps":25},"frames":[[0,null,null]]}"#.to_string()),
            ("a video of no bytes", two(48, 32, 10, "0", "[[0,null,null]]")),
            ("a video of fewer bytes than frames", two(48, 32, 10, "2", "[[0,null,null],[1,null,null],[2,null,null]]")),
            ("a video of a negative size", two(48, 32, 10, "-9", "[[0,null,null]]")),
            ("rows of a frames index in a video index", two(48, 32, 10, "9", "[[0,5,true,0,null,null]]")),
            ("rows of a video index in a frames index", one(48, 32, 10, "[[0,null,null]]")),
            ("a frames index: no frames", one(48, 32, 0, "[]")),
            ("a frames index: a picture too large", one(70_000, 70_000, 10, "[[0,5,true,0,null,null]]")),
            ("a frames index: time running backwards", one(48, 32, 10, "[[0,5,true,7,null,null],[5,5,false,5,null,null]]")),
            ("a frames index: an end before the last frame", one(48, 32, 4, "[[0,5,true,0,null,null],[5,5,false,5,null,null]]")),
            ("a frames index: an end later than any recording", one(48, 32, late, "[[0,5,true,0,null,null]]")),
            ("a frames index: a pointer right of the picture", one(48, 32, 10, "[[0,5,true,0,48,4]]")),
            ("a frames index: a pointer too far up", one(48, 32, 10, "[[0,5,true,0,3,-33]]")),
        ];
        for (what, text) in cases {
            std::fs::write(kept.join(INDEX_FILE), text).unwrap();
            assert!(matches!(open_video_index(&kept), Err(OpenError::Damaged(_))), "{what}");
        }
        // The same checks pass an index that does fit, in either format.
        std::fs::write(kept.join(INDEX_FILE), two(48, 32, 10, "9", "[[0,null,null],[5,3,4]]")).unwrap();
        assert_eq!(open_video_index(&kept).unwrap(), VideoIndex { width: 48, height: 32, end_ms: 10, times: vec![(0, None), (5, Some((3, 4)))], video_bytes: Some(9) });
        std::fs::write(kept.join(INDEX_FILE), one(48, 32, 10, "[[0,5,true,0,null,null],[5,5,false,5,3,4]]")).unwrap();
        assert_eq!(open_video_index(&kept).unwrap(), VideoIndex { width: 48, height: 32, end_ms: 10, times: vec![(0, None), (5, Some((3, 4)))], video_bytes: None });
        // The largest picture, the longest time and the outermost pointer positions are fine.
        let edge = two(16_384, 4_096, u32::MAX as u64, "2", &format!("[[0,-32,-32],[{},16383,4095]]", u32::MAX));
        std::fs::write(kept.join(INDEX_FILE), edge).unwrap();
        let index = open_video_index(&kept).unwrap();
        assert_eq!((index.times[0], index.times[1], index.end_ms), ((0, Some((-32, -32))), (u32::MAX as u64, Some((16_383, 4_095))), u32::MAX as u64));
        // A pointer with only one of its two numbers is no pointer, as in a frames index.
        std::fs::write(kept.join(INDEX_FILE), two(48, 32, 10, "9", "[[0,3,null],[5,null,4]]")).unwrap();
        assert_eq!(open_video_index(&kept).unwrap().times, [(0, None), (5, None)]);
        // A missing index is missing, and nothing was changed or removed.
        assert_eq!(names(&kept), [INDEX_FILE]);
        assert_eq!(open_video_index(&folder(tmp.path(), "empty")), Err(OpenError::Missing));
        assert_eq!(open_video_index(&tmp.path().join("never-made")), Err(OpenError::Missing));
    }

    #[test]
    fn the_form_of_a_folder_is_read_without_reading_the_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let (frames, video) = (folder(tmp.path(), "frames"), folder(tmp.path(), "video"));
        rec.save_into(&frames).unwrap();
        assert_eq!(form_of(&frames), Ok(Form::Frames));
        // Neither the frames file nor the rest of the index is looked at.
        std::fs::remove_file(frames.join(FRAMES_FILE)).unwrap();
        assert_eq!(form_of(&frames), Ok(Form::Frames));
        std::fs::write(frames.join(INDEX_FILE), r#"{"format":1,"frames":"elsewhere"}"#).unwrap();
        assert_eq!(form_of(&frames), Ok(Form::Frames));

        rec.write_video_index(&video, 4321).unwrap();
        assert_eq!(form_of(&video), Ok(Form::Video));
        std::fs::write(video.join(INDEX_FILE), r#"{"width":"wide","format":2}"#).unwrap();
        assert_eq!(form_of(&video), Ok(Form::Video));
        // A frames file left beside a video index does not change what the index says.
        std::fs::write(video.join(FRAMES_FILE), b"left over").unwrap();
        assert_eq!(form_of(&video), Ok(Form::Video));

        for (what, text) in [("empty", ""), ("not json", "frames"), ("no format", r#"{"width":48}"#), ("format 0", r#"{"format":0}"#), ("a format that is no number", r#"{"format":"2"}"#), ("a negative format", r#"{"format":-1}"#)] {
            std::fs::write(video.join(INDEX_FILE), text).unwrap();
            assert!(matches!(form_of(&video), Err(OpenError::Damaged(_))), "{what}");
        }
        assert_eq!(form_of(&folder(tmp.path(), "empty")), Err(OpenError::Missing));
        assert_eq!(form_of(&tmp.path().join("never-made")), Err(OpenError::Missing));
        assert_eq!(names(&video), [FRAMES_FILE, INDEX_FILE]);
    }

    #[test]
    fn open_saved_says_video_for_a_video_form_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let kept = folder(tmp.path(), "kept");
        std::fs::copy(&video, kept.join(VIDEO_FILE)).unwrap();
        rec.write_video_index(&kept, bytes).unwrap();
        assert_eq!(refused(&kept), OpenError::Video);
        // Also while the frames file of an unfinished swap still lies beside it, and whatever
        // the rest of the index holds: nothing is opened as frames on the word of a video index.
        std::fs::copy(tmp.path().join("rec").join(FRAMES_FILE), kept.join(FRAMES_FILE)).unwrap();
        assert_eq!(refused(&kept), OpenError::Video);
        let index = std::fs::read(kept.join(INDEX_FILE)).unwrap();
        std::fs::write(kept.join(INDEX_FILE), r#"{"format":2,"frames":"elsewhere"}"#).unwrap();
        assert_eq!(refused(&kept), OpenError::Video);
        std::fs::write(kept.join(INDEX_FILE), &index).unwrap();
        // Nothing was changed or removed.
        assert_eq!(names(&kept), [VIDEO_FILE, FRAMES_FILE, INDEX_FILE]);
        assert_eq!(std::fs::metadata(kept.join(VIDEO_FILE)).unwrap().len(), bytes);
    }

    /// What every index that was taken must hold, whatever it was read from.
    fn assert_within_limits(index: &VideoIndex, what: &str) {
        assert!((1..=MAX_SIDE).contains(&index.width) && (1..=MAX_SIDE).contains(&index.height), "{what}");
        assert!(index.width as u64 * index.height as u64 <= MAX_PIXELS, "{what}");
        assert!(index.end_ms <= MAX_MS && !index.times.is_empty() && index.times.len() <= MAX_FRAMES, "{what}");
        assert!(index.times.windows(2).all(|pair| pair[0].0 <= pair[1].0) && index.times.last().unwrap().0 <= index.end_ms, "{what}");
        assert!(index.video_bytes.is_none_or(|bytes| bytes >= index.times.len() as u64), "{what}");
        let fits = |c: &(i32, i32)| (-POINTER_MARGIN..index.width as i32).contains(&c.0) && (-POINTER_MARGIN..index.height as i32).contains(&c.1);
        assert!(index.times.iter().filter_map(|(_, pointer)| pointer.as_ref()).all(fits), "{what}");
    }

    #[test]
    fn a_hostile_video_index_never_panics() {
        let tmp = tempfile::tempdir().unwrap();
        let kept = folder(tmp.path(), "kept");
        let rec = recorded_as(tmp.path(), "short", 4);
        rec.write_video_index(&kept, 4321).unwrap();
        let good_video = std::fs::read(kept.join(INDEX_FILE)).unwrap();
        rec.save_into(&kept).unwrap();
        let good_frames = std::fs::read(kept.join(INDEX_FILE)).unwrap();
        assert_within_limits(&read_video_index(&good_video).unwrap(), "the video index itself");
        assert_within_limits(&read_video_index(&good_frames).unwrap(), "the frames index itself");

        let nested = |open: &str, depth: usize| open.repeat(depth);
        let hostile: Vec<(&str, Vec<u8>)> = vec![
            ("nothing", Vec::new()),
            ("one brace", b"{".to_vec()),
            ("null", b"null".to_vec()),
            ("a number", b"2".to_vec()),
            ("a list", b"[2]".to_vec()),
            ("a text", b"\"format\"".to_vec()),
            ("zero bytes", vec![0; 64]),
            ("bytes that are no text", vec![0xff, 0xfe, 0x00, 0xc3, 0x28]),
            ("a byte order mark", [&[0xef, 0xbb, 0xbf][..], &good_video[..]].concat()),
            ("lists nested deeply", nested("[", 200_000).into_bytes()),
            ("objects nested deeply", nested("{\"format\":", 200_000).into_bytes()),
            ("a deep nest where the frames are", format!(r#"{{"format":2,"width":48,"height":32,"end_ms":10,"video":{{"bytes":9}},"frames":{}}}"#, nested("[", 100_000)).into_bytes()),
            ("a deep nest where the width is", format!(r#"{{"format":2,"width":{}"#, nested("{\"a\":", 100_000)).into_bytes()),
            ("a deep nest before the format", format!(r#"{{"junk":{}1{},"format":2}}"#, nested("[", 5_000), nested("]", 5_000)).into_bytes()),
            ("the format twice", br#"{"format":1,"format":2,"width":48,"height":32,"end_ms":10,"frames":[]}"#.to_vec()),
            ("a format past every number", br#"{"format":99999999999999999999999999999999999999}"#.to_vec()),
            ("a format with an exponent", br#"{"format":1e999}"#.to_vec()),
            ("a fraction of a format", br#"{"format":2.5}"#.to_vec()),
            ("a negative format", br#"{"format":-2}"#.to_vec()),
            ("a format that is a list", br#"{"format":[2]}"#.to_vec()),
            ("a huge width", br#"{"format":2,"width":4294967295,"height":4294967295,"end_ms":10,"video":{"bytes":9},"frames":[[0,null,null]]}"#.to_vec()),
            ("a width past every number", br#"{"format":2,"width":18446744073709551616,"height":32,"end_ms":10,"video":{"bytes":9},"frames":[[0,null,null]]}"#.to_vec()),
            ("a negative width", br#"{"format":2,"width":-48,"height":32,"end_ms":10,"video":{"bytes":9},"frames":[[0,null,null]]}"#.to_vec()),
            ("a width that is a text", br#"{"format":2,"width":"48","height":32,"end_ms":10,"video":{"bytes":9},"frames":[[0,null,null]]}"#.to_vec()),
            ("an end past every number", br#"{"format":2,"width":48,"height":32,"end_ms":1e400,"video":{"bytes":9},"frames":[[0,null,null]]}"#.to_vec()),
            ("the longest end and the last time", br#"{"format":2,"width":48,"height":32,"end_ms":18446744073709551615,"video":{"bytes":9},"frames":[[18446744073709551615,null,null]]}"#.to_vec()),
            ("a negative time", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":9},"frames":[[-1,null,null]]}"#.to_vec()),
            ("a video that is a number", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":9,"frames":[[0,null,null]]}"#.to_vec()),
            ("a codec that is a list", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":9,"codec":[1]},"frames":[[0,null,null]]}"#.to_vec()),
            ("frames that are a text", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":9},"frames":"[[0,null,null]]"}"#.to_vec()),
            ("frames that are an object", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":9},"frames":{"0":[0,null,null]}}"#.to_vec()),
            ("a row too short", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":9},"frames":[[0,null]]}"#.to_vec()),
            ("a row too long", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":9},"frames":[[0,null,null,null]]}"#.to_vec()),
            ("a pointer past every number", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":9},"frames":[[0,2147483648,-2147483649]]}"#.to_vec()),
            ("a pointer that is a fraction", br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":9},"frames":[[0,1.5,2]]}"#.to_vec()),
            ("more frames than the video has bytes", format!(r#"{{"format":2,"width":48,"height":32,"end_ms":10,"video":{{"bytes":3}},"frames":[{}[0,null,null]]}}"#, "[0,null,null],".repeat(50_000)).into_bytes()),
            ("a frames index with a negative length", br#"{"format":1,"width":48,"height":32,"end_ms":10,"frames":[[0,-5,true,0,null,null]]}"#.to_vec()),
            ("a frames index whose rows are numbers", br#"{"format":1,"width":48,"height":32,"end_ms":10,"frames":[1,2,3]}"#.to_vec()),
        ];
        // Odd and still a recording: the largest video size, a rate and a codec nobody reads.
        std::fs::write(kept.join(INDEX_FILE), br#"{"format":2,"width":48,"height":32,"end_ms":10,"video":{"bytes":18446744073709551615,"fps":4294967295,"codec":""},"frames":[[0,null,null]]}"#).unwrap();
        assert_within_limits(&open_video_index(&kept).unwrap(), "the largest video");
        for (what, text) in &hostile {
            std::fs::write(kept.join(INDEX_FILE), text).unwrap();
            // Whatever is answered, it is an answer; none of these is a recording.
            assert!(open_video_index(&kept).is_err(), "{what}");
            assert!(Recording::open_saved(&kept).is_err(), "{what}");
            if let Ok(form) = form_of(&kept) {
                assert!(matches!(form, Form::Frames | Form::Video), "{what}");
            }
        }
        // A folder where the index should be is not readable, and not a panic.
        std::fs::remove_file(kept.join(INDEX_FILE)).unwrap();
        std::fs::create_dir(kept.join(INDEX_FILE)).unwrap();
        assert!(matches!(open_video_index(&kept), Err(OpenError::Damaged(_) | OpenError::Missing)));
        assert!(matches!(form_of(&kept), Err(OpenError::Damaged(_) | OpenError::Missing)));

        // Every index cut off anywhere is refused, and so is its form unless the format was
        // already read whole... which a cut-off text never is: it is not JSON.
        for good in [&good_video, &good_frames] {
            for cut in 0..good.len() {
                assert!(matches!(read_video_index(&good[..cut]), Err(OpenError::Damaged(_))), "cut at {cut}");
                assert!(matches!(form_in(&good[..cut]), Err(OpenError::Damaged(_))), "cut at {cut}");
            }
            // One byte changed anywhere: an answer, and what is taken is within the limits.
            for at in 0..good.len() {
                for byte in [b'9', b'-', b'"', b'[', b']', b'{', b'}', b',', b'e', b'.', b'0', b' ', 0x00, 0xff] {
                    let mut text = good.to_vec();
                    text[at] = byte;
                    let _ = form_in(&text);
                    if let Ok(index) = read_video_index(&text) {
                        assert_within_limits(&index, &format!("byte {at} as {byte}"));
                    }
                }
                // And one byte taken out or put in twice.
                let (mut without, mut twice) = (good.to_vec(), good.to_vec());
                without.remove(at);
                twice.insert(at, good[at]);
                for text in [without, twice] {
                    let _ = form_in(&text);
                    if let Ok(index) = read_video_index(&text) {
                        assert_within_limits(&index, &format!("byte {at} dropped or doubled"));
                    }
                }
            }
        }
    }

    #[test]
    fn an_expanded_recording_has_the_index_times_and_decodes_every_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let index = index_of(&rec, Some(bytes));
        let into = folder(tmp.path(), "edit");
        // A codec that never asks whether to go on: the expansion asks by itself.
        let asked = std::cell::RefCell::new(Vec::new());
        let expanded = expand_video(&index, &video, &into, &Fake::quiet(), &|done, total| {
            asked.borrow_mut().push((done, total));
            true
        })
        .unwrap();
        assert_eq!(*asked.borrow(), (0..N).map(|done| (done, N)).collect::<Vec<_>>(), "asked once before every frame");
        assert_eq!(names(&into), [FRAMES_FILE]);

        // The index's size, end, times and pointers, and whole frames where the recorder puts them.
        assert_eq!((expanded.width, expanded.height, expanded.end_ms, expanded.frames.len()), (W, H, N as u64 * 66, N));
        for (i, (m, (t_ms, cursor))) in expanded.frames.iter().zip(&index.times).enumerate() {
            assert_eq!((m.t_ms, m.cursor, m.key), (*t_ms, *cursor, i % KEYFRAME_EVERY == 0), "frame {i}");
        }
        assert!(expanded.same_timing(&index));
        assert_every_frame(&expanded);
        assert_eq!(expanded.frame(N - 1).unwrap().into_raw(), synthetic(N - 1, W, H));
        // The fake video is exact, so the frames file is the one the recorder wrote.
        assert!(expanded.same_frames(&rec));
        assert_eq!(std::fs::read(into.join(FRAMES_FILE)).unwrap(), std::fs::read(tmp.path().join("rec").join(FRAMES_FILE)).unwrap());

        // It remembers the video it came from, which no other recording does.
        assert_eq!(expanded.expanded_from(), Some((video.as_path(), bytes)));
        assert_eq!(rec.expanded_from(), None);
        // The editor's reader works on it like on any recording.
        let mut reader = FrameReader::new(Arc::new(expanded)).unwrap();
        assert_eq!(reader.get(KEYFRAME_EVERY + 2).unwrap().into_raw(), synthetic(KEYFRAME_EVERY + 2, W, H));
        // The video was only read.
        assert_eq!(std::fs::metadata(&video).unwrap().len(), bytes);

        // An index of the frames form, which names no video size, expands the same video.
        let other = folder(tmp.path(), "other");
        let again = expand_video(&index_of(&rec, None), &video, &other, &Fake::new(), &always).unwrap();
        assert_every_frame(&again);
        assert_eq!(again.expanded_from(), Some((video.as_path(), bytes)));
    }

    #[test]
    fn a_video_with_fewer_or_more_frames_than_the_index_is_damaged_and_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let into = folder(tmp.path(), "edit");
        let video = tmp.path().join(VIDEO_FILE);
        let frame = (W * H * 4) as u64;
        // The index names no size, so the count is found only by reading the video.
        for pictures in [N - 1, N + 1, 1, 2 * N] {
            let bytes = raw_video(&rec, &video, pictures);
            let fake = Fake::new();
            let result = expand_video(&index_of(&rec, None), &video, &into, &fake, &always);
            assert!(matches!(result, Err(ExpandError::Damaged(_))), "{pictures} pictures");
            assert_eq!(fake.decodes(), 1);
            assert_eq!(names(&into), Vec::<String>::new(), "{pictures} pictures");
            assert_eq!(std::fs::metadata(&video).unwrap().len(), bytes, "the video is left as it is");
        }
        // A byte short or a byte long is not the video either.
        for len in [N as u64 * frame - 1, N as u64 * frame + 1] {
            raw_video(&rec, &video, N + 1);
            std::fs::OpenOptions::new().write(true).open(&video).unwrap().set_len(len).unwrap();
            assert!(matches!(expand_video(&index_of(&rec, None), &video, &into, &Fake::new(), &always), Err(ExpandError::Damaged(_))), "{len} bytes");
            assert_eq!(names(&into), Vec::<String>::new());
        }
        // A video that is not the size its index notes is refused before it is read.
        let bytes = raw_video(&rec, &video, N);
        for noted in [bytes - 1, bytes + 1, 1, u64::MAX] {
            let fake = Fake::new();
            assert!(matches!(expand_video(&index_of(&rec, Some(noted)), &video, &into, &fake, &always), Err(ExpandError::Damaged(_))), "noted as {noted}");
            assert_eq!(fake.decodes(), 0);
        }
        // So is a video with fewer bytes than the index has frames, one that is not there, and a folder.
        std::fs::write(tmp.path().join("tiny.mkv"), [0u8; 5]).unwrap();
        for path in [tmp.path().join("tiny.mkv"), tmp.path().join("gone.mkv"), tmp.path().join("rec")] {
            let fake = Fake::new();
            assert!(matches!(expand_video(&index_of(&rec, None), &path, &into, &fake, &always), Err(ExpandError::Damaged(_))), "{}", path.display());
            assert_eq!(fake.decodes(), 0);
        }
        assert_eq!(names(&into), Vec::<String>::new());

        // What the program says about a file it cannot read is damage, in this crate's words.
        for fails in [(|| CodecError::Failed { exit: Some(1) }) as fn() -> CodecError, || CodecError::Failed { exit: Some(-22) }, || CodecError::WrongFrames { got: 0 }] {
            match expand_video(&index_of(&rec, Some(bytes)), &video, &into, &Fake::failing(fails), &always) {
                Err(ExpandError::Damaged(why)) => assert!(why.starts_with("the video ") && !why.contains(['\\', '/', ':']), "{why}"),
                other => panic!("{:?}", other.map(|_| ())),
            }
            assert_eq!(names(&into), Vec::<String>::new());
        }
        // What says nothing about the video stays the codec's: a time bound, a program that
        // cannot do this or could not be started.
        let timed_out = expand_video(&index_of(&rec, Some(bytes)), &video, &into, &Fake::failing(|| CodecError::TimedOut { bound: Bound::Stall }), &always);
        assert!(matches!(timed_out, Err(ExpandError::Codec(CodecError::TimedOut { bound: Bound::Stall }))));
        let cannot = expand_video(&index_of(&rec, Some(bytes)), &video, &into, &Fake::failing(|| CodecError::Failed { exit: None }), &always);
        assert!(matches!(cannot, Err(ExpandError::Codec(CodecError::Failed { exit: None }))));
        let not_started = expand_video(&index_of(&rec, Some(bytes)), &video, &into, &Fake::failing(|| CodecError::Io(io::Error::other("no such program"))), &always);
        assert!(matches!(not_started, Err(ExpandError::Codec(CodecError::Io(_)))));
        // Each of them had written every frame before it failed, and left nothing.
        assert_eq!(names(&into), Vec::<String>::new());
        assert_eq!(std::fs::metadata(&video).unwrap().len(), bytes);
        // The same folder takes the whole video afterwards.
        assert_every_frame(&expand_video(&index_of(&rec, Some(bytes)), &video, &into, &Fake::new(), &always).unwrap());
    }

    #[test]
    fn an_index_that_asks_for_too_much_is_refused_before_anything_is_made() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let into = folder(tmp.path(), "edit");
        let good = || index_of(&rec, Some(bytes));
        let cases = [
            ("no width", VideoIndex { width: 0, ..good() }),
            ("a side too long", VideoIndex { width: MAX_SIDE + 1, height: 1, ..good() }),
            ("the largest numbers", VideoIndex { width: u32::MAX, height: u32::MAX, ..good() }),
            ("too many pixels", VideoIndex { width: MAX_SIDE, height: MAX_SIDE, ..good() }),
            ("no frames", VideoIndex { times: Vec::new(), ..good() }),
            ("an end later than any recording", VideoIndex { end_ms: MAX_MS + 1, ..good() }),
            ("an end before the last frame", VideoIndex { end_ms: 5, ..good() }),
            ("time running backwards", VideoIndex { times: vec![(7, None), (5, None)], ..good() }),
            ("a pointer outside", VideoIndex { times: vec![(0, Some((W as i32, 0)))], ..good() }),
            ("a video of no bytes", VideoIndex { video_bytes: Some(0), ..good() }),
        ];
        for (what, index) in cases {
            let fake = Fake::new();
            assert!(matches!(expand_video(&index, &video, &into, &fake, &|_, _| panic!("{what}: nothing to go on with")), Err(ExpandError::Damaged(_))), "{what}");
            assert_eq!(fake.decodes(), 0, "{what}");
            assert_eq!(names(&into), Vec::<String>::new(), "{what}");
        }
    }

    #[test]
    fn a_cancelled_expansion_leaves_the_folder_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let index = index_of(&rec, Some(bytes));
        let into = folder(tmp.path(), "edit");
        // The answer before the sixth frame is no, from a codec that never asks by itself.
        let asked = std::cell::RefCell::new(Vec::new());
        let result = expand_video(&index, &video, &into, &Fake::quiet(), &|done, total| {
            asked.borrow_mut().push((done, total));
            done < 5
        });
        assert!(matches!(result, Err(ExpandError::Cancelled)), "{:?}", result.map(|_| ()));
        assert_eq!(*asked.borrow(), (0..=5).map(|done| (done, N)).collect::<Vec<_>>(), "it stopped where it was told to");
        assert_eq!(names(&into), Vec::<String>::new());
        // A no at the very start, a no after the last frame, and a codec that reports the cancel itself.
        assert!(matches!(expand_video(&index, &video, &into, &Fake::new(), &|_, _| false), Err(ExpandError::Cancelled)));
        assert_eq!(names(&into), Vec::<String>::new());
        assert!(matches!(expand_video(&index, &video, &into, &Fake::new(), &|done, _| done < N), Err(ExpandError::Cancelled)));
        assert_eq!(names(&into), Vec::<String>::new());
        assert!(matches!(expand_video(&index, &video, &into, &Fake::failing(|| CodecError::Cancelled), &always), Err(ExpandError::Cancelled)));
        assert_eq!(names(&into), Vec::<String>::new());
        // The video is as it was, and the folder is still there for the next try.
        assert_eq!(std::fs::metadata(&video).unwrap().len(), bytes);
        assert_every_frame(&expand_video(&index, &video, &into, &Fake::new(), &always).unwrap());
    }

    #[test]
    fn a_panic_during_an_expansion_leaves_the_folder_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let index = index_of(&rec, Some(bytes));
        let into = folder(tmp.path(), "edit");
        let fake = Fake::quiet();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = expand_video(&index, &video, &into, &fake, &|done, _| if done == 7 { panic!("the caller's check panicked, as this test asks") } else { true });
        }));
        assert!(panicked.is_err());
        assert_eq!(names(&into), Vec::<String>::new());
        assert_eq!(std::fs::metadata(&video).unwrap().len(), bytes);
    }

    #[test]
    fn the_folder_to_expand_into_must_be_there_and_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let index = index_of(&rec, Some(bytes));
        // Something is in it: it stays, nothing joins it, and the video is not read.
        let used = folder(tmp.path(), "used");
        std::fs::write(used.join("notes.txt"), "not this call's").unwrap();
        let fake = Fake::new();
        assert!(matches!(expand_video(&index, &video, &used, &fake, &always), Err(ExpandError::Io(_))));
        assert_eq!(names(&used), ["notes.txt"]);
        // A frames file that is already there is somebody's, and is not written over or removed.
        let taken = folder(tmp.path(), "taken");
        std::fs::write(taken.join(FRAMES_FILE), "somebody's frames").unwrap();
        assert!(matches!(expand_video(&index, &video, &taken, &fake, &always), Err(ExpandError::Io(_))));
        assert_eq!(std::fs::read_to_string(taken.join(FRAMES_FILE)).unwrap(), "somebody's frames");
        // A folder that was never made is not made.
        assert!(matches!(expand_video(&index, &video, &tmp.path().join("never-made"), &fake, &always), Err(ExpandError::Io(_))));
        assert!(!tmp.path().join("never-made").exists());
        assert_eq!(fake.decodes(), 0);
    }

    /// A frames file on a disk with room for `room` more bytes.
    struct Full {
        file: File,
        room: usize,
        error: fn() -> io::Error,
    }

    impl Write for Full {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if buf.len() > self.room {
                return Err((self.error)());
            }
            self.room -= buf.len();
            self.file.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }

    fn on_a_disk(room: usize, error: fn() -> io::Error) -> impl FnOnce(&Path) -> io::Result<Full> {
        move |path| Ok(Full { file: File::create(path)?, room, error })
    }

    #[test]
    fn a_write_that_fails_for_lack_of_room_is_reported_as_no_room() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let index = index_of(&rec, Some(bytes));
        let into = folder(tmp.path(), "edit");
        // Small frames wait in the writer's buffer: the disk is found full when they are written out.
        let result = expand_with(&index, &video, &into, &Fake::new(), &always, on_a_disk(100, no_room_error));
        assert!(matches!(result, Err(ExpandError::NoRoom)), "{:?}", result.map(|_| ()));
        assert_eq!(names(&into), Vec::<String>::new());
        // The kind the system gives a full disk or a used-up quota, however it is numbered.
        let result = expand_with(&index, &video, &into, &Fake::new(), &always, on_a_disk(100, || io::Error::from(io::ErrorKind::QuotaExceeded)));
        assert!(matches!(result, Err(ExpandError::NoRoom)));
        // The frames file cannot even be made for lack of room.
        let result = expand_with(&index, &video, &into, &Fake::new(), &always, |_: &Path| -> io::Result<File> { Err(no_room_error()) });
        assert!(matches!(result, Err(ExpandError::NoRoom)));
        // Another failure of the write is that failure, and leaves as little.
        let result = expand_with(&index, &video, &into, &Fake::new(), &always, on_a_disk(100, || io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(matches!(&result, Err(ExpandError::Io(e)) if e.kind() == io::ErrorKind::PermissionDenied), "{:?}", result.map(|_| ()));
        assert_eq!(names(&into), Vec::<String>::new());

        // Frames too large to wait in the buffer: the disk fills while a frame is written,
        // part of the file is already there, and it is removed.
        let (w, h, n) = (512u32, 512u32, 6usize);
        let big = folder(tmp.path(), "big");
        let mut writer = FrameWriter::create(&big, w, h).unwrap();
        let mut seed = 0x2545_f491u32;
        for i in 0..n {
            let noise: Vec<u8> = (0..w * h * 4)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            writer.push(&noise, i as u64 * 40, None).unwrap();
        }
        let big_rec = writer.finish(big, n as u64 * 40).unwrap();
        let big_video = tmp.path().join("big.mkv");
        let big_bytes = raw_video(&big_rec, &big_video, n);
        let written = std::cell::Cell::new(0);
        let result = expand_with(&index_of(&big_rec, Some(big_bytes)), &big_video, &into, &Fake::new(), &|done, _| {
            written.set(done);
            true
        }, on_a_disk(2 << 20, no_room_error));
        assert!(matches!(result, Err(ExpandError::NoRoom)), "{:?}", result.map(|_| ()));
        assert!(written.get() < n - 1, "the write failed while frames were still coming, at {}", written.get());
        assert_eq!(names(&into), Vec::<String>::new());
        // With room, the same writer keeps every frame.
        let whole = expand_with(&index, &video, &into, &Fake::new(), &always, on_a_disk(usize::MAX, no_room_error)).unwrap();
        assert_every_frame(&whole);
    }

    #[test]
    fn deleting_an_expanded_recording_removes_its_own_folder_and_never_the_video() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let index = index_of(&rec, Some(bytes));
        let before = std::fs::read(&video).unwrap();
        // The video lies in a project's folder, beside its index.
        rec.write_video_index(tmp.path(), bytes).unwrap();
        let into = folder(tmp.path(), "edit");
        let expanded = expand_video(&index, &video, &into, &Fake::new(), &always).unwrap();
        expanded.delete();
        assert!(!into.exists());
        assert_eq!(std::fs::read(&video).unwrap(), before);
        assert!(tmp.path().join(INDEX_FILE).is_file());
        // Shared with an editor's reader, as when an editor closes.
        let into = folder(tmp.path(), "edit");
        let shared = Arc::new(expand_video(&index, &video, &into, &Fake::new(), &always).unwrap());
        let reader = FrameReader::new(shared.clone()).unwrap();
        drop(reader);
        shared.delete_files();
        assert!(!into.exists());
        assert_eq!(std::fs::read(&video).unwrap(), before);
        assert_eq!(open_video_index(tmp.path()).unwrap(), index);
    }

    #[test]
    fn an_expanded_recording_is_kept_as_its_video_by_link_and_as_a_copy_when_a_link_cannot_be() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let index = index_of(&rec, Some(bytes));
        let expanded = expand_video(&index, &video, &folder(tmp.path(), "edit"), &Fake::new(), &always).unwrap();
        let the_video = std::fs::read(&video).unwrap();
        let as_video = SavedAs { form: Form::Video, bytes };
        let is_the_video_form = |dir: &Path| {
            assert_eq!(names(dir), [VIDEO_FILE, INDEX_FILE]);
            assert_eq!(std::fs::read(dir.join(VIDEO_FILE)).unwrap(), the_video);
            assert_eq!(form_of(dir), Ok(Form::Video));
            assert_eq!(open_video_index(dir).unwrap(), index);
            assert_eq!(refused(dir), OpenError::Video);
        };

        // By link: no copy is made, and neither the frames nor a format-1 index are written.
        let linked = folder(tmp.path(), "linked");
        assert_eq!(expanded.save_with(&linked, |from, to| std::fs::hard_link(from, to), no_copy), Ok(as_video));
        is_the_video_form(&linked);
        // By copy when a link cannot be made.
        let copied = folder(tmp.path(), "copied");
        assert_eq!(expanded.save_with(&copied, no_link, copy_whole), Ok(as_video));
        is_the_video_form(&copied);
        // The two ways in that the app uses.
        let (plain, checked) = (folder(tmp.path(), "plain"), folder(tmp.path(), "checked"));
        assert_eq!(expanded.save_into(&plain), Ok(as_video));
        assert!(matches!(expanded.save_into_with(&checked, &|| panic!("a link needs no check")), Ok(kept) if kept == as_video));
        is_the_video_form(&plain);
        is_the_video_form(&checked);
        // Saved again over itself, it is replaced whole.
        assert_eq!(expanded.save_with(&copied, no_link, copy_whole), Ok(as_video));
        is_the_video_form(&copied);

        // A copy that fails or is stopped leaves nothing, in a new folder and over a kept one.
        let failed = folder(tmp.path(), "failed");
        assert!(expanded.save_with(&failed, no_link, |_, _| Err(io::Error::other("the disk is full"))).is_err());
        assert!(expanded.save_with(&failed, no_link, |from, to| std::fs::write(to, &std::fs::read(from)?[1..])).is_err(), "a short copy is not kept");
        assert!(matches!(expanded.save_while(&failed, no_link, 64, &|| false), Err(SaveError::Cancelled)));
        assert_eq!(names(&failed), Vec::<String>::new());
        assert!(expanded.save_with(&copied, no_link, |_, _| Err(io::Error::other("the disk is full"))).is_err());
        is_the_video_form(&copied);

        // A folder that keeps the frames form is taken over only by the index: the frames
        // file is not this call's to remove, and the index says which of the two counts.
        let frames = folder(tmp.path(), "frames");
        rec.save_into(&frames).unwrap();
        assert_eq!(expanded.save_into(&frames), Ok(as_video));
        assert_eq!(names(&frames), [VIDEO_FILE, FRAMES_FILE, INDEX_FILE]);
        assert_eq!(form_of(&frames), Ok(Form::Video));

        // The kept video outlives the original and the expanded frames, and expands again.
        expanded.delete();
        std::fs::remove_file(&video).unwrap();
        is_the_video_form(&linked);
        let again = expand_video(&open_video_index(&linked).unwrap(), &linked.join(VIDEO_FILE), &folder(tmp.path(), "again"), &Fake::new(), &always).unwrap();
        assert_every_frame(&again);
    }

    #[test]
    fn an_expanded_recording_whose_video_is_gone_is_kept_as_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let (rec, video, bytes) = with_video(tmp.path());
        let expanded = expand_video(&index_of(&rec, Some(bytes)), &video, &folder(tmp.path(), "edit"), &Fake::new(), &always).unwrap();
        let size = std::fs::metadata(tmp.path().join("edit").join(FRAMES_FILE)).unwrap().len();
        let is_the_frames_form = |dir: &Path| {
            assert_eq!(names(dir), [FRAMES_FILE, INDEX_FILE]);
            assert_eq!(form_of(dir), Ok(Form::Frames));
            assert_every_frame(&opened(dir));
        };
        // A video of another size is not the one the frames came from.
        std::fs::OpenOptions::new().append(true).open(&video).unwrap().write_all(b"more").unwrap();
        let other = folder(tmp.path(), "other");
        assert_eq!(expanded.save_into(&other), Ok(as_frames(size)));
        is_the_frames_form(&other);
        // Nor is a folder of its name, or nothing at all.
        std::fs::remove_file(&video).unwrap();
        std::fs::create_dir(&video).unwrap();
        let folder_there = folder(tmp.path(), "folder-there");
        assert_eq!(expanded.save_into(&folder_there), Ok(as_frames(size)));
        is_the_frames_form(&folder_there);
        std::fs::remove_dir(&video).unwrap();
        let gone = folder(tmp.path(), "gone");
        assert!(matches!(expanded.save_into_with(&gone, &|| true), Ok(kept) if kept == as_frames(size)));
        is_the_frames_form(&gone);
        // The frames were linked, so they outlive the expanded recording's folder.
        expanded.delete();
        is_the_frames_form(&gone);
    }

    #[test]
    fn a_new_recording_is_kept_exactly_as_before() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let size = std::fs::metadata(tmp.path().join("rec").join(FRAMES_FILE)).unwrap().len();
        let (plain, checked) = (folder(tmp.path(), "plain"), folder(tmp.path(), "checked"));
        assert_eq!(rec.save_into(&plain), Ok(as_frames(size)));
        assert!(matches!(rec.save_into_with(&checked, &|| true), Ok(kept) if kept == as_frames(size)));
        // The index is, byte for byte, the text every version so far has written: format 1,
        // and one row of offset, length, whole frame, time and pointer per frame.
        let rows: Vec<String> = rec
            .frames
            .iter()
            .map(|m| {
                let pointer = m.cursor.map_or("null,null".to_string(), |(x, y)| format!("{x},{y}"));
                format!("[{},{},{},{},{pointer}]", m.offset, m.len, m.key, m.t_ms)
            })
            .collect();
        let text = format!(r#"{{"format":1,"width":{W},"height":{H},"end_ms":{},"frames":[{}]}}"#, N * 66, rows.join(","));
        for dir in [&plain, &checked] {
            assert_eq!(names(dir), [FRAMES_FILE, INDEX_FILE]);
            assert_eq!(std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap(), text);
            assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), std::fs::read(tmp.path().join("rec").join(FRAMES_FILE)).unwrap());
            assert_eq!(form_of(dir), Ok(Form::Frames));
            assert_every_frame(&opened(dir));
        }
        // A recording opened from a kept folder is kept the same way.
        let third = folder(tmp.path(), "third");
        assert_eq!(opened(&plain).save_into(&third), Ok(as_frames(size)));
        assert_eq!(std::fs::read_to_string(third.join(INDEX_FILE)).unwrap(), text);
    }

    /// A frame like a screen: flat areas, a dark bar, and a block that moves with `i`.
    fn screen(i: usize, w: u32, h: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        let left = 2 + i as u32 % (w - 16);
        for y in 0..h {
            for x in 0..w {
                let rgb = if y < 6 {
                    [32, 36, 48]
                } else if (left..left + 12).contains(&x) && (12..24).contains(&y) {
                    [30, 120, 220]
                } else {
                    [238, 238, 242]
                };
                px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        px
    }

    /// The signal-to-noise ratio over red, green and blue, in dB; infinite for equal frames.
    fn psnr_rgb(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let squares: u64 = a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(x, y)| (0..3).map(|c| (x[c] as i64 - y[c] as i64).pow(2) as u64).sum::<u64>()).sum();
        if squares == 0 {
            return f64::INFINITY;
        }
        10.0 * (255.0f64 * 255.0 / (squares as f64 / (a.len() / 4 * 3) as f64)).log10()
    }

    #[test]
    fn a_recording_archived_and_expanded_again_has_the_same_times_and_frames_within_the_tolerance() {
        let Some(ffmpeg) = crate::ffmpeg::find_now() else {
            println!("{SKIPPED}");
            return;
        };
        // Screen-like frames, past one group of 30, with irregular times and a pointer.
        let (w, h, n) = (64u32, 48u32, 40usize);
        let tmp = tempfile::tempdir().unwrap();
        let dir = folder(tmp.path(), "rec");
        let mut writer = FrameWriter::create(&dir, w, h).unwrap();
        for i in 0..n {
            writer.push(&screen(i, w, h), (i * i) as u64 * 3 + i as u64 * 17, (i % 4 != 0).then_some((i as i32 - 20, 40 - i as i32))).unwrap();
        }
        let rec = writer.finish(dir, 9_000).unwrap();

        // Archived: every frame into the video, and the video-form index beside it.
        let project = folder(tmp.path(), "project");
        let video = project.join(VIDEO_FILE);
        let mut reader = FrameReader::new(Arc::new(opened_as_is(&rec))).unwrap();
        let mut at = 0;
        let mut next = |buf: &mut Vec<u8>| {
            if at == n {
                return Ok(false);
            }
            buf.extend_from_slice(reader.get(at)?.as_raw());
            at += 1;
            Ok(true)
        };
        ffmpeg.encode((w, h), n, &mut next, &video, 64 << 20, &|_| true).unwrap();
        let bytes = std::fs::metadata(&video).unwrap().len();
        rec.write_video_index(&project, bytes).unwrap();
        assert_eq!(names(&project), [VIDEO_FILE, INDEX_FILE]);

        // Expanded again from the folder alone.
        let index = open_video_index(&project).unwrap();
        assert_eq!(index, index_of(&rec, Some(bytes)));
        let into = folder(tmp.path(), "edit");
        let progress = std::cell::Cell::new(0);
        let expanded = expand_video(&index, &video, &into, ffmpeg, &|done, total| {
            assert_eq!(total, n);
            progress.set(progress.get().max(done));
            true
        })
        .unwrap();
        assert_eq!(progress.get(), n);
        assert_eq!(names(&into), [FRAMES_FILE]);
        assert!(expanded.same_timing(&index));
        assert_eq!((expanded.width, expanded.height, expanded.end_ms, expanded.frames.len()), (w, h, 9_000, n));
        for (a, b) in expanded.frames.iter().zip(&rec.frames) {
            assert_eq!((a.t_ms, a.cursor, a.key), (b.t_ms, b.cursor, b.key));
        }
        assert_eq!(expanded.expanded_from(), Some((video.as_path(), bytes)));

        // Every frame comes back as itself within the tolerance. Measured on this content with
        // ffmpeg 9.0.2 (libx264, crf 10, 4:4:4, full range): 43.8 dB for the worst
        // frame. The floor asked for here is the plan's 40 dB for a frame; a range or matrix
        // mistake lands near 25 dB, and a picture one frame off far below that on the block.
        let mut worst = f64::INFINITY;
        expanded
            .for_each_frame(|i, img| {
                let db = psnr_rgb(&screen(i, w, h), img.as_raw());
                worst = worst.min(db);
                assert!(db >= 40.0, "frame {i} came back at {db:.1} dB");
                assert!(img.as_raw().chunks_exact(4).all(|p| p[3] == 255), "frame {i} lost its alpha");
                // Picture i is frame i: it is closer to its own frame than to its neighbours.
                for other in [i.wrapping_sub(1), i + 1] {
                    if other < n {
                        assert!(db > psnr_rgb(&screen(other, w, h), img.as_raw()), "picture {i} is closer to frame {other}");
                    }
                }
                Ok(())
            })
            .unwrap();
        println!("worst frame after archive and expansion: {worst:.1} dB");

        // The video in the project was only read, and a video cut short is damage that leaves nothing.
        assert_eq!(std::fs::metadata(&video).unwrap().len(), bytes);
        let cut = tmp.path().join("cut.mkv");
        std::fs::write(&cut, &std::fs::read(&video).unwrap()[..bytes as usize * 2 / 3]).unwrap();
        let other = folder(tmp.path(), "other");
        let result = expand_video(&VideoIndex { video_bytes: None, ..index.clone() }, &cut, &other, ffmpeg, &always);
        assert!(matches!(result, Err(ExpandError::Damaged(_))), "{:?}", result.map(|_| ()));
        assert_eq!(names(&other), Vec::<String>::new());
        // A file that is no video at all, the same.
        std::fs::write(&cut, b"#EXTM3U\nhttp://example.invalid/a.ts\n").unwrap();
        let result = expand_video(&VideoIndex { video_bytes: None, times: index.times[..3].to_vec(), ..index.clone() }, &cut, &other, ffmpeg, &always);
        assert!(matches!(result, Err(ExpandError::Damaged(_))), "{:?}", result.map(|_| ()));
        assert_eq!(names(&other), Vec::<String>::new());

        // Kept again, the expanded recording is its video: the same bytes, not a second encode.
        let second = folder(tmp.path(), "second");
        assert_eq!(expanded.save_into(&second), Ok(SavedAs { form: Form::Video, bytes }));
        assert_eq!(std::fs::read(second.join(VIDEO_FILE)).unwrap(), std::fs::read(&video).unwrap());
        assert_eq!(open_video_index(&second).unwrap(), index);
    }

    /// The same recording through a second handle that does not own the folder, for a reader.
    fn opened_as_is(rec: &Recording) -> Recording {
        Recording { dir: rec.dir.clone(), owned: false, width: rec.width, height: rec.height, frames: rec.frames.clone(), end_ms: rec.end_ms, expanded_from: None }
    }
}
