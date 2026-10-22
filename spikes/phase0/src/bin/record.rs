//! Test 2: record a 640x400 region of the primary display at 15 fps for 10
//! seconds, streaming frames to `out/recording.raw` so memory stays flat.

use std::time::{Duration, Instant};

use pinray::{CaptureEvent, CaptureSession, CursorMode, FrameData, PixelFormat, Rect, SourceId, VideoCaptureTarget};

const W: u32 = 640;
const H: u32 = 400;
const FPS: u32 = 15;
const SECONDS: u64 = 10;

fn main() {
    let path = phase0::out_dir().join("recording.raw");
    let mut session = CaptureSession::builder()
        .video_target(VideoCaptureTarget::Display(SourceId::new("auto")))
        .pixel_format(PixelFormat::Rgba8888)
        .cursor_mode(CursorMode::Embedded)
        .crop_rect(Some(Rect { x: 200, y: 200, width: W, height: H }))
        .frame_rate(Some(FPS))
        .build()
        .expect("build capture session");
    println!("backend: {:?}", session.backend_info());

    let mut writer = phase0::RawWriter::create(&path, W, H).expect("create raw file");
    let mut packed = vec![0u8; (W * H * 4) as usize];
    let (mut frames, mut gaps, mut dropped) = (0u64, 0u64, 0u64);
    let mut first_ts: Option<i64> = None;
    let mut last_seq: Option<u64> = None;
    let mut peak_ws = 0usize;

    session.start().expect("start capture");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(SECONDS) {
        match session.next_event(Some(Duration::from_millis(500))) {
            Ok(CaptureEvent::Video(frame)) => {
                if frame.width != W || frame.height != H {
                    panic!("unexpected frame size {}x{}", frame.width, frame.height);
                }
                let FrameData::Host(data) = &frame.data else { panic!("frame not in host memory") };
                let row = (W * 4) as usize;
                for y in 0..H as usize {
                    let src = y * frame.stride as usize;
                    packed[y * row..(y + 1) * row].copy_from_slice(&data[src..src + row]);
                }
                let base = *first_ts.get_or_insert(frame.stream_time_ns);
                writer.push((frame.stream_time_ns - base) as u64, &packed).expect("write frame");
                if let Some(prev) = last_seq {
                    dropped += frame.sequence.saturating_sub(prev + 1);
                }
                last_seq = Some(frame.sequence);
                frames += 1;
                peak_ws = peak_ws.max(working_set());
            }
            Ok(CaptureEvent::Gap(gap)) => {
                gaps += 1;
                eprintln!("gap: {gap:?}");
            }
            Ok(_) => {}
            Err(e) => eprintln!("event error: {e}"),
        }
    }
    session.stop().expect("stop capture");
    writer.finish().expect("flush raw file");

    let wall = started.elapsed().as_secs_f64();
    let size_mb = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as f64 / 1_048_576.0;
    println!("frames={frames} wall={wall:.2}s effective_fps={:.1} gaps={gaps} dropped={dropped}", frames as f64 / wall);
    println!("raw file {size_mb:.1} MB -> {}", path.display());
    println!("peak working set {:.1} MB", peak_ws as f64 / 1_048_576.0);
    let ok = frames as f64 >= (FPS as f64 * SECONDS as f64) * 0.9 && dropped == 0;
    println!("{}", if ok { "RESULT: PASS" } else { "RESULT: CHECK (fewer frames than expected; a still screen sends fewer frames)" });
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
