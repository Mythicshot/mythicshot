//! Test helper: list top-level windows of processes whose name contains the
//! first argument, with their rectangles.

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_default().to_lowercase();
    // `wins <app> capture` also saves each matching window's contents to out/.
    let capture = std::env::args().nth(2).as_deref() == Some("capture");
    for w in xcap::Window::all().expect("list windows") {
        let app = w.app_name().unwrap_or_default();
        if !app.to_lowercase().contains(&filter) {
            continue;
        }
        println!(
            "pid={} app={app} title={:?} pos=({},{}) size={}x{} minimized={}",
            w.pid().unwrap_or_default(),
            w.title().unwrap_or_default(),
            w.x().unwrap_or_default(),
            w.y().unwrap_or_default(),
            w.width().unwrap_or_default(),
            w.height().unwrap_or_default(),
            w.is_minimized().unwrap_or_default()
        );
        if capture {
            let path = phase0::out_dir().join(format!("window-{}.png", w.id().unwrap_or_default()));
            match w.capture_image() {
                Ok(img) => {
                    img.save(&path).expect("save window image");
                    println!("  saved {}", path.display());
                }
                Err(e) => println!("  capture failed: {e}"),
            }
        }
    }
}
