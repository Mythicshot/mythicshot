//! Test 5: encode `out/recording.raw` (from the `record` test) as a GIF with
//! gifski and report encode time and file size.

use std::fs::File;
use std::thread;
use std::time::Instant;

use gifski::{Repeat, Settings, progress::NoProgress};
use imgref::ImgVec;
use rgb::RGBA8;

fn main() {
    let out = phase0::out_dir();
    let mut reader = phase0::RawReader::open(&out.join("recording.raw")).expect("open recording.raw (run the record test first)");
    let (w, h) = (reader.width, reader.height);
    let settings = Settings { width: Some(w), height: Some(h), quality: 90, fast: false, repeat: Repeat::Infinite };
    let (collector, writer) = gifski::new(settings).expect("start gifski");
    let started = Instant::now();

    let feeder = thread::spawn(move || {
        let mut index = 0usize;
        while let Some((pts_ns, px)) = reader.next_frame().expect("read frame") {
            let pixels: Vec<RGBA8> = px.chunks_exact(4).map(|c| RGBA8::new(c[0], c[1], c[2], 255)).collect();
            collector
                .add_frame_rgba(index, ImgVec::new(pixels, w as usize, h as usize), pts_ns as f64 / 1e9)
                .expect("add frame");
            index += 1;
        }
        index
    });

    let path = out.join("recording.gif");
    writer.write(File::create(&path).expect("create gif"), &mut NoProgress {}).expect("write gif");
    let frames = feeder.join().expect("feeder thread");
    let secs = started.elapsed().as_secs_f64();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("frames={frames} {w}x{h} encode={secs:.1}s size={:.2} MB -> {}", size as f64 / 1_048_576.0, path.display());
    println!("{}", if frames > 0 && size > 0 { "RESULT: PASS" } else { "RESULT: FAIL" });
}
