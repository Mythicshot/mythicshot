//! The roomy sizes on the main window's pages: the Workspace page in its main states, at 100%
//! and 200% and at the window's smallest size, with a profile and a stand-in site of the test's
//! own. Nothing here reads the settings file or the credential store, and no host but the
//! in-process stub is asked anything.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use super::super::*;
use crate::profiles;
use crate::profiles::holds::Holds;
use crate::profiles::{Book, Ending, Profile, State};
use crate::profiles_ui::{AddFlow, Change, Done, Panel, Shelf};
use crate::publish::ending::Outcome;
use crate::publish::tests::{Stub, reply, stub};
use crate::settings::Settings;
use crate::share_ui::tests::{shapes_of, text_rect};

const TOKEN: &str = "dp_0123456789abcdef0123456789abcdef01234567";
const ACCOUNT: &str = "alex@mythicshot.test";

/// Profiles in memory: nothing is stored, switched or ended anywhere.
struct MemShelf(Mutex<Book>);

impl Shelf for MemShelf {
    fn book(&self) -> Book {
        self.0.lock().unwrap().clone()
    }
    fn change(&self, _: Change) -> Done {
        Done::Failed("not in a test".into())
    }
    fn switch(&self, id: &str) -> bool {
        self.0.lock().unwrap().switch(id)
    }
    fn end(&self, _: &Ending) -> Outcome {
        Outcome::NotReached("not in a test".into())
    }
    fn holds(&self) -> Holds {
        Holds::default()
    }
    fn held(&self, _: &Ending) -> bool {
        true
    }
}

/// The states of the Workspace page that are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// "Sandbox test", signed in, the site answering.
    Connected,
    /// The same without an account named by the site.
    NoAccount,
    /// The site refused the token: "Sign in again".
    SignInAgain,
    /// No profile at all: the add form.
    Add,
    /// The list with Remove.
    Manage,
    /// Profiles, none of them active.
    NoActive,
    /// Profiles, and an active id that is none of theirs.
    GoneActive,
}

fn site() -> Stub {
    stub(|req, _| match req.line.as_str() {
        "GET /api/v1/me" => reply(200, r#"{"workspace":"Sandbox test","version":"0.4.2"}"#),
        "GET /api/v1/tours" => reply(200, r#"{"tours":[{"id":"tour_a","title":"A","status":"published","slug":"abc","url":"/t/abc","chapters":2,"steps":22,"updated_at":1,"published_at":2}]}"#),
        _ => reply(404, r#"{"error":"not_found"}"#),
    })
}

struct Fixture {
    s: Shared,
    given: WorkspaceGiven,
    _site: Stub,
    _dir: tempfile::TempDir,
}

fn fixture(kind: Kind) -> Fixture {
    let site = site();
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::default();
    settings.output_dir = dir.path().to_path_buf();
    let mut s = Shared::new(settings_ui::Shared { settings, errors: Default::default(), snipping_tool: None, autostart: false, state: Default::default(), actions: Vec::new() });
    let profile = |id: &str, name: &str, account: Option<&str>| Profile { id: id.into(), name: name.into(), server: site.base.clone(), account: account.map(str::to_string), workspace: Some("Sandbox test".into()), identity: None, state: State::Ready, named: true };
    let book = match kind {
        Kind::Add => Book::default(),
        Kind::NoAccount => Book { list: vec![profile("pa", "Sandbox test", None)], active: Some("pa".into()), legacy: None },
        Kind::NoActive => Book { list: vec![profile("pa", "Sandbox test", Some(ACCOUNT)), profile("pb", "Customer demo", None)], active: None, legacy: None },
        Kind::GoneActive => Book { list: vec![profile("pa", "Sandbox test", Some(ACCOUNT)), profile("pb", "Customer demo", None)], active: Some("gone".into()), legacy: None },
        _ => Book { list: vec![profile("pa", "Sandbox test", Some(ACCOUNT)), profile("pb", "Customer demo", None)], active: Some("pa".into()), legacy: None },
    };
    let active = book.active().cloned();
    let shelf: Arc<dyn Shelf> = Arc::new(MemShelf(Mutex::new(book)));
    let make_shelf = shelf.clone();
    let make = Arc::new(move |purpose, address| AddFlow::new(purpose, address, make_shelf.clone(), Arc::new(|_: &str| Ok(())), Arc::new(crate::publish::connect::RealClock::new())));
    let mut panel = Panel::with(shelf, make);
    let token = if kind == Kind::Add { TokenStatus::Missing } else { TokenStatus::Saved };
    let refused = kind == Kind::SignInAgain;
    let state = token.clone();
    panel.token_state = Arc::new(move || (state.clone(), refused));
    if kind == Kind::Manage {
        panel.mode = crate::profiles_ui::Mode::Manage;
    }
    s.profiles = panel;
    s.workspace = WorkspaceState::with_token(|_| Ok(TOKEN.to_string()));
    let conn = workspace_ui::Conn { token, epoch: 1, test_server: None, bound: Some(site.base.clone()), refused, profile: Some("pa".into()) };
    s.nav.request(Page::Workspace);
    s.nav.take_focus();
    Fixture { s, given: (active, conn), _site: site, _dir: dir }
}

fn frame(ctx: &egui::Context, f: &mut Fixture, size: egui::Vec2) -> egui::FullOutput {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), time: Some(ctx.input(|i| i.time) + 0.5), ..Default::default() };
    let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
    let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, Some(&f.given)));
    out.textures_delta.clear();
    out
}

fn texts(out: &egui::FullOutput) -> Vec<String> {
    shapes_of(out).into_iter().filter_map(|s| if let egui::Shape::Text(t) = s { Some(t.galley.text().to_string()) } else { None }).collect()
}

/// Draw `kind` at `size` and `ppp` until the page has settled (the site answered when it is asked).
fn settled(kind: Kind, size: egui::Vec2, ppp: f32) -> (egui::Context, Fixture, egui::FullOutput) {
    let ctx = crate::theme::test_context();
    ctx.set_pixels_per_point(ppp);
    let mut f = fixture(kind);
    let deadline = Instant::now() + Duration::from_secs(20);
    // Drawn tall first, so what the page would scroll is drawn too and can be waited for.
    let tall = egui::vec2(size.x, 1400.0);
    loop {
        let out = frame(&ctx, &mut f, tall);
        let t = texts(&out);
        let done = match kind {
            Kind::Connected | Kind::NoAccount => t.iter().any(|t| t == workspace_ui::CHECKED_NOW),
            _ => t.len() > 8,
        };
        if done {
            frame(&ctx, &mut f, size);
            let out = frame(&ctx, &mut f, size);
            return (ctx, f, out);
        }
        assert!(Instant::now() < deadline, "{kind:?} did not settle: {t:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The smallest filled or outlined rectangle around the middle of `text`.
fn holder(out: &egui::FullOutput, text: &str) -> egui::Rect {
    let at = text_rect(out, text).unwrap_or_else(|| panic!("{text:?} was not drawn")).center();
    shapes_of(out)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) if r.rect.contains(at) && (r.fill.a() > 0 || r.stroke.width > 0.0) && r.rect.height() < 100.0 => Some(r.rect),
            _ => None,
        })
        .min_by(|a, b| a.area().total_cmp(&b.area()))
        .unwrap_or_else(|| panic!("nothing holds {text:?}"))
}

/// Where `text` was first drawn on the page, right of the left menu.
fn on_page(out: &egui::FullOutput, text: &str) -> egui::Rect {
    shapes_of(out)
        .into_iter()
        .find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == text && t.pos.x > NAV_WIDTH => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{text:?} was not drawn on the page"))
}

fn top(out: &egui::FullOutput, text: &str) -> f32 {
    on_page(out, text).top()
}

/// Nothing runs off the window's sides: every text in sight lies between its left and right
/// edges (what a page scrolls is cut at the bottom, and is checked by its clip).
fn assert_inside_across(out: &egui::FullOutput, window: egui::Rect, what: &str) {
    for c in &out.shapes {
        let mut texts = Vec::new();
        fn collect<'a>(s: &'a egui::Shape, into: &mut Vec<&'a egui::epaint::TextShape>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                egui::Shape::Text(t) => into.push(t),
                _ => {}
            }
        }
        collect(&c.shape, &mut texts);
        for t in texts {
            let r = t.visual_bounding_rect();
            if !c.clip_rect.intersects(r) || t.galley.text().trim().is_empty() {
                continue;
            }
            assert!(r.left() >= window.left() - 0.5 && r.right() <= window.right() + 0.5, "{what}: {:?} at {r:?} runs off the window {window:?}", t.galley.text());
            assert!(c.clip_rect.top() >= window.top() - 0.5 && c.clip_rect.intersect(r).bottom() <= window.bottom() + 0.5, "{what}: {:?} at {r:?} shows below the window", t.galley.text());
        }
    }
}

const SIZES: [(&str, [f32; 2]); 2] = [("default", SIZE), ("smallest", MIN_SIZE)];

/// The approved Workspace page, top to bottom, with every control 36 points high and every row
/// at least 44: the title and Add profile, the profile card, Connection, Tours, and the last line
/// with the note at the left and Sign out at the right, below everything else.
#[test]
fn the_workspace_page_is_laid_out_as_approved_on_the_roomy_sizes() {
    // Tall enough that nothing scrolls, so the order can be read off.
    let (_ctx, _f, out) = settled(Kind::Connected, egui::vec2(SIZE[0], 1100.0), 1.0);
    let t = texts(&out);
    let signed_in = format!("Signed in as {ACCOUNT}");
    let order = ["Workspace", "Sandbox test", signed_in.as_str(), workspace_ui::CONNECTION, "Site", "Account", workspace_ui::THIS_COMPUTER, "Site version", "Tours", workspace_ui::ON_THIS_SITE, workspace_ui::TOKEN_NOTE];
    let tops: Vec<f32> = order.iter().map(|w| top(&out, w)).collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "out of order: {:?}", order.iter().zip(&tops).collect::<Vec<_>>());
    for word in [profiles::ADD_PROFILE_BUTTON, profiles::SWITCH_PROFILE, workspace_ui::CONNECTED, workspace_ui::OPEN_SITE, workspace_ui::TEST_CONNECTION, workspace_ui::CHECKED_NOW, "1 public, 0 drafts", workspace_ui::OPEN_TOURS, workspace_ui::SIGN_OUT, "0.4.2"] {
        assert!(t.iter().any(|x| x == word), "{word:?} missing from {t:?}");
    }
    // The workspace name shows in the card and in its row; the site label in the Site row.
    assert!(t.iter().filter(|x| *x == "Sandbox test").count() >= 2, "{t:?}");
    // No corner dropdown, no inline link: the old "+ Add profile…" lives only in the list.
    assert!(!t.iter().any(|x| x == profiles::ADD_PROFILE), "{t:?}");
    // Add profile at the right of the title, on its line.
    let (title, add) = (on_page(&out, "Workspace"), holder(&out, profiles::ADD_PROFILE_BUTTON));
    assert!(add.left() > title.right() && (add.center().y - title.center().y).abs() < 2.0, "{title:?} {add:?}");
    // The icons: the plus before "Add profile", the chevron after "Switch profile", and each
    // menu item's icon before its label.
    let (add_text, add_box) = (text_rect(&out, profiles::ADD_PROFILE_BUTTON).unwrap(), holder(&out, profiles::ADD_PROFILE_BUTTON));
    assert!((add_text.left() - add_box.left() - (theme::CONTROL_PAD_X + theme::BUTTON_ICON_SIZE + theme::BUTTON_ICON_GAP)).abs() < 1.0, "{add_text:?} {add_box:?}");
    let (switch_text, switch_box) = (text_rect(&out, profiles::SWITCH_PROFILE).unwrap(), holder(&out, profiles::SWITCH_PROFILE));
    assert!((switch_text.left() - switch_box.left() - theme::CONTROL_PAD_X).abs() < 1.0 && switch_box.right() - switch_text.right() >= theme::CONTROL_PAD_X + theme::BUTTON_ICON_SIZE, "{switch_text:?} {switch_box:?}");
    for page in Page::ALL {
        let label = text_rect(&out, page.label()).unwrap();
        assert!(label.left() >= theme::NAV_PAD_X + theme::NAV_ICON_SIZE + theme::NAV_ICON_GAP - 0.5, "{page:?}: {label:?}");
    }
    // Every control is 36 points high, with radius-md corners.
    for b in [profiles::ADD_PROFILE_BUTTON, profiles::SWITCH_PROFILE, workspace_ui::OPEN_SITE, workspace_ui::TEST_CONNECTION, workspace_ui::OPEN_TOURS, workspace_ui::SIGN_OUT] {
        let r = holder(&out, b);
        assert!((r.height() - theme::CONTROL_HEIGHT).abs() < 0.6, "{b}: {r:?}");
    }
    // Rows: at least 44 points apart, label column at the same x.
    let labels = ["Site", "Account", "Workspace", workspace_ui::THIS_COMPUTER, "Site version"];
    let rects: Vec<egui::Rect> = labels.iter().map(|l| if *l == "Workspace" { shapes_of(&out).into_iter().filter_map(|s| match s { egui::Shape::Text(t) if t.galley.text() == "Workspace" && t.pos.x > NAV_WIDTH => Some(t.galley.rect.translate(t.pos.to_vec2())), _ => None }).nth(1).unwrap() } else { on_page(&out, l) }).collect();
    for w in rects.windows(2) {
        assert!(w[1].center().y - w[0].center().y >= theme::ROW_HEIGHT - 0.5, "{rects:?}");
        assert!((w[1].left() - w[0].left()).abs() < 0.5, "{rects:?}");
    }
    // The value column starts after the label column.
    assert!(text_rect(&out, ACCOUNT).unwrap().left() >= rects[0].left() + theme::LABEL_WIDTH - 0.5);
    // Sign out is last, at the right; the note is on its line, at the left.
    let sign_out = holder(&out, workspace_ui::SIGN_OUT);
    let note = text_rect(&out, workspace_ui::TOKEN_NOTE).unwrap();
    assert!((sign_out.center().y - note.center().y).abs() < 2.0 && note.right() < sign_out.left(), "{note:?} {sign_out:?}");
    let lowest = shapes_of(&out).into_iter().filter_map(|s| match s {
        egui::Shape::Text(t) if t.pos.x > NAV_WIDTH => Some(t.visual_bounding_rect().bottom()),
        _ => None,
    }).fold(0.0, f32::max);
    assert!(sign_out.bottom() + 0.5 >= lowest, "Sign out {sign_out:?} is not the last thing on the page ({lowest})");
    // The danger outline: no fill, a danger-border edge.
    let outline = shapes_of(&out).into_iter().find_map(|s| match s {
        egui::Shape::Rect(r) if r.rect == sign_out => Some((r.fill, r.stroke.color)),
        _ => None,
    });
    assert_eq!(outline.map(|(_, c)| c), Some(theme::DANGER_BORDER));
}

/// "Signed in as" names the account the site gave, and is left out when it gave none.
#[test]
fn the_profile_card_names_the_account_only_when_the_site_did() {
    let (_ctx, _f, out) = settled(Kind::Connected, egui::vec2(SIZE[0], 1100.0), 1.0);
    assert!(texts(&out).contains(&format!("Signed in as {ACCOUNT}")));
    let (_ctx, _f, out) = settled(Kind::NoAccount, egui::vec2(SIZE[0], 1100.0), 1.0);
    let t = texts(&out);
    assert!(!t.iter().any(|x| x.starts_with("Signed in as")) && !t.iter().any(|x| x == "Account"), "{t:?}");
    // The pill: the other states keep their words, in the warning tone.
    assert_eq!(workspace_ui::pill_for(&TokenStatus::Saved, false, true), (workspace_ui::CONNECTED, theme::Tone::Success));
    assert_eq!(workspace_ui::pill_for(&TokenStatus::Saved, false, false), (workspace_ui::NOT_CONNECTED, theme::Tone::Warning));
    assert_eq!(workspace_ui::pill_for(&TokenStatus::Missing, false, false), (workspace_ui::NOT_CONNECTED, theme::Tone::Warning));
    assert_eq!(workspace_ui::pill_for(&TokenStatus::Checking, false, false).0, workspace_ui::CHECKING);
    let (_ctx, _f, out) = settled(Kind::SignInAgain, egui::vec2(SIZE[0], 1100.0), 1.0);
    let t = texts(&out);
    assert!(t.iter().any(|x| x == profiles::SIGN_IN_AGAIN) && !t.iter().any(|x| x == workspace_ui::CONNECTED), "{t:?}");
}

/// Switch profile opens the list the corner dropdown had, "Manage profiles…" in it.
#[test]
fn switch_profile_opens_the_profile_list_with_manage_profiles() {
    let size = egui::vec2(SIZE[0], SIZE[1]);
    let (ctx, mut f, out) = settled(Kind::Connected, size, 1.0);
    let at = holder(&out, profiles::SWITCH_PROFILE).center();
    let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let mut last = None;
    for events in [vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![], vec![]] {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default() };
        let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
        let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, Some(&f.given)));
        out.textures_delta.clear();
        last = Some(out);
    }
    let t = texts(&last.unwrap());
    for word in [profiles::ADD_PROFILE, profiles::MANAGE_PROFILES, "Customer demo", profiles::ACTIVE] {
        assert!(t.iter().any(|x| x == word), "{word:?} not in the list: {t:?}");
    }
}

/// Nothing sticks out, overlaps or lets the desktop through, in every state, at 100% and 200%
/// and at the window's smallest size; the controls stay 36 points high and the left menu's items 36.
#[test]
fn every_workspace_state_fits_the_smallest_window_at_every_scale() {
    for kind in [Kind::Connected, Kind::NoAccount, Kind::SignInAgain, Kind::Add, Kind::Manage, Kind::NoActive, Kind::GoneActive] {
        for (name, size) in SIZES {
            for ppp in [1.0, 2.0] {
                let what = format!("{kind:?} {name} at {ppp}");
                let size = egui::vec2(size[0], size[1]);
                let (_ctx, mut f, out) = settled(kind, size, ppp);
                let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                theme::checks::assert_window_opaque(&out, window, &what);
                assert!(theme::checks::assert_texts_fit(&out, &what) >= 4, "{what}");
                theme::checks::assert_no_overlaps(&out, &what);
                assert_inside_across(&out, window, &what);
                // The menu's Workspace item (the page title is not in a box).
                let nav = shapes_of(&out).into_iter().find_map(|s| match s {
                    egui::Shape::Rect(r) if r.fill == theme::SELECTION && r.rect.right() <= NAV_WIDTH => Some(r.rect),
                    _ => None,
                });
                let nav = nav.unwrap_or_else(|| panic!("{what}: the selected menu item"));
                assert!((nav.height() - theme::NAV_ITEM_HEIGHT).abs() < 0.6, "{what}: {nav:?}");
                for b in [workspace_ui::TEST_CONNECTION, profiles::SWITCH_PROFILE, profiles::SIGN_IN, profiles::REMOVE_PROFILE] {
                    if let Some(r) = text_rect(&out, b)
                        && window.contains(r.center())
                    {
                        let h = holder(&out, b);
                        assert!((h.height() - theme::CONTROL_HEIGHT).abs() < 0.6, "{what}: {b} {h:?}");
                    }
                }
                let shot = crate::snapshot::shot(&crate::theme::test_context(), size, ppp, 2, |ui| {
                    let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
                    window_ui_given(ui, &mut f.s, &tours, Some(&f.given));
                });
                assert_eq!(shot.uncovered, 0, "{what}: pixels the desktop shows through");
            }
        }
    }
}

/// Every page of the window fits its smallest size at 100% and 200%: no text sticks out of its
/// box or runs over another, and buttons on the pages are 36 points high.
#[test]
fn every_page_fits_the_smallest_window_on_the_roomy_sizes() {
    let size = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    let mut pages: Vec<(Page, Option<settings_ui::Tab>)> = vec![(Page::Home, None), (Page::Tours, None)];
    pages.extend(settings_ui::Tab::ALL.map(|t| (Page::Preferences, Some(t))));
    for (page, tab) in pages {
        for ppp in [1.0, 2.0] {
            let what = format!("{page:?} {tab:?} at {ppp}");
            let ctx = crate::theme::test_context();
            ctx.set_pixels_per_point(ppp);
            let mut f = fixture(Kind::Add);
            if page == Page::Tours {
                f.s.tours = crate::tours_ui::tests::page_with(Vec::new());
            }
            f.s.nav.request(page);
            if let Some(tab) = tab {
                f.s.prefs.state.tab = tab;
            }
            let tours = crate::tours_ui::tests::conn(TokenStatus::Saved);
            let mut draw = |events: Vec<egui::Event>| {
                let input = egui::RawInput { screen_rect: Some(window), events, ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, None));
                out.textures_delta.clear();
                out
            };
            draw(vec![]);
            let out = draw(vec![]);
            theme::checks::assert_window_opaque(&out, window, &what);
            theme::checks::assert_texts_fit(&out, &what);
            theme::checks::assert_no_overlaps(&out, &what);
            assert_inside_across(&out, window, &what);
        }
    }
}

/// Pictures of the pages on the roomy sizes, for the owner: each main state at 100% and 200%,
/// at the window's usual and smallest sizes. Run with
/// `MYTHICSHOT_SNAPSHOTS=target-roomy/renders cargo test -p mythicshot roomy_renders -- --ignored`.
#[test]
#[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
fn roomy_renders() {
    let tours_conn = crate::tours_ui::tests::conn(TokenStatus::Saved);
    for (name, size) in SIZES {
        for ppp in [1.0f32, 2.0] {
            let size = egui::vec2(size[0], size[1]);
            let suffix = format!("{name}-{}x", ppp);
            for (kind, label) in [(Kind::Connected, "workspace-connected"), (Kind::NoAccount, "workspace-no-account"), (Kind::SignInAgain, "workspace-sign-in-again"), (Kind::Add, "workspace-add-profile"), (Kind::Manage, "workspace-manage")] {
                let (_ctx, mut f, _) = settled(kind, size, ppp);
                let shot = crate::snapshot::shot(&crate::theme::test_context(), size, ppp, 3, |ui| {
                    let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
                    window_ui_given(ui, &mut f.s, &tours, Some(&f.given));
                });
                assert_eq!(crate::snapshot::save(&shot, &format!("{label}-{suffix}")), 0, "{label}");
            }
            let mut pages: Vec<(String, Page, Option<settings_ui::Tab>)> = vec![("home".into(), Page::Home, None), ("tours".into(), Page::Tours, None)];
            pages.extend(settings_ui::Tab::ALL.map(|t| (format!("preferences-{}", t.label().to_lowercase()), Page::Preferences, Some(t))));
            for (label, page, tab) in pages {
                let mut f = fixture(Kind::Add);
                if page == Page::Tours {
                    f.s.tours = crate::tours_ui::tests::page_with(Vec::new());
                }
                f.s.nav.request(page);
                if let Some(tab) = tab {
                    f.s.prefs.state.tab = tab;
                }
                let shot = crate::snapshot::shot(&crate::theme::test_context(), size, ppp, 4, |ui| window_ui_given(ui, &mut f.s, &tours_conn, None));
                assert_eq!(crate::snapshot::save(&shot, &format!("{label}-{suffix}")), 0, "{label}");
            }
        }
    }
}

/// The rectangles keyboard focus visits, in order, pressing Tab `steps` times on what `draw` draws.
fn focus_walk(ctx: &egui::Context, steps: usize, draw: &mut dyn FnMut(Vec<egui::Event>) -> egui::FullOutput) -> Vec<egui::Rect> {
    let tab = egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    let mut seen = Vec::new();
    for _ in 0..steps {
        draw(vec![tab.clone()]);
        draw(vec![]);
        if let Some(r) = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id)) {
            seen.push(r.rect);
        }
    }
    seen
}

/// Focus reaches the control around `a` before the control around `b`.
fn assert_before(seen: &[egui::Rect], a: egui::Pos2, b: egui::Pos2, what: &str) {
    let at = |p: egui::Pos2| seen.iter().position(|r| r.contains(p)).unwrap_or_else(|| panic!("{what}: focus never reached {p:?} in {seen:?}"));
    assert!(at(a) < at(b), "{what}: Tab reaches {b:?} before {a:?}: {seen:?}");
}

/// Tab goes from a field to its button, as they are seen: the add form's address and Sign in,
/// the pasted token and Test connection, Captures folder and Browse…, File name pattern and
/// Default, and a shortcut's keys and its Reset.
#[test]
fn tab_goes_from_each_field_to_its_button() {
    let size = egui::vec2(SIZE[0], 2400.0);
    // The add form, with an address and a pasted token.
    let ctx = crate::theme::test_context();
    let mut f = fixture(Kind::Add);
    frame(&ctx, &mut f, size);
    let base = f._site.base.clone();
    if let crate::profiles_ui::Mode::Add(flow) = &mut f.s.profiles.mode {
        flow.address = base.clone();
        flow.begin_paste();
        flow.type_paste_for_test(TOKEN);
    }
    frame(&ctx, &mut f, size);
    let out = frame(&ctx, &mut f, size);
    let address = text_rect(&out, &base).expect("the address").center();
    let sign_in = holder(&out, profiles::SIGN_IN).center();
    let check = holder(&out, workspace_ui::TEST_CONNECTION);
    let paste = check.left_center() - egui::vec2(30.0, 0.0);
    let seen = focus_walk(&ctx, 30, &mut |events| {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default() };
        let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
        let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, Some(&f.given)));
        out.textures_delta.clear();
        out
    });
    assert_before(&seen, address, sign_in, "the address");
    assert_before(&seen, paste, check.center(), "the pasted token");

    // Preferences: Output, then Hotkeys.
    for tab in [settings_ui::Tab::Output, settings_ui::Tab::Hotkeys] {
        let ctx = crate::theme::test_context();
        let mut f = fixture(Kind::Add);
        f.s.nav.request(Page::Preferences);
        f.s.prefs.state.tab = tab;
        let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
        let mut draw = |events: Vec<egui::Event>| {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, None));
            out.textures_delta.clear();
            out
        };
        draw(vec![]);
        let out = draw(vec![]);
        let pairs: Vec<(egui::Pos2, egui::Pos2, &str)> = match tab {
            settings_ui::Tab::Output => {
                let folder = shapes_of(&out).into_iter().find_map(|s| match s {
                    egui::Shape::Text(t) if t.galley.text().contains("mythicshot") || t.galley.text().contains(std::path::MAIN_SEPARATOR) => Some(t.visual_bounding_rect().center()),
                    _ => None,
                });
                vec![(folder.expect("the captures folder"), holder(&out, "Browse…").center(), "Captures folder"), (text_rect(&out, mythicshot_destinations::DEFAULT_PATTERN).expect("the pattern").center(), holder(&out, "Default").center(), "File name pattern")]
            }
            _ => {
                let keys = holder(&out, "PrtSc");
                let reset = shapes_of(&out)
                    .into_iter()
                    .filter_map(|s| match s {
                        egui::Shape::Text(t) if t.galley.text() == "Reset" => Some(t.visual_bounding_rect().center()),
                        _ => None,
                    })
                    .find(|c| (c.y - keys.center().y).abs() < 2.0)
                    .expect("the row's Reset");
                vec![(keys.center(), reset, "a shortcut")]
            }
        };
        let seen = focus_walk(&ctx, 60, &mut draw);
        for (a, b, what) in pairs {
            assert_before(&seen, a, b, what);
        }
    }
}

/// With profiles but none active (or an active id that is none of theirs), the card is still
/// there: "Not connected" in its pill, and Switch profile opens the list with Manage profiles.
#[test]
fn the_profile_card_stays_when_no_profile_is_active() {
    for kind in [Kind::NoActive, Kind::GoneActive] {
        let size = egui::vec2(SIZE[0], SIZE[1]);
        let (ctx, mut f, out) = settled(kind, size, 1.0);
        let t = texts(&out);
        // In the card's pill, not only at the foot of the left menu.
        let pill = on_page(&out, workspace_ui::NOT_CONNECTED);
        assert!((pill.center().y - holder(&out, profiles::SWITCH_PROFILE).center().y).abs() < 2.0, "{kind:?}: {t:?}");
        let at = holder(&out, profiles::SWITCH_PROFILE).center();
        let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut last = None;
        for events in [vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![], vec![]] {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default() };
            let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
            let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, Some(&f.given)));
            out.textures_delta.clear();
            last = Some(out);
        }
        let t = texts(&last.unwrap());
        for word in [profiles::MANAGE_PROFILES, profiles::ADD_PROFILE, "Sandbox test", "Customer demo"] {
            assert!(t.iter().any(|x| x == word), "{kind:?}: {word:?} not in the list: {t:?}");
        }
    }
}

/// The foot of the left menu says this computer holds a saved connection, "Signed in to" the
/// host, without claiming the site answered; with none, "Not connected" as before.
#[test]
fn the_left_menu_says_signed_in_to_the_host_of_a_saved_connection() {
    assert_eq!(super::super::nav_footer(Some("tours.example.com")), vec!["Signed in to".to_string(), "tours.example.com".to_string()]);
    assert_eq!(super::super::nav_footer(None), vec![workspace_ui::NOT_CONNECTED.to_string()]);
    let (_ctx, f, out) = settled(Kind::Connected, egui::vec2(SIZE[0], SIZE[1]), 1.0);
    let t = texts(&out);
    let host = crate::publish::host_of(&f._site.base).unwrap();
    let (label, shown) = (text_rect(&out, "Signed in to").expect("Signed in to"), text_rect(&out, &host).expect("the host"));
    assert!(label.right() <= NAV_WIDTH && shown.right() <= NAV_WIDTH && label.bottom() <= shown.top() + 0.5, "{t:?}");
    assert!(!t.iter().any(|x| x == "Connected to"), "{t:?}");
}

/// The Site row keeps a site's path; a name alone stands level with the badge; the tray
/// choices wrap under the first choice; and a row of one line has as much space above as below.
#[test]
fn the_smaller_findings_hold() {
    assert_eq!(profiles::site_address_label("https://tours.example.com/demo/"), "tours.example.com/demo");
    assert_eq!(profiles::site_address_label("https://Tours.Example.com"), "tours.example.com");
    // The name alone, level with the badge's middle.
    let (_ctx, _f, out) = settled(Kind::NoAccount, egui::vec2(SIZE[0], 1100.0), 1.0);
    let name = on_page(&out, "Sandbox test");
    let badge = text_rect(&out, "ST").unwrap();
    assert!((name.center().y - badge.center().y).abs() < 1.5, "{name:?} {badge:?}");
    // The sign-in-again state's one-line row in the Connection card: equal space above and below.
    let (_ctx, _f, out) = settled(Kind::SignInAgain, egui::vec2(SIZE[0], 1100.0), 1.0);
    let line = text_rect(&out, crate::publish::REJECTED_TOKEN).unwrap();
    let hairlines: Vec<f32> = shapes_of(&out)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::LineSegment { points, stroke } if stroke.color == theme::BORDER && (points[0].y - points[1].y).abs() < 0.1 => Some(points[0].y),
            _ => None,
        })
        .collect();
    let above = hairlines.iter().copied().filter(|y| *y <= line.top()).fold(f32::MIN, f32::max);
    let below = hairlines.iter().copied().filter(|y| *y >= line.bottom()).fold(f32::MAX, f32::min);
    assert!(((line.top() - above) - (below - line.bottom())).abs() < 1.5, "{above} {line:?} {below}");
    // General at the smallest window: the second choice wraps under the first.
    for ppp in [1.0, 2.0] {
        let ctx = crate::theme::test_context();
        ctx.set_pixels_per_point(ppp);
        let mut f = fixture(Kind::Add);
        f.s.nav.request(Page::Preferences);
        f.s.prefs.state.tab = settings_ui::Tab::General;
        let size = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]);
        frame(&ctx, &mut f, size);
        let out = frame(&ctx, &mut f, size);
        let (words, first, second) = (text_rect(&out, "Left-click on the tray icon").unwrap(), text_rect(&out, "Open Mythicshot").unwrap(), text_rect(&out, "Capture a region").unwrap());
        assert!(first.left() > words.right(), "{words:?} {first:?}");
        if second.top() > first.bottom() {
            assert!((second.left() - first.left()).abs() < 0.6, "wrapped under the first choice: {first:?} {second:?}");
        }
    }
}
