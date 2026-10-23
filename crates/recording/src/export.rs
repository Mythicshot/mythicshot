//! Export a recording to GIF (gifski), APNG (png) or MP4/WebM (ffmpeg, if
//! installed). The caller builds the list of output frames, which is where
//! trimming, deleted frames, speed and reverse happen, and supplies
//! `decorate`, which turns a source frame into the final picture (annotations,
//! overlays, crop, scale). Frames are decoded and decorated one at a time, so
//! memory stays bounded.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use gifski::progress::ProgressReporter;
use gifski::{Repeat, Settings};
use image::RgbaImage;
use imgref::ImgVec;
use rgb::RGBA8;

use crate::{FrameReader, Recording};

/// Browsers slow down GIF frames shorter than 20 ms, so never go above 50 fps.
pub const MAX_GIF_FPS: u32 = 50;

/// One output frame: which recorded frame to show, and when.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutFrame {
    pub src: usize,
    pub t_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gif,
    Apng,
    Mp4,
    Webm,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Gif, Format::Apng, Format::Mp4, Format::Webm];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Gif => "gif",
            Format::Apng => "png",
            Format::Mp4 => "mp4",
            Format::Webm => "webm",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Gif => "GIF",
            Format::Apng => "APNG",
            Format::Mp4 => "MP4",
            Format::Webm => "WebM",
        }
    }

    pub fn needs_ffmpeg(self) -> bool {
        matches!(self, Format::Mp4 | Format::Webm)
    }
}

pub struct Job {
    pub frames: Vec<OutFrame>,
    /// When the last frame ends.
    pub end_ms: u64,
    /// Output size; `decorate` must return frames of exactly this size.
    pub width: u32,
    pub height: u32,
    pub format: Format,
    /// GIF quality, 1–100.
    pub quality: u8,
    pub loop_forever: bool,
    /// Frame rate for MP4/WebM, which need a constant rate.
    pub video_fps: u32,
}

/// The frame list for an unedited recording: every frame at its own time.
pub fn all_frames(rec: &Recording) -> Vec<OutFrame> {
    rec.frames.iter().enumerate().map(|(i, f)| OutFrame { src: i, t_ms: f.t_ms }).collect()
}

/// Drop frames that would play faster than `max_fps`.
pub fn limit_rate(frames: &[OutFrame], max_fps: u32) -> Vec<OutFrame> {
    let gap = 1000 / max_fps.max(1) as u64;
    let mut out: Vec<OutFrame> = Vec::with_capacity(frames.len());
    for f in frames {
        if out.last().is_none_or(|l| f.t_ms >= l.t_ms + gap) {
            out.push(*f);
        }
    }
    out
}

/// Whether ffmpeg can be run, for MP4 and WebM.
pub fn ffmpeg_available() -> bool {
    hidden(Command::new("ffmpeg").arg("-version")).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Export `job` to `path`. `progress` gets 0.0–1.0 and returns false to
/// cancel. Returns the file size in bytes.
pub fn export(
    rec: Arc<Recording>,
    job: &Job,
    path: &Path,
    decorate: impl Fn(usize, RgbaImage) -> RgbaImage + Sync,
    mut progress: impl FnMut(f32) -> bool + Send,
) -> Result<u64, String> {
    if job.frames.is_empty() {
        return Err("there are no frames to export".into());
    }
    match job.format {
        Format::Gif => gif(rec, job, path, &decorate, &mut progress)?,
        Format::Apng => apng(rec, job, path, &decorate, &mut progress)?,
        Format::Mp4 | Format::Webm => video(rec, job, path, &decorate, &mut progress)?,
    }
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| e.to_string())
}

type Decorate<'a> = &'a (dyn Fn(usize, RgbaImage) -> RgbaImage + Sync);
type Progress<'a> = &'a mut (dyn FnMut(f32) -> bool + Send);

struct GifProgress<'a> {
    done: usize,
    total: usize,
    callback: Progress<'a>,
}

impl ProgressReporter for GifProgress<'_> {
    fn increase(&mut self) -> bool {
        self.done += 1;
        (self.callback)(self.done as f32 / self.total.max(1) as f32)
    }
}

fn gif(rec: Arc<Recording>, job: &Job, path: &Path, decorate: Decorate, progress: Progress) -> Result<(), String> {
    let frames = limit_rate(&job.frames, MAX_GIF_FPS);
    let repeat = if job.loop_forever { Repeat::Infinite } else { Repeat::Finite(0) };
    // Without a size gifski shrinks frames to about 800×600.
    let settings = Settings { width: Some(job.width), height: Some(job.height), quality: job.quality.clamp(1, 100), fast: false, repeat };
    let (collector, writer) = gifski::new(settings).map_err(|e| e.to_string())?;
    let file = File::create(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
    let total = frames.len();
    std::thread::scope(|scope| {
        let feeder = scope.spawn(move || -> Result<(), String> {
            let mut reader = FrameReader::new(rec)?;
            for (i, f) in frames.iter().enumerate() {
                let img = decorate(f.src, reader.get(f.src)?);
                let (w, h) = (img.width() as usize, img.height() as usize);
                let px: Vec<RGBA8> = img.into_raw().chunks_exact(4).map(|c| RGBA8::new(c[0], c[1], c[2], 255)).collect();
                collector.add_frame_rgba(i, ImgVec::new(px, w, h), f.t_ms as f64 / 1000.0).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        let mut reporter = GifProgress { done: 0, total, callback: progress };
        let written = writer.write(BufWriter::new(file), &mut reporter).map_err(|e| e.to_string());
        let fed = feeder.join().map_err(|_| "the frame reader panicked".to_string())?;
        fed.and(written)
    })
}

fn apng(rec: Arc<Recording>, job: &Job, path: &Path, decorate: Decorate, progress: Progress) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), job.width, job.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_animated(job.frames.len() as u32, if job.loop_forever { 0 } else { 1 }).map_err(|e| e.to_string())?;
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    let mut reader = FrameReader::new(rec)?;
    for (i, f) in job.frames.iter().enumerate() {
        let next = job.frames.get(i + 1).map(|n| n.t_ms).unwrap_or(job.end_ms);
        let delay = next.saturating_sub(f.t_ms).clamp(10, u16::MAX as u64) as u16;
        writer.set_frame_delay(delay, 1000).map_err(|e| e.to_string())?;
        let img = decorate(f.src, reader.get(f.src)?);
        writer.write_image_data(img.as_raw()).map_err(|e| e.to_string())?;
        if !progress((i + 1) as f32 / job.frames.len() as f32) {
            return Err("cancelled".into());
        }
    }
    writer.finish().map_err(|e| e.to_string())
}

/// MP4/WebM through ffmpeg: raw frames on stdin at a constant rate, each
/// output tick showing the latest frame due by then.
fn video(rec: Arc<Recording>, job: &Job, path: &Path, decorate: Decorate, progress: Progress) -> Result<(), String> {
    let fps = job.video_fps.clamp(1, 60);
    let codec: &[&str] = match job.format {
        Format::Webm => &["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", "34", "-row-mt", "1"],
        _ => &["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-movflags", "+faststart"],
    };
    let mut child = hidden(Command::new("ffmpeg").args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"]))
        .arg(format!("{}x{}", job.width, job.height))
        .args(["-r", &fps.to_string(), "-i", "-"])
        .args(codec)
        // Most players need even dimensions and 4:2:0 colour.
        .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2", "-pix_fmt", "yuv420p"])
        .arg(path)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ffmpeg (is it installed?): {e}"))?;
    let mut stdin = BufWriter::new(child.stdin.take().ok_or("no ffmpeg input")?);
    let mut reader = FrameReader::new(rec)?;
    let first = job.frames[0].t_ms;
    let ticks = (job.end_ms.saturating_sub(first) * fps as u64 / 1000).max(1);
    let mut cursor = 0usize;
    let mut last: Option<(usize, RgbaImage)> = None;
    for tick in 0..ticks {
        let t = first + tick * 1000 / fps as u64;
        while cursor + 1 < job.frames.len() && job.frames[cursor + 1].t_ms <= t {
            cursor += 1;
        }
        if last.as_ref().is_none_or(|(c, _)| *c != cursor) {
            let f = job.frames[cursor];
            last = Some((cursor, decorate(f.src, reader.get(f.src)?)));
        }
        let img = &last.as_ref().expect("frame").1;
        if stdin.write_all(img.as_raw()).is_err() {
            break;
        }
        if !progress((tick + 1) as f32 / ticks as f32) {
            let _ = child.kill();
            return Err("cancelled".into());
        }
    }
    drop(stdin);
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(format!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

/// Run a console program without flashing a console window.
fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_keeps_spacing() {
        let frames: Vec<OutFrame> = (0..10).map(|i| OutFrame { src: i, t_ms: i as u64 * 10 }).collect();
        let kept = limit_rate(&frames, 50);
        assert_eq!(kept.iter().map(|f| f.src).collect::<Vec<_>>(), vec![0, 2, 4, 6, 8]);
    }
}
