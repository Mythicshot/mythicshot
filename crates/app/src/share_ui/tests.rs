//! The share window's wording and steps, without a window and without the real clipboard.

use super::*;
use crate::publish::tests::{Stub, TOKEN, reply, stub};
use std::cell::RefCell;

const SERVER: &str = "https://tours.example.com";
/// 1.4 MB as the app writes sizes.
const BYTES: u64 = 1_468_006;

fn image_form() -> ShareDialog {
    ShareDialog::new(Kind::Image, SERVER.into(), "Loot window, settings tab", image_details(1920, 1080, MediaType::Png, BYTES), false, None, None)
}

fn clip_form(media: MediaType) -> ShareDialog {
    let gif = media == MediaType::Gif;
    ShareDialog::new(Kind::Clip, SERVER.into(), "Sending a note to the raid", clip_details(12_000, 960, 540, media, None), gif, None, None)
}

/// What a copy function was given.
type Log = RefCell<Vec<String>>;

/// A copy function that records what it was given and answers `works`.
fn copier(works: bool) -> (Log, impl Fn(&Log, &str) -> bool) {
    (RefCell::new(Vec::new()), move |log: &Log, text: &str| {
        log.borrow_mut().push(text.to_string());
        works
    })
}

/// A job that ends with `result` once something is sent on the channel, or with a cancel.
fn held_job(kind: Kind, result: Result<Shared, ShareError>) -> (ShareJob, std::sync::mpsc::Sender<()>) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let job = ShareJob::spawn(kind, move |r| {
        while rx.recv_timeout(Duration::from_millis(20)).is_err() {
            if r.cancelled() {
                return Err(ShareError::Cancelled);
            }
        }
        result
    });
    (job, tx)
}

/// Poll until the job has ended; what the last poll asked of the editor.
fn settle(d: &mut ShareDialog, copy: &mut dyn FnMut(&str) -> bool) -> Option<ShareAction> {
    for _ in 0..500 {
        if let Some(action) = d.poll(copy) {
            return Some(action);
        }
        d.job.as_ref()?;
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the job did not end");
}

#[test]
fn the_image_window_says_what_the_mockup_says() {
    let d = image_form();
    d.set_workspace("Cowfee Raid Tools");
    let v = d.view();
    assert_eq!(v.caption, "Publish image");
    assert_eq!(v.heading, "Share this image with a link");
    assert_eq!(
        v.lines,
        vec![
            Line::Muted("To Cowfee Raid Tools at tours.example.com".into()),
            Line::Title,
            Line::Muted("1920 × 1080 · PNG · 1.4 MB".into()),
            Line::Warning("Anyone who has the link can open the image. It stays online until you delete it in the dashboard.".into()),
        ]
    );
    assert_eq!(v.buttons, vec![Button::Cancel, Button::Publish]);
    assert_eq!(v.texts().iter().filter(|t| *t == "Title (optional)").count(), 1);
    assert_eq!(d.title, "Loot window, settings tab");
    assert_eq!((v.progress, v.link), (None, None));
}

#[test]
fn the_destination_line_names_the_host_until_the_workspace_is_known() {
    assert_eq!(to_line(None, "https://Tours.Example.com/demo/").as_deref(), Some("To tours.example.com"));
    assert_eq!(to_line(Some("  "), SERVER).as_deref(), Some("To tours.example.com"));
    assert_eq!(to_line(Some("Acme\n"), "http://127.0.0.1:8787").as_deref(), Some("To Acme at 127.0.0.1"));
    assert_eq!(to_line(Some(&"W".repeat(60)), SERVER).unwrap(), format!("To {}… at tours.example.com", "W".repeat(39)));
    assert_eq!(to_line(Some("Acme"), "not an address"), None);
    // Before the site answers, the window says only where it goes.
    assert_eq!(image_form().view().lines[0], Line::Muted("To tours.example.com".into()));
}

#[test]
fn the_recording_window_says_what_the_mockup_says_with_and_without_ffmpeg() {
    let v = clip_form(MediaType::Mp4).view();
    assert_eq!((v.caption, v.heading.as_str()), ("Publish recording", "Share this recording with a link"));
    assert_eq!(
        v.lines[1..],
        [
            Line::Title,
            Line::Muted("12 s · 960 × 540 · MP4".into()),
            Line::Warning("Anyone who has the link can open the clip. It stays online until you delete it in the dashboard.".into()),
        ]
    );
    let v = clip_form(MediaType::Gif).view();
    assert_eq!(
        v.lines[2..],
        [
            Line::Muted("12 s · 960 × 540 · GIF".into()),
            Line::Muted("Mythicshot did not find the video tool (ffmpeg) on this computer, so the clip is sent as a GIF. With it, the same clip is far smaller.".into()),
            Line::Warning("Anyone who has the link can open the clip. It stays online until you delete it in the dashboard.".into()),
        ]
    );
    assert_eq!(v.buttons, vec![Button::Cancel, Button::Publish]);
    // An estimate is shown only when there is one; there is never a made-up one.
    assert_eq!(clip_details(12_000, 960, 540, MediaType::Mp4, Some(18 * 1_048_576)), "12 s · 960 × 540 · MP4 · about 18.0 MB");
    assert_eq!(clip_details(6_400, 640, 360, MediaType::Gif, None), "6 s · 640 × 360 · GIF");
    assert_eq!(clip_details(120, 640, 360, MediaType::Gif, None), "1 s · 640 × 360 · GIF");
}

#[test]
fn the_title_is_optional_and_cut_to_the_sites_limit() {
    let d = ShareDialog::new(Kind::Image, SERVER.into(), &"x".repeat(200), String::new(), false, None, None);
    assert_eq!(d.title.chars().count(), 120);
    let mut d = image_form();
    d.title.clear();
    let (log, copy) = copier(true);
    assert_eq!(d.press(Button::Publish, &mut |t| copy(&log, t)), Some(ShareAction::Publish), "an empty title can be published");
}

#[test]
fn no_workspace_shows_the_same_step_as_publishing_a_tour_and_follows_the_connection() {
    let why = publish_ui::ADD_SERVER_AND_TOKEN.to_string();
    let mut d = ShareDialog::new(Kind::Image, String::new(), "x", String::new(), false, Some(why.clone()), None);
    let v = d.view();
    assert_eq!((v.heading.as_str(), v.lines.clone(), v.buttons.clone()), ("Share this image with a link", vec![Line::Text(why.clone())], vec![Button::Close]));
    let (log, copy) = copier(true);
    assert_eq!(d.press(Button::Publish, &mut |t| copy(&log, t)), None, "nothing can be published from it");
    assert_eq!(d.press(Button::Close, &mut |t| copy(&log, t)), Some(ShareAction::Closed(None)));
    // Connected meanwhile: the form; disconnected again: the step.
    d.update_blocked(None);
    assert_eq!(d.stage(), &Stage::Form);
    d.update_blocked(Some(why.clone()));
    assert_eq!(d.stage(), &Stage::Blocked(why.clone()));
    // Once something is sent, the connection no longer changes the window.
    let mut d = image_form();
    let (job, _tx) = held_job(Kind::Image, Err(ShareError::Cancelled));
    d.start(job);
    d.update_blocked(Some(why));
    assert!(matches!(d.stage(), Stage::Uploading { .. }));
    // The reason is the tour's own, for every way the connection can be missing.
    assert_eq!(blocked_reason(""), Some(publish_ui::ADD_SERVER_AND_TOKEN.to_string()));
    assert_eq!(blocked_reason("http://tours.example.com"), Some("The server address must start with https:// (http:// is only for localhost).".to_string()));
}

#[test]
fn uploading_shows_the_bytes_sent_of_the_total_and_cancel() {
    let mut d = image_form();
    d.stage = Stage::Uploading { sent: 943_719, total: BYTES };
    let v = d.view();
    assert_eq!(v.heading, "Uploading…");
    assert_eq!(v.lines, vec![Line::Muted("0.9 of 1.4 MB".into())]);
    assert_eq!(v.buttons, vec![Button::Cancel]);
    assert!((v.progress.unwrap() - 0.6429).abs() < 0.001);
    d.stage = Stage::Preparing(0.25);
    let v = d.view();
    assert_eq!((v.heading.as_str(), v.progress, v.buttons.clone()), ("Preparing the clip…", Some(0.25), vec![Button::Cancel]));
}

#[test]
fn the_bytes_sent_are_written_in_the_totals_unit_and_never_read_as_done_too_soon() {
    assert_eq!(sent_text(943_719, BYTES), "0.9 of 1.4 MB");
    assert_eq!(sent_text(0, BYTES), "0.0 of 1.4 MB");
    assert_eq!(sent_text(BYTES - 1, BYTES), "1.3 of 1.4 MB");
    assert_eq!(sent_text(BYTES, BYTES), "1.4 of 1.4 MB");
    assert_eq!(sent_text(300 * 1024, 840 * 1024), "300 of 840 KB");
    assert_eq!(sent_text(40 * 1_048_576, 150 * 1_048_576), "40 of 150 MB");
    assert_eq!(sent_text(1_073_741_824, 2 * 1_073_741_824), "1.0 of 2.0 GB");
}

#[test]
fn cancel_while_uploading_waits_for_the_clean_up_then_closes_with_a_sentence() {
    let mut d = image_form();
    let (job, _tx) = held_job(Kind::Image, Ok(Shared { id: "x".into(), link: None }));
    let waiter = job.waiter();
    d.start(job);
    let (log, copy) = copier(true);
    assert_eq!(d.press(Button::Cancel, &mut |t| copy(&log, t)), None);
    assert_eq!(d.stage(), &Stage::Cancelling);
    let v = d.view();
    assert_eq!((v.heading.as_str(), v.buttons.is_empty()), ("Cancelling…", true));
    assert_eq!(settle(&mut d, &mut |t| copy(&log, t)), Some(ShareAction::Closed(Some("Publishing was cancelled. Nothing was shared.".into()))));
    assert!(waiter.is_finished());
    assert!(log.borrow().is_empty());
}

#[test]
fn the_result_puts_the_link_on_the_clipboard_and_says_so_only_when_that_worked() {
    let link = "https://img.example.com/i/abcdefghijklmnopqrst";
    for works in [true, false] {
        let mut d = image_form();
        let (job, tx) = held_job(Kind::Image, Ok(Shared { id: "x".into(), link: Some(link.into()) }));
        d.start(job);
        tx.send(()).unwrap();
        let (log, copy) = copier(works);
        assert_eq!(settle(&mut d, &mut |t| copy(&log, t)), Some(ShareAction::Status("The image is online".into())));
        assert_eq!(log.borrow().as_slice(), [link.to_string()]);
        let v = d.view();
        assert_eq!(v.heading, "The image is online");
        assert_eq!(v.link.as_deref(), Some(link));
        assert_eq!(v.lines, if works { vec![Line::Muted("The link was copied. Paste it to whoever should see it.".into())] } else { vec![] });
        assert_eq!(v.buttons, vec![Button::CopyLink, Button::Open, Button::Done]);
        assert_eq!(v.buttons.iter().map(|b| v.label(*b)).collect::<Vec<_>>(), [if works { "Copied" } else { "Copy link" }, "Open in browser", "Done"]);
        assert_eq!(d.press(Button::Open, &mut |t| copy(&log, t)), Some(ShareAction::Open(link.into())));
        assert_eq!(d.press(Button::CopyLink, &mut |t| copy(&log, t)), None);
        assert_eq!(log.borrow().len(), 2, "Copy link copies again");
        // The button says it, not a line of its own.
        assert_eq!(d.view().copied, works);
        assert!(!d.view().texts().iter().any(|t| t == "Link copied"));
        assert_eq!(d.press(Button::Done, &mut |t| copy(&log, t)), Some(ShareAction::Closed(None)));
    }
    let mut d = clip_form(MediaType::Mp4);
    d.stage = Stage::Online { link: Some(link.into()), copied: true };
    assert_eq!(d.view().heading, "The clip is online");
}

#[test]
fn a_link_mythicshot_cannot_open_says_where_to_find_the_item() {
    let mut d = clip_form(MediaType::Mp4);
    let (job, tx) = held_job(Kind::Clip, Ok(Shared { id: "x".into(), link: None }));
    d.start(job);
    tx.send(()).unwrap();
    let (log, copy) = copier(true);
    settle(&mut d, &mut |t| copy(&log, t));
    assert!(log.borrow().is_empty(), "nothing to copy");
    let v = d.view();
    assert_eq!(v.heading, "The clip is online");
    assert_eq!(v.lines, vec![Line::Text("The site did not give a link Mythicshot can open. Find the clip in the dashboard.".into())]);
    assert_eq!((v.link, v.buttons), (None, vec![Button::Done]));
}

#[test]
fn refusals_are_shown_with_their_heading_sentence_and_note_and_close() {
    let mut d = image_form();
    d.refuse(&ShareError::TooLarge(share::TooLarge::Bytes { bytes: 14 * 1_048_576, max: 10 * 1_048_576, exact: true }));
    let v = d.view();
    assert_eq!(v.caption, "Publish image");
    assert_eq!(v.heading, "This image is too large to share");
    assert_eq!(v.lines, vec![Line::Text("It is 14.0 MB, and a shared image can be at most 10.0 MB. Crop it, or make it smaller, and publish again.".into())]);
    assert_eq!(v.buttons, vec![Button::Close]);
    let (log, copy) = copier(true);
    assert_eq!(d.press(Button::Close, &mut |t| copy(&log, t)), Some(ShareAction::Closed(None)));

    let mut d = clip_form(MediaType::Mp4);
    d.refuse(&ShareError::TooLarge(share::TooLarge::Bytes { bytes: 74 * 1_048_576, max: 50 * 1_048_576, exact: false }));
    let v = d.view();
    assert_eq!((v.caption, v.heading.as_str()), ("Publish recording", "This clip is too large to share"));
    assert_eq!(v.lines, vec![Line::Text("It would be 74.0 MB, and a shared clip can be at most 50.0 MB. Trim it, or lower the size or quality in Export, and publish again.".into())]);

    let mut d = image_form();
    let (job, tx) = held_job(Kind::Image, Err(ShareError::Full { needed: 1_400_000, left: 650_000, allowance: 5 * 1_073_741_824 }));
    d.start(job);
    tx.send(()).unwrap();
    assert_eq!(settle(&mut d, &mut |t| copy(&log, t)), Some(ShareAction::Status("The image was not uploaded".into())));
    let v = d.view();
    assert_eq!(v.heading, "The image was not uploaded");
    assert_eq!(
        v.lines,
        vec![
            Line::Text("This image needs 1.4 MB, and your workspace has 634 KB left of its 5.0 GB. Delete things you no longer need, or ask for more room, and publish again.".into()),
            Line::Note("Everything that is already online is not affected.".into()),
        ]
    );
    assert_eq!(v.buttons, vec![Button::Close]);

    let mut d = clip_form(MediaType::Mp4);
    d.refuse(&ShareError::Full { needed: 1_400_000, left: 650_000, allowance: 5 * 1_073_741_824 });
    assert_eq!(d.view().heading, "The clip was not uploaded");
    assert!(matches!(&d.view().lines[0], Line::Text(t) if t.starts_with("This clip needs 1.4 MB")));

    let mut d = image_form();
    d.refuse(&ShareError::Failed("Could not send the image: the connection failed.".into()));
    assert_eq!(d.view().lines, vec![Line::Text("Could not send the image: the connection failed.".into())]);
    d.refuse(&ShareError::Cancelled);
    assert!(matches!(d.stage(), Stage::Refused(_)), "a cancel changes nothing here");
}

#[test]
fn publish_runs_the_starter_once_with_the_cleaned_title_and_the_server() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let starter: Starter = Box::new(move |server, title| {
        seen2.lock().unwrap().push((server, title));
        ShareJob::spawn(Kind::Image, |_| Ok(Shared { id: "x".into(), link: None }))
    });
    let mut d = ShareDialog::new(Kind::Image, SERVER.into(), "  Loot\twindow  ", String::new(), false, None, Some(starter));
    d.begin();
    d.begin();
    assert_eq!(seen.lock().unwrap().as_slice(), [(SERVER.to_string(), "Lootwindow".to_string())]);
    assert!(matches!(d.stage(), Stage::Uploading { .. }));
}

#[test]
fn closing_the_window_cancels_its_job() {
    let mut d = image_form();
    let (job, _tx) = held_job(Kind::Image, Ok(Shared { id: "x".into(), link: None }));
    let waiter = job.waiter();
    d.start(job);
    drop(d);
    for _ in 0..200 {
        if waiter.is_finished() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the job ran on after its window closed");
}

#[test]
fn a_job_that_panics_is_a_failure_with_a_sentence() {
    let mut d = image_form();
    d.start(ShareJob::spawn(Kind::Image, |_| panic!("broken")));
    let (log, copy) = copier(true);
    settle(&mut d, &mut |t| copy(&log, t));
    assert_eq!(d.view().lines, vec![Line::Text("Publishing failed because of an internal error.".into())]);
}

/// A stand-in site for a whole share through the window's job.
fn media_site() -> Stub {
    stub(|req, _| {
        let l = req.line.as_str();
        if l == "POST /api/v1/media" {
            reply(201, r#"{"id":"med_0123456789abcdef01234567","parts":1,"part_bytes":2000000}"#)
        } else if l.ends_with("/commit") {
            reply(200, r#"{"id":"med_0123456789abcdef01234567","link_id":"abcdefghijklmnopqrst","kind":"image","path":"/i/abcdefghijklmnopqrst","link":"https://img.example.com/i/abcdefghijklmnopqrst"}"#)
        } else {
            reply(204, "")
        }
    })
}

#[test]
fn a_whole_share_through_the_window_ends_with_the_link_copied() {
    let s = media_site();
    let img = image::RgbaImage::from_pixel(64, 48, image::Rgba([200, 30, 30, 255]));
    let png = share::png_bytes(&img).unwrap();
    let base = s.base.clone();
    let starter: Starter = Box::new(move |server, title| {
        ShareJob::spawn(Kind::Image, move |r| {
            let item = Item { media: MediaType::Png, data: png, width: 64, height: 48, duration_ms: None, title, preview: share::preview_of(&img) };
            share::share(&server, TOKEN, &item, |sent, total| r.uploading(sent, total))
        })
    });
    let mut d = ShareDialog::new(Kind::Image, base, "Red", String::new(), false, None, Some(starter));
    d.begin();
    let (log, copy) = copier(true);
    assert_eq!(settle(&mut d, &mut |t| copy(&log, t)), Some(ShareAction::Status("The image is online".into())));
    assert_eq!(log.borrow().as_slice(), ["https://img.example.com/i/abcdefghijklmnopqrst".to_string()]);
    let lines = s.lines();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&s.requests()[0].body).unwrap()["title"], "Red");
}

#[test]
fn every_stage_draws_and_escape_and_enter_stand_for_its_buttons() {
    let ctx = egui::Context::default();
    let key = |k: egui::Key| egui::RawInput { events: vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], ..Default::default() };
    let refused = Stage::Refused(ShareError::Full { needed: 1, left: 0, allowance: 1 }.refusal(Kind::Image).unwrap());
    let online = Stage::Online { link: Some("https://img.example.com/i/a".into()), copied: true };
    let cases: [(Stage, Option<ShareAction>, Option<ShareAction>); 6] = [
        (Stage::Form, Some(ShareAction::Closed(None)), Some(ShareAction::Publish)),
        (Stage::Blocked("Add a site address.".into()), Some(ShareAction::Closed(None)), Some(ShareAction::Closed(None))),
        (refused, Some(ShareAction::Closed(None)), Some(ShareAction::Closed(None))),
        (online, Some(ShareAction::Closed(None)), Some(ShareAction::Closed(None))),
        (Stage::Online { link: None, copied: false }, Some(ShareAction::Closed(None)), Some(ShareAction::Closed(None))),
        (Stage::Cancelling, None, None),
    ];
    let (log, copy) = copier(false);
    for (stage, escape, enter) in cases {
        let mut d = image_form();
        d.stage = stage.clone();
        let mut out = None;
        ctx.run_ui(egui::RawInput::default(), |ui| out = d.show(ui.ctx(), &mut |t| copy(&log, t))).textures_delta.clear();
        assert_eq!(out, None, "{stage:?} stays until the user chooses");
        ctx.run_ui(key(egui::Key::Escape), |ui| out = d.show(ui.ctx(), &mut |t| copy(&log, t))).textures_delta.clear();
        assert_eq!(out, escape, "{stage:?}");
        d.stage = stage.clone();
        ctx.run_ui(key(egui::Key::Enter), |ui| out = d.show(ui.ctx(), &mut |t| copy(&log, t))).textures_delta.clear();
        assert_eq!(out, enter, "{stage:?}");
    }
    // While uploading, Escape cancels and Enter does nothing.
    let mut d = image_form();
    let (job, _tx) = held_job(Kind::Image, Err(ShareError::Cancelled));
    d.start(job);
    let mut out = None;
    ctx.run_ui(key(egui::Key::Enter), |ui| out = d.show(ui.ctx(), &mut |t| copy(&log, t))).textures_delta.clear();
    assert_eq!((out.clone(), d.stage().clone()), (None, Stage::Uploading { sent: 0, total: 0 }));
    ctx.run_ui(key(egui::Key::Escape), |ui| out = d.show(ui.ctx(), &mut |t| copy(&log, t))).textures_delta.clear();
    assert_eq!(d.stage(), &Stage::Cancelling);
}

#[test]
fn only_old_clip_folders_with_the_exact_name_holding_a_clip_are_swept() {
    let temp = tempfile::tempdir().unwrap();
    let make = |name: &str, files: &[&str]| {
        let dir = temp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        dir
    };
    let old = make("mythicshot-share-a1B2c3", &["clip.mp4"]);
    let gif = make("mythicshot-share-zzzzzz", &["clip.gif"]);
    let empty = make("mythicshot-share-000000", &[]);
    let other = make("mythicshot-share-a1B2c3x", &["clip.mp4"]);
    let foreign = make("mythicshot-share-qwerty", &["notes.txt"]);
    sweep_stale(temp.path(), Duration::from_secs(3600));
    assert!(old.exists() && gif.exists(), "young folders stay");
    sweep_stale(temp.path(), Duration::ZERO);
    assert!(!old.exists() && !gif.exists() && !empty.exists());
    assert!(other.exists() && foreign.exists());
    assert!(is_temp_dir_name("mythicshot-share-abc123") && !is_temp_dir_name("mythicshot-publish-abc123"));
}

fn image_editor(img: image::RgbaImage) -> Editor {
    let s = crate::settings::Settings::default();
    Editor::new(img, "Loot window".into(), Default::default(), s.dest_context("Loot window"), s.editor.clone())
}

#[test]
fn publish_to_opens_the_image_window_and_measures_the_png_off_the_ui_thread() {
    let mut ed = image_editor(image::RgbaImage::from_pixel(40, 30, image::Rgba([1, 2, 3, 255])));
    ed.open_share_image();
    let v = ed.share.as_ref().expect("the window is open").view();
    assert_eq!((v.caption, v.heading.as_str()), ("Publish image", "Share this image with a link"));
    // Opening it again while it is open changes nothing.
    ed.open_share_image();
    assert!(ed.share.is_some());

    // The size comes from the PNG made off the UI thread, once it is there.
    let d = ed.share.as_mut().unwrap();
    assert_eq!(d.details, "40 × 30 · PNG");
    for _ in 0..500 {
        d.poll(&mut |_| false);
        if d.measure.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let size = publish_ui::size_text(share::png_bytes(&image::RgbaImage::from_pixel(40, 30, image::Rgba([1, 2, 3, 255]))).unwrap().len() as u64);
    assert_eq!(d.details, format!("40 × 30 · PNG · {size}"));
}

static WIDE_ENCODED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn wide_encoder(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    WIDE_ENCODED.store(true, std::sync::atomic::Ordering::SeqCst);
    share::png_bytes(img)
}

#[test]
fn an_image_too_wide_for_any_site_is_refused_at_once_and_never_encoded() {
    let mut ed = image_editor(image::RgbaImage::new(70_000, 1));
    ed.open_share_image_with(wide_encoder);
    let v = ed.share.as_ref().unwrap().view();
    assert_eq!(v.heading, "This image is too large to share");
    assert_eq!(v.lines, vec![Line::Text("It is 70000 × 1 pixels, and a shared image can be at most 65535 pixels on a side. Crop it, or make it smaller, and publish again.".into())]);
    assert_eq!(v.buttons, vec![Button::Close]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!WIDE_ENCODED.load(std::sync::atomic::Ordering::SeqCst), "the encoder was never asked");
}

#[test]
fn publish_waits_until_the_file_is_made_and_a_file_that_cannot_be_made_is_said() {
    let ready = Arc::new(Mutex::new(None::<Result<String, ShareError>>));
    let slot = ready.clone();
    let mut d = image_form().measuring(Box::new(move || slot.lock().unwrap().take()));
    d.details = "1920 × 1080 · PNG".into();
    let (log, copy) = copier(true);
    let v = d.view();
    assert_eq!((v.lines[2].clone(), v.disabled), (Line::Muted("1920 × 1080 · PNG".into()), Some(Button::Publish)));
    assert_eq!(d.press(Button::Publish, &mut |t| copy(&log, t)), None, "Publish waits");
    *ready.lock().unwrap() = Some(Ok(image_details(1920, 1080, MediaType::Png, BYTES)));
    assert_eq!(d.poll(&mut |t| copy(&log, t)), None);
    let v = d.view();
    assert_eq!((v.lines[2].clone(), v.disabled), (Line::Muted("1920 × 1080 · PNG · 1.4 MB".into()), None));
    assert_eq!(d.press(Button::Publish, &mut |t| copy(&log, t)), Some(ShareAction::Publish));

    let mut d = image_form().measuring(Box::new(|| Some(Err(ShareError::Failed(NOT_ENCODED.into())))));
    d.poll(&mut |t| copy(&log, t));
    let v = d.view();
    assert_eq!((v.heading.as_str(), v.lines.clone()), ("The image was not uploaded", vec![Line::Text("Mythicshot could not make a PNG file of the image.".into())]));
}

#[test]
fn a_commit_that_lands_after_cancel_shows_the_link_but_leaves_the_clipboard_alone() {
    let link = "https://img.example.com/i/abcdefghijklmnopqrst";
    let mut d = image_form();
    // The job has already sent the commit: it ends with the link whatever cancel says.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    d.start(ShareJob::spawn(Kind::Image, move |_| {
        let _ = rx.recv_timeout(Duration::from_secs(5));
        Ok(Shared { id: "x".into(), link: Some(link.into()) })
    }));
    let (log, copy) = copier(true);
    d.press(Button::Cancel, &mut |t| copy(&log, t));
    tx.send(()).unwrap();
    assert_eq!(settle(&mut d, &mut |t| copy(&log, t)), Some(ShareAction::Status("The image is online".into())));
    assert!(log.borrow().is_empty(), "nothing was put on the clipboard");
    let v = d.view();
    assert_eq!((v.heading.as_str(), v.link.as_deref(), v.lines.clone()), ("The image is online", Some(link), vec![]));
    assert_eq!(v.buttons, vec![Button::CopyLink, Button::Open, Button::Done]);
    assert!(!v.copied, "nothing was copied, so the button never says so");
}

#[test]
fn a_copy_that_fails_says_to_copy_the_link_by_hand() {
    let mut d = image_form();
    d.stage = Stage::Online { link: Some("https://img.example.com/i/a".into()), copied: false };
    let (log, copy) = copier(false);
    assert_eq!(d.press(Button::CopyLink, &mut |t| copy(&log, t)), None);
    assert_eq!(d.view().lines, vec![Line::Muted("Could not copy the link. Select it and copy it yourself.".into())]);
    let (log, copy) = copier(true);
    d.press(Button::CopyLink, &mut |t| copy(&log, t));
    let v = d.view();
    assert_eq!((v.lines.clone(), v.copied, v.label(Button::CopyLink)), (vec![], true, "Copied"), "a copy that works takes the line away");
}

#[test]
fn the_destination_line_drops_invisible_and_bidi_characters() {
    assert_eq!(to_line(Some("Acme\u{202e}moc.live\u{200b}"), SERVER).as_deref(), Some("To Acmemoc.live at tours.example.com"));
    assert_eq!(to_line(Some("\u{2066}\u{feff}"), SERVER).as_deref(), Some("To tours.example.com"));
}

// ---- The online window: Copy link, Copied, and the layout of the mockup ----

const LINK: &str = "https://img.example.com/i/abcdefghijklmnopqrst";

/// `d` on a clock the test moves by hand: no test waits for "Copied" to go.
fn clocked(d: ShareDialog) -> (ShareDialog, Arc<Mutex<Instant>>) {
    let now = Arc::new(Mutex::new(Instant::now()));
    let shared = now.clone();
    (d.with_clock(move || *shared.lock().unwrap()), now)
}

fn advance(now: &Mutex<Instant>, ms: u64) {
    *now.lock().unwrap() += Duration::from_millis(ms);
}

/// Every form the window has: an image, and a recording sent as MP4 or as GIF.
fn every_kind() -> [(Kind, ShareDialog); 3] {
    [(Kind::Image, image_form()), (Kind::Clip, clip_form(MediaType::Mp4)), (Kind::Clip, clip_form(MediaType::Gif))]
}

#[test]
fn online_offers_copy_link_then_open_in_browser_then_done_for_every_kind() {
    for (kind, mut d) in every_kind() {
        d.stage = Stage::Online { link: Some(LINK.into()), copied: true };
        let v = d.view();
        assert_eq!((v.caption, v.heading.clone()), (caption(kind), format!("The {} is online", kind.noun())));
        assert_eq!(v.lines, vec![Line::Muted("The link was copied. Paste it to whoever should see it.".into())]);
        assert_eq!(v.link.as_deref(), Some(LINK));
        assert_eq!(v.buttons, vec![Button::CopyLink, Button::Open, Button::Done], "the order keyboard focus goes in");
        assert_eq!(v.buttons.iter().map(|b| v.label(*b)).collect::<Vec<_>>(), ["Copy link", "Open in browser", "Done"]);
        let (log, copy) = copier(true);
        assert_eq!(d.press(Button::Open, &mut |t| copy(&log, t)), Some(ShareAction::Open(LINK.into())), "Open in browser opens the link, as Open did");
        assert_eq!(d.press(Button::Done, &mut |t| copy(&log, t)), Some(ShareAction::Closed(None)));
        // Without a link there is nothing to copy or open: only Done, and the sentence as before.
        d.stage = Stage::Online { link: None, copied: false };
        let v = d.view();
        assert_eq!((v.buttons.clone(), v.copied), (vec![Button::Done], false));
        assert_eq!(v.lines, vec![Line::Text(format!("The site did not give a link Mythicshot can open. Find the {} in the dashboard.", kind.noun()))]);
    }
}

#[test]
fn the_copy_after_publishing_shows_copied_for_two_seconds_then_copy_link_again() {
    for (kind, form) in every_kind() {
        let (mut d, now) = clocked(form);
        let (job, tx) = held_job(kind, Ok(Shared { id: "x".into(), link: Some(LINK.into()) }));
        d.start(job);
        tx.send(()).unwrap();
        let (log, copy) = copier(true);
        settle(&mut d, &mut |t| copy(&log, t));
        let v = d.view();
        assert!(v.copied, "{kind:?}");
        assert_eq!(v.label(Button::CopyLink), "Copied");
        assert_eq!(v.lines, vec![Line::Muted(COPIED.into())]);
        advance(&now, 1999);
        assert_eq!(d.view().label(Button::CopyLink), "Copied");
        advance(&now, 1);
        let v = d.view();
        assert_eq!((v.copied, v.label(Button::CopyLink)), (false, "Copy link"));
        assert_eq!(v.lines, vec![Line::Muted(COPIED.into())], "the sentence about the clipboard stays");
    }
}

#[test]
fn a_press_on_copy_link_shows_copied_for_two_seconds_from_the_last_press() {
    let (mut d, now) = clocked(image_form());
    d.stage = Stage::Online { link: Some(LINK.into()), copied: false };
    assert_eq!(d.view().label(Button::CopyLink), "Copy link");
    let (log, copy) = copier(true);
    d.press(Button::CopyLink, &mut |t| copy(&log, t));
    assert_eq!((d.view().copied, d.view().label(Button::CopyLink)), (true, "Copied"));
    advance(&now, 1500);
    d.press(Button::CopyLink, &mut |t| copy(&log, t));
    advance(&now, 1500);
    assert!(d.view().copied, "the second press starts the two seconds again");
    advance(&now, 500);
    assert!(!d.view().copied);
    assert_eq!(log.borrow().as_slice(), [LINK.to_string(), LINK.to_string()]);
    assert!(d.view().lines.is_empty());
}

#[test]
fn a_copy_that_fails_keeps_copy_link_and_the_could_not_copy_line() {
    for (kind, form) in every_kind() {
        // The copy after publishing failed: no "Copied", and no sentence, as before.
        let (mut d, now) = clocked(form);
        let (job, tx) = held_job(kind, Ok(Shared { id: "x".into(), link: Some(LINK.into()) }));
        d.start(job);
        tx.send(()).unwrap();
        let (log, broken) = copier(false);
        settle(&mut d, &mut |t| broken(&log, t));
        let v = d.view();
        assert_eq!((v.copied, v.label(Button::CopyLink), v.lines.clone()), (false, "Copy link", vec![]));
        // A press that fails says so, and the button stays Copy link.
        d.press(Button::CopyLink, &mut |t| broken(&log, t));
        let v = d.view();
        assert_eq!((v.copied, v.label(Button::CopyLink)), (false, "Copy link"));
        assert_eq!(v.lines, vec![Line::Muted("Could not copy the link. Select it and copy it yourself.".into())]);
        // One that works shows Copied; one that fails while it shows takes it away again.
        let (log2, works) = copier(true);
        d.press(Button::CopyLink, &mut |t| works(&log2, t));
        assert!(d.view().copied && d.view().lines.is_empty());
        advance(&now, 500);
        d.press(Button::CopyLink, &mut |t| broken(&log, t));
        let v = d.view();
        assert_eq!((v.copied, v.lines.len()), (false, 1));
    }
}

/// Every shape one frame drew, flattened.
pub(crate) fn shapes_of(out: &egui::FullOutput) -> Vec<&egui::Shape> {
    fn collect<'a>(shape: &'a egui::Shape, into: &mut Vec<&'a egui::Shape>) {
        match shape {
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
            other => into.push(other),
        }
    }
    let mut shapes = Vec::new();
    out.shapes.iter().for_each(|c| collect(&c.shape, &mut shapes));
    shapes
}

/// Where `text` was drawn: the letters' rectangle. A truncated link is found by its full text.
pub(crate) fn text_rect(out: &egui::FullOutput, text: &str) -> Option<egui::Rect> {
    shapes_of(out).into_iter().find_map(|s| match s {
        egui::Shape::Text(t) if t.galley.text() == text => Some(t.galley.rect.translate(t.pos.to_vec2())),
        _ => None,
    })
}

/// The filled rectangle of the button that says `text`: the smallest button-high one around the text.
pub(crate) fn button_rect(out: &egui::FullOutput, text: &str) -> egui::Rect {
    let at = text_rect(out, text).unwrap_or_else(|| panic!("{text} was not drawn")).center();
    shapes_of(out)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) if r.rect.contains(at) && (30.0..=60.0).contains(&r.rect.height()) && r.fill != Color32::TRANSPARENT => Some(r.rect),
            _ => None,
        })
        .min_by(|a, b| a.area().total_cmp(&b.area()))
        .unwrap_or_else(|| panic!("no button around {text}"))
}

/// The fills drawn exactly on `rect`.
pub(crate) fn fills_on(out: &egui::FullOutput, rect: egui::Rect) -> Vec<Color32> {
    shapes_of(out)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(x) if x.rect == rect => Some(x.fill),
            _ => None,
        })
        .collect()
}

/// The window's own frame, its stroke included: the largest rectangle filled with the window colour.
pub(crate) fn window_rect(ctx: &egui::Context, out: &egui::FullOutput) -> egui::Rect {
    let fill = ctx.global_style().visuals.window_fill();
    shapes_of(out)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) if r.fill == fill => Some(r.rect),
            _ => None,
        })
        .max_by(|a, b| a.area().total_cmp(&b.area()))
        .unwrap_or_else(|| panic!("the window frame was drawn: {fill:?} {:?}", shapes_of(out).iter().filter_map(|s| match s { egui::Shape::Rect(r) => Some((r.rect, r.fill)), _ => None }).collect::<Vec<_>>()))
}

/// What the window holds: inside its frame, its stroke and its padding.
pub(crate) fn content_rect(ctx: &egui::Context, out: &egui::FullOutput) -> egui::Rect {
    window_rect(ctx, out).shrink(WINDOW_PADDING + ctx.global_style().visuals.window_stroke().width)
}

/// A few frames of the window on a large screen, a second apart so it has faded in (a new
/// window's first frame only measures it); the last one.
fn draw(ctx: &egui::Context, d: &mut ShareDialog) -> egui::FullOutput {
    let mut last = None;
    for _ in 0..3 {
        let time = ctx.input(|i| i.time) + 1.0;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 900.0))), time: Some(time), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            d.show(ui.ctx(), &mut |_| false);
        });
        out.textures_delta.clear();
        last = Some(out);
    }
    last.unwrap()
}

pub(crate) fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1.0
}

#[test]
fn the_link_and_copy_link_share_one_row_and_the_two_actions_split_the_row_below() {
    for (kind, form) in every_kind() {
        let ctx = egui::Context::default();
        let (mut d, now) = clocked(form);
        d.stage = Stage::Online { link: Some(LINK.into()), copied: false };
        let out = draw(&ctx, &mut d);
        let window = content_rect(&ctx, &out);
        assert!(near(window.width(), WINDOW_WIDTH), "{kind:?}: {window:?}");
        // The link and the button are on one line, the button on the right, closing the row.
        let link = text_rect(&out, LINK).expect("the link is drawn");
        let copy_text = text_rect(&out, "Copy link").unwrap();
        let copy = button_rect(&out, "Copy link");
        assert!(near(link.center().y, copy.center().y), "{link:?} {copy:?}");
        assert!(link.right() < copy.left(), "the link stays left of the button");
        assert!(copy.contains(copy_text.center()));
        assert!(near(copy.height(), LINK_ROW_HEIGHT));
        assert!(near(copy.right(), window.right()), "{copy:?} {window:?}");
        // Open in browser and Done: one line below, equal widths, 12 apart, across the whole width.
        let open = button_rect(&out, "Open in browser");
        let done = button_rect(&out, "Done");
        assert!(near(open.top(), done.top()) && open.top() > copy.bottom(), "{open:?} {done:?} {copy:?}");
        assert!(near(open.width(), done.width()) && near(open.height(), BUTTON_HEIGHT) && near(done.height(), BUTTON_HEIGHT), "{open:?} {done:?}");
        assert!(near(done.left() - open.right(), BUTTON_GAP));
        assert!(near(open.left(), window.left()) && near(done.right(), window.right()), "{open:?} {done:?} {window:?}");
        assert!(near(open.top() - copy.bottom(), BLOCK_GAP), "{open:?} {copy:?}");
        // Neither action has the accent: it belongs to Copy link.
        assert!(fills_on(&out, copy).contains(&PRIMARY));
        assert!(!fills_on(&out, open).contains(&PRIMARY) && !fills_on(&out, done).contains(&PRIMARY));

        // After a press: "Copied" in the success colour, on a button just as wide, and a repaint for when it goes.
        let (log, copy_works) = copier(true);
        d.press(Button::CopyLink, &mut |t| copy_works(&log, t));
        advance(&now, 1500);
        let out = draw(&ctx, &mut d);
        let copied = button_rect(&out, "Copied");
        assert_eq!(copied, copy, "the row does not move when the label changes");
        assert!(fills_on(&out, copied).contains(&SUCCESS));
        assert!(text_rect(&out, "Copy link").is_none());
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).unwrap().repaint_delay;
        assert!(delay <= Duration::from_millis(500), "a repaint for when Copied goes: {delay:?}");
        advance(&now, 500);
        let out = draw(&ctx, &mut d);
        assert!(text_rect(&out, "Copied").is_none() && text_rect(&out, "Copy link").is_some());
    }
}

#[test]
fn a_very_long_link_is_cut_short_and_never_widens_the_window() {
    let ctx = egui::Context::default();
    let mut d = image_form();
    d.stage = Stage::Online { link: Some(LINK.into()), copied: true };
    let short = draw(&ctx, &mut d);
    let long_link = format!("https://img.example.com/i/{}", "abcdefghij".repeat(200));
    d.stage = Stage::Online { link: Some(long_link.clone()), copied: true };
    let long = draw(&ctx, &mut d);
    assert_eq!(window_rect(&ctx, &long), window_rect(&ctx, &short));
    assert_eq!(button_rect(&long, "Copy link"), button_rect(&short, "Copy link"));
    assert_eq!(button_rect(&long, "Done"), button_rect(&short, "Done"));
    let link = text_rect(&long, &long_link).expect("the link is drawn");
    assert!(link.right() < button_rect(&long, "Copy link").left() && link.height() < 30.0, "one line, clipped: {link:?}");
}

#[test]
fn every_other_stage_draws_at_the_same_width_with_buttons_as_high() {
    let ctx = egui::Context::default();
    let mut d = image_form();
    d.stage = Stage::Online { link: Some(LINK.into()), copied: true };
    let width = window_rect(&ctx, &draw(&ctx, &mut d)).width();
    let refused = Stage::Refused(ShareError::Full { needed: 1_400_000, left: 650_000, allowance: 5 * 1_073_741_824 }.refusal(Kind::Image).unwrap());
    let stages: [(Stage, &[&str]); 7] = [
        (Stage::Form, &["Cancel", "Publish"]),
        (Stage::Blocked(publish_ui::ADD_SERVER_AND_TOKEN.into()), &["Close"]),
        (Stage::Preparing(0.4), &["Cancel"]),
        (Stage::Uploading { sent: 943_719, total: BYTES }, &["Cancel"]),
        (Stage::Cancelling, &[]),
        (refused, &["Close"]),
        (Stage::Online { link: None, copied: false }, &["Done"]),
    ];
    let forms: [fn() -> ShareDialog; 2] = [image_form, || clip_form(MediaType::Gif)];
    for form in forms {
        for (stage, buttons) in &stages {
            let mut d = form();
            d.stage = stage.clone();
            let out = draw(&ctx, &mut d);
            assert!(near(window_rect(&ctx, &out).width(), width), "{stage:?}");
            for b in *buttons {
                assert!(near(button_rect(&out, b).height(), BUTTON_HEIGHT), "{stage:?}: {b}");
            }
            // Every sentence of the view is drawn, as it is.
            for t in d.view().texts() {
                assert!(text_rect(&out, &t).is_some(), "{stage:?}: {t} was not drawn");
            }
        }
    }
}
