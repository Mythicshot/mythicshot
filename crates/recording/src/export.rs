//! GIF export with gifski. Frames are decoded from disk one at a time on a
//! feeder thread while gifski encodes, so memory stays bounded.

use std::fs::File;
use std::path::Path;

use gifski::progress::ProgressReporter;
use gifski::{Repeat, Settings};
use imgref::ImgVec;
use rgb::RGBA8;

use crate::{Recording, cursor};

/// Browsers slow down GIF frames shorter than 20 ms, so never go above 50 fps.
pub const MAX_GIF_FPS: u32 = 50;

pub struct Progress<F: FnMut(f32) -> bool + Send> {
    done: usize,
    total: usize,
    callback: F,
}

impl<F: FnMut(f32) -> bool + Send> ProgressReporter for Progress<F> {
    fn increase(&mut self) -> bool {
        self.done += 1;
        (self.callback)(self.done as f32 / self.total.max(1) as f32)
    }
}

/// Encode `rec` as a looping GIF at `path`. `on_progress` gets 0.0–1.0 and
/// returns false to cancel. Returns the file size in bytes.
pub fn encode_gif(rec: &Recording, path: &Path, quality: u8, draw_cursor: bool, on_progress: impl FnMut(f32) -> bool + Send) -> Result<u64, String> {
    if rec.frames.is_empty() {
        return Err("the recording has no frames".into());
    }
    // Without a size gifski shrinks frames to about 800×600; keep the recorded size.
    let settings = Settings { width: Some(rec.width), height: Some(rec.height), quality: quality.clamp(1, 100), fast: false, repeat: Repeat::Infinite };
    let (collector, writer) = gifski::new(settings).map_err(|e| e.to_string())?;
    let file = File::create(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
    let total = rec.frames.len();
    let min_gap = 1000 / MAX_GIF_FPS as u64;

    std::thread::scope(|scope| {
        let feeder = scope.spawn(move || -> Result<(), String> {
            let mut last_t: Option<u64> = None;
            let mut index = 0usize;
            rec.for_each_frame(|i, mut img| {
                let meta = rec.frames[i];
                // Drop frames that would play faster than browsers allow.
                if last_t.is_some_and(|t| meta.t_ms < t + min_gap) {
                    return Ok(());
                }
                last_t = Some(meta.t_ms);
                if draw_cursor {
                    if let Some((x, y)) = meta.cursor {
                        cursor::draw(&mut img, x, y);
                    }
                }
                let (w, h) = (img.width() as usize, img.height() as usize);
                let px: Vec<RGBA8> = img.into_raw().chunks_exact(4).map(|c| RGBA8::new(c[0], c[1], c[2], 255)).collect();
                collector.add_frame_rgba(index, ImgVec::new(px, w, h), meta.t_ms as f64 / 1000.0).map_err(|e| e.to_string())?;
                index += 1;
                Ok(())
            })
        });
        let mut progress = Progress { done: 0, total, callback: on_progress };
        let written = writer.write(std::io::BufWriter::new(file), &mut progress).map_err(|e| e.to_string());
        let fed = feeder.join().map_err(|_| "the frame reader panicked".to_string())?;
        fed.and(written)
    })?;
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| e.to_string())
}
