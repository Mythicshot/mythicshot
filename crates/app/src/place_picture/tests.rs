//! The size, place and pixels of a placed picture, and the reads behind it. Every picture here is
//! made in the OS temp folder.

use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, Instant};

use image::{ImageFormat, Rgba};

use super::*;

/// A picture file of `w` by `h` in `dir`, as PNG.
pub(crate) fn picture_file(dir: &Path, name: &str, w: u32, h: u32, colour: [u8; 4]) -> PathBuf {
    let path = dir.join(name);
    let mut bytes = Cursor::new(Vec::new());
    RgbaImage::from_pixel(w, h, Rgba(colour)).write_to(&mut bytes, ImageFormat::Png).unwrap();
    std::fs::write(&path, bytes.into_inner()).unwrap();
    path
}

/// Wait (at most ten seconds) until `placing` hands over something, and take it.
pub(crate) fn wait(placing: &mut Placing) -> Vec<Placed> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let got = placing.poll();
        if !got.is_empty() || Instant::now() > until {
            return got;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_picture_that_fits_keeps_its_size_and_a_larger_one_fits_inside_with_a_margin() {
    // Fits, even exactly: its own size.
    assert_eq!(placed_size((20, 10), (64, 48)), (20, 10));
    assert_eq!(placed_size((64, 48), (64, 48)), (64, 48));
    // Too wide: 90% of the width, its proportions kept.
    assert_eq!(placed_size((200, 60), (100, 100)), (90, 27));
    // Too tall: 90% of the height.
    assert_eq!(placed_size((40, 400), (100, 200)), (18, 180));
    // Too large both ways: the tighter side decides.
    let (w, h) = placed_size((4000, 3000), (1920, 1080));
    assert_eq!(h, 972, "90% of 1080");
    assert!(((w as f32 / h as f32) - 4.0 / 3.0).abs() < 0.01, "{w}x{h}");
    assert!(w as f32 <= 1920.0 * 0.9 + 0.5);
    // Never smaller than a pixel.
    assert_eq!(placed_size((10_000, 1), (100, 100)), (90, 1));
}

#[test]
fn a_picture_is_centred_on_the_point_and_moved_only_as_far_as_it_must_to_lie_inside() {
    let capture = (200, 100);
    // Room all round: centred on the point.
    assert_eq!(placed_rect((40.0, 20.0), pt(100.0, 50.0), capture), Bounds::new(pt(80.0, 40.0), pt(120.0, 60.0)));
    // Near the top left corner: pushed in to touch the edges.
    assert_eq!(placed_rect((40.0, 20.0), pt(5.0, 3.0), capture), Bounds::new(pt(0.0, 0.0), pt(40.0, 20.0)));
    // Past the bottom right, even outside the capture: pushed back in.
    assert_eq!(placed_rect((40.0, 20.0), pt(260.0, 140.0), capture), Bounds::new(pt(160.0, 80.0), pt(200.0, 100.0)));
    // Only the side that sticks out moves.
    assert_eq!(placed_rect((40.0, 20.0), pt(190.0, 50.0), capture), Bounds::new(pt(160.0, 40.0), pt(200.0, 60.0)));
    // As large as the capture: it covers it exactly, wherever it was dropped.
    assert_eq!(placed_rect((200.0, 100.0), pt(10.0, 90.0), capture), Bounds::new(pt(0.0, 0.0), pt(200.0, 100.0)));
}

#[test]
fn a_larger_picture_is_resampled_down_and_a_smaller_one_is_left_alone() {
    let small = RgbaImage::from_pixel(20, 10, Rgba([1, 2, 3, 255]));
    let same = resample(small.clone(), 40, 40);
    assert_eq!(same, small, "never enlarged, never touched");
    // An even colour stays that colour, at the size asked for.
    let big = RgbaImage::from_pixel(1000, 500, Rgba([200, 100, 50, 255]));
    let out = resample(big, 90, 45);
    assert_eq!(out.dimensions(), (90, 45));
    assert!(out.pixels().all(|p| p.0.iter().zip([200u8, 100, 50, 255]).all(|(a, b)| a.abs_diff(b) <= 1)), "{:?}", out.get_pixel(45, 22));
    // Fine detail is averaged, not dropped: black and white lines come out grey.
    let lines = RgbaImage::from_fn(400, 400, |x, _| if x % 2 == 0 { Rgba([0, 0, 0, 255]) } else { Rgba([255, 255, 255, 255]) });
    let out = resample(lines, 40, 40);
    let g = out.get_pixel(20, 20).0[0];
    assert!((110..=145).contains(&g), "grey, got {g}");
}

#[test]
fn see_through_edges_keep_their_colour_when_resampled() {
    // Red on fully see-through black: resampled, no pixel turns dark at the edge.
    let img = RgbaImage::from_fn(300, 300, |x, _| if x < 150 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 0, 0]) });
    let out = resample(img, 30, 30);
    for p in out.pixels().filter(|p| p.0[3] > 16) {
        assert!(p.0[0] >= 230 && p.0[1] <= 20 && p.0[2] <= 20, "a dark fringe: {p:?}");
    }
}

#[test]
fn the_filter_runs_only_when_its_memory_fits_and_the_block_path_still_makes_the_size() {
    // A capture-sized result from a picture averaged to twice its width fits.
    assert!(filter_fits(2 * 1920, 1080));
    // The float pass of 16 bytes a pixel is held to the bound.
    let limit = (RESAMPLE_BYTES / 16) as u32;
    assert!(filter_fits(limit, 1) && !filter_fits(limit + 1, 1));
    assert!(!filter_fits(2 * 16_384, 16_384), "the largest picture would need 8 GB");
    // Either way the picture comes out at the size asked for, in its colour.
    let img = RgbaImage::from_pixel(2100, 1500, Rgba([9, 99, 199, 255]));
    let out = resample(img, 700, 500);
    assert_eq!(out.dimensions(), (700, 500));
    assert_eq!(out.get_pixel(350, 250).0, [9, 99, 199, 255]);
}

#[test]
fn a_huge_picture_is_stored_at_the_size_it_is_shown() {
    let img = RgbaImage::from_pixel(3000, 2000, Rgba([0, 128, 255, 255]));
    let data = prepare(img, (640, 480));
    assert_eq!((data.width(), data.height()), placed_size((3000, 2000), (640, 480)));
    assert_eq!((data.width(), data.height()), (576, 384));
    // One that fits is stored as it is.
    let data = prepare(RgbaImage::new(30, 20), (640, 480));
    assert_eq!((data.width(), data.height()), (30, 20));
}

fn target() -> Target {
    Target { capture: (64, 48), view: None }
}

#[test]
fn a_read_hands_over_the_picture_made_ready_on_the_loaders_thread_with_its_place() {
    let tmp = tempfile::tempdir().unwrap();
    let path = picture_file(tmp.path(), "big.png", 640, 120, [0, 0, 255, 255]);
    let mut placing = Placing::default();
    assert!(!placing.pending());
    placing.request(path, pt(10.0, 20.0), target());
    assert!(placing.pending(), "an update waits while the picture is read");
    let got = wait(&mut placing);
    assert!(!placing.pending());
    match got.as_slice() {
        [Placed::Picture { image, centre, target: t }] => {
            // Resampled before it was handed over: 90% of 64 wide.
            assert_eq!((image.width(), image.height()), (58, 11));
            assert_eq!((*centre, *t), (pt(10.0, 20.0), target()));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_that_is_not_a_picture_is_said_with_the_usual_sentence() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.png");
    std::fs::write(&path, "not a picture").unwrap();
    let mut placing = Placing::default();
    placing.request(path.clone(), pt(0.0, 0.0), target());
    match wait(&mut placing).as_slice() {
        [Placed::Failed { sentence, target: t }] => {
            assert_eq!(*sentence, open_image::could_not_open(&path, &open_image::load(&path).unwrap_err()));
            assert!(sentence.starts_with(&format!("Could not open {}: ", path.display())));
            assert_eq!(*t, target());
        }
        other => panic!("{other:?}"),
    }
    assert!(!placing.pending());
}

#[test]
fn only_the_newest_request_of_an_editor_is_placed_and_two_editors_never_cancel_each_other() {
    let tmp = tempfile::tempdir().unwrap();
    let a = picture_file(tmp.path(), "a.png", 8, 8, [255, 0, 0, 255]);
    let b = picture_file(tmp.path(), "b.png", 6, 4, [0, 255, 0, 255]);
    let mut one = Placing::default();
    let mut two = Placing::default();
    one.request(a.clone(), pt(1.0, 1.0), target());
    one.request(b.clone(), pt(2.0, 2.0), target());
    two.request(a.clone(), pt(3.0, 3.0), target());
    // The first editor places only its newer picture; the older read ends silently.
    let mut got = Vec::new();
    let until = Instant::now() + Duration::from_secs(10);
    while one.pending() && Instant::now() < until {
        got.extend(one.poll());
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    got.extend(one.poll());
    assert!(matches!(got.as_slice(), [Placed::Picture { image, centre, .. }] if (image.width(), image.height()) == (6, 4) && *centre == pt(2.0, 2.0)), "{got:?}");
    // The second editor's own request still arrives.
    assert!(matches!(wait(&mut two).as_slice(), [Placed::Picture { image, centre, .. }] if image.width() == 8 && *centre == pt(3.0, 3.0)));
}

#[test]
fn a_picture_for_an_editor_that_is_gone_is_dropped_without_harm() {
    let tmp = tempfile::tempdir().unwrap();
    let path = picture_file(tmp.path(), "a.png", 8, 8, [255, 0, 0, 255]);
    let mut placing = Placing::default();
    placing.request(path, pt(1.0, 1.0), target());
    // The editor closes before the picture arrives: its reads still end, and send to nobody.
    drop(placing);
    std::thread::sleep(Duration::from_millis(200));
}
