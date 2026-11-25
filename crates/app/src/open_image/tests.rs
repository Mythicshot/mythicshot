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
fn a_folder_a_device_a_shortcut_and_a_missing_file_are_refused() {
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
    // A pipe-like device: refused, or not opened at all where the test has no console.
    #[cfg(windows)]
    {
        let con = load(Path::new(r"\\.\CONIN$"));
        assert!(matches!(con, Err(ImageError::Unsupported(_)) | Err(ImageError::IoError(_))), "{con:?}");
    }
}

/// A link to a picture is followed and the picture opens, where this computer lets a test make a
/// link without elevation (Developer Mode); elsewhere the test says so and checks the rest.
#[test]
fn a_link_to_a_picture_is_followed_and_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let img = RgbaImage::from_pixel(2, 2, Rgba([9, 8, 7, 255]));
    let target = tmp.path().join("real.png");
    std::fs::write(&target, png(&img)).unwrap();
    let link = tmp.path().join("link.png");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&target, &link).is_ok();
    #[cfg(not(windows))]
    let made = std::os::unix::fs::symlink(&target, &link).is_ok();
    if made {
        assert_eq!(load(&link).unwrap(), img);
        assert_eq!(judge(&[Some(link.clone())]), Verdict::Accept(link));
    } else {
        eprintln!("skipped: this computer does not let a test make a link");
    }
}

/// A file in a cloud-synced folder carries a reparse attribute even when it is on this computer.
/// A test cannot give a file one without elevation, so this opens a real one only when the temp
/// folder is itself inside a synced folder; otherwise it says it was skipped.
#[cfg(windows)]
#[test]
fn a_file_with_a_cloud_reparse_attribute_still_opens() {
    use std::os::windows::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("synced.png");
    std::fs::write(&path, png(&RgbaImage::new(3, 3))).unwrap();
    const REPARSE: u32 = 0x400;
    if std::fs::metadata(&path).unwrap().file_attributes() & REPARSE != 0 {
        assert_eq!(load(&path).unwrap().dimensions(), (3, 3));
    } else {
        eprintln!("skipped: no file with a reparse attribute can be made here without elevation");
        // The rest of the check still holds: a plain file opens.
        assert!(load(&path).is_ok());
    }
}

#[test]
fn a_picture_is_read_off_the_calling_thread_and_always_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.png");
    std::fs::write(&path, png(&RgbaImage::new(3, 2))).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let caller = std::thread::current().id();
    spawn_load(path.clone(), move |p, result| tx.send((p, result.map(|i| i.dimensions()).ok(), std::thread::current().id())).unwrap()).join().unwrap();
    let (p, dims, on) = rx.recv().unwrap();
    assert_eq!((p, dims), (path, Some((3, 2))));
    assert_ne!(on, caller, "read on a thread of its own");
    // A file that cannot be opened still reports, so the read is counted as ended.
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_load(tmp.path().join("gone.png"), move |_, result| tx.send(result.is_err()).unwrap()).join().unwrap();
    assert!(rx.recv().unwrap());
}

fn gone() -> ImageError {
    ImageError::IoError(std::io::Error::from(std::io::ErrorKind::NotFound))
}

#[test]
fn the_newest_request_wins_and_an_older_read_is_dropped_silently() {
    let (a, b) = (PathBuf::from("C:/p/a.png"), PathBuf::from("C:/p/b.png"));
    let mut loads = ImageLoads::default();
    assert!(!loads.pending(), "nothing asked for yet");
    let ga = loads.request(a.clone()).expect("starts at once").generation;
    let gb = loads.request(b.clone()).expect("a second read runs beside the first").generation;
    assert!(loads.pending());
    // The newer one ends first and opens.
    let (settled, next) = loads.finished(gb, &b, Ok(RgbaImage::new(4, 4)));
    assert!(matches!(settled, Settled::Open(ref img, ref title) if img.dimensions() == (4, 4) && title == "b.png"), "{settled:?}");
    assert!(next.is_none());
    assert!(!loads.pending(), "the newest is answered");
    // The older one ends later: dropped, picture and all, nothing said.
    let (settled, _) = loads.finished(ga, &a, Ok(RgbaImage::new(2, 2)));
    assert!(matches!(settled, Settled::Superseded));
    // A superseded read that fails says nothing either.
    let gc = loads.request(a.clone()).unwrap().generation;
    let gd = loads.request(b.clone()).unwrap().generation;
    assert!(matches!(loads.finished(gc, &a, Err::<RgbaImage, _>(gone())).0, Settled::Superseded));
    assert!(loads.pending(), "the newest is still being read");
    // The newest failing says the usual sentence.
    let (settled, _) = loads.finished(gd, &b, Err::<RgbaImage, _>(gone()));
    match settled {
        Settled::Failed(sentence) => assert_eq!(sentence, could_not_open(&b, &gone())),
        other => panic!("{other:?}"),
    }
    assert!(!loads.pending());
}

#[test]
fn at_most_two_reads_run_and_only_the_newest_waiting_request_is_kept() {
    let mut loads = ImageLoads::default();
    let p = |n: &str| PathBuf::from(format!("C:/p/{n}.png"));
    let g1 = loads.request(p("1")).unwrap().generation;
    let g2 = loads.request(p("2")).unwrap().generation;
    assert!(loads.request(p("3")).is_none(), "a third waits");
    assert!(loads.request(p("4")).is_none(), "and a fourth replaces it");
    // One ends: the newest waiting request starts, the replaced one never does.
    let (settled, next) = loads.finished(g1, &p("1"), Ok(RgbaImage::new(1, 1)));
    assert!(matches!(settled, Settled::Superseded));
    let job = next.expect("the waiting request starts");
    let (g4, path) = (job.generation, job.path.clone());
    assert_eq!(path, p("4"));
    assert!(loads.pending(), "an update waits for it");
    // The other old one ends: nothing waits, nothing starts.
    let (settled, next) = loads.finished(g2, &p("2"), Err::<RgbaImage, _>(gone()));
    assert!(matches!(settled, Settled::Superseded) && next.is_none());
    let (settled, _) = loads.finished(g4, &p("4"), Ok(RgbaImage::new(1, 1)));
    assert!(matches!(settled, Settled::Open(..)));
    assert!(!loads.pending(), "an update may close the app now");
}

/// Five windows asking at once share four slots: four reads start, the fifth window's request
/// waits (and an update waits for it), and a slot given back wakes the windows so the waiting
/// read starts on its next frame.
#[test]
fn a_request_beyond_the_apps_four_reads_waits_for_any_windows_read_to_end() {
    let pool = ReadPool::new(MAX_READS_ALL);
    let woken = Arc::new(AtomicU64::new(0));
    let mut windows: Vec<ImageLoads> = (0..5).map(|_| ImageLoads::with_pool(pool.clone())).collect();
    for w in &mut windows {
        let woken = woken.clone();
        w.set_wake(move || { woken.fetch_add(1, Ordering::SeqCst); });
    }
    let mut jobs: Vec<ReadJob> = Vec::new();
    for (i, w) in windows.iter_mut().enumerate() {
        if let Some(job) = w.request(PathBuf::from(format!("C:/p/{i}.png"))) {
            jobs.push(job);
        }
    }
    assert_eq!((jobs.len(), pool.in_flight()), (4, 4), "four start");
    assert!(windows[4].pending(), "the fifth waits, and an update waits for it");
    assert!(windows[4].retry().is_none(), "no slot yet");
    // Another window's read ends: its slot comes back and every window is woken.
    drop(jobs.remove(0));
    assert_eq!(pool.in_flight(), 3);
    assert_eq!(woken.load(Ordering::SeqCst), 5);
    let job = windows[4].retry().expect("the waiting read starts");
    assert_eq!((job.generation, job.path.clone(), pool.in_flight()), (1, PathBuf::from("C:/p/4.png"), 4));
    // Its own window's limit still holds: a window with two running waits even with a slot free.
    drop(jobs.remove(0));
    let mut busy = ImageLoads::with_pool(ReadPool::new(MAX_READS_ALL));
    let _a = busy.request(PathBuf::from("a")).unwrap();
    let _b = busy.request(PathBuf::from("b")).unwrap();
    assert!(busy.request(PathBuf::from("c")).is_none() && busy.retry().is_none());
}

/// The slot comes back however the read ends: a picture read, a file that is no picture, a
/// panic while preparing the picture, and a panic in what is done with the result.
#[test]
fn a_reads_slot_comes_back_on_every_way_out_panics_included() {
    let tmp = tempfile::tempdir().unwrap();
    let pic = tmp.path().join("a.png");
    std::fs::write(&pic, png(&RgbaImage::new(3, 2))).unwrap();
    let pool = ReadPool::new(MAX_READS_ALL);
    let mut loads = ImageLoads::with_pool(pool.clone());
    let job = |loads: &mut ImageLoads, path: &Path| {
        let job = loads.request(path.to_path_buf()).unwrap();
        loads.finished::<()>(job.generation, path, Ok(()));
        job
    };
    let (tx, rx) = std::sync::mpsc::channel();
    job(&mut loads, &pic).spawn(|img| img.dimensions(), move |_, _, r| tx.send(r.ok()).unwrap()).join().unwrap();
    assert_eq!((rx.recv().unwrap(), pool.in_flight()), (Some((3, 2)), 0));
    let (tx, rx) = std::sync::mpsc::channel();
    job(&mut loads, &tmp.path().join("gone.png")).spawn(|img| img, move |_, _, r| tx.send(r.is_err()).unwrap()).join().unwrap();
    assert!(rx.recv().unwrap() && pool.in_flight() == 0);
    let (tx, rx) = std::sync::mpsc::channel();
    job(&mut loads, &pic).spawn(|_| -> u8 { panic!("a hostile picture") }, move |_, _, r| tx.send(r.is_err()).unwrap()).join().unwrap();
    assert!(rx.recv().unwrap() && pool.in_flight() == 0, "a panic while preparing is a failure, and the slot is back");
    let ended = job(&mut loads, &pic).spawn(|img| img, |_, _, _| panic!("the window is gone")).join();
    assert!(ended.is_err());
    assert_eq!(pool.in_flight(), 0, "the slot was back before the result was handed over");
}

/// Five windows each asking at once, their reads on real threads: never more than four run,
/// and every window's newest request is answered in the end.
#[test]
fn five_windows_asking_at_once_never_run_more_than_four_reads_and_all_are_answered() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = ReadPool::new(MAX_READS_ALL);
    let mut windows: Vec<ImageLoads> = (0..5).map(|_| ImageLoads::with_pool(pool.clone())).collect();
    let (tx, rx) = std::sync::mpsc::channel::<(usize, u64, PathBuf, Result<(u32, u32), ImageError>)>();
    let most = Arc::new(AtomicU64::new(0));
    let start = |w: usize, job: ReadJob| {
        let tx = tx.clone();
        let (p, most) = (pool.clone(), most.clone());
        job.spawn(
            move |img| {
                most.fetch_max(p.in_flight() as u64, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(20));
                img.dimensions()
            },
            move |g, path, r| tx.send((w, g, path, r)).unwrap(),
        );
    };
    let mut answers: Vec<Option<(u32, u32)>> = vec![None; 5];
    for (w, window) in windows.iter_mut().enumerate() {
        // Each window asks twice; only its newest picture counts.
        for n in 0..2 {
            let path = tmp.path().join(format!("{w}-{n}.png"));
            std::fs::write(&path, png(&RgbaImage::new(w as u32 + 1, n + 1))).unwrap();
            if let Some(job) = window.request(path) {
                start(w, job);
            }
            assert!(pool.in_flight() <= MAX_READS_ALL);
        }
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while windows.iter().any(ImageLoads::pending) && std::time::Instant::now() < until {
        assert!(pool.in_flight() <= MAX_READS_ALL, "{} reads at once", pool.in_flight());
        if let Ok((w, g, path, r)) = rx.recv_timeout(std::time::Duration::from_millis(10)) {
            let (settled, next) = windows[w].finished(g, &path, r);
            if let Some(job) = next {
                start(w, job);
            }
            if let Settled::Open(dims, _) = settled {
                answers[w] = Some(dims);
            }
        }
        // Each window's frame: a read waiting for a slot starts once one is free.
        for (w, window) in windows.iter_mut().enumerate() {
            if let Some(job) = window.retry() {
                start(w, job);
            }
        }
    }
    assert!(windows.iter().all(|w| !w.pending()), "every newest request was answered");
    let most = most.load(Ordering::SeqCst);
    assert!(most >= 1 && most <= MAX_READS_ALL as u64, "at most four ran at once: {most}");
    assert_eq!(answers, (0..5).map(|w| Some((w as u32 + 1, 2))).collect::<Vec<_>>(), "with its newest picture");
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

/// An editor's overlay says its own words over a picture, the same refusal over anything else,
/// and while nothing may be placed it refuses a picture with its own reason (anything else with
/// the usual sentence), and a drop hands nothing over.
#[test]
fn a_window_with_its_own_words_takes_a_picture_only_while_it_is_allowed() {
    let tmp = tempfile::tempdir().unwrap();
    let pic = tmp.path().join("shot.png");
    std::fs::write(&pic, "x").unwrap();
    let notes = tmp.path().join("notes.txt");
    std::fs::write(&notes, "x").unwrap();
    let ctx = theme::test_context();
    let mut state = DropState::default();
    let frame = |state: &mut DropState, hovered: Vec<Option<PathBuf>>, dropped: Vec<Option<PathBuf>>, allowed: bool| {
        let mut got = None;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| got = state.frame_for(ui.ctx(), ui.ctx().content_rect(), hovered.clone(), dropped.clone(), Words { accept: "Place it", blocked: "Not now" }, allowed));
        out.textures_delta.clear();
        (texts(&out), got)
    };
    assert_eq!(frame(&mut state, vec![Some(pic.clone())], vec![], true), (vec!["Place it".to_string()], None));
    assert_eq!(frame(&mut state, vec![Some(notes.clone())], vec![], true).0, [ONLY_PICTURES]);
    assert_eq!(frame(&mut state, vec![], vec![Some(pic.clone())], true), (vec![], Some(pic.clone())));
    // Not allowed now: the picture is refused with that reason, anything else as always, and a
    // drop does nothing.
    frame(&mut state, vec![], vec![], false);
    assert_eq!(frame(&mut state, vec![Some(pic.clone())], vec![], false).0, ["Not now"]);
    assert_eq!(frame(&mut state, vec![Some(notes.clone())], vec![], false).0, [ONLY_PICTURES]);
    assert_eq!(frame(&mut state, vec![Some(pic.clone()), Some(pic.clone())], vec![], false).0, [ONLY_PICTURES]);
    // Allowed again in the same drag: the picture is taken again.
    assert_eq!(frame(&mut state, vec![Some(pic.clone())], vec![], false).0, ["Not now"]);
    assert_eq!(frame(&mut state, vec![Some(pic.clone())], vec![], true).0, ["Place it"]);
    frame(&mut state, vec![], vec![], false);
    assert_eq!(frame(&mut state, vec![], vec![Some(pic.clone())], false), (vec![], None));
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

/// The words stand on a solid chip, so their contrast is measured against it whatever the
/// see-through tint lets show of the page: at least 4.5:1, and the tints are see-through.
#[test]
fn the_overlay_words_keep_their_contrast_on_a_solid_chip() {
    for accept in [true, false] {
        let (tint, _, chip, ink) = overlay_colours(accept);
        assert_eq!(chip.a(), 255, "the chip is solid");
        assert!(theme::contrast(ink, chip) >= 4.5, "{accept}: {}", theme::contrast(ink, chip));
        assert!(tint.a() > 0 && tint.a() < 255, "the page shows faintly through");
    }
}

/// An editor is a window of its own: the main window's overlay, which opens a new editor, never
/// shows there (the editor shows its own, which places the picture on its capture), and a drop on
/// it asks nothing of the app.
#[test]
fn the_main_windows_overlay_never_appears_in_an_editor_and_a_drop_there_asks_nothing_of_the_app() {
    let tmp = tempfile::tempdir().unwrap();
    let pic = tmp.path().join("shot.png");
    std::fs::write(&pic, png(&RgbaImage::new(1, 1))).unwrap();
    let s = crate::settings::Settings::default();
    let mut ed = crate::editor_ui::Editor::new(RgbaImage::new(64, 48), "t".into(), Default::default(), s.dest_context("t"), s.editor.clone());
    let ctx = theme::test_context();
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 820.0));
    let hovered = vec![egui::HoveredFile { path: Some(pic.clone()), mime: String::new() }];
    let mut seen = Vec::new();
    for _ in 0..3 {
        let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(window), hovered_files: hovered.clone(), ..Default::default() }, |ui| ed.ui(ui));
        out.textures_delta.clear();
        let t = texts(&out);
        assert!(!t.iter().any(|t| t == DROP_TO_OPEN), "{t:?}");
        seen.extend(t);
    }
    assert!(seen.iter().any(|t| t == crate::place_picture::PLACE_HERE), "the editor's own overlay: {seen:?}");
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

/// The production read of one real picture named by `MYTHICSHOT_TEST_PICTURE`, for checking by
/// hand a file this machine has (one in a cloud-synced folder, say). It prints the file's
/// attributes, reads only that file and writes nothing. Run with
/// `MYTHICSHOT_TEST_PICTURE=<path> cargo test -p mythicshot a_picture_named_by_the_environment_opens -- --ignored --nocapture`.
#[test]
#[ignore = "reads the picture named by MYTHICSHOT_TEST_PICTURE"]
fn a_picture_named_by_the_environment_opens() {
    let Some(path) = std::env::var_os("MYTHICSHOT_TEST_PICTURE").map(PathBuf::from) else {
        eprintln!("skipped: MYTHICSHOT_TEST_PICTURE is not set");
        return;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Windows hides a cloud placeholder's reparse attribute from a process that has not opted in, so this may not show it.
        let attributes = std::fs::symlink_metadata(&path).unwrap().file_attributes();
        eprintln!("{}: attributes as this process sees them (cloud placeholders may be disguised): 0x{attributes:08x}", path.display());
    }
    let img = load(&path).unwrap_or_else(|e| panic!("{}", could_not_open(&path, &e)));
    eprintln!("opened: {} x {}", img.width(), img.height());
    assert!(img.width() > 0 && img.height() > 0);
}
