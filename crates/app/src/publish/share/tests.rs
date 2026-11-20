//! The share protocol against a stand-in site on 127.0.0.1, in the style of the tour upload tests.

use super::*;
use crate::publish::tests::{Answer, Req, Stub, TOKEN, reply, stub};
use std::cell::{Cell, RefCell};

const MED: &str = "med_0123456789abcdef01234567";
const CREATE: &str = "POST /api/v1/media";
const COMMIT: &str = "POST /api/v1/media/med_0123456789abcdef01234567/commit";
const PREVIEW: &str = "PUT /api/v1/media/med_0123456789abcdef01234567/preview";
const DELETE: &str = "DELETE /api/v1/media/med_0123456789abcdef01234567";
const LINK_ID: &str = "abcdefghijklmnopqrst";

fn part(n: usize) -> String {
    format!("PUT /api/v1/media/{MED}/parts/{n}")
}

/// A PNG file's first bytes and some more: the stand-in does not look inside.
fn png(len: usize) -> Vec<u8> {
    let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
    data.extend((0..len.saturating_sub(8)).map(|i| (i % 251) as u8));
    data
}

fn image(len: usize) -> Item {
    Item { media: MediaType::Png, data: png(len), width: 1920, height: 1080, duration_ms: None, title: "Loot window, settings tab".into(), preview: Some(png(40)) }
}

fn clip(len: usize) -> Item {
    Item { media: MediaType::Mp4, data: (0..len).map(|i| (i % 7) as u8).collect(), width: 960, height: 540, duration_ms: Some(12_000), title: String::new(), preview: None }
}

/// The stand-in site: every route answers as the platform does, with parts of `part_bytes`
/// and the commit's link `link`. `over` may answer a request first (with how many earlier
/// requests had the same line).
fn site(part_bytes: u64, link: &'static str, over: impl Fn(&Req, usize) -> Option<Answer> + Send + 'static) -> Stub {
    stub(move |req, seen| {
        if let Some(answer) = over(req, seen) {
            return answer;
        }
        let l = req.line.as_str();
        if l == CREATE {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let bytes = body.get("bytes").and_then(Value::as_u64).unwrap_or(1);
            reply(201, &format!(r#"{{"id":"{MED}","parts":{},"part_bytes":{part_bytes}}}"#, bytes.div_ceil(part_bytes)))
        } else if l.starts_with(&format!("PUT /api/v1/media/{MED}/")) {
            reply(204, "")
        } else if l == COMMIT {
            reply(200, &json!({ "id": MED, "link_id": LINK_ID, "kind": "image", "path": format!("/i/{LINK_ID}"), "link": link }).to_string())
        } else if l == DELETE {
            reply(204, "")
        } else {
            reply(404, r#"{"error":"not_found","detail":"no such item"}"#)
        }
    })
}

fn platform_site() -> Stub {
    site(2_000_000, "https://img.example.com/i/abcdefghijklmnopqrst", |_, _| None)
}

fn client(s: &Stub) -> Client {
    Client::new(&s.base, TOKEN, Timeouts::default()).unwrap()
}

/// Shares `item`, never really sleeping: the waits asked for are added up instead.
struct Run {
    result: Result<Shared, ShareError>,
    reports: Vec<(u64, u64)>,
    slept: Duration,
}

fn run(s: &Stub, item: &Item, mut go: impl FnMut(u64, u64) -> bool) -> Run {
    let slept = Cell::new(Duration::ZERO);
    let reports = RefCell::new(Vec::new());
    let result = share_with(
        &client(s),
        item,
        &mut |sent, total| {
            reports.borrow_mut().push((sent, total));
            go(sent, total)
        },
        &|d| slept.set(slept.get() + d),
    );
    Run { result, reports: reports.into_inner(), slept: slept.get() }
}

fn count(lines: &[String], line: &str) -> usize {
    lines.iter().filter(|l| *l == line).count()
}

fn created_body(s: &Stub) -> Value {
    serde_json::from_slice(&s.requests().iter().find(|r| r.line == CREATE).expect("a create").body).unwrap()
}

#[test]
fn an_image_is_created_sent_previewed_and_committed_and_answers_the_link() {
    let s = platform_site();
    let item = image(1000);
    let r = run(&s, &item, |_, _| true);
    assert_eq!(r.result, Ok(Shared { id: MED.into(), link: Some("https://img.example.com/i/abcdefghijklmnopqrst".into()) }));
    assert_eq!(s.lines(), vec![CREATE.to_string(), part(0), PREVIEW.to_string(), COMMIT.to_string()]);
    assert_eq!(created_body(&s), json!({ "kind": "image", "content_type": "image/png", "bytes": 1000, "width": 1920, "height": 1080, "title": "Loot window, settings tab" }));
    let reqs = s.requests();
    assert_eq!(reqs[1].body, item.data, "the part is the file");
    assert_eq!(reqs[1].content_type.as_deref(), Some("application/octet-stream"));
    assert_eq!(reqs[2].body, item.preview.clone().unwrap());
    assert_eq!(reqs[2].content_type.as_deref(), Some("image/png"));
    assert!(reqs.iter().all(|r| r.auth.as_deref() == Some(&format!("Bearer {TOKEN}") as &str)), "every request carries the token");
    assert_eq!(r.reports.first(), Some(&(0, 1000)));
    assert_eq!(r.reports.last(), Some(&(1000, 1000)));
}

#[test]
fn a_clip_goes_in_parts_of_the_length_the_site_names_in_order() {
    let s = site(4, "/r/abcdefghijklmnopqrst", |_, _| None);
    let item = clip(10);
    let r = run(&s, &item, |_, _| true);
    assert_eq!(r.result, Ok(Shared { id: MED.into(), link: Some(format!("{}/r/{LINK_ID}", s.base)) }));
    assert_eq!(s.lines(), vec![CREATE.to_string(), part(0), part(1), part(2), COMMIT.to_string()]);
    // No title is no title key; a clip says how long it runs.
    assert_eq!(created_body(&s), json!({ "kind": "clip", "content_type": "video/mp4", "bytes": 10, "width": 960, "height": 540, "duration_ms": 12_000 }));
    let reqs = s.requests();
    assert_eq!((reqs[1].body.as_slice(), reqs[2].body.as_slice(), reqs[3].body.as_slice()), (&item.data[0..4], &item.data[4..8], &item.data[8..10]));
    // Bytes sent of the total, climbing to all of it before the commit.
    let sent: Vec<u64> = r.reports.iter().map(|(sent, _)| *sent).collect();
    assert_eq!(sent, vec![0, 0, 4, 8, 10]);
    assert!(r.reports.iter().all(|(_, total)| *total == 10));
}

#[test]
fn a_gif_is_a_clip_too() {
    let s = site(4, "/r/abcdefghijklmnopqrst", |_, _| None);
    let item = Item { media: MediaType::Gif, data: b"GIF89a....".to_vec(), ..clip(0) };
    assert!(run(&s, &item, |_, _| true).result.is_ok());
    assert_eq!(created_body(&s)["content_type"], "image/gif");
    assert_eq!(created_body(&s)["kind"], "clip");
}

#[test]
fn a_part_whose_answer_was_lost_is_sent_once_more() {
    let s = site(4, "/r/abcdefghijklmnopqrst", |req, seen| (req.line == part(1) && seen == 0).then_some(Answer::Drop));
    let r = run(&s, &clip(10), |_, _| true);
    assert!(r.result.is_ok(), "{:?}", r.result);
    let lines = s.lines();
    assert_eq!(count(&lines, &part(1)), 2);
    assert_eq!(count(&lines, DELETE), 0);
}

#[test]
fn a_part_lost_twice_ends_the_share_with_one_delete() {
    let s = site(4, "/r/abcdefghijklmnopqrst", |req, _| (req.line == part(1)).then_some(Answer::Drop));
    let r = run(&s, &clip(10), |_, _| true);
    let Err(ShareError::Failed(e)) = r.result else { panic!("{:?}", r.result) };
    assert!(e.starts_with("Could not send the clip: ") && e.ends_with('.') && !e.contains("dp_"), "{e}");
    let lines = s.lines();
    assert_eq!(count(&lines, &part(1)), 2);
    assert_eq!(count(&lines, DELETE), 1);
    assert_eq!(lines.last().map(String::as_str), Some(DELETE));
    assert_eq!(count(&lines, COMMIT), 0);
}

#[test]
fn a_superseded_part_is_sent_again_once() {
    let superseded = || reply(409, r#"{"error":"superseded","detail":"this send did not take its place"}"#);
    let s = site(4, "/r/abcdefghijklmnopqrst", move |req, seen| (req.line == part(0) && seen == 0).then(superseded));
    assert!(run(&s, &clip(10), |_, _| true).result.is_ok());
    assert_eq!(count(&s.lines(), &part(0)), 2);

    let s = site(4, "/r/abcdefghijklmnopqrst", move |req, _| (req.line == part(0)).then(superseded));
    let r = run(&s, &clip(10), |_, _| true);
    assert!(matches!(&r.result, Err(ShareError::Failed(e)) if e.starts_with("Could not send the clip: ")), "{:?}", r.result);
    let lines = s.lines();
    assert_eq!((count(&lines, &part(0)), count(&lines, DELETE), count(&lines, COMMIT)), (2, 1, 0));
}

fn too_many(seconds: Option<&str>) -> Answer {
    let headers = seconds.map(|s| vec![("retry-after", s.to_string())]).unwrap_or_default();
    Answer::Reply(429, r#"{"error":"too_many_items","detail":"this workspace started items 30 times in the last hour; try again later"}"#.into(), headers)
}

#[test]
fn a_429_is_waited_out_once_when_the_site_asks_for_a_minute_or_less() {
    let s = site(4, "/r/abcdefghijklmnopqrst", |req, seen| (req.line == CREATE && seen == 0).then(|| too_many(Some("2"))));
    let r = run(&s, &clip(10), |_, _| true);
    assert!(r.result.is_ok(), "{:?}", r.result);
    assert_eq!(r.slept, Duration::from_secs(2));
    assert_eq!(count(&s.lines(), CREATE), 2);

    let s = site(4, "/r/abcdefghijklmnopqrst", |req, seen| (req.line == part(2) && seen == 0).then(|| too_many(Some("60"))));
    let r = run(&s, &clip(10), |_, _| true);
    assert!(r.result.is_ok(), "{:?}", r.result);
    assert_eq!(r.slept, Duration::from_secs(60));
    assert_eq!(count(&s.lines(), &part(2)), 2);
}

#[test]
fn a_longer_wait_a_second_429_or_none_said_gives_up_with_a_sentence() {
    let s = site(4, "/r/abcdefghijklmnopqrst", |req, _| (req.line == CREATE).then(|| too_many(Some("61"))));
    let r = run(&s, &clip(10), |_, _| true);
    assert_eq!(r.result, Err(ShareError::Failed("Could not start the upload: the site asks to wait 61 seconds before more is sent. Publish again later.".into())));
    assert_eq!((r.slept, s.lines()), (Duration::ZERO, vec![CREATE.to_string()]), "nothing was created, so nothing is deleted");

    let s = site(4, "/r/abcdefghijklmnopqrst", |req, _| (req.line == part(1)).then(|| too_many(Some("3"))));
    let r = run(&s, &clip(10), |_, _| true);
    assert_eq!(r.result, Err(ShareError::Failed("Could not send the clip: the site is busy. Publish again later.".into())));
    assert_eq!(r.slept, Duration::from_secs(3), "waited once");
    let lines = s.lines();
    assert_eq!((count(&lines, &part(1)), count(&lines, DELETE)), (2, 1));

    let s = site(4, "/r/abcdefghijklmnopqrst", |req, _| (req.line == part(0)).then(|| too_many(None)));
    let r = run(&s, &clip(10), |_, _| true);
    assert_eq!(r.result, Err(ShareError::Failed("Could not send the clip: the site is busy. Publish again later.".into())));
    assert_eq!(count(&s.lines(), DELETE), 1);
}

#[test]
fn cancel_between_parts_stops_and_deletes_the_item_exactly_once() {
    let s = site(4, "/r/abcdefghijklmnopqrst", |_, _| None);
    let r = run(&s, &clip(10), |sent, _| sent < 4);
    assert_eq!(r.result, Err(ShareError::Cancelled));
    assert_eq!(s.lines(), vec![CREATE.to_string(), part(0), DELETE.to_string()]);
}

#[test]
fn cancel_before_the_create_sends_nothing_and_cancel_before_the_commit_deletes() {
    let s = platform_site();
    assert_eq!(run(&s, &image(100), |_, _| false).result, Err(ShareError::Cancelled));
    assert!(s.lines().is_empty());

    let s = platform_site();
    let calls = Cell::new(0);
    // Every check passes but the last one, just before the commit.
    let r = run(&s, &image(100), |_, _| {
        calls.set(calls.get() + 1);
        calls.get() < 4
    });
    assert_eq!(r.result, Err(ShareError::Cancelled));
    assert_eq!(s.lines(), vec![CREATE.to_string(), part(0), PREVIEW.to_string(), DELETE.to_string()]);
}

#[test]
fn cancel_while_waiting_for_the_site_ends_the_wait() {
    let s = site(4, "/r/abcdefghijklmnopqrst", |req, _| (req.line == part(1)).then(|| too_many(Some("30"))));
    let waits = Cell::new(0);
    let r = run(&s, &clip(10), |sent, _| {
        if sent == 4 {
            waits.set(waits.get() + 1);
        }
        waits.get() < 5
    });
    assert_eq!(r.result, Err(ShareError::Cancelled));
    assert!(r.slept < Duration::from_secs(1), "{:?}", r.slept);
    assert_eq!(count(&s.lines(), DELETE), 1);
}

fn storage_full(used: u64, allowance: u64, needed: u64) -> String {
    json!({ "error": "storage_full", "detail": "this image needs 1.4 MB of room, but the workspace has 0.6 MB left of its 5 GB allowance. Nothing was uploaded.", "used_bytes": used, "allowance_bytes": allowance, "needed_bytes": needed }).to_string()
}

const FIVE_GB: u64 = 5 * 1024 * 1024 * 1024;

#[test]
fn a_full_workspace_is_said_with_the_sites_numbers_and_nothing_is_sent_or_deleted() {
    let s = site(2_000_000, "/i/x", |req, _| (req.line == CREATE).then(|| reply(413, &storage_full(FIVE_GB - 650_000, FIVE_GB, 1_400_000))));
    let r = run(&s, &image(1000), |_, _| true);
    let e = r.result.unwrap_err();
    assert_eq!(e, ShareError::Full { needed: 1_400_000, left: 650_000, allowance: FIVE_GB });
    assert_eq!(s.lines(), vec![CREATE.to_string()]);
    let image = e.refusal(Kind::Image).unwrap();
    assert_eq!(image.heading, "The image was not uploaded");
    assert_eq!(image.text, "This image needs 1.4 MB, and your workspace has 634 KB left of its 5.0 GB. Delete things you no longer need, or ask for more room, and publish again.");
    assert_eq!(image.note, Some("Everything that is already online is not affected."));
    let clip = e.refusal(Kind::Clip).unwrap();
    assert_eq!(clip.heading, "The clip was not uploaded");
    assert_eq!(clip.text, "This clip needs 1.4 MB, and your workspace has 634 KB left of its 5.0 GB. Delete things you no longer need, or ask for more room, and publish again.");
    assert_eq!(clip.note, Some(FULL_NOTE));
}

#[test]
fn a_full_workspace_without_numbers_keeps_the_sites_sentence_as_a_tour_does() {
    let s = site(2_000_000, "/i/x", |req, _| (req.line == CREATE).then(|| reply(413, r#"{"error":"storage_full","detail":"no room left"}"#)));
    let e = run(&s, &image(1000), |_, _| true).result.unwrap_err();
    assert_eq!(e, ShareError::Failed("Could not start the upload: no room left (HTTP 413).".into()));
    let r = e.refusal(Kind::Image).unwrap();
    assert_eq!((r.heading.as_str(), r.note), ("The image was not uploaded", None));
}

#[test]
fn an_item_over_the_default_limits_is_refused_before_anything_is_sent() {
    let s = platform_site();
    let big = Item { data: png(IMAGE_MAX_BYTES as usize + 1), ..image(0) };
    let r = run(&s, &big, |_, _| true);
    assert_eq!(r.result, Err(ShareError::TooLarge(TooLarge::Bytes { bytes: IMAGE_MAX_BYTES + 1, max: IMAGE_MAX_BYTES, exact: true })));
    let wide = Item { width: 9000, height: 3000, ..image(100) };
    assert_eq!(run(&s, &wide, |_, _| true).result, Err(ShareError::TooLarge(TooLarge::Side { width: 9000, height: 3000, max: 8192 })));
    let long = Item { duration_ms: Some(CLIP_MAX_DURATION_MS + 1), ..clip(10) };
    assert_eq!(run(&s, &long, |_, _| true).result, Err(ShareError::TooLarge(TooLarge::Duration { ms: 600_001, max: 600_000 })));
    assert!(s.lines().is_empty(), "{:?}", s.lines());
    // A clip may be as large as 50 MiB; an image of that size may not.
    assert_eq!(too_large(Kind::Clip, Some(CLIP_MAX_BYTES), true, 960, 540, Some(12_000)), None);
    assert!(too_large(Kind::Image, Some(CLIP_MAX_BYTES), true, 960, 540, None).is_some());
    assert_eq!(too_large(Kind::Clip, None, false, 960, 540, Some(600_000)), None, "no size known yet, and ten minutes is allowed");
}

#[test]
fn the_sites_own_limit_is_said_with_its_number_and_nothing_is_deleted() {
    let s = site(2_000_000, "/i/x", |req, _| {
        (req.line == CREATE).then(|| reply(413, r#"{"error":"media_too_large","detail":"the file is too long: an image may have at most 1000 bytes","max_bytes":1000,"max_side":8192}"#))
    });
    let r = run(&s, &image(2000), |_, _| true);
    assert_eq!(r.result, Err(ShareError::TooLarge(TooLarge::Bytes { bytes: 2000, max: 1000, exact: true })));
    assert_eq!(s.lines(), vec![CREATE.to_string()]);

    let s = site(2_000_000, "/i/x", |req, _| (req.line == CREATE).then(|| reply(413, r#"{"error":"media_too_large","detail":"x","max_bytes":5000000,"max_side":1000}"#)));
    assert_eq!(run(&s, &image(2000), |_, _| true).result, Err(ShareError::TooLarge(TooLarge::Side { width: 1920, height: 1080, max: 1000 })));
}

#[test]
fn the_too_large_sentences_are_mythicshots_own() {
    let refusal = |kind, t| ShareError::TooLarge(t).refusal(kind).unwrap();
    let mib = 1024 * 1024;
    let r = refusal(Kind::Image, TooLarge::Bytes { bytes: 14 * mib, max: IMAGE_MAX_BYTES, exact: true });
    assert_eq!(r.heading, "This image is too large to share");
    assert_eq!(r.text, "It is 14.0 MB, and a shared image can be at most 10.0 MB. Crop it, or make it smaller, and publish again.");
    assert_eq!(r.note, None);
    let r = refusal(Kind::Clip, TooLarge::Bytes { bytes: 74 * mib, max: CLIP_MAX_BYTES, exact: false });
    assert_eq!(r.heading, "This clip is too large to share");
    assert_eq!(r.text, "It would be 74.0 MB, and a shared clip can be at most 50.0 MB. Trim it, or lower the size or quality in Export, and publish again.");
    let r = refusal(Kind::Clip, TooLarge::Bytes { bytes: 74 * mib, max: CLIP_MAX_BYTES, exact: true });
    assert_eq!(r.text, "It is 74.0 MB, and a shared clip can be at most 50.0 MB. Trim it, or lower the size or quality in Export, and publish again.");
    // Just over the limit never reads as the same size twice.
    let r = refusal(Kind::Image, TooLarge::Bytes { bytes: IMAGE_MAX_BYTES + 1, max: IMAGE_MAX_BYTES, exact: true });
    assert_eq!(r.text, "It is 10.1 MB, and a shared image can be at most 10.0 MB. Crop it, or make it smaller, and publish again.");
    let r = refusal(Kind::Image, TooLarge::Side { width: 9000, height: 3000, max: 8192 });
    assert_eq!(r.text, "It is 9000 × 3000 pixels, and a shared image can be at most 8192 pixels on a side. Crop it, or make it smaller, and publish again.");
    let r = refusal(Kind::Clip, TooLarge::Side { width: 9000, height: 3000, max: 8192 });
    assert_eq!(r.text, "It is 9000 × 3000 pixels, and a shared clip can be at most 8192 pixels on a side. Lower the size in Export, and publish again.");
    let r = refusal(Kind::Clip, TooLarge::Duration { ms: 650_200, max: CLIP_MAX_DURATION_MS });
    assert_eq!(r.text, "It is 10 minutes 51 seconds long, and a shared clip can be at most 10 minutes long. Trim it, and publish again.");
    assert_eq!(ShareError::Cancelled.refusal(Kind::Image), None);
}

#[test]
fn a_preview_that_fails_never_fails_the_share() {
    let answers: [fn() -> Answer; 3] = [|| reply(500, r#"{"error":"internal"}"#), || Answer::Drop, || reply(413, r#"{"error":"storage_full","detail":"x","used_bytes":1,"allowance_bytes":1,"needed_bytes":1}"#)];
    for answer in answers {
        let s = site(2_000_000, "/i/abcdefghijklmnopqrst", move |req, _| (req.line == PREVIEW).then(answer));
        let r = run(&s, &image(100), |_, _| true);
        assert!(matches!(&r.result, Ok(Shared { link: Some(_), .. })), "{:?}", r.result);
        let lines = s.lines();
        assert!(count(&lines, PREVIEW) <= 2 && count(&lines, PREVIEW) >= 1, "{lines:?}");
        assert_eq!((count(&lines, COMMIT), count(&lines, DELETE)), (1, 0));
    }
    // A superseded preview is sent once more, and then the share goes on whatever it answers.
    let s = site(2_000_000, "/i/abcdefghijklmnopqrst", |req, _| (req.line == PREVIEW).then(|| reply(409, r#"{"error":"superseded"}"#)));
    assert!(run(&s, &image(100), |_, _| true).result.is_ok());
    assert_eq!(count(&s.lines(), PREVIEW), 2);
    // A preview that is neither PNG nor JPEG, or too large, is not sent at all.
    let s = platform_site();
    let odd = Item { preview: Some(b"GIF89a".to_vec()), ..image(100) };
    assert!(run(&s, &odd, |_, _| true).result.is_ok());
    let big = Item { preview: Some(png(PREVIEW_MAX_BYTES + 1)), ..image(100) };
    assert!(run(&s, &big, |_, _| true).result.is_ok());
    assert_eq!(count(&s.lines(), PREVIEW), 0);
}

#[test]
fn the_commit_is_sent_again_once_after_a_lost_answer_or_an_incomplete() {
    let s = site(2_000_000, "/i/abcdefghijklmnopqrst", |req, seen| (req.line == COMMIT && seen == 0).then_some(Answer::Drop));
    assert!(run(&s, &image(100), |_, _| true).result.is_ok());
    assert_eq!(count(&s.lines(), COMMIT), 2);

    let s = site(2_000_000, "/i/abcdefghijklmnopqrst", |req, seen| (req.line == COMMIT && seen == 0).then(|| reply(409, r#"{"error":"incomplete","detail":"a part was being sent again while finishing; finish it once more"}"#)));
    assert!(run(&s, &image(100), |_, _| true).result.is_ok());
    assert_eq!((count(&s.lines(), COMMIT), count(&s.lines(), DELETE)), (2, 0));
}

#[test]
fn a_commit_that_fails_or_answers_about_another_item_deletes_once() {
    let answers: [fn() -> Answer; 4] = [
        || reply(409, r#"{"error":"incomplete","detail":"part 0 has not been sent"}"#),
        || Answer::Drop,
        || reply(200, r#"{"id":"med_ffffffffffffffffffffffff","link":"/i/x"}"#),
        || reply(200, "<html>not json</html>"),
    ];
    for answer in answers {
        let s = site(2_000_000, "/i/abcdefghijklmnopqrst", move |req, _| (req.line == COMMIT).then(answer));
        let r = run(&s, &image(100), |_, _| true);
        assert!(matches!(&r.result, Err(ShareError::Failed(e)) if e.starts_with("Could not finish the upload: ")), "{:?}", r.result);
        let lines = s.lines();
        assert_eq!(count(&lines, DELETE), 1, "{lines:?}");
        assert_eq!(lines.last().map(String::as_str), Some(DELETE));
    }
}

#[test]
fn a_create_answer_that_makes_no_sense_is_refused_and_an_item_it_names_is_deleted() {
    // No usable id: nothing to delete.
    let s = site(2_000_000, "/i/x", |req, _| (req.line == CREATE).then(|| reply(201, r#"{"id":"up_0123","parts":1,"part_bytes":2000000}"#)));
    assert_eq!(run(&s, &image(100), |_, _| true).result, Err(ShareError::Failed("Could not start the upload: the server's answer was not understood.".into())));
    assert_eq!(s.lines(), vec![CREATE.to_string()]);
    // Parts that do not add up to the file: the item exists, and is deleted.
    let s = site(2_000_000, "/i/x", |req, _| (req.line == CREATE).then(|| reply(201, &format!(r#"{{"id":"{MED}","parts":3,"part_bytes":2000000}}"#))));
    assert!(matches!(run(&s, &image(100), |_, _| true).result, Err(ShareError::Failed(_))));
    assert_eq!(s.lines(), vec![CREATE.to_string(), DELETE.to_string()]);
}

#[test]
fn a_create_whose_answer_was_lost_is_never_sent_twice() {
    let s = site(2_000_000, "/i/x", |req, _| (req.line == CREATE).then_some(Answer::Drop));
    let r = run(&s, &image(100), |_, _| true);
    assert!(matches!(&r.result, Err(ShareError::Failed(e)) if e.starts_with("Could not start the upload: ")), "{:?}", r.result);
    assert_eq!(s.lines(), vec![CREATE.to_string()]);
}

#[test]
fn a_rejected_token_is_said_without_the_token() {
    let s = site(2_000_000, "/i/x", |_, _| Some(reply(401, r#"{"error":"unauthorized"}"#)));
    let r = run(&s, &image(100), |_, _| true);
    assert_eq!(r.result, Err(ShareError::Failed(crate::publish::REJECTED_TOKEN.into())));
}

#[test]
fn the_title_is_trimmed_cleaned_and_cut_to_120_characters() {
    assert_eq!(clean_title("  Loot window \u{202e}\t "), "Loot window");
    assert_eq!(clean_title("a\nb\u{200b}c"), "abc");
    assert_eq!(clean_title("   "), "");
    let long = "é".repeat(130);
    assert_eq!(clean_title(&long).chars().count(), TITLE_MAX);
    let s = platform_site();
    let item = Item { title: format!("  {long}  "), ..image(100) };
    assert!(run(&s, &item, |_, _| true).result.is_ok());
    assert_eq!(created_body(&s)["title"].as_str().unwrap().chars().count(), 120);
    let s = platform_site();
    let blank = Item { title: " \t ".into(), ..image(100) };
    assert!(run(&s, &blank, |_, _| true).result.is_ok());
    assert!(created_body(&s).get("title").is_none(), "an empty title is no title");
}

#[test]
fn every_link_form_the_site_may_answer_with() {
    let server = "https://tours.example.com";
    // A full https address on the site's own media address, on the server itself, or anywhere.
    assert_eq!(item_link(server, "https://img.example.com/i/abcdefghijklmnopqrst").as_deref(), Some("https://img.example.com/i/abcdefghijklmnopqrst"));
    assert_eq!(item_link(server, "https://tours.example.com/r/abcdefghijklmnopqrst").as_deref(), Some("https://tours.example.com/r/abcdefghijklmnopqrst"));
    assert_eq!(item_link(server, "https://media.other-host.net:8443/i/abc").as_deref(), Some("https://media.other-host.net:8443/i/abc"));
    // A path goes after the configured server, path and all.
    assert_eq!(item_link(server, "/i/abcdefghijklmnopqrst").as_deref(), Some("https://tours.example.com/i/abcdefghijklmnopqrst"));
    assert_eq!(item_link("https://example.com/demo/", "/r/abc").as_deref(), Some("https://example.com/demo/r/abc"));
    assert_eq!(item_link("http://127.0.0.1:8787", "/i/abc").as_deref(), Some("http://127.0.0.1:8787/i/abc"));
    // Plain http only next to a plain-http server on this computer.
    assert_eq!(item_link("http://localhost:8787", "http://localhost:9000/i/abc").as_deref(), Some("http://localhost:9000/i/abc"));
    for bad in [
        "http://img.example.com/i/abc",
        "http://localhost/i/abc",
        "ftp://img.example.com/i/abc",
        "javascript:alert(1)",
        "file:///C:/Windows/System32/calc.exe",
        "mailto:x@example.com",
        "https://user@img.example.com/i/abc",
        "https://user:pw@img.example.com/i/abc",
        "https://img.example.com/i/abc?x=1",
        "https://img.example.com/i/abc#x",
        "https://img.example.com/i/a\u{7}bc",
        "https://img.example.com/i/abc\n",
        "https://img.example.com/i/a bc",
        "https://img.example.com/i/a\u{202e}bc",
        "https://img.example.com/i/a%20bc",
        "https://img.example.com/../i/abc",
        "//evil.example/i/abc",
        "/i/a\\b",
        "/i/a\u{0}b",
        "i/abc",
        "HTTPS://img.example.com/i/abc",
        "",
    ] {
        assert_eq!(item_link(server, bad), None, "{bad:?}");
    }
    assert_eq!(item_link(server, &format!("https://img.example.com/{}", "a".repeat(400))), None, "too long to be shown or opened");
    assert_eq!(item_link("not a server", "/i/abc"), None);
    // The tour rule is not loosened: a tour link on another host is still refused.
    assert_eq!(crate::publish::public_url(server, Some("https://img.example.com/t/abc"), None), None);
}

#[test]
fn a_link_mythicshot_cannot_use_still_leaves_the_item_shared_and_not_deleted() {
    for link in ["javascript:alert(1)", "http://img.example.com/i/abc", "https://u@img.example.com/i/abc", "https://img.example.com/i/\u{1b}[0m"] {
        let s = site(2_000_000, Box::leak(link.to_string().into_boxed_str()), |_, _| None);
        let r = run(&s, &image(100), |_, _| true);
        assert_eq!(r.result, Ok(Shared { id: MED.into(), link: None }), "{link:?}");
        assert_eq!(count(&s.lines(), DELETE), 0);
    }
}

#[test]
fn the_token_goes_only_to_the_configured_server_never_to_the_links_host() {
    // The link's host is a second stand-in on this computer; it must never hear from Mythicshot.
    let media_host = stub(|_, _| reply(200, "{}"));
    let link: &'static str = Box::leak(format!("{}/i/{LINK_ID}", media_host.base).into_boxed_str());
    let s = site(4, link, |_, _| None);
    let r = run(&s, &clip(10), |_, _| true);
    assert_eq!(r.result, Ok(Shared { id: MED.into(), link: Some(link.to_string()) }));
    assert!(media_host.lines().is_empty(), "{:?}", media_host.lines());
    let reqs = s.requests();
    assert!(!reqs.is_empty() && reqs.iter().all(|r| r.auth.as_deref() == Some(&format!("Bearer {TOKEN}") as &str)));
    // Nor is the token in any error.
    let s = site(4, link, |req, _| (req.line == part(0)).then(|| reply(400, &format!(r#"{{"error":"bad_media","detail":"echo {TOKEN}"}}"#))));
    let Err(ShareError::Failed(e)) = run(&s, &clip(10), |_, _| true).result else { panic!() };
    assert!(!e.contains(TOKEN) && !e.contains("dp_"), "{e}");
}

#[test]
fn a_preview_fits_512_pixels_and_100000_bytes() {
    let wide = image::RgbaImage::from_fn(1920, 1080, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8, 255]));
    let p = preview_of(&wide).unwrap();
    assert!(p.len() <= PREVIEW_MAX_BYTES, "{}", p.len());
    let read = image::load_from_memory(&p).unwrap();
    assert_eq!((read.width(), read.height()), (512, 288));
    // Noise does not compress: it comes back as a JPEG, still within the rule.
    let mut seed = 1u32;
    let noise = image::RgbaImage::from_fn(600, 900, |_, _| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let b = seed.to_le_bytes();
        image::Rgba([b[0], b[1], b[2], 255])
    });
    let p = preview_of(&noise).unwrap();
    assert!(p.len() <= PREVIEW_MAX_BYTES && p.starts_with(&[0xFF, 0xD8, 0xFF]), "{}", p.len());
    let read = image::load_from_memory(&p).unwrap();
    assert!(read.width() <= 512 && read.height() <= 512);
    // A small picture keeps its size.
    let small = image::RgbaImage::from_pixel(40, 30, image::Rgba([1, 2, 3, 255]));
    assert_eq!(image::load_from_memory(&preview_of(&small).unwrap()).unwrap().width(), 40);
    assert_eq!(preview_of(&image::RgbaImage::new(0, 0)), None);
}

#[test]
fn the_image_is_sent_as_a_png_of_the_picture() {
    let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]));
    let bytes = png_bytes(&img).unwrap();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), img);
}
