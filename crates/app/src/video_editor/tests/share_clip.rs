//! "Publish to" for a recording: the clip is prepared through the real export into a temporary
//! folder, then shared with the stand-in site. Tests that need ffmpeg skip, saying so, when it
//! is not on this computer.

use super::*;
use crate::share_ui::{Button, ShareAction, ShareDialog, ShareJob, Stage};
use serde_json::Value;
use std::time::Duration;

const MED: &str = "med_0123456789abcdef01234567";

/// The stand-in site for items, with every request noted. `create` answers the create.
fn media_site(create: impl Fn() -> Option<String> + Send + 'static) -> Stub {
    stub(move |req, _| {
        let l = req.line.as_str();
        if l == "POST /api/v1/media" {
            if let Some(body) = create() {
                return reply(413, &body);
            }
            let bytes = serde_json::from_slice::<Value>(&req.body).ok().and_then(|v| v["bytes"].as_u64()).unwrap_or(1);
            reply(201, &format!(r#"{{"id":"{MED}","parts":{},"part_bytes":2000000}}"#, bytes.div_ceil(2_000_000)))
        } else if l.ends_with("/commit") {
            reply(200, &format!(r#"{{"id":"{MED}","link_id":"abcdefghijklmnopqrst","kind":"clip","path":"/r/abcdefghijklmnopqrst","link":"/r/abcdefghijklmnopqrst"}}"#))
        } else {
            reply(204, "")
        }
    })
}

fn spec(root: &Path, media: share::MediaType) -> ClipSpec {
    let state = state(root);
    ClipSpec { rec: state.rec, items: state.items, events: state.events, edit: state.edit, media, quality: 80, fps: 10 }
}

/// Run the clip job as the window does, in a dialog that follows it, until it ends.
fn shared(clip: ClipSpec, temp: &Path, server: &str, cancel_at_once: bool) -> (ShareDialog, Option<ShareAction>) {
    let (temp, server2) = (temp.to_path_buf(), server.to_string());
    let job = ShareJob::spawn(share::Kind::Clip, move |r| share_clip_job(&clip, &temp, &server2, "Sending a note to the raid".into(), &crate::publish::access::Access::Public, || Ok(TOKEN.to_string()), r));
    let mut d = ShareDialog::new(share::Kind::Clip, server.to_string(), "x", String::new(), false, None, None);
    d.start(job);
    let mut never = |_: &str| false;
    if cancel_at_once {
        d.press(Button::Cancel, &mut never);
    }
    for _ in 0..3000 {
        if let Some(action) = d.poll(&mut never) {
            return (d, Some(action));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the clip job did not end");
}

fn empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}

fn created(s: &Stub) -> Value {
    serde_json::from_slice(&s.requests()[0].body).unwrap()
}

#[test]
fn a_clip_is_an_mp4_with_ffmpeg_and_a_gif_without_and_runs_from_its_first_frame_to_its_end() {
    assert_eq!(clip_media(true), share::MediaType::Mp4);
    assert_eq!(clip_media(false), share::MediaType::Gif);
    let frames = [mythicshot_recording::export::OutFrame { src: 1, t_ms: 500 }, mythicshot_recording::export::OutFrame { src: 2, t_ms: 1000 }];
    assert_eq!(clip_duration_ms(&frames, 2500), 2000);
    assert_eq!(clip_duration_ms(&[], 0), 1);
}

#[test]
fn a_gif_clip_is_prepared_with_the_export_menus_choices_shared_and_its_folder_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let s = media_site(|| None);
    let clip = spec(tmp.path(), share::MediaType::Gif);
    let (w, h) = video::output_size(&clip.rec, &clip.edit);
    let (frames, end) = video::plan(&clip.rec, &clip.edit);
    let duration = clip_duration_ms(&frames, end);
    let (d, action) = shared(clip, temp.path(), &s.base, false);
    assert_eq!(action, Some(ShareAction::Status("The clip is online".into())));
    assert_eq!(d.stage(), &Stage::Online { link: Some(format!("{}/r/abcdefghijklmnopqrst", s.base)), copied: false });
    let body = created(&s);
    assert_eq!((body["kind"].as_str(), body["content_type"].as_str()), (Some("clip"), Some("image/gif")));
    assert_eq!((body["width"].as_u64(), body["height"].as_u64(), body["duration_ms"].as_u64()), (Some(u64::from(w)), Some(u64::from(h)), Some(duration)));
    assert_eq!(body["title"], "Sending a note to the raid");
    let reqs = s.requests();
    let part = reqs.iter().find(|r| r.line.ends_with("/parts/0")).unwrap();
    assert!(part.body.starts_with(b"GIF89a"), "the file is the exported GIF");
    assert_eq!(body["bytes"].as_u64(), Some(part.body.len() as u64));
    let gif = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&part.body)).unwrap();
    assert_eq!(image::ImageDecoder::dimensions(&gif), (w, h));
    // The preview is the first frame, small enough for the site.
    let preview = reqs.iter().find(|r| r.line.ends_with("/preview")).expect("a preview");
    assert!(preview.body.len() <= share::PREVIEW_MAX_BYTES);
    let pic = image::load_from_memory(&preview.body).unwrap();
    assert!(pic.width() <= 512 && pic.height() <= 512 && (pic.width(), pic.height()) == (w.min(512), h.min(512)));
    assert!(empty(temp.path()), "the temporary folder is gone");
}

#[test]
fn a_clip_the_site_has_no_room_for_leaves_no_temporary_file() {
    let tmp = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let s = media_site(|| Some(r#"{"error":"storage_full","detail":"x","used_bytes":100,"allowance_bytes":100,"needed_bytes":5000}"#.into()));
    let (d, action) = shared(spec(tmp.path(), share::MediaType::Gif), temp.path(), &s.base, false);
    assert_eq!(action, Some(ShareAction::Status("The clip was not uploaded".into())));
    assert!(matches!(d.stage(), Stage::Refused(r) if r.note == Some(share::FULL_NOTE)));
    assert!(empty(temp.path()));
}

#[test]
fn a_clip_cancelled_while_it_is_prepared_sends_nothing_and_leaves_no_temporary_file() {
    let tmp = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let s = media_site(|| None);
    let (_d, action) = shared(spec(tmp.path(), share::MediaType::Gif), temp.path(), &s.base, true);
    assert_eq!(action, Some(ShareAction::Closed(Some(crate::share_ui::CANCELLED.into()))));
    assert!(s.lines().is_empty(), "{:?}", s.lines());
    assert!(empty(temp.path()));
}

/// The handler types of the tracks of an MP4: "vide", "soun", and so on.
fn track_kinds(mp4: &[u8]) -> Vec<String> {
    mp4.windows(4).enumerate().filter(|(_, w)| *w == b"hdlr").filter_map(|(i, _)| mp4.get(i + 12..i + 16)).map(|k| String::from_utf8_lossy(k).into_owned()).collect()
}

#[test]
fn an_mp4_clip_is_sent_without_any_sound_track() {
    if mythicshot_recording::ffmpeg::find_now().is_none() {
        eprintln!("skipped: ffmpeg is not on this computer, so no MP4 can be made");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let s = media_site(|| None);
    let (_d, action) = shared(spec(tmp.path(), share::MediaType::Mp4), temp.path(), &s.base, false);
    assert_eq!(action, Some(ShareAction::Status("The clip is online".into())));
    assert_eq!(created(&s)["content_type"], "video/mp4");
    let part = s.requests().into_iter().find(|r| r.line.ends_with("/parts/0")).unwrap();
    assert_eq!(&part.body[4..8], b"ftyp", "an MP4 file");
    let kinds = track_kinds(&part.body);
    assert!(kinds.iter().any(|k| k == "vide"), "{kinds:?}");
    assert!(!kinds.iter().any(|k| k == "soun"), "no audio track: {kinds:?}");
    assert!(empty(temp.path()));
}

#[test]
fn publish_to_in_the_export_menu_opens_the_recording_window() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ed = editor_of(&state(tmp.path()));
    ed.open_share_clip();
    let v = ed.share.as_ref().expect("the window is open").view();
    assert_eq!((v.caption, v.heading.as_str()), ("Publish recording", "Share this recording with a link"));
}
