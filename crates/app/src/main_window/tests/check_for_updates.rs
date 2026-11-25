//! Help › Check for updates in the main window: it shows Preferences at the Updates section and
//! starts the check that section's Check now starts, by mouse and by keyboard, one check at a time,
//! whatever the check-at-start setting says. Every check goes to the stand-in network of
//! `update_ui`'s tests; nothing is asked of any real site.

use eframe::egui;

use super::*;
use crate::menu_bar::tests::{choose, click_at, last_text_rect, open_menu};
use crate::share_ui::tests::{focused_rect, text_rect};
use crate::update::Checked;
use crate::update_ui::tests::{FakeNetHandle, on_fake_net, settle_for_test};

const ENTRY: &str = "Check for updates";

/// The window on `page`, with Preferences last left on the Hotkeys tab, the check at start
/// switched off (and the start done: no check), and an updater whose checks answer `answer`.
fn window(dir: &std::path::Path, page: Page, answer: Option<Checked>) -> (Shared, FakeNetHandle) {
    let mut s = shared(dir);
    s.prefs.settings.check_for_updates = false;
    s.prefs.state.tab = settings_ui::Tab::Hotkeys;
    let (mut u, net) = on_fake_net(answer);
    u.start(s.prefs.settings.check_for_updates);
    settle_for_test(&mut u);
    assert_eq!(net.checks(), 0, "with the setting off, no check at start");
    s.update = u;
    s.nav.request(page);
    s.nav.take_focus();
    (s, net)
}

fn size() -> egui::Vec2 {
    egui::vec2(SIZE[0], SIZE[1])
}

/// Where the Updates section's status line is drawn in `out`, if it is.
fn status_at(s: &Shared, out: &egui::FullOutput) -> Option<egui::Rect> {
    text_rect(out, &s.update.status_line(chrono::Local::now().naive_local()))
}

fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
}

/// Ends on Preferences › General (the tab with the Updates section), with the section's status
/// line on screen saying a check is running, and one check asked of the site.
fn assert_ends_on_the_section_with_one_check(ctx: &egui::Context, s: &mut Shared, net: &FakeNetHandle, how: &str) {
    assert_eq!((s.nav.page(), s.prefs.state.tab), (Page::Preferences, settings_ui::Tab::General), "{how}");
    assert!(s.nav.is_open() && s.app_requests.is_empty(), "{how}: the window does it itself");
    let out = window_frame(ctx, s, size(), vec![]);
    let status = s.update.status_line(chrono::Local::now().naive_local());
    assert!(status.ends_with("Checking now…"), "{how}: {status}");
    let at = status_at(s, &out).unwrap_or_else(|| panic!("{how}: the status line is drawn"));
    assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size()).contains_rect(at), "{how}: the status line {at:?} is in view");
    assert!(text_rect(&out, crate::update_ui::SECTION).is_some(), "{how}: the Updates section");
    settle_for_test(&mut s.update);
    assert_eq!(net.checks(), 1, "{how}: one check");
    let status = s.update.status_line(chrono::Local::now().naive_local());
    assert!(status.ends_with(": up to date."), "{how}: {status}");
}

#[test]
fn choosing_check_for_updates_with_the_mouse_shows_the_section_and_checks_once() {
    let tmp = tempfile::tempdir().unwrap();
    for page in [Page::Home, Page::Tours, Page::Preferences] {
        let ctx = crate::theme::test_context();
        let (mut s, net) = window(tmp.path(), page, Some(Checked::UpToDate));
        choose(&mut |events| window_frame(&ctx, &mut s, size(), events), "Help", ENTRY);
        assert_ends_on_the_section_with_one_check(&ctx, &mut s, &net, &format!("mouse from {page:?}"));
        assert!(!s.prefs.settings.check_for_updates, "the setting is left as it was");
    }
}

/// In the smallest window the Updates section is below General's bottom edge: the entry scrolls
/// the tab so the section's status line and Check now are in view, at every scale; once, so the
/// tab can be scrolled back by hand.
#[test]
fn the_status_line_is_scrolled_into_view_at_the_smallest_window() {
    let tmp = tempfile::tempdir().unwrap();
    let small = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]);
    for ppp in [1.0, 1.25, 2.0] {
        let ctx = crate::theme::test_context();
        ctx.set_pixels_per_point(ppp);
        let (mut s, _net) = window(tmp.path(), Page::Home, Some(Checked::UpToDate));
        choose(&mut |events| window_frame(&ctx, &mut s, small, events), "Help", ENTRY);
        let out = window_frame(&ctx, &mut s, small, vec![]);
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, small);
        for what in [s.update.status_line(chrono::Local::now().naive_local()), crate::update_ui::CHECK_NOW.to_string()] {
            let at = text_rect(&out, &what).unwrap_or_else(|| panic!("{what} at {ppp}"));
            assert!(window.contains_rect(at), "{what} at {ppp}: {at:?} in {window:?}");
        }
        assert!(!s.prefs.state.reveal_updates, "scrolled once");
        settle_for_test(&mut s.update);
    }
}

/// The menu bar is reached with Tab and opened with Enter or Space; the arrow keys move inside the
/// open menu, and Enter chooses.
#[test]
fn choosing_check_for_updates_with_the_keyboard_shows_the_section_and_checks_once() {
    let tmp = tempfile::tempdir().unwrap();
    for open_with in [egui::Key::Enter, egui::Key::Space] {
        let ctx = crate::theme::test_context();
        let (mut s, net) = window(tmp.path(), Page::Home, Some(Checked::UpToDate));
        let mut frame = |events| window_frame(&ctx, &mut s, size(), events);
        let first = frame(vec![]);
        let help = text_rect(&first, "Help").expect("the Help title");
        let mut on_help = false;
        for _ in 0..80 {
            frame(vec![key(egui::Key::Tab)]);
            frame(vec![]);
            if focused_rect(&ctx).is_some_and(|r| r.contains(help.center())) {
                on_help = true;
                break;
            }
        }
        assert!(on_help, "Tab reaches Help");
        frame(vec![key(open_with)]);
        let open = frame(vec![]);
        let entry = last_text_rect(&open, ENTRY).unwrap_or_else(|| panic!("{open_with:?} opens Help"));
        let mut on_entry = false;
        for _ in 0..10 {
            frame(vec![key(egui::Key::ArrowDown)]);
            // A frame more, so the focus has settled on its widget before it is read.
            frame(vec![]);
            if focused_rect(&ctx).is_some_and(|r| r.contains(entry.center())) {
                on_entry = true;
                break;
            }
        }
        assert!(on_entry, "the arrow keys reach {ENTRY}");
        frame(vec![key(egui::Key::Enter)]);
        frame(vec![]);
        assert_ends_on_the_section_with_one_check(&ctx, &mut s, &net, &format!("keyboard, opened with {open_with:?}"));
    }
}

/// While a check runs, Check now is greyed out; the entry still shows the section, where the
/// status line says a check is running, and starts no second one. That holds for a Check now
/// that runs, for the entry chosen twice, and for the check at start.
#[test]
fn with_a_check_running_the_entry_starts_no_second_one() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = crate::theme::test_context();
    // Chosen twice before the first check is taken in.
    let (mut s, net) = window(tmp.path(), Page::Home, Some(Checked::UpToDate));
    choose(&mut |events| window_frame(&ctx, &mut s, size(), events), "Help", ENTRY);
    let help = menu_bar::menus(&s.menu_state()).into_iter().find(|m| m.title == "Help").unwrap();
    assert!(help.items().any(|i| i.entry == Entry::CheckForUpdates && i.enabled), "the entry stays usable: it shows the running check");
    choose(&mut |events| window_frame(&ctx, &mut s, size(), events), "Help", ENTRY);
    assert_ends_on_the_section_with_one_check(&ctx, &mut s, &net, "chosen twice");
    // After Check now was pressed on the page.
    let (mut s, net) = window(tmp.path(), Page::Preferences, Some(Checked::UpToDate));
    s.prefs.state.tab = settings_ui::Tab::General;
    let out = window_frame(&ctx, &mut s, size(), vec![]);
    let button = text_rect(&out, crate::update_ui::CHECK_NOW).expect("Check now");
    click_at(&mut |events| window_frame(&ctx, &mut s, size(), events), button.center());
    choose(&mut |events| window_frame(&ctx, &mut s, size(), events), "Help", ENTRY);
    assert_ends_on_the_section_with_one_check(&ctx, &mut s, &net, "after Check now");
    // While the check at start runs (the setting on).
    let mut s = shared(tmp.path());
    let (mut u, net) = on_fake_net(Some(Checked::UpToDate));
    u.start(true);
    s.update = u;
    s.nav.request(Page::Home);
    choose(&mut |events| window_frame(&ctx, &mut s, size(), events), "Help", ENTRY);
    assert_eq!((s.nav.page(), s.prefs.state.tab), (Page::Preferences, settings_ui::Tab::General));
    settle_for_test(&mut s.update);
    assert_eq!(net.checks(), 1, "only the check at start");
}

/// A check from the entry that fails is noted exactly as a Check now that fails: the same status
/// line and the same log line.
#[test]
fn a_failing_check_from_the_entry_is_reported_as_a_failing_check_now() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = crate::theme::test_context();
    let (mut by_menu, menu_net) = window(tmp.path(), Page::Home, None);
    choose(&mut |events| window_frame(&ctx, &mut by_menu, size(), events), "Help", ENTRY);
    settle_for_test(&mut by_menu.update);
    let (mut by_button, button_net) = window(tmp.path(), Page::Preferences, None);
    by_button.prefs.state.tab = settings_ui::Tab::General;
    let out = window_frame(&ctx, &mut by_button, size(), vec![]);
    let button = text_rect(&out, crate::update_ui::CHECK_NOW).expect("Check now");
    click_at(&mut |events| window_frame(&ctx, &mut by_button, size(), events), button.center());
    settle_for_test(&mut by_button.update);
    assert_eq!((menu_net.checks(), button_net.checks()), (1, 1));
    let now = chrono::Local::now().naive_local();
    let line = by_menu.update.status_line(now);
    assert_eq!(line, by_button.update.status_line(now));
    assert!(line.ends_with(": mythicshot.com could not be reached or did not answer as expected."), "{line}");
    assert_eq!(menu_net.logged(), button_net.logged());
    assert!(menu_net.logged().iter().any(|l| l == "check now: failed (no answer)"), "{:?}", menu_net.logged());
    // And it is on the page, where Check now's result is.
    let out = window_frame(&ctx, &mut by_menu, size(), vec![]);
    assert!(status_at(&by_menu, &out).is_some(), "the failure is shown in the section");
}

/// A newer version found by the entry shows the same line and controls as one found by Check now.
#[test]
fn a_newer_version_found_by_the_entry_shows_the_usual_line() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = crate::theme::test_context();
    let newer = || Checked::Newer(crate::update::Release::for_test(crate::update::Version::parse("9.0.0").unwrap(), 1024, "0".repeat(64)));
    let (mut by_menu, _a) = window(tmp.path(), Page::Home, Some(newer()));
    choose(&mut |events| window_frame(&ctx, &mut by_menu, size(), events), "Help", ENTRY);
    settle_for_test(&mut by_menu.update);
    let (mut by_hand, _b) = window(tmp.path(), Page::Home, Some(newer()));
    by_hand.update.check_now();
    settle_for_test(&mut by_hand.update);
    assert!(by_menu.update.banner().is_some());
    assert_eq!(by_menu.update.banner(), by_hand.update.banner());
    assert_eq!(by_menu.update.tray_label(), by_hand.update.tray_label());
}

/// Help open in the main window at 1x, for the owner. Run with
/// `MYTHICSHOT_SNAPSHOTS=<absolute folder> cargo test -p mythicshot help_menu_renders -- --ignored`.
#[test]
#[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
fn help_menu_renders() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = crate::theme::test_context();
    let (mut s, _net) = window(tmp.path(), Page::Home, Some(Checked::UpToDate));
    let mut painter = crate::snapshot::Painter::default();
    let mut draw = |events: Vec<egui::Event>| {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size())), events, time: Some(ctx.input(|i| i.time) + 0.5), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| window_ui(ui, &mut s));
        painter.update(&mut out.textures_delta);
        out
    };
    let out = open_menu(&mut draw, "Help");
    assert!(last_text_rect(&out, ENTRY).is_some(), "Help is open");
    let shot = crate::snapshot::shot_with(&ctx, painter, size(), 1.0, 2, |ui| window_ui(ui, &mut s));
    assert_eq!(crate::snapshot::save(&shot, "help-menu-check-for-updates-1x"), 0);
}
