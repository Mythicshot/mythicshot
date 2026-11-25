//! Opening and dropping picture files: the dialog asked for, the limits a file is read within,
//! and the verdict on what is dragged. Every file here is made in the OS temp folder.

use std::io::{Cursor, Read, Seek, SeekFrom};
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

/// A reader that counts the bytes taken from it, to show a refusal read only a header.
struct Counted {
    inner: Cursor<Vec<u8>>,
    read: Arc<AtomicU64>,
}

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::SeqCst);
        Ok(n)
    }
}

impl Seek for Counted {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Decode `bytes` through a counting reader: the result and how many bytes were read.
fn decode_counted(bytes: Vec<u8>, len: u64) -> (Result<RgbaImage, ImageError>, u64) {
    let read = Arc::new(AtomicU64::new(0));
    let result = decode(BufReader::new(Counted { inner: Cursor::new(bytes), read: read.clone() }), len);
    (result, read.load(Ordering::SeqCst))
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

/// A real PNG whose header says it is `w` by `h`, followed by `junk` bytes of nothing: what a
/// decompression bomb looks like from the outside.
fn png_claiming(w: u32, h: u32, junk: usize) -> Vec<u8> {
    let mut bytes = png(&RgbaImage::new(1, 1));
    bytes[16..20].copy_from_slice(&w.to_be_bytes());
    bytes[20..24].copy_from_slice(&h.to_be_bytes());
    let crc = crc32(&bytes[12..29]);
    bytes[29..33].copy_from_slice(&crc.to_be_bytes());
    bytes.extend(std::iter::repeat_n(0u8, junk));
    bytes
}

/// A BMP header saying `w` by `h` at 24 bits, followed by `junk` bytes.
fn bmp_claiming(w: i32, h: i32, junk: usize) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(b"BM");
    b.extend(((54 + junk) as u32).to_le_bytes());
    b.extend([0u8; 4]);
    b.extend(54u32.to_le_bytes());
    b.extend(40u32.to_le_bytes());
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend([0u8; 24]);
    b.extend(std::iter::repeat_n(0u8, junk));
    b
}

const JUNK: usize = 4 * 1024 * 1024;
/// Far less than the file: the header and a buffer's worth around it.
const HEADER_READ: u64 = 64 * 1024;

#[test]
fn the_limits_are_the_ones_stated() {
    assert_eq!(MAX_FILE_BYTES, 67_108_864);
    assert_eq!(MAX_SIDE, 16_384);
    assert_eq!(MAX_PIXELS, 67_108_864);
}

#[test]
fn a_file_over_the_size_limit_is_refused_before_a_byte_is_read() {
    let (result, read) = decode_counted(png(&RgbaImage::new(2, 2)), MAX_FILE_BYTES + 1);
    assert!(matches!(result, Err(ImageError::Limits(_))), "{result:?}");
    assert_eq!(read, 0);
    // A file exactly at the limit is read.
    let (result, _) = decode_counted(png(&RgbaImage::new(2, 2)), MAX_FILE_BYTES);
    assert!(result.is_ok());
    // From disk: a file one byte over (sparse, nothing written) is refused by its size.
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
fn a_picture_whose_header_is_too_large_is_refused_after_reading_only_the_header() {
    // Over the side limit, and under it on each side but over the pixel limit.
    for (w, h) in [(MAX_SIDE + 1, 10), (10, MAX_SIDE + 1), (9_000, 9_000), (40_000, 40_000)] {
        let (result, read) = decode_counted(png_claiming(w, h, JUNK), (JUNK + 100) as u64);
        assert!(matches!(result, Err(ImageError::Limits(_))), "{w}x{h}: {result:?}");
        assert!(read < HEADER_READ, "{w}x{h}: read {read} bytes");
    }
    let (result, read) = decode_counted(bmp_claiming(20_000, 20_000, JUNK), (JUNK + 54) as u64);
    assert!(matches!(result, Err(ImageError::Limits(_))), "{result:?}");
    assert!(read < HEADER_READ, "read {read} bytes");
    // The largest picture allowed on a side still opens.
    let wide = RgbaImage::from_pixel(MAX_SIDE, 2, Rgba([1, 2, 3, 255]));
    let (result, _) = decode_counted(png(&wide), 1000);
    assert_eq!(result.map(|i| i.dimensions()).ok(), Some((MAX_SIDE, 2)));
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
fn a_folder_a_shortcut_and_a_missing_file_are_refused_and_never_followed() {
    let tmp = tempfile::tempdir().unwrap();
    // A folder with a picture's name.
    let folder = tmp.path().join("pictures.png");
    std::fs::create_dir(&folder).unwrap();
    assert!(matches!(load(&folder), Err(ImageError::Unsupported(_))));
    // A shortcut: its content is no picture, and only its header is looked at.
    let mut lnk = vec![0x4c, 0, 0, 0, 0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
    lnk.extend(std::iter::repeat_n(0u8, JUNK));
    let (result, read) = decode_counted(lnk.clone(), lnk.len() as u64);
    assert!(matches!(result, Err(ImageError::Unsupported(_))), "{result:?}");
    assert!(read < HEADER_READ, "read {read} bytes");
    let shortcut = tmp.path().join("shot.png.lnk");
    std::fs::write(&shortcut, &lnk).unwrap();
    assert!(load(&shortcut).is_err());
    // A file that is gone.
    let gone = tmp.path().join("gone.png");
    assert!(matches!(load(&gone), Err(ImageError::IoError(e)) if e.kind() == std::io::ErrorKind::NotFound));
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
        let (out, got) = drag_frame(&ctx, &mut state, vec![], dropped);
        assert!(texts(&out).is_empty() && got.is_none());
    }
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
