//! The update line, the Preferences section and the tray entry, against a stand-in network that
//! sends nothing anywhere and a clock the test sets.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;

use super::*;
use crate::update::{Checked, Downloaded, Release, Version};

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn release(s: &str) -> Release {
    Release::for_test(v(s), 41_943_040, "0".repeat(64))
}

fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 24).unwrap().and_hms_opt(h, m, 0).unwrap()
}

/// What the stand-in network answers, and what it was asked.
#[derive(Default)]
struct FakeNet {
    answer: Mutex<Option<Result<Checked, String>>>,
    checks: AtomicUsize,
    cleans: AtomicUsize,
    downloads: AtomicUsize,
    /// What a download ends in, once `release_download` is set; it watches `cancel` meanwhile.
    download_end: Mutex<Option<Downloaded>>,
    release_download: AtomicBool,
    saw_cancel: AtomicBool,
    discarded: Mutex<Vec<PathBuf>>,
    logged: Mutex<Vec<String>>,
}

impl Net for FakeNet {
    fn check(&self, _running: Version) -> Result<Checked, String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.answer.lock().unwrap().clone().unwrap_or(Err("no answer".into()))
    }
    fn download(&self, release: &Release, _attempt: u64, done: &AtomicU64, cancel: &AtomicBool) -> Downloaded {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        done.store(release.size() / 4, Ordering::SeqCst);
        for _ in 0..2000 {
            if cancel.load(Ordering::SeqCst) {
                self.saw_cancel.store(true, Ordering::SeqCst);
                return Downloaded::Cancelled;
            }
            if self.release_download.load(Ordering::SeqCst) {
                return self.download_end.lock().unwrap().clone().unwrap_or(Downloaded::Failed("no answer".into()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Downloaded::Failed("the test never let it end".into())
    }
    fn clean(&self, _running: Version) -> usize {
        self.cleans.fetch_add(1, Ordering::SeqCst);
        0
    }
    fn discard(&self, path: &Path) {
        self.discarded.lock().unwrap().push(path.to_path_buf());
    }
    fn log(&self, step: &str) {
        self.logged.lock().unwrap().push(step.to_string());
    }
}

fn updater(net: &Arc<FakeNet>) -> Updater {
    Updater::new(net.clone(), v("0.2.34"))
}

/// Poll until the background work it started has ended and been taken in.
fn settle(u: &mut Updater) {
    for _ in 0..1000 {
        u.poll(at(18, 13));
        if !u.busy() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("the background work never ended");
}

fn offered(net: &Arc<FakeNet>) -> Updater {
    *net.answer.lock().unwrap() = Some(Ok(Checked::Newer(release("0.2.35"))));
    let mut u = updater(net);
    u.start(true);
    settle(&mut u);
    u
}

fn text(u: &Updater) -> Option<String> {
    u.banner().map(|b| b.text)
}

fn buttons(u: &Updater) -> Vec<Button> {
    u.banner().map(|b| b.buttons).unwrap_or_default()
}

// ---- Wording ----

#[test]
fn the_approved_status_line_is_built_exactly_from_its_pattern() {
    let last = LastCheck { at: at(18, 13), outcome: Outcome::UpToDate };
    assert_eq!(status_text(v("0.2.34"), false, Some(&last), at(19, 0)), "Version 0.2.34. Last checked today, 6:13 PM: up to date.");
    let morning = LastCheck { at: at(9, 5), outcome: Outcome::UpToDate };
    assert_eq!(status_text(v("0.2.34"), false, Some(&morning), at(19, 0)), "Version 0.2.34. Last checked today, 9:05 AM: up to date.");
}

#[test]
fn the_approved_sentences_are_word_for_word() {
    assert_eq!(SECTION, "Updates");
    assert_eq!(CHECKBOX, "Check for a newer version when Mythicshot starts");
    assert_eq!(HELPER, "Asks mythicshot.com for the current version number. Nothing about you or this computer is sent.");
    assert_eq!(CHECK_NOW, "Check now");
    assert_eq!(available_text(v("0.2.35"), v("0.2.34")), "Mythicshot 0.2.35 is available. You have 0.2.34.");
    assert_eq!(WHATS_NEW, "What's new");
    assert_eq!(Button::Later.label(), "Later");
    assert_eq!(Button::UpdateNow.label(), "Update now");
    assert_eq!(tray_text(v("0.2.35")), "Update to 0.2.35…");
    assert_eq!(downloading_text(v("0.2.35"), 3_355_443, 41_943_040), "Downloading Mythicshot 0.2.35… 3.2 of 40.0 MB");
    assert_eq!(Button::Cancel.label(), "Cancel");
    assert_eq!(closing_text(v("0.2.35")), "Checked. Mythicshot will close, install 0.2.35 and open again.");
    assert_eq!(WAITING, "A recording or upload is still running. The update starts when it finishes.");
    assert_eq!(Button::CancelUpdate.label(), "Cancel update");
    assert_eq!(MISMATCH, "The download did not match what mythicshot.com lists, so it was not installed. Nothing was changed.");
    assert_eq!(Button::TryAgain.label(), "Try again");
    assert_eq!(Button::OpenDownloadPage.label(), "Open download page");
}

#[test]
fn every_status_line_and_the_download_failure_are_word_for_word() {
    let last = |outcome| LastCheck { at: at(18, 13), outcome };
    let r = v("0.2.34");
    assert_eq!(status_text(r, false, None, at(19, 0)), "Version 0.2.34. Not checked yet.");
    assert_eq!(status_text(r, true, None, at(19, 0)), "Version 0.2.34. Checking now…");
    assert_eq!(status_text(r, false, Some(&last(Outcome::Newer(v("0.2.35")))), at(19, 0)), "Version 0.2.34. Last checked today, 6:13 PM: 0.2.35 is available.");
    assert_eq!(status_text(r, false, Some(&last(Outcome::Failed)), at(19, 0)), "Version 0.2.34. Last checked today, 6:13 PM: mythicshot.com could not be reached or did not answer as expected.");
    // Checks of other days.
    let next_day = at(18, 13) + chrono::Duration::days(1);
    assert_eq!(status_text(r, false, Some(&last(Outcome::UpToDate)), next_day), "Version 0.2.34. Last checked yesterday, 6:13 PM: up to date.");
    let later = at(18, 13) + chrono::Duration::days(3);
    assert_eq!(status_text(r, false, Some(&last(Outcome::UpToDate)), later), "Version 0.2.34. Last checked Nov 24, 6:13 PM: up to date.");
    assert_eq!(status_text(r, false, Some(&last(Outcome::Failed)), next_day), "Version 0.2.34. Last checked yesterday, 6:13 PM: mythicshot.com could not be reached or did not answer as expected.");
    // The download that did not finish: one sentence, the second one dropped.
    assert_eq!(DOWNLOAD_FAILED, "The download from mythicshot.com did not finish, so nothing was installed.");
}

// ---- The check at start ----

#[test]
fn with_the_setting_off_no_request_is_made_at_all() {
    let net = Arc::new(FakeNet::default());
    let mut u = updater(&net);
    u.start(false);
    settle(&mut u);
    assert_eq!(net.checks.load(Ordering::SeqCst), 0);
    assert_eq!(net.cleans.load(Ordering::SeqCst), 1, "old installers are still removed");
    assert_eq!(text(&u), None);
    // A second start in the same run does nothing.
    u.start(true);
    settle(&mut u);
    assert_eq!(net.checks.load(Ordering::SeqCst), 0);
}

#[test]
fn with_the_setting_on_one_check_runs_in_the_background_at_start() {
    let net = Arc::new(FakeNet::default());
    *net.answer.lock().unwrap() = Some(Ok(Checked::UpToDate));
    let mut u = updater(&net);
    u.start(true);
    u.start(true);
    settle(&mut u);
    assert_eq!(net.checks.load(Ordering::SeqCst), 1);
    assert_eq!(u.status_line(at(19, 0)), "Version 0.2.34. Last checked today, 6:13 PM: up to date.");
    assert_eq!(text(&u), None);
    assert_eq!(u.tray_label(), None);
    assert_eq!(*net.logged.lock().unwrap(), ["clean: 0 old installers removed", "check: up to date at 0.2.34"]);
}

#[test]
fn a_check_at_start_that_fails_shows_nothing_anywhere() {
    let net = Arc::new(FakeNet::default());
    let mut u = updater(&net);
    u.start(true);
    settle(&mut u);
    assert_eq!(net.checks.load(Ordering::SeqCst), 1);
    assert_eq!(text(&u), None);
    assert_eq!(u.tray_label(), None);
    assert_eq!(u.status_line(at(19, 0)), "Version 0.2.34. Not checked yet.", "the status line says nothing about it either");
    assert!(net.logged.lock().unwrap().iter().any(|l| l == "check: failed (no answer)"));
}

#[test]
fn check_now_reports_a_failure_and_works_with_the_setting_off() {
    let net = Arc::new(FakeNet::default());
    let mut u = updater(&net);
    u.start(false);
    settle(&mut u);
    u.check_now();
    assert_eq!(u.status_line(at(19, 0)), "Version 0.2.34. Checking now…");
    settle(&mut u);
    assert_eq!(net.checks.load(Ordering::SeqCst), 1);
    assert_eq!(u.status_line(at(19, 0)), "Version 0.2.34. Last checked today, 6:13 PM: mythicshot.com could not be reached or did not answer as expected.");
    *net.answer.lock().unwrap() = Some(Ok(Checked::UpToDate));
    u.check_now();
    settle(&mut u);
    assert_eq!(u.status_line(at(19, 0)), "Version 0.2.34. Last checked today, 6:13 PM: up to date.");
}

// ---- The line in the main window ----

#[test]
fn a_newer_version_is_offered_once_and_later_hides_it_until_the_next_start() {
    let net = Arc::new(FakeNet::default());
    let mut u = offered(&net);
    assert_eq!(text(&u).as_deref(), Some("Mythicshot 0.2.35 is available. You have 0.2.34."));
    assert_eq!(buttons(&u), [Button::WhatsNew, Button::Later, Button::UpdateNow]);
    assert_eq!(u.tray_label().as_deref(), Some("Update to 0.2.35…"));
    u.press(Button::Later);
    assert_eq!(text(&u), None);
    assert_eq!(u.tray_label().as_deref(), Some("Update to 0.2.35…"), "the tray still offers it");
    // The next start offers it again.
    let again = offered(&net);
    assert_eq!(text(&again).as_deref(), Some("Mythicshot 0.2.35 is available. You have 0.2.34."));
}

#[test]
fn whats_new_and_the_download_page_are_asked_of_the_app() {
    let net = Arc::new(FakeNet::default());
    let mut u = offered(&net);
    u.press(Button::WhatsNew);
    assert_eq!(std::mem::take(&mut u.asks), [Ask::Open(crate::update::CHANGES_PAGE, "v0-2-35".into())]);
    u.press(Button::OpenDownloadPage);
    assert_eq!(std::mem::take(&mut u.asks), [Ask::Open(crate::update::DOWNLOAD_PAGE, String::new())]);
}

#[test]
fn update_now_downloads_with_progress_and_cancel_ends_it() {
    let net = Arc::new(FakeNet::default());
    let mut u = offered(&net);
    u.press(Button::UpdateNow);
    std::thread::sleep(Duration::from_millis(20));
    u.poll(at(18, 14));
    let b = u.banner().unwrap();
    assert_eq!(b.text, "Downloading Mythicshot 0.2.35… 10.0 of 40.0 MB");
    assert_eq!(b.buttons, [Button::Cancel]);
    assert!((b.progress.unwrap() - 0.25).abs() < 0.01);
    // A second press, or the tray's entry, starts no second download.
    u.press(Button::UpdateNow);
    u.tray_update();
    u.press(Button::Cancel);
    settle(&mut u);
    assert_eq!(net.downloads.load(Ordering::SeqCst), 1);
    for _ in 0..200 {
        if net.saw_cancel.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(net.saw_cancel.load(Ordering::SeqCst), "the download was told to stop");
    assert_eq!(text(&u).as_deref(), Some("Mythicshot 0.2.35 is available. You have 0.2.34."));
    assert!(u.ready().is_none());
}

#[test]
fn a_verified_download_waits_for_running_work_and_cancel_update_deletes_it() {
    let net = Arc::new(FakeNet::default());
    *net.download_end.lock().unwrap() = Some(Downloaded::Ready(PathBuf::from("C:/x/Mythicshot-0.2.35-x64.msi")));
    net.release_download.store(true, Ordering::SeqCst);
    let mut u = offered(&net);
    u.press(Button::UpdateNow);
    settle(&mut u);
    let (r, path) = u.ready().unwrap();
    assert_eq!((r.version(), path.as_path()), (v("0.2.35"), Path::new("C:/x/Mythicshot-0.2.35-x64.msi")));
    u.set_waiting(true);
    assert_eq!(text(&u).as_deref(), Some(WAITING));
    assert_eq!(buttons(&u), [Button::CancelUpdate]);
    u.set_waiting(false);
    assert_eq!(text(&u).as_deref(), Some("Checked. Mythicshot will close, install 0.2.35 and open again."));
    assert!(buttons(&u).is_empty());
    u.set_waiting(true);
    u.press(Button::CancelUpdate);
    assert_eq!(*net.discarded.lock().unwrap(), [PathBuf::from("C:/x/Mythicshot-0.2.35-x64.msi")]);
    assert!(u.ready().is_none());
    assert_eq!(text(&u).as_deref(), Some("Mythicshot 0.2.35 is available. You have 0.2.34."));
}

#[test]
fn a_download_that_does_not_match_offers_try_again_and_the_download_page() {
    let net = Arc::new(FakeNet::default());
    *net.download_end.lock().unwrap() = Some(Downloaded::Mismatch);
    net.release_download.store(true, Ordering::SeqCst);
    let mut u = offered(&net);
    u.press(Button::UpdateNow);
    settle(&mut u);
    assert_eq!(text(&u).as_deref(), Some(MISMATCH));
    assert_eq!(buttons(&u), [Button::TryAgain, Button::OpenDownloadPage]);
    assert!(u.ready().is_none());
    // Try again downloads again; this time it matches.
    *net.download_end.lock().unwrap() = Some(Downloaded::Ready(PathBuf::from("C:/x/Mythicshot-0.2.35-x64.msi")));
    u.press(Button::TryAgain);
    settle(&mut u);
    assert_eq!(net.downloads.load(Ordering::SeqCst), 2);
    assert!(u.ready().is_some());
    // A file that no longer matches when it is about to be started counts the same.
    u.install_refused();
    assert_eq!(text(&u).as_deref(), Some(MISMATCH));
    assert_eq!(net.discarded.lock().unwrap().len(), 1);
}

#[test]
fn a_download_that_breaks_off_says_so() {
    let net = Arc::new(FakeNet::default());
    *net.download_end.lock().unwrap() = Some(Downloaded::Failed("the connection broke or stalled".into()));
    net.release_download.store(true, Ordering::SeqCst);
    let mut u = offered(&net);
    u.tray_update();
    settle(&mut u);
    assert_eq!(text(&u).as_deref(), Some(DOWNLOAD_FAILED));
    assert_eq!(buttons(&u), [Button::TryAgain, Button::OpenDownloadPage]);
    assert!(net.logged.lock().unwrap().iter().any(|l| l == "download: failed (the connection broke or stalled)"));
}

#[test]
fn the_tray_entry_starts_what_update_now_starts() {
    let net = Arc::new(FakeNet::default());
    let mut u = offered(&net);
    u.press(Button::Later);
    assert!(u.tray_update(), "it starts the download");
    assert!(u.banner().is_some_and(|b| b.buttons == [Button::Cancel]), "the line shows the download, Later or not");
    u.press(Button::Cancel);
    settle(&mut u);
    let net = Arc::new(FakeNet::default());
    let mut none = updater(&net);
    assert!(!none.tray_update(), "nothing to update to");
}

#[test]
fn only_an_installer_that_waits_for_running_work_asks_to_be_woken() {
    let net = Arc::new(FakeNet::default());
    let mut u = offered(&net);
    assert_eq!(u.wake_after(), None, "an offer alone does not");
    *net.download_end.lock().unwrap() = Some(Downloaded::Ready(PathBuf::from("C:/x/Mythicshot-0.2.35-x64.msi")));
    net.release_download.store(true, Ordering::SeqCst);
    u.press(Button::UpdateNow);
    settle(&mut u);
    u.set_waiting(true);
    assert_eq!(u.wake_after(), Some(Duration::from_secs(1)), "about once a second while it waits");
    u.set_waiting(false);
    assert_eq!(u.wake_after(), None, "nothing to wait for: the app closes for it");
    u.set_waiting(true);
    u.press(Button::CancelUpdate);
    assert_eq!(u.wake_after(), None, "called off");
}

// ---- Drawing ----

fn texts_of(out: &egui::FullOutput) -> Vec<String> {
    fn collect(shape: &egui::Shape, into: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => into.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
            _ => {}
        }
    }
    let mut texts = Vec::new();
    for s in &out.shapes {
        collect(&s.shape, &mut texts);
    }
    texts
}

#[test]
fn the_line_and_the_section_draw_their_sentences() {
    let net = Arc::new(FakeNet::default());
    let mut u = offered(&net);
    let ctx = crate::theme::test_context();
    let mut settings = crate::settings::Settings::default();
    let mut changed = false;
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        banner_ui(ui, &mut u);
        changed |= prefs_ui(ui, &mut u, &mut settings, at(19, 0));
    });
    out.textures_delta.clear();
    let texts = texts_of(&out).join(" | ");
    for want in ["Mythicshot 0.2.35 is available. You have 0.2.34.", WHATS_NEW, "Later", "Update now", SECTION, CHECKBOX, HELPER, CHECK_NOW] {
        assert!(texts.contains(want), "{want} in {texts}");
    }
    assert!(!changed);
}

/// An updater whose line shows `state` ("available", "downloading", "checked", "waiting" or
/// "mismatch"), driven there through its own flow against the stand-in network, for pictures.
/// The network is handed back too: a download in progress watches it.
pub(crate) fn showing(state: &str) -> (Updater, Arc<FakeNetHandle>) {
    let net = Arc::new(FakeNet::default());
    let mut u = offered(&net);
    match state {
        "available" => {}
        "downloading" => u.press(Button::UpdateNow),
        "checked" | "waiting" | "mismatch" => {
            *net.download_end.lock().unwrap() = Some(if state == "mismatch" { Downloaded::Mismatch } else { Downloaded::Ready(std::env::temp_dir().join("mythicshot-render-installer.exe")) });
            net.release_download.store(true, Ordering::SeqCst);
            u.press(Button::UpdateNow);
            settle(&mut u);
            u.set_waiting(state == "waiting");
        }
        other => panic!("no such state: {other}"),
    }
    (u, Arc::new(FakeNetHandle(net)))
}

/// Keeps the stand-in network of [`showing`] alive.
pub(crate) struct FakeNetHandle(#[allow(dead_code)] Arc<FakeNet>);

// ---- After an update: the popup at the next start ----

use crate::update::pending::AfterUpdate;

fn updated() -> AfterUpdate {
    AfterUpdate::Updated { from: v("0.2.38"), to: v("0.2.39") }
}

fn not_finished() -> AfterUpdate {
    AfterUpdate::NotFinished { from: v("0.2.38") }
}

#[test]
fn the_popups_words_are_the_owners() {
    assert_eq!(UPDATED_TITLE, "Mythicshot is up to date");
    assert_eq!(updated_text(v("0.2.38"), v("0.2.39")), "Updated from 0.2.38 to 0.2.39.");
    assert_eq!(NOT_FINISHED_TITLE, "The update did not finish");
    assert_eq!(not_finished_text(v("0.2.38")), "Mythicshot is still 0.2.38.");
    assert_eq!(NoticeButton::WhatsNew.label(), "What's new");
    assert_eq!(NoticeButton::Ok.label(), "OK");
    assert_eq!(update::CHANGES_PAGE, "https://mythicshot.com/changes/");
    // Each popup and its buttons, OK last (the primary).
    assert_eq!(notice_view(updated()), (UPDATED_TITLE, "Updated from 0.2.38 to 0.2.39.".to_string(), &[NoticeButton::WhatsNew, NoticeButton::Ok][..]));
    assert_eq!(notice_view(not_finished()), (NOT_FINISHED_TITLE, "Mythicshot is still 0.2.38.".to_string(), &[NoticeButton::Ok][..]));
}

#[test]
fn there_is_no_popup_until_the_start_hands_one_in() {
    let net = Arc::new(FakeNet::default());
    let mut u = updater(&net);
    assert_eq!(u.notice(), None);
    u.set_notice(updated(), false);
    assert_eq!(u.notice(), Some(updated()));
}

#[test]
fn whats_new_opens_the_changes_page_and_closes_the_popup() {
    let net = Arc::new(FakeNet::default());
    let mut u = updater(&net);
    u.set_notice(updated(), false);
    assert!(!u.press_notice(NoticeButton::WhatsNew), "the window was open already: it stays");
    assert_eq!(u.asks, vec![Ask::Open(update::CHANGES_PAGE, String::new())]);
    assert_eq!(u.notice(), None);
}

#[test]
fn ok_closes_the_popup_and_opens_nothing() {
    let net = Arc::new(FakeNet::default());
    for after in [updated(), not_finished()] {
        let mut u = updater(&net);
        u.set_notice(after, false);
        assert!(!u.press_notice(NoticeButton::Ok));
        assert_eq!(u.notice(), None);
        assert!(u.asks.is_empty());
    }
}

#[test]
fn a_window_opened_only_for_the_popup_closes_with_it() {
    let net = Arc::new(FakeNet::default());
    for button in [NoticeButton::Ok, NoticeButton::WhatsNew] {
        let mut u = updater(&net);
        u.set_notice(updated(), true);
        assert!(u.press_notice(button), "{button:?}");
        // Once: nothing is left to close a window for.
        assert!(!u.press_notice(button));
    }
    // A window closed by hand takes the popup with it, so it never shows twice.
    let mut u = updater(&net);
    u.set_notice(not_finished(), true);
    u.dismiss_notice();
    assert_eq!(u.notice(), None);
    assert!(u.asks.is_empty());
}

/// Draw the popup over an empty window `frames` times with `events` in the last frame.
fn notice_frames(u: &mut Updater, events: Vec<egui::Event>) -> (egui::FullOutput, bool) {
    let ctx = crate::theme::test_context();
    let input = |events| egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 520.0))), events, ..Default::default() };
    let mut close = false;
    for _ in 0..2 {
        let mut out = ctx.run_ui(input(vec![]), |ui| close |= notice_ui(ui.ctx(), u));
        out.textures_delta.clear();
    }
    let mut out = ctx.run_ui(input(events), |ui| close |= notice_ui(ui.ctx(), u));
    out.textures_delta.clear();
    (out, close)
}

#[test]
fn each_popup_shows_its_title_its_line_and_its_buttons() {
    let net = Arc::new(FakeNet::default());
    let mut u = updater(&net);
    u.set_notice(updated(), false);
    let (out, _) = notice_frames(&mut u, vec![]);
    let texts = texts_of(&out);
    for want in [UPDATED_TITLE, "Updated from 0.2.38 to 0.2.39.", "What's new", "OK"] {
        assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
    }
    let mut u = updater(&net);
    u.set_notice(not_finished(), false);
    let (out, _) = notice_frames(&mut u, vec![]);
    let texts = texts_of(&out);
    for want in [NOT_FINISHED_TITLE, "Mythicshot is still 0.2.38.", "OK"] {
        assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
    }
    assert!(!texts.iter().any(|t| t == "What's new"), "no other button: {texts:?}");
    // Without a popup, nothing is drawn.
    let mut u = updater(&net);
    let (out, close) = notice_frames(&mut u, vec![]);
    assert!(texts_of(&out).is_empty() && !close);
}

#[test]
fn enter_or_escape_is_ok() {
    let net = Arc::new(FakeNet::default());
    for key in [egui::Key::Enter, egui::Key::Escape] {
        let mut u = updater(&net);
        u.set_notice(updated(), true);
        let press = egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let (_, close) = notice_frames(&mut u, vec![press]);
        assert!(close, "{key:?}");
        assert_eq!(u.notice(), None, "{key:?}");
        assert!(u.asks.is_empty(), "{key:?}");
    }
}
