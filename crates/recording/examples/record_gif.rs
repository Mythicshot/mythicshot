//! Record a 640×400 area of the primary display and export a GIF.
//!
//! `cargo run --release -p mythicshot-recording --example record_gif -- <seconds> <fps> <out.gif> [full]`
//!
//! With `full` it records the whole primary display instead.
//!
//! Prints frame counts, timing and the recorder's memory, for the Phase 4
//! "60 seconds at 15 fps keeps memory flat" check.

use std::time::{Duration, Instant};

use mythicshot_recording::{Recorder, Target, export};

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(5);
    let fps: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(15);
    let out = args.next().unwrap_or_else(|| "record_gif.gif".into());

    let full = args.next().as_deref() == Some("full");
    let primary = xcap_primary();
    let (width, height, x, y) = if full { (primary.2.0, primary.2.1, 0, 0) } else { (640, 400, 200, 200) };
    let target = Target { display: primary.0, origin: primary.1, x, y, width, height };
    println!("recording {}x{} on {} for {seconds}s at {fps} fps", target.width, target.height, target.display);

    let rec = Recorder::start(target, fps).expect("start recording");
    let started = Instant::now();
    let mut peak = 0usize;
    let mut samples = Vec::new();
    while started.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(500));
        let ws = working_set();
        peak = peak.max(ws);
        samples.push(ws);
    }
    let stats = rec.stats();
    let wall = started.elapsed().as_secs_f64();
    let recording = rec.stop().expect("stop recording");
    println!("frames={} active={:.2}s wall={wall:.2}s effective_fps={:.1}", recording.frames.len(), recording.end_ms as f64 / 1000.0, recording.frames.len() as f64 / wall);
    println!("compressed on disk {:.1} MB ({:.0} KB per frame)", stats.compressed_bytes as f64 / 1_048_576.0, stats.compressed_bytes as f64 / 1024.0 / recording.frames.len().max(1) as f64);
    let mb = |b: usize| b as f64 / 1_048_576.0;
    let first = samples.get(2).copied().unwrap_or(0);
    let last = samples.last().copied().unwrap_or(0);
    println!("working set: after 1s {:.1} MB, at end {:.1} MB, peak {:.1} MB", mb(first), mb(last), mb(peak));

    let t = Instant::now();
    let recording = std::sync::Arc::new(recording);
    let job = export::Job {
        frames: export::all_frames(&recording),
        end_ms: recording.end_ms,
        width: recording.width,
        height: recording.height,
        format: export::Format::Gif,
        quality: 90,
        loop_forever: true,
        video_fps: fps,
    };
    let size = export::export(recording.clone(), &job, std::path::Path::new(&out), |_, img| img, |_| true).expect("encode gif");
    println!("gif {:.2} MB in {:.1}s -> {out}", size as f64 / 1_048_576.0, t.elapsed().as_secs_f64());
    recording.delete_files();
}

fn xcap_primary() -> (String, (i32, i32), (u32, u32)) {
    let m = xcap::Monitor::all().expect("monitors").into_iter().find(|m| m.is_primary().unwrap_or(false)).expect("primary monitor");
    (m.name().unwrap_or_default(), (m.x().unwrap_or(0), m.y().unwrap_or(0)), (m.width().unwrap_or(0), m.height().unwrap_or(0)))
}

#[cfg(windows)]
fn working_set() -> usize {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb);
    }
    c.WorkingSetSize
}

#[cfg(not(windows))]
fn working_set() -> usize {
    0
}
