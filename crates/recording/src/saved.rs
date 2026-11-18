//! Keeping a recording, and opening a kept one again.
//!
//! [`Recording::save_into`] puts the frames file and an index (`recording.json`) into a
//! folder the caller made. The frames get there by hard link, so nothing is copied and the
//! data stays when the recording's own temp folder is deleted; a copy is made only when a
//! link cannot be. [`Recording::open_saved`] reads such a folder back into a recording that
//! does not own it, so `delete` and `delete_files` never remove it.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{FRAMES_FILE, FrameMeta, Recording, now_nanos};

/// The index's format number. A higher one was written by a newer version.
const FORMAT: u32 = 1;
const INDEX_FILE: &str = "recording.json";

/// The largest picture an index may name. A frame is decoded into `width * height * 4`
/// bytes, so a damaged index must not be able to ask for more memory than a screen needs.
const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 64 << 20;

/// The longest recording an index may name, in ms (about 49 days), so time arithmetic on a
/// damaged index cannot overflow.
const MAX_MS: u64 = u32::MAX as u64;

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
}

impl Recording {
    /// Keep this recording in `dir`, which must exist: the frames file by hard link, or by
    /// copy when a link cannot be made, then the index. Returns the frames file's size.
    /// A failure removes what this call wrote and nothing else. Saving into the folder the
    /// recording already lives in only rewrites the index.
    pub fn save_into(&self, dir: &Path) -> Result<u64, String> {
        self.save_with(dir, |from, to| std::fs::hard_link(from, to), copy_whole)
    }

    /// [`Recording::save_into`] with the link and the copy handed in, so a test can make
    /// either fail. Both put the file `from` at the new path `to`.
    fn save_with(&self, dir: &Path, link: impl FnOnce(&Path, &Path) -> io::Result<()>, copy: impl FnOnce(&Path, &Path) -> io::Result<()>) -> Result<u64, String> {
        if !dir.is_dir() {
            return Err("the folder to keep the recording in does not exist".into());
        }
        let expected = self.frames_end().filter(|end| *end > 0).ok_or("the recording has no frames to keep")?;
        let dest = dir.join(FRAMES_FILE);
        if same_folder(&self.dir, dir) {
            let size = std::fs::metadata(&dest).map_err(|e| format!("could not read the frames file: {e}"))?.len();
            if size != expected {
                return Err("the frames file is not the one this recording was read from".into());
            }
            self.write_index(dir)?;
            return Ok(size);
        }

        let staged = self.stage(dir, expected, link, copy)?;
        if let Err(e) = staged.swap_frames() {
            staged.undo(false);
            return Err(format!("could not keep the frames: {e}"));
        }
        if let Err(e) = staged.swap_index() {
            staged.undo(true);
            return Err(format!("could not write the recording's index: {e}"));
        }
        Ok(expected)
    }

    /// The first half of a save: the frames file and the index, each whole and on the disk
    /// under a temporary name in `dir`. Nothing that was in the folder has been touched yet,
    /// and a failure removes both temporary files.
    fn stage(&self, dir: &Path, expected: u64, link: impl FnOnce(&Path, &Path) -> io::Result<()>, copy: impl FnOnce(&Path, &Path) -> io::Result<()>) -> Result<Staged, String> {
        let src = self.dir.join(FRAMES_FILE);
        let (frames, index) = (dir.join(FRAMES_FILE), dir.join(INDEX_FILE));
        let staged = Staged { frames_tmp: temp_beside(&frames), index_tmp: temp_beside(&index), replaces: frames.exists(), frames, index };
        let tmp = &staged.frames_tmp;
        // A link is the same data under a second name: make sure that data is on the disk.
        let result = link(&src, tmp)
            .and_then(|()| sync(tmp))
            .or_else(|_| {
                let _ = std::fs::remove_file(tmp);
                copy(&src, tmp)
            })
            .and_then(|()| match std::fs::metadata(tmp)?.len() {
                size if size == expected => Ok(()),
                _ => Err(io::Error::other("the frames file is not complete")),
            })
            .map_err(|e| format!("could not keep the frames: {e}"))
            .and_then(|()| self.write_index_to(&staged.index_tmp).map_err(|e| format!("could not write the recording's index: {e}")));
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
    fn write_index(&self, dir: &Path) -> Result<(), String> {
        let path = dir.join(INDEX_FILE);
        let tmp = temp_beside(&path);
        self.write_index_to(&tmp).and_then(|()| patiently(|| std::fs::rename(&tmp, &path))).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("could not write the recording's index: {e}")
        })
    }

    /// Open a recording kept by [`Recording::save_into`]. The folder is not this recording's
    /// to delete: `delete` and `delete_files` leave it alone. Nothing is changed or removed,
    /// whatever is found.
    pub fn open_saved(dir: &Path) -> Result<Recording, OpenError> {
        let text = std::fs::read(dir.join(INDEX_FILE)).map_err(not_readable)?;
        let head: Head = serde_json::from_slice(&text).map_err(|_| damaged("the index cannot be read"))?;
        if head.format > FORMAT {
            return Err(OpenError::Newer(head.format));
        }
        if head.format != FORMAT {
            return Err(damaged("the index has no known format"));
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
        Ok(Recording { dir: dir.to_path_buf(), owned: false, width: index.width, height: index.height, frames, end_ms: index.end_ms })
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::synthetic;
    use crate::{FrameReader, FrameWriter, KEYFRAME_EVERY};
    use std::sync::Arc;

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

    #[test]
    fn a_saved_recording_opens_again_and_every_frame_decodes_the_same() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        let size = rec.save_into(&kept).unwrap();
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
        let size = rec.save_with(&kept, no_link, copy_whole).unwrap();
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
        let size = rec.save_with(&kept, no_link, copy_whole).unwrap();
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
        let newer = r#"{"format": 2, "picture": {"w": 48}, "frames": "elsewhere"}"#;
        std::fs::write(kept.join(INDEX_FILE), newer).unwrap();
        assert_eq!(refused(&kept), OpenError::Newer(2));
        assert_eq!(std::fs::read_to_string(kept.join(INDEX_FILE)).unwrap(), newer);
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        // Newer is said even when the frames file is not where this version looks for it.
        std::fs::remove_file(kept.join(FRAMES_FILE)).unwrap();
        assert_eq!(refused(&kept), OpenError::Newer(2));
    }

    #[test]
    fn an_index_that_is_cut_off_or_does_not_fit_is_damaged_and_never_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorded(tmp.path());
        let kept = folder(tmp.path(), "kept");
        let size = rec.save_into(&kept).unwrap();
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
        let size = rec.save_into(&kept).unwrap();
        rec.delete();
        let again = opened(&kept);
        let index = std::fs::read(kept.join(INDEX_FILE)).unwrap();
        std::fs::remove_file(kept.join(INDEX_FILE)).unwrap();
        // Neither a link nor a copy is asked for; the frames file stays as it is.
        let no_link = |_: &Path, _: &Path| -> io::Result<()> { panic!("a link was made") };
        assert_eq!(again.save_with(&kept, no_link, no_copy), Ok(size));
        assert_eq!(std::fs::read(kept.join(INDEX_FILE)).unwrap(), index);
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        assert_every_frame(&opened(&kept));
        // The folder named another way is still the same folder.
        assert_eq!(again.save_with(&kept.join("..").join("kept"), no_link, no_copy), Ok(size));
        // Into a second folder, an opened recording is kept like any other.
        let second = folder(tmp.path(), "second");
        assert_eq!(again.save_into(&second), Ok(size));
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
        let size = rec.save_into(&kept).unwrap();
        assert_eq!(rec.save_with(&kept, no_link, copy_whole), Ok(size));
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        // A save that fails leaves the frames that were there before it.
        assert!(rec.save_with(&kept, no_link, |_, _| Err(io::Error::other("the disk is full"))).is_err());
        assert_eq!(names(&kept), [FRAMES_FILE, INDEX_FILE]);
        assert_every_frame(&opened(&kept));
    }
}
