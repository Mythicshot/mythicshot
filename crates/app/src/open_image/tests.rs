//! Opening and dropping picture files: the dialog asked for, the limits a file is read within,
//! and the verdict on what is dragged. Every file here is made in the OS temp folder.

use std::io::{Cursor, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use image::{ImageFormat, Rgba};
use mythicshot_destinations::stubs;

use super::*;

#[test]
fn picking_asks_the_open_image_dialog_and_no_real_dialog_opens() {
    let tmp = tempfile::tempdir().unwrap();
    stubs::take();
    assert_eq!(pick(tmp.path()), None, "a test build answers as if cancelled");
    assert_eq!(stubs::take(), ["open dialog: open image"]);
}

/// A reader that counts the bytes taken from it.
struct Counted<R> {
    inner: R,
    read: Arc<AtomicU64>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::SeqCst);
        Ok(n)
    }
}

fn png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A real PNG whose header says it is `w` by `h` at `depth` bits a channel (RGBA): what a
/// decompression bomb looks like from the outside.
fn png_claiming_depth(w: u32, h: u32, depth: u8) -> Vec<u8> {
    let mut bytes = png(&RgbaImage::new(1, 1));
    bytes[16..20].copy_from_slice(&w.to_be_bytes());
    bytes[20..24].copy_from_slice(&h.to_be_bytes());
    bytes[24] = depth;
    let crc = crc32(&bytes[12..29]);
    bytes[29..33].copy_from_slice(&crc.to_be_bytes());
    bytes
}

fn png_claiming(w: u32, h: u32) -> Vec<u8> {
    png_claiming_depth(w, h, 8)
}

/// A BMP header saying `w` by `h` at 24 bits, and a little of nothing after it.
fn bmp_claiming(w: i32, h: i32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(b"BM");
    b.extend(1054u32.to_le_bytes());
    b.extend([0u8; 4]);
    b.extend(54u32.to_le_bytes());
    b.extend(40u32.to_le_bytes());
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend([0u8; 24]);
    b.extend([0u8; 1000]);
    b
}

#[test]
fn the_limits_are_the_ones_stated() {
    assert_eq!(MAX_FILE_BYTES, 67_108_864);
    assert_eq!(MAX_SIDE, 16_384);
    assert_eq!(MAX_PIXELS, 67_108_864);
    assert_eq!(MAX_DECODED_BYTES, 268_435_456);
}

#[test]
fn a_file_over_the_size_limit_is_refused_by_its_size() {
    // A file one byte over (sparse, nothing written) is refused by its size, unread.
    let tmp = tempfile::tempdir().unwrap();
    let big = tmp.path().join("big.png");
    let f = File::create(&big).unwrap();
    f.set_len(MAX_FILE_BYTES + 1).unwrap();
    drop(f);
    let started = std::time::Instant::now();
    assert!(matches!(load(&big), Err(ImageError::Limits(_))));
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "refused without reading it");
}

#[test]
fn the_read_stops_one_byte_past_the_limit_however_much_more_there_is() {
    // A reader that never ends, as a file that keeps growing or lies about its size would.
    let read = Arc::new(AtomicU64::new(0));
    let result = read_bounded(Counted { inner: std::io::repeat(7), read: read.clone() });
    assert!(matches!(result, Err(ImageError::Limits(_))), "{:?}", result.map(|b| b.len()));
    assert_eq!(read.load(Ordering::SeqCst), MAX_FILE_BYTES + 1);
    // Up to the limit is read whole.
    let read = Arc::new(AtomicU64::new(0));
    let bytes = read_bounded(Counted { inner: std::io::repeat(7).take(MAX_FILE_BYTES), read }).unwrap();
    assert_eq!(bytes.len() as u64, MAX_FILE_BYTES);
}

#[test]
fn a_picture_whose_header_is_too_large_or_empty_is_refused_from_the_header() {
    // Over the side limit, and under it on each side but over the pixel limit.
    for (w, h) in [(MAX_SIDE + 1, 10), (10, MAX_SIDE + 1), (9_000, 9_000), (40_000, 40_000)] {
        let result = decode(&png_claiming(w, h));
        assert!(matches!(result, Err(ImageError::Limits(_))), "{w}x{h}: {result:?}");
    }
    let result = decode(&bmp_claiming(20_000, 20_000));
    assert!(matches!(result, Err(ImageError::Limits(_))), "{result:?}");
    // No width or no height.
    for (w, h) in [(0, 10), (10, 0), (0, 0)] {
        assert!(decode(&png_claiming(w, h)).is_err(), "{w}x{h}");
        assert!(decode(&bmp_claiming(w as i32, h as i32)).is_err(), "bmp {w}x{h}");
    }
    // The largest picture allowed on a side still opens.
    let wide = RgbaImage::from_pixel(MAX_SIDE, 2, Rgba([1, 2, 3, 255]));
    assert_eq!(decode(&png(&wide)).map(|i| i.dimensions()).ok(), Some((MAX_SIDE, 2)));
}

#[test]
fn a_sixteen_bit_picture_may_have_half_the_pixels() {
    // 8000 x 5000 is 40 million pixels, under the pixel limit: at 8 bits a channel it takes 160 MB
    // as stored and passes the header check (this one then fails to decode, its pixels are not
    // there); at 16 bits it would take 320 MB and is refused from the header.
    let deep = decode(&png_claiming_depth(8_000, 5_000, 16));
    assert!(matches!(deep, Err(ImageError::Limits(_))), "{deep:?}");
    let shallow = decode(&png_claiming_depth(8_000, 5_000, 8));
    assert!(shallow.is_err() && !matches!(shallow, Err(ImageError::Limits(_))), "{shallow:?}");
    // Half the pixels at 16 bits is under the memory limit.
    let half = decode(&png_claiming_depth(4_000, 5_000, 16));
    assert!(!matches!(half, Err(ImageError::Limits(_))), "{half:?}");
}

#[test]
fn a_decoder_that_panics_gives_the_not_a_picture_error() {
    let result = guarded(|| panic!("a hostile file"));
    assert!(matches!(result, Err(ImageError::Unsupported(_))), "{result:?}");
    // A decode that ends normally is passed through.
    assert_eq!(guarded(|| Ok(RgbaImage::new(2, 1))).unwrap().dimensions(), (2, 1));
}

#[test]
fn a_picture_opens_by_its_content_whatever_its_name() {
    let tmp = tempfile::tempdir().unwrap();
    let img = RgbaImage::from_fn(5, 4, |x, y| Rgba([x as u8 * 40, y as u8 * 60, 7, 255]));
    let path = tmp.path().join("shot.PNG");
    std::fs::write(&path, png(&img)).unwrap();
    assert_eq!(load(&path).unwrap(), img);
    // A JPEG named .png is read as the JPEG it is.
    let jpeg_named_png = tmp.path().join("photo.png");
    image::DynamicImage::ImageRgba8(img.clone()).to_rgb8().save_with_format(&jpeg_named_png, ImageFormat::Jpeg).unwrap();
    assert_eq!(load(&jpeg_named_png).unwrap().dimensions(), (5, 4));
    // Text named .png is not a picture.
    let text = tmp.path().join("notes.png");
    std::fs::write(&text, "not a picture at all").unwrap();
    let e = load(&text).unwrap_err();
    assert!(matches!(e, ImageError::Unsupported(_)), "{e:?}");
}

#[test]
fn an_animated_gif_opens_its_first_frame() {
    use image::codecs::gif::GifEncoder;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("moving.gif");
    let red = RgbaImage::from_pixel(4, 3, Rgba([255, 0, 0, 255]));
    let blue = RgbaImage::from_pixel(4, 3, Rgba([0, 0, 255, 255]));
    {
        let mut enc = GifEncoder::new(File::create(&path).unwrap());
        enc.encode_frames([image::Frame::new(red.clone()), image::Frame::new(blue)]).unwrap();
    }
    assert_eq!(load(&path).unwrap(), red);
}

#[test]
fn a_folder_a_device_a_shortcut_and_a_missing_file_are_refused_and_never_followed() {
    let tmp = tempfile::tempdir().unwrap();
    // A folder with a picture's name.
    let folder = tmp.path().join("pictures.png");
    std::fs::create_dir(&folder).unwrap();
    assert!(matches!(load(&folder), Err(ImageError::Unsupported(_))), "{:?}", load(&folder));
    // A shortcut: its content is no picture.
    let mut lnk = vec![0x4c, 0, 0, 0, 0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
    lnk.extend([0u8; 200]);
    assert!(matches!(decode(&lnk), Err(ImageError::Unsupported(_))));
    let shortcut = tmp.path().join("shot.png.lnk");
    std::fs::write(&shortcut, &lnk).unwrap();
    assert!(load(&shortcut).is_err());
    // A file that is gone says why.
    let gone = tmp.path().join("gone.png");
    assert!(matches!(load(&gone), Err(ImageError::IoError(e)) if e.kind() == std::io::ErrorKind::NotFound));
    // A device is refused by its kind and never read: a read of one could wait for ever.
    #[cfg(windows)]
    {
        let nul = load(Path::new(r"\\.\NUL"));
        assert!(matches!(nul, Err(ImageError::Unsupported(_))), "{nul:?}");
    }
    // A link to a picture is opened as the link and refused, where this computer lets a test make one.
    #[cfg(windows)]
    {
        let target = tmp.path().join("real.png");
        std::fs::write(&target, png(&RgbaImage::new(2, 2))).unwrap();
        let link = tmp.path().join("link.png");
        if std::os::windows::fs::symlink_file(&target, &link).is_ok() {
            assert!(matches!(load(&link), Err(ImageError::Unsupported(_))), "{:?}", load(&link));
        }
    }
}

#[test]
fn one_picture_is_read_at_a_time_off_the_calling_thread() {
    let mut gate = LoadGate::default();
    assert!(gate.begin() && gate.is_loading());
    assert!(!gate.begin(), "a second request while one is read is ignored");
    gate.end();
    assert!(gate.begin());
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.png");
    std::fs::write(&path, png(&RgbaImage::new(3, 2))).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let caller = std::thread::current().id();
    spawn_load(path.clone(), move |p, result| tx.send((p, result.map(|i| i.dimensions()).ok(), std::thread::current().id())).unwrap()).join().unwrap();
    let (p, dims, on) = rx.recv().unwrap();
    assert_eq!((p, dims), (path, Some((3, 2))));
    assert_ne!(on, caller, "read on a thread of its own");
    // A file that cannot be opened still reports, so the gate is let go.
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_load(tmp.path().join("gone.png"), move |_, result| tx.send(result.is_err()).unwrap()).join().unwrap();
    assert!(rx.recv().unwrap());
}

#[test]
fn the_failure_is_said_with_the_sentence_the_open_path_always_used() {
    let tmp = tempfile::tempdir().unwrap();
    let text = tmp.path().join("notes.png");
    std::fs::write(&text, "x").unwrap();
    let e = load(&text).unwrap_err();
    assert_eq!(could_not_open(&text, &e), format!("Could not open {}: {e}", text.display()));
}

#[test]
fn only_a_picture_name_is_supported_in_any_letter_case() {
    for name in ["a.png", "a.PNG", "a.jpg", "a.JPeG", "a.bmp", "a.gif", "a.webp", "C:/x y/b.WebP"] {
        assert!(supported_name(Path::new(name)), "{name}");
    }
    for name in ["a.txt", "a.png.lnk", "a.lnk", "png", ".png", "a.tiff", "a.svg", "a.pngx", "a", "https://example.com/a.html"] {
        assert!(!supported_name(Path::new(name)), "{name}");
    }
}

#[test]
fn a_drag_is_taken_only_when_it_is_one_picture_file() {
    let tmp = tempfile::tempdir().unwrap();
    let pic = tmp.path().join("shot.png");
    std::fs::write(&pic, png(&RgbaImage::new(1, 1))).unwrap();
    let other = tmp.path().join("b.JPG");
    std::fs::write(&other, "x").unwrap();
    let notes = tmp.path().join("notes.txt");
    std::fs::write(&notes, "x").unwrap();
    let folder = tmp.path().join("folder.png");
    std::fs::create_dir(&folder).unwrap();
    let shortcut = tmp.path().join("shot.lnk");
    std::fs::write(&shortcut, "x").unwrap();
    assert_eq!(judge(&[Some(pic.clone())]), Verdict::Accept(pic.clone()));
    // The name decides here; the content is checked when the file is opened.
    assert_eq!(judge(&[Some(other.clone())]), Verdict::Accept(other.clone()));
    for paths in [
        vec![Some(notes)],
        vec![Some(folder)],
        vec![Some(shortcut)],
        vec![Some(pic.clone()), Some(other)],
        vec![None],
        vec![Some(tmp.path().join("gone.png"))],
        vec![],
    ] {
        assert_eq!(judge(&paths), Verdict::Refuse, "{paths:?}");
    }
}

/// The texts one frame drew.
fn texts(out: &egui::FullOutput) -> Vec<String> {
    crate::share_ui::tests::shapes_of(out).into_iter().filter_map(|s| if let egui::Shape::Text(t) = s { Some(t.galley.text().to_string()) } else { None }).collect()
}

fn drag_frame(ctx: &egui::Context, state: &mut DropState, hovered: Vec<Option<PathBuf>>, dropped: Vec<Option<PathBuf>>) -> (egui::FullOutput, Option<PathBuf>) {
    let mut got = None;
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| got = state.frame(ui.ctx(), ui.ctx().content_rect(), hovered.clone(), dropped.clone()));
    out.textures_delta.clear();
    (out, got)
}

#[test]
fn the_overlay_says_what_a_drop_would_do_and_a_drop_hands_over_only_a_picture() {
    let tmp = tempfile::tempdir().unwrap();
    let pic = tmp.path().join("shot.webp");
    std::fs::write(&pic, "x").unwrap();
    let notes = tmp.path().join("notes.txt");
    std::fs::write(&notes, "x").unwrap();
    let ctx = theme::test_context();
    let mut state = DropState::default();
    let (out, got) = drag_frame(&ctx, &mut state, vec![Some(pic.clone())], vec![]);
    assert_eq!((texts(&out), got), (vec![DROP_TO_OPEN.to_string()], None));
    let (out, _) = drag_frame(&ctx, &mut state, vec![Some(notes.clone())], vec![]);
    assert_eq!(texts(&out), [ONLY_PICTURES]);
    let (out, _) = drag_frame(&ctx, &mut state, vec![Some(pic.clone()), Some(pic.clone())], vec![]);
    assert_eq!(texts(&out), [ONLY_PICTURES]);
    // No drag, no overlay.
    let (out, got) = drag_frame(&ctx, &mut state, vec![], vec![]);
    assert!(texts(&out).is_empty() && got.is_none());
    // A drop: the picture is handed over and the overlay goes; anything else does nothing.
    let (out, got) = drag_frame(&ctx, &mut state, vec![], vec![Some(pic.clone())]);
    assert_eq!((texts(&out).len(), got), (0, Some(pic.clone())));
    for dropped in [vec![Some(notes.clone())], vec![Some(pic.clone()), Some(pic.clone())], vec![None]] {
        drag_frame(&ctx, &mut state, vec![], vec![]);
        let (out, got) = drag_frame(&ctx, &mut state, vec![], dropped);
        assert!(texts(&out).is_empty() && got.is_none());
    }
}

#[test]
fn a_drop_of_several_files_arriving_a_file_a_frame_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.png"), tmp.path().join("b.png"));
    std::fs::write(&a, "x").unwrap();
    std::fs::write(&b, "x").unwrap();
    let ctx = theme::test_context();
    let mut state = DropState::default();
    drag_frame(&ctx, &mut state, vec![Some(a.clone()), Some(b.clone())], vec![]);
    // The drop lands one file a frame: neither opens.
    let (_, got) = drag_frame(&ctx, &mut state, vec![], vec![Some(a.clone())]);
    assert_eq!(got, None);
    let (_, got) = drag_frame(&ctx, &mut state, vec![], vec![Some(b.clone())]);
    assert_eq!(got, None);
    // The next drag of one picture is taken again.
    drag_frame(&ctx, &mut state, vec![], vec![]);
    drag_frame(&ctx, &mut state, vec![Some(a.clone())], vec![]);
    let (_, got) = drag_frame(&ctx, &mut state, vec![], vec![Some(a.clone())]);
    assert_eq!(got, Some(a));
}

#[test]
fn the_verdict_is_kept_while_the_same_files_are_dragged() {
    let tmp = tempfile::tempdir().unwrap();
    let pic = tmp.path().join("shot.png");
    std::fs::write(&pic, "x").unwrap();
    let ctx = theme::test_context();
    let mut state = DropState::default();
    drag_frame(&ctx, &mut state, vec![Some(pic.clone())], vec![]);
    // The file goes away mid-drag: the disk is not looked at again for the same drag.
    std::fs::remove_file(&pic).unwrap();
    let (out, _) = drag_frame(&ctx, &mut state, vec![Some(pic.clone())], vec![]);
    assert_eq!(texts(&out), [DROP_TO_OPEN]);
    // A new drag is judged afresh, and a drop is always judged when it lands.
    drag_frame(&ctx, &mut state, vec![], vec![]);
    let (out, _) = drag_frame(&ctx, &mut state, vec![Some(pic.clone())], vec![]);
    assert_eq!(texts(&out), [ONLY_PICTURES]);
    let (_, got) = drag_frame(&ctx, &mut state, vec![], vec![Some(pic)]);
    assert_eq!(got, None);
}

/// An editor is a window of its own and does not read dragged or dropped files: no overlay is
/// drawn there and a drop on it asks nothing of the app.
#[test]
fn the_overlay_never_appears_in_an_editor_and_a_drop_there_does_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let pic = tmp.path().join("shot.png");
    std::fs::write(&pic, png(&RgbaImage::new(1, 1))).unwrap();
    let s = crate::settings::Settings::default();
    let mut ed = crate::editor_ui::Editor::new(RgbaImage::new(64, 48), "t".into(), Default::default(), s.dest_context("t"), s.editor.clone());
    let ctx = theme::test_context();
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 820.0));
    let hovered = vec![egui::HoveredFile { path: Some(pic.clone()), mime: String::new() }];
    for _ in 0..3 {
        let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(window), hovered_files: hovered.clone(), ..Default::default() }, |ui| ed.ui(ui));
        out.textures_delta.clear();
        let t = texts(&out);
        assert!(!t.iter().any(|t| t == DROP_TO_OPEN || t == ONLY_PICTURES), "{t:?}");
    }
    let dropped: Vec<egui::DroppedFileHandle> = vec![Arc::new(Dropped(pic))];
    let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(window), dropped_files: dropped, ..Default::default() }, |ui| ed.ui(ui));
    out.textures_delta.clear();
    assert!(ed.app_requests.is_empty(), "{:?}", ed.app_requests);
}

/// A file dropped in a test frame, as the window's backend hands one over.
#[derive(Debug)]
pub(crate) struct Dropped(pub(crate) PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        panic!("the app never reads a dropped file through egui")
    }
}
