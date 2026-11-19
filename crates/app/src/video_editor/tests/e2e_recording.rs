//! One real pass of a real recording through the folder of saved recordings, with the real
//! ffmpeg and no window: save, convert, list, expand, update, remove, and the start-up steps.
//! The normal suite never runs it: it needs a recording, and minutes.
//!
//! ```text
//! MYTHICSHOT_E2E_RECORDING=<a folder with frames.zst and recording.json> \
//!   cargo test --release -p mythicshot a_real_recording_goes_through -- --ignored --nocapture
//! ```
//!
//! The folder is only read: the save links or copies its frames into a temporary projects
//! folder, as a publish does with a recording's own folder, and everything else is written
//! below that temporary folder. The pass ends by checking that the folder is as it was.
//!
//! The conversion is the queue's own (`convert_one`, with the tolerance and the least size that
//! ship). Run it by its name alone: the store has one conversion and one codec per process.

use crate::project::{ConvertState, Expand, Form, MIN_BYTES, Outcome, ProjectInfo, ProjectKey, Removed, Snapshot, Store, convert_one, converting_now, set_current_codec};
use crate::video::Edit;
use mythicshot_recording::video::{mean_shift, psnr_rgb, worst_block_psnr};
use mythicshot_recording::{FrameReader, Recording, TOLERANCE, ffmpeg};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const ENV_RECORDING: &str = "MYTHICSHOT_E2E_RECORDING";
const SITE: &str = "https://tours.example.com";
const TOUR: &str = "tour_0123456789abcdef01234567";
const CHAPTER: &str = "ch_0123456789abcdef0123456a";
/// What a project kept as frames holds, and one kept as a video: nothing but what the store writes.
const AS_FRAMES: [&str; 3] = ["frames.zst", "project.json", "recording.json"];
const AS_VIDEO: [&str; 3] = ["frames.mkv", "project.json", "recording.json"];

/// Every file below `dir` with its bytes, by its path from there, sorted.
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

/// Every file below `dir` with its size, by its path from there, sorted.
fn sizes(dir: &Path) -> Vec<(String, u64)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push((path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"), entry.metadata().unwrap().len()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn names(dir: &Path) -> Vec<String> {
    sizes(dir).into_iter().map(|(name, _)| name).collect()
}

fn size_of(dir: &Path, name: &str) -> u64 {
    std::fs::metadata(dir.join(name)).unwrap().len()
}

/// Every entry directly in `dir`, by name.
fn entries(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    out.sort();
    out
}

/// What the source folder is: every file's name, size, time of its last change, and a sum of
/// its bytes. Read only.
fn fingerprint(dir: &Path) -> Vec<(String, u64, SystemTime, u64)> {
    use std::io::Read;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let meta = entry.metadata().unwrap();
        if !meta.is_file() {
            out.push((entry.file_name().to_string_lossy().to_string(), 0, meta.modified().unwrap(), 0));
            continue;
        }
        let mut file = std::fs::File::open(entry.path()).unwrap();
        let (mut sum, mut buf) = (0xcbf2_9ce4_8422_2325u64, vec![0u8; 1 << 20]);
        loop {
            let n = file.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            for byte in &buf[..n] {
                sum = (sum ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        out.push((entry.file_name().to_string_lossy().to_string(), meta.len(), meta.modified().unwrap(), sum));
    }
    out.sort();
    out
}

fn format_of(dir: &Path, file: &str) -> u64 {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap();
    value["format"].as_u64().unwrap()
}

fn listed(store: &Store) -> ProjectInfo {
    let list = store.list();
    assert_eq!(list.len(), 1, "one project is listed");
    list.into_iter().next().unwrap()
}

type Timing = (u32, u32, usize, u64, Vec<u64>, Vec<Option<(i32, i32)>>);

/// The size, frame count, end, and every frame's time and pointer.
fn timing(rec: &Recording) -> Timing {
    (rec.width, rec.height, rec.frames.len(), rec.end_ms, rec.frames.iter().map(|m| m.t_ms).collect(), rec.frames.iter().map(|m| m.cursor).collect())
}

#[test]
#[ignore = "needs a real recording and ffmpeg, and takes minutes: set MYTHICSHOT_E2E_RECORDING to a folder with frames.zst and recording.json, then run in release mode with --ignored --nocapture"]
fn a_real_recording_goes_through_save_convert_list_expand_update_and_remove() {
    let Some(source) = std::env::var_os(ENV_RECORDING).map(std::path::PathBuf::from) else {
        println!("skipped: {ENV_RECORDING} is not set");
        return;
    };
    let Some(ffmpeg) = ffmpeg::find_now().filter(|found| found.can_archive()) else {
        println!("skipped: ffmpeg with libx264 was not found on this computer");
        return;
    };
    // As the start-up does once ffmpeg was found.
    set_current_codec(Some(ffmpeg::CODEC_ID));
    println!("ffmpeg: {}; codec: {}; tolerance: frame {} dB, block {} dB over {} px, mean shift {} levels, smaller by {}; least size {} bytes", ffmpeg.version(), ffmpeg::CODEC_ID, TOLERANCE.frame_db, TOLERANCE.block_db, TOLERANCE.block, TOLERANCE.mean_shift, TOLERANCE.smaller_by, MIN_BYTES);

    let source_before = fingerprint(&source);
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::at(tmp.path().join("projects"));
    let key = ProjectKey { server: SITE.into(), tour_id: TOUR.into(), chapter_id: CHAPTER.into() };

    // 1. Open the recording and save it as a publish does.
    let original = Arc::new(Recording::open_saved(&source).unwrap_or_else(|e| panic!("the recording in {ENV_RECORDING} did not open: {e:?}")));
    let (width, height, count) = (original.width, original.height, original.frames.len());
    println!("1. recording: {width} x {height}, {count} frames, {} ms", original.end_ms);
    let edit = Edit::new(&original, Default::default());
    let snap = Snapshot { rec: &original, title: "A real recording", tour_title: "End to end", steps: 1, edit: &edit, events: &[], items: &[] };
    let saved = store.save_while(&key, &snap, &|| true).unwrap();
    assert!(saved.left_out.is_empty(), "{:?}", saved.left_out);
    let dir = saved.info.dir.clone();
    assert!(dir.starts_with(tmp.path()));
    let info = listed(&store);
    assert_eq!(info, saved.info);
    assert_eq!((info.form, info.convert, info.problem.is_none()), (Form::Frames, ConvertState::NotTried, true));
    assert_eq!((format_of(&dir, "project.json"), format_of(&dir, "recording.json")), (1, 1));
    assert_eq!(names(&dir), AS_FRAMES);
    let frames_bytes = size_of(&dir, "frames.zst");
    assert_eq!((info.frames_bytes, frames_bytes), (frames_bytes, size_of(&source, "frames.zst")));
    let totals_frames = store.totals();
    assert_eq!((totals_frames.count, totals_frames.bytes), (1, sizes(&dir).iter().map(|(_, bytes)| bytes).sum::<u64>()));
    println!("1. saved: frames form, format 1, {} bytes in all", totals_frames.bytes);

    // 2. Convert it as the queue does.
    let candidate = store.next_candidate(ffmpeg::CODEC_ID, MIN_BYTES).expect("the project is the queue's next");
    assert_eq!((&candidate.dir, candidate.frames_bytes), (&dir, frames_bytes));
    let started = Instant::now();
    let outcome = convert_one(&store, ffmpeg, &candidate, &|| true);
    let took = started.elapsed();
    let video_bytes = std::fs::metadata(dir.join("frames.mkv")).map(|m| m.len()).unwrap_or(0);
    println!("2. outcome: {outcome:?}");
    println!("2. frames bytes before: {frames_bytes}; video bytes after: {video_bytes}; ratio: {:.4} (the video is {:.1}% of the frames, {:.2} times smaller); conversion: {:.1} s", video_bytes as f64 / frames_bytes as f64, 100.0 * video_bytes as f64 / frames_bytes as f64, frames_bytes as f64 / video_bytes.max(1) as f64, took.as_secs_f64());
    println!("2. worst frame dB / worst block dB of the verification: not handed out by convert_one (its outcome carries the two sizes only); step 4 measures every expanded frame instead");
    assert_eq!(outcome, Outcome::Swapped { before: frames_bytes, after: video_bytes });

    // 3. It lists as a video, holds only what the store writes, and takes that much less.
    let info = listed(&store);
    assert_eq!((info.form, info.convert, info.problem), (Form::Video, ConvertState::Done, None));
    assert_eq!((info.dir.as_path(), info.frames_bytes), (dir.as_path(), frames_bytes));
    assert!(!dir.join("frames.zst").exists());
    assert_eq!(names(&dir), AS_VIDEO);
    assert_eq!((entries(store.root()).len(), converting_now(), store.next_candidate(ffmpeg::CODEC_ID, 0)), (1, None, None));
    let totals_video = store.totals();
    let on_disk: u64 = sizes(&dir).iter().map(|(_, bytes)| bytes).sum();
    assert_eq!((totals_video.count, totals_video.bytes, info.bytes), (1, on_disk, on_disk));
    assert!(video_bytes * 5 <= frames_bytes * 4, "the video is not a fifth smaller: {video_bytes} of {frames_bytes}");
    let shrank = totals_frames.bytes - totals_video.bytes;
    // What is not the frames or the video is two small text files, before and after.
    let small = |total: u64, big: u64| total - big;
    assert_eq!(i128::from(shrank), i128::from(frames_bytes - video_bytes) + i128::from(small(totals_frames.bytes, frames_bytes)) - i128::from(small(totals_video.bytes, video_bytes)));
    assert!(small(totals_frames.bytes, frames_bytes) < 1 << 20 && small(totals_video.bytes, video_bytes) < 1 << 20);
    println!("3. listed: video form, no problem; project.json format {}, recording.json format {}; totals {} -> {} bytes (smaller by {shrank})", format_of(&dir, "project.json"), format_of(&dir, "recording.json"), totals_frames.bytes, totals_video.bytes);

    // 4. Open it through the real program, into a temporary folder.
    let temp = tmp.path().join("edit");
    let started = Instant::now();
    let loaded = match store.load_with(&key, &Expand { codec: Some(ffmpeg), temp: &temp, keep_going: &|_, _| true }) {
        Ok(loaded) => loaded,
        Err(e) => panic!("the project did not open: {e:?}"),
    };
    println!("4. expansion: {:.1} s; expanded frames take {} bytes", started.elapsed().as_secs_f64(), sizes(&temp).iter().map(|(_, bytes)| bytes).sum::<u64>());
    assert_eq!(timing(&loaded.rec), timing(&original));
    assert!(loaded.restored.dropped.is_empty(), "{:?}", loaded.restored.dropped);
    assert_eq!((loaded.info.form, loaded.restored.title.as_str()), (Form::Video, "A real recording"));
    {
        let (mut a, mut b) = (FrameReader::new(original.clone()).unwrap(), FrameReader::new(loaded.rec.clone()).unwrap());
        let (mut worst_frame, mut worst_block, mut worst_shift, mut sum_db, mut exact) = ((f64::INFINITY, 0), (f64::INFINITY, 0), (0.0f64, 0), 0.0, 0);
        for i in 0..count {
            let (before, after) = (a.get(i).unwrap().into_raw(), b.get(i).unwrap().into_raw());
            assert_eq!(after.len(), width as usize * height as usize * 4, "frame {i}");
            assert!(after.chunks_exact(4).all(|px| px[3] == 255), "frame {i} is not opaque");
            let db = psnr_rgb(&before, &after);
            let block = worst_block_psnr(&before, &after, width, height, TOLERANCE.block);
            let shift = mean_shift(&before, &after).into_iter().fold(0.0f64, |most, s| most.max(s.abs()));
            if db.is_infinite() {
                exact += 1;
            } else {
                sum_db += db;
            }
            if db < worst_frame.0 {
                worst_frame = (db, i);
            }
            if block < worst_block.0 {
                worst_block = (block, i);
            }
            if shift > worst_shift.0 {
                worst_shift = (shift, i);
            }
        }
        let mean = if exact == count { f64::INFINITY } else { sum_db / (count - exact) as f64 };
        println!("4. every one of {count} frames compared with the original: worst frame PSNR {:.2} dB (frame {}), mean {:.2} dB over the {} frames that are not exact, {exact} exact; worst {} px block {:.2} dB (frame {}); largest mean shift of a channel {:.3} levels (frame {})", worst_frame.0, worst_frame.1, mean, count - exact, TOLERANCE.block, worst_block.0, worst_block.1, worst_shift.0, worst_shift.1);
        assert!(worst_frame.0 >= 40.0, "frame {} came back at {:.2} dB", worst_frame.1, worst_frame.0);
        assert!(worst_block.0 >= TOLERANCE.block_db && worst_shift.0 <= TOLERANCE.mean_shift, "block {:.2} dB, shift {:.3}", worst_block.0, worst_shift.0);
    }

    // 5. An Update saves over the open project: only the project file changes.
    let before = files(&dir);
    std::thread::sleep(Duration::from_millis(5));
    let update = Snapshot { rec: &loaded.rec, title: "A real recording, updated", tour_title: "End to end", steps: 1, edit: &loaded.restored.edit, events: &loaded.restored.events, items: &loaded.restored.items };
    let updated = store.save_while(&key, &update, &|| true).unwrap();
    let after = files(&dir);
    assert_eq!(names(&dir), AS_VIDEO);
    let changed: Vec<&str> = before.iter().zip(&after).filter(|(a, b)| a != b).map(|(a, _)| a.0.as_str()).collect();
    assert_eq!(changed, ["project.json"]);
    assert!(before.iter().zip(&after).any(|(a, b)| a.0 == "frames.mkv" && a.1 == b.1 && a.1.len() as u64 == video_bytes), "the video is byte for byte the same");
    let info = listed(&store);
    assert_eq!(info, updated.info);
    assert_eq!((info.form, info.convert, info.problem, info.title.as_str()), (Form::Video, ConvertState::Done, None, "A real recording, updated"));
    assert!(info.saved_at > loaded.info.saved_at);
    assert_eq!(entries(store.root()).len(), 1, "no staging folder, nothing set aside");
    println!("5. update in place: only project.json changed; still video form; frames.mkv byte-identical ({video_bytes} bytes)");

    // 6. The editor closes: it removes the frames it had expanded, and lets the project go.
    assert!(temp.is_dir());
    loaded.rec.delete_files();
    drop(loaded);
    assert!(!temp.exists(), "the temporary folder of the expansion is still there");
    assert_eq!(names(&dir), AS_VIDEO);
    println!("6. closed: the temporary folder of the expansion is gone");

    // 7. Remove the project.
    assert_eq!(store.remove_project(&dir), Ok(Removed::Now));
    assert_eq!((entries(store.root()), store.list(), store.totals().bytes), (Vec::<String>::new(), Vec::new(), 0));
    println!("7. removed: the projects folder holds nothing");

    // 8. The start-up steps find nothing to do.
    store.finish_removals();
    store.finish_conversions();
    store.sweep_converting(Duration::ZERO);
    assert_eq!((entries(store.root()), store.list(), store.totals(), converting_now()), (Vec::<String>::new(), Vec::new(), Default::default(), None));
    assert_eq!(entries(tmp.path()), ["projects"]);
    println!("8. start-up steps: nothing left, nothing changed");

    // The folder the recording was read from is as it was.
    drop(original);
    assert_eq!(fingerprint(&source), source_before, "the source folder changed");
    println!("source folder: unchanged (names, sizes, times and bytes)");
}
