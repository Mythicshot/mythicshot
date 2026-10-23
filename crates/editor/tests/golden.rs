//! Golden-image tests: render known documents and compare with reference PNGs
//! in `tests/golden/`. The renderer is pure CPU code, so the output should be
//! identical on every OS. To accept a deliberate change, run with
//! `MYTHICSHOT_BLESS=1` and review the new PNGs before committing them.

use std::path::PathBuf;

use mythicshot_editor::{Bounds, Document, ObfuscateMode, Rgba, RgbaImage, Shape, Style, effects, pt};

fn checker(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let on = ((x / 20) + (y / 20)) % 2 == 0;
        let g = (x * 255 / w) as u8;
        if on { image::Rgba([235, 238, 242, 255]) } else { image::Rgba([g, 120, 200, 255]) }
    })
}

fn all_tools() -> Document {
    let mut d = Document::new(checker(640, 400));
    let red = Style::default();
    let blue = Style { stroke: Rgba([47, 111, 235, 255]), width: 4.0, ..Style::default() };
    d.add(Shape::Rect(Bounds::new(pt(20.0, 20.0), pt(160.0, 100.0))), red);
    d.add(Shape::Ellipse(Bounds::new(pt(190.0, 20.0), pt(330.0, 100.0))), Style { fill: Some(Rgba([255, 230, 0, 160])), ..blue });
    d.add(Shape::Line(pt(360.0, 30.0), pt(480.0, 90.0)), red);
    d.add(Shape::Arrow(pt(500.0, 100.0), pt(610.0, 25.0)), blue);
    let wave: Vec<_> = (0..60).map(|i| pt(20.0 + i as f32 * 4.0, 150.0 + (i as f32 * 0.3).sin() * 20.0)).collect();
    d.add(Shape::Freehand(wave), red);
    d.add(Shape::Text { at: pt(290.0, 130.0), text: "Click Save first\nthen Export".into() }, Style { font_size: 24.0, ..red });
    d.add(Shape::Bubble { rect: Bounds::new(pt(30.0, 210.0), pt(250.0, 290.0)), tail: pt(140.0, 340.0), text: "Speech bubbles wrap their text inside the box".into() }, blue);
    d.add(Shape::Step { center: pt(300.0, 260.0), number: 1 }, red);
    d.add(Shape::Step { center: pt(350.0, 260.0), number: 2 }, red);
    d.add(Shape::Highlight(Bounds::new(pt(400.0, 200.0), pt(620.0, 240.0))), red);
    d.add(Shape::Obfuscate { rect: Bounds::new(pt(400.0, 260.0), pt(500.0, 380.0)), mode: ObfuscateMode::Pixelate }, red);
    d.add(Shape::Obfuscate { rect: Bounds::new(pt(520.0, 260.0), pt(620.0, 380.0)), mode: ObfuscateMode::Blur }, red);
    d
}

fn check(name: &str, img: &RgbaImage) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let path = dir.join(format!("{name}.png"));
    if std::env::var_os("MYTHICSHOT_BLESS").is_some() || !path.exists() {
        std::fs::create_dir_all(&dir).unwrap();
        img.save(&path).unwrap();
        eprintln!("wrote {}", path.display());
        return;
    }
    let want = image::open(&path).unwrap().to_rgba8();
    assert_eq!(want.dimensions(), img.dimensions(), "{name}: size changed");
    let worst = want.pixels().zip(img.pixels()).map(|(a, b)| (0..4).map(|c| (a.0[c] as i32 - b.0[c] as i32).abs()).max().unwrap()).max().unwrap_or(0);
    assert!(worst <= 1, "{name}: pixels differ by up to {worst}; rerun with MYTHICSHOT_BLESS=1 if the change is intended");
}

#[test]
fn every_tool() {
    check("every_tool", &all_tools().flatten());
}

#[test]
fn effects_on_annotated_image() {
    let mut d = all_tools();
    d.crop(Bounds::new(pt(0.0, 0.0), pt(340.0, 200.0)));
    d.apply(effects::torn_edge);
    d.apply(effects::drop_shadow);
    check("torn_edge_shadow", &d.flatten());
}
