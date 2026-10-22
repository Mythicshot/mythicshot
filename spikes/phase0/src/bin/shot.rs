//! Test 1: screenshot every monitor and report size, scale factor and timing.

use std::time::Instant;

fn main() {
    let out = phase0::out_dir();
    let monitors = xcap::Monitor::all().expect("list monitors");
    println!("monitors: {}", monitors.len());
    let mut failed = false;
    for (i, m) in monitors.iter().enumerate() {
        let name = m.name().unwrap_or_default();
        let (x, y) = (m.x().unwrap_or_default(), m.y().unwrap_or_default());
        let (w, h) = (m.width().unwrap_or_default(), m.height().unwrap_or_default());
        let scale = m.scale_factor().unwrap_or_default();
        let primary = m.is_primary().unwrap_or_default();
        let started = Instant::now();
        match m.capture_image() {
            Ok(img) => {
                let ms = started.elapsed().as_millis();
                let path = out.join(format!("shot-monitor{i}.png"));
                img.save(&path).expect("save png");
                println!(
                    "[{i}] {name} primary={primary} pos=({x},{y}) logical={w}x{h} scale={scale} image={}x{} capture={ms}ms -> {}",
                    img.width(),
                    img.height(),
                    path.display()
                );
            }
            Err(e) => {
                failed = true;
                println!("[{i}] {name} FAILED: {e}");
            }
        }
    }
    println!("{}", if failed { "RESULT: FAIL" } else { "RESULT: PASS" });
}
