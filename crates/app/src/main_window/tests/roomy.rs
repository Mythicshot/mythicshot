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
const ACCOUNT: &str = "robin@mythicshot.test";

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
    /// Every texture the fixture's frames handed out, so a shot of its context shows its letters.
    painter: crate::snapshot::Painter,
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
    Fixture { s, given: (active, conn), _site: site, _dir: dir, painter: crate::snapshot::Painter::default() }
}

fn frame(ctx: &egui::Context, f: &mut Fixture, size: egui::Vec2) -> egui::FullOutput {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), time: Some(ctx.input(|i| i.time) + 0.5), ..Default::default() };
    let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
    let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, Some(&f.given)));
    f.painter.update(&mut out.textures_delta);
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

/// The five Home tiles at the usual and the smallest window, at 100% and 200%: each tile lies
/// inside the page beside the menu, no two overlap, each name and caption sits inside its own
/// tile, and at the smallest window they stand in two rows, the Open image tile on the second.
#[test]
fn the_five_tiles_fit_without_overlap_on_every_size_and_scale() {
    for (name, size) in SIZES {
        for ppp in [1.0, 2.0] {
            let what = format!("{name} at {ppp}");
            let ctx = crate::theme::test_context();
            ctx.set_pixels_per_point(ppp);
            let mut f = fixture(Kind::Add);
            f.s.nav.request(Page::Home);
            let size = egui::vec2(size[0], size[1]);
            frame(&ctx, &mut f, size);
            let out = frame(&ctx, &mut f, size);
            let tiles: Vec<egui::Rect> = shapes_of(&out)
                .into_iter()
                .filter_map(|s| match s {
                    egui::Shape::Rect(r) if (r.rect.height() - home_ui::TILE_HEIGHT).abs() < 0.5 && r.stroke.color == theme::BORDER_STRONG => Some(r.rect),
                    _ => None,
                })
                .collect();
            assert_eq!(tiles.len(), 5, "{what}: {tiles:?}");
            for (i, a) in tiles.iter().enumerate() {
                assert!(a.left() >= NAV_WIDTH + theme::PAGE_PAD - 0.5 && a.right() <= size.x - theme::PAGE_PAD + 0.5 && a.bottom() <= size.y, "{what}: tile {i} at {a:?}");
                assert!(a.width() >= home_ui::TILE_MIN_WIDTH - 0.5, "{what}: tile {i} at {a:?}");
                for b in &tiles[i + 1..] {
                    assert!(!a.intersects(*b), "{what}: {a:?} overlaps {b:?}");
                }
            }
            for (t, rect) in home_ui::TILES.iter().zip(&tiles) {
                let words = text_rect(&out, t.name).unwrap_or_else(|| panic!("{what}: {}", t.name));
                assert!(rect.contains_rect(words), "{what}: {} at {words:?} outside {rect:?}", t.name);
            }
            let caption = text_rect(&out, home_ui::OR_DROP_A_FILE).unwrap();
            assert!(tiles[4].contains_rect(caption), "{what}: {caption:?} outside {:?}", tiles[4]);
            let rows = if name == "smallest" { 2 } else { 1 };
            let mut tops: Vec<i32> = tiles.iter().map(|r| r.top().round() as i32).collect();
            tops.dedup();
            assert_eq!(tops.len(), rows, "{what}: {tiles:?}");
            assert!(theme::checks::assert_texts_fit(&out, &what) >= 4);
            theme::checks::assert_no_overlaps(&out, &what);
            assert_inside_across(&out, egui::Rect::from_min_size(egui::Pos2::ZERO, size), &what);
        }
    }
}

/// A clock that moves only when slept on, so a sign-in in a picture never waits.
#[derive(Default)]
struct FastClock(Mutex<Duration>);

impl crate::publish::connect::Clock for FastClock {
    fn now(&self) -> Duration {
        *self.0.lock().unwrap()
    }
    fn sleep(&self, d: Duration) {
        *self.0.lock().unwrap() += d;
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The add form at its dead end: the browser signed in as the account of a profile that is
/// there already. Signed in against a stand-in site of its own.
fn dead_end_fixture() -> (Fixture, Stub) {
    const WS: &str = "ws_0123456789abcdef01234567";
    let s = stub(|req, _| match req.line.as_str() {
        "POST /api/v1/connect/start" => reply(201, r#"{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"K7QM-2HXD","verify_url":"/connections","expires_in":600,"interval":5}"#),
        "POST /api/v1/connect/token" => reply(200, r#"{"status":"connected","token":"dp_c0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ff","workspace":{"name":"Sandbox test"},"account":{"username":"robin@mythicshot.test"}}"#),
        "GET /api/v1/me" => reply(200, &format!(r#"{{"workspace":"Sandbox test","workspace_id":"{WS}","account_id":"acc-a"}}"#)),
        _ => reply(404, r#"{"error":"not_found"}"#),
    });
    let mut f = fixture(Kind::Connected);
    let pa = Profile { id: "pa".into(), name: "Sandbox test".into(), server: s.base.clone(), account: Some(ACCOUNT.into()), workspace: Some("Sandbox test".into()), identity: Some(crate::profiles::tests::ided(WS, "acc-a")), state: State::Ready, named: true };
    let shelf: Arc<dyn Shelf> = Arc::new(MemShelf(Mutex::new(Book { list: vec![pa.clone()], active: Some("pa".into()), legacy: None })));
    let mut flow = AddFlow::new(crate::profiles_ui::Purpose::New, s.base.clone(), shelf.clone(), Arc::new(|_: &str| Ok(())), Arc::new(FastClock::default()));
    flow.begin_sign_in();
    let deadline = Instant::now() + Duration::from_secs(20);
    while flow.is_signing_in() {
        assert!(Instant::now() < deadline, "the sign-in did not end");
        flow.poll_sign_in();
        std::thread::sleep(Duration::from_millis(5));
    }
    let make_shelf = shelf.clone();
    let mut panel = Panel::with(shelf, Arc::new(move |purpose, address| AddFlow::new(purpose, address, make_shelf.clone(), Arc::new(|_: &str| Ok(())), Arc::new(FastClock::default()))));
    panel.token_state = Arc::new(|| (TokenStatus::Saved, false));
    panel.mode = crate::profiles_ui::Mode::Add(Box::new(flow));
    f.s.profiles = panel;
    f.given.0 = Some(pa);
    (f, s)
}

/// Pictures of the pages on the roomy sizes, for the owner: each main state at 100% and 200%,
/// at the window's usual and smallest sizes. Run with
/// `MYTHICSHOT_SNAPSHOTS=<absolute folder> cargo test -p mythicshot roomy_renders -- --ignored`.
#[test]
#[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
fn roomy_renders() {
    let tours_conn = crate::tours_ui::tests::conn(TokenStatus::Saved);
    let tour_book = || {
        let p = |id: &str, name: &str| Profile { id: id.into(), name: name.into(), server: "https://tours.example.com".into(), account: Some(ACCOUNT.into()), workspace: Some("Sandbox test".into()), identity: None, state: State::Ready, named: true };
        Book { list: vec![p("pa", "Sandbox test"), p("pb", "Customer demo")], active: Some("pa".into()), legacy: None }
    };
    for (name, size) in SIZES {
        for ppp in [1.0f32, 2.0] {
            let size = egui::vec2(size[0], size[1]);
            let suffix = format!("{name}-{}x", ppp);
            let save = |f: &mut Fixture, label: &str, given: bool, tours: &crate::tours_ui::Conn| {
                let shot = crate::snapshot::shot(&crate::theme::test_context(), size, ppp, 3, |ui| {
                    let ws = given.then(|| f.given.clone());
                    window_ui_given(ui, &mut f.s, tours, ws.as_ref());
                });
                assert_eq!(crate::snapshot::save(&shot, &format!("{label}-{suffix}")), 0, "{label}");
            };
            let missing = crate::tours_ui::tests::conn(TokenStatus::Missing);
            for (kind, label) in [(Kind::Connected, "workspace-connected"), (Kind::NoAccount, "workspace-no-account"), (Kind::SignInAgain, "workspace-sign-in-again"), (Kind::Add, "workspace-add-profile"), (Kind::Manage, "workspace-manage"), (Kind::NoActive, "workspace-no-active-profile")] {
                let (_ctx, mut f, _) = settled(kind, size, ppp);
                save(&mut f, label, true, &missing);
            }
            let (mut f, _site) = dead_end_fixture();
            save(&mut f, "workspace-add-profile-dead-end", true, &missing);
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
                save(&mut f, &label, false, &tours_conn);
            }
            // The Tours page with profiles: the dropdown names the active one.
            let mut f = fixture(Kind::Add);
            f.s.tours = crate::tours_ui::tests::page_with(Vec::new());
            f.s.nav.request(Page::Tours);
            crate::publish_ui::with_test_book(tour_book(), || save(&mut f, "tours-with-profile", false, &tours_conn));
            // The drop overlay over the Home page: a picture that will open, and anything else.
            for (label, accept) in [("drop-picture", true), ("drop-refused", false)] {
                let mut f = fixture(Kind::Add);
                f.s.nav.request(Page::Home);
                f.s.drop.shown_for_render = Some(accept);
                let shot = crate::snapshot::shot(&crate::theme::test_context(), size, ppp, 3, |ui| window_ui_given(ui, &mut f.s, &tours_conn, None));
                assert_eq!(crate::snapshot::save(&shot, &format!("{label}-{suffix}")), 0, "{label}");
            }
            // The profile list open, one row under the pointer: from Switch profile on the
            // Workspace page, and from the Tours page's dropdown.
            let (ctx, mut f, out) = settled(Kind::Connected, size, ppp);
            switch_list_hovered(&ctx, &mut f, size, &out, "Customer demo");
            let painter = std::mem::take(&mut f.painter);
            let shot = crate::snapshot::shot_with(&ctx, painter, size, ppp, 2, |ui| window_ui_given(ui, &mut f.s, &missing, Some(&f.given)));
            assert_eq!(crate::snapshot::save(&shot, &format!("workspace-profile-menu-hover-{suffix}")), 0);
            let mut f = fixture(Kind::Add);
            f.s.tours = crate::tours_ui::tests::page_with(Vec::new());
            f.s.nav.request(Page::Tours);
            crate::publish_ui::with_test_book(tour_book(), || {
                let ctx = crate::theme::test_context();
                ctx.set_pixels_per_point(ppp);
                let mut painter = crate::snapshot::Painter::default();
                let mut draw = |events: Vec<egui::Event>| {
                    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, time: Some(ctx.input(|i| i.time) + 0.5), ..Default::default() };
                    let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours_conn, None));
                    painter.update(&mut out.textures_delta);
                    out
                };
                draw(vec![]);
                let at = on_page(&draw(vec![]), "Sandbox test").center();
                let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
                let mut open = None;
                for events in [vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![], vec![]] {
                    open = Some(draw(events));
                }
                let row = crate::menu_bar::tests::last_text_rect(&open.unwrap(), "Customer demo").expect("the list is open").center();
                draw(vec![egui::Event::PointerMoved(row + egui::vec2(120.0, 0.0))]);
                draw(vec![]);
                let shot = crate::snapshot::shot_with(&ctx, painter, size, ppp, 2, |ui| window_ui_given(ui, &mut f.s, &tours_conn, None));
                assert_eq!(crate::snapshot::save(&shot, &format!("tours-profile-menu-hover-{suffix}")), 0);
            });
            // The update line, in each of its states, over the Home page.
            for state in ["available", "downloading", "checked", "waiting", "mismatch"] {
                let mut f = fixture(Kind::Add);
                f.s.nav.request(Page::Home);
                let (u, _net) = crate::update_ui::tests::showing(state);
                f.s.update = u;
                save(&mut f, &format!("update-line-{state}"), false, &tours_conn);
            }
        }
    }
}

/// Pictures of the popup at the start after an in-app update, over the window opened for it:
/// the update worked, and it did not finish, at 100% and 200%, at the usual and smallest sizes.
/// Run with `MYTHICSHOT_SNAPSHOTS=<absolute folder> cargo test -p mythicshot roomy_renders_update_done -- --ignored`.
#[test]
#[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
fn roomy_renders_update_done() {
    use crate::update::pending::AfterUpdate;
    let v = |s: &str| crate::update::Version::parse(s).unwrap();
    let tours_conn = crate::tours_ui::tests::conn(TokenStatus::Saved);
    for (name, size) in SIZES {
        for ppp in [1.0f32, 2.0] {
            let size = egui::vec2(size[0], size[1]);
            for (label, after) in [("update-done-success", AfterUpdate::Updated { from: v("0.2.38"), to: v("0.2.39") }), ("update-done-not-finished", AfterUpdate::NotFinished { from: v("0.2.38") })] {
                let mut f = fixture(Kind::Add);
                // As at the relaunch: the window closed until the popup opens it.
                f.s.nav.close();
                let (request, opened) = notice_request(None);
                f.s.nav.apply(request);
                f.s.update.set_notice(after, opened);
                let shot = crate::snapshot::shot(&crate::theme::test_context(), size, ppp, 3, |ui| window_ui_given(ui, &mut f.s, &tours_conn, None));
                assert_eq!(crate::snapshot::save(&shot, &format!("{label}-{name}-{ppp}x")), 0, "{label}");
                assert!(f.s.update.notice().is_some(), "still up: nothing was pressed");
            }
        }
    }
}

/// The popup fits the window at its usual and smallest sizes at every scale: its words fit their
/// places, nothing overlaps, nothing leaves the window, and each line of its text stays one line
/// (the did-not-finish popup's two sentences are two lines, never three).
#[test]
fn the_update_popup_fits_the_smallest_window_at_every_scale() {
    use crate::update::pending::AfterUpdate;
    let v = |s: &str| crate::update::Version::parse(s).unwrap();
    let tours_conn = crate::tours_ui::tests::conn(TokenStatus::Saved);
    for (name, size) in SIZES {
        let size = egui::vec2(size[0], size[1]);
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        for ppp in [1.0, 1.25, 2.0] {
            for after in [AfterUpdate::Updated { from: v("0.2.38"), to: v("0.2.39") }, AfterUpdate::NotFinished { from: v("0.2.38") }] {
                let what = format!("{after:?} at {name} {ppp}");
                let ctx = crate::theme::test_context();
                ctx.set_pixels_per_point(ppp);
                let mut f = fixture(Kind::Add);
                f.s.nav.close();
                f.s.nav.apply(notice_request(None).0);
                assert_eq!(f.s.nav.page(), Page::Home, "{what}");
                f.s.update.set_notice(after, true);
                let mut last = None;
                for _ in 0..3 {
                    let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(window), ..Default::default() }, |ui| window_ui_given(ui, &mut f.s, &tours_conn, None));
                    out.textures_delta.clear();
                    last = Some(out);
                }
                let out = last.unwrap();
                // The popup's own shapes, from its frame on.
                let title = text_rect(&out, crate::update_ui::notice_view(after).0).unwrap_or_else(|| panic!("{what}: the title"));
                // The nearest filled rectangle under the title, drawn before it: the popup's frame.
                let at_title = out.shapes.iter().rposition(|c| matches!(&c.shape, egui::Shape::Text(t) if t.galley.text() == crate::update_ui::notice_view(after).0)).unwrap();
                let frame = out.shapes[..at_title].iter().rposition(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.fill.a() > 0 && r.rect.contains(title.center()) && r.rect.width() > title.width() + theme::DIALOG_PAD)).unwrap_or_else(|| panic!("{what}: the popup's frame"));
                let egui::Shape::Rect(box_) = &out.shapes[frame].shape else { unreachable!() };
                assert!((box_.rect.width() - (theme::DIALOG_WIDTH + 2.0 * theme::DIALOG_PAD)).abs() <= 2.5, "{what}: a dialog's width: {:?}", box_.rect);
                let popup = egui::FullOutput { shapes: out.shapes[frame..].to_vec(), ..Default::default() };
                theme::checks::assert_no_overlaps(&popup, &what);
                assert!(theme::checks::assert_texts_fit(&popup, &what) >= 3, "{what}");
                assert_inside_across(&popup, window, &what);
                // The line under the title: as many rows as it has lines, so nothing wrapped.
                let body = crate::update_ui::notice_view(after).1;
                let rows = shapes_of(&popup).into_iter().find_map(|s| match s {
                    egui::Shape::Text(t) if t.galley.text() == body => Some(t.galley.rows.len()),
                    _ => None,
                });
                assert_eq!(rows, Some(body.lines().count()), "{what}: {body:?}");
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

/// Open the list Switch profile opens and put the pointer on the row of `hover`, near the row's
/// right end (past its words): the frame after, with the list open and that row under the pointer.
fn switch_list_hovered(ctx: &egui::Context, f: &mut Fixture, size: egui::Vec2, out: &egui::FullOutput, hover: &str) -> egui::FullOutput {
    let at = holder(out, profiles::SWITCH_PROFILE).center();
    let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let mut draw = |events: Vec<egui::Event>| {
        // Half a second apart, so the list has faded in.
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, time: Some(ctx.input(|i| i.time) + 0.5), ..Default::default() };
        let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
        let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, Some(&f.given)));
        f.painter.update(&mut out.textures_delta);
        out
    };
    let mut open = None;
    for events in [vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![], vec![]] {
        open = Some(draw(events));
    }
    let open = open.unwrap();
    let word = crate::menu_bar::tests::last_text_rect(&open, hover).unwrap_or_else(|| panic!("{hover:?} is not in the list"));
    let list = theme::checks::fills(&open).into_iter().filter(|(r, c, radius)| *c == theme::RAISED && *radius == theme::RADIUS_LG && r.contains(word.center())).min_by(|a, b| a.0.area().total_cmp(&b.0.area())).expect("the list's frame").0;
    let to = egui::pos2(list.right() - 16.0, word.center().y);
    draw(vec![egui::Event::PointerMoved(to)]);
    draw(vec![])
}

/// In the list Switch profile opens, the row under the pointer is lit across the list with the
/// hand, the active row keeps its "Active" on the selection fill, and nothing overlaps or runs
/// off the window, at the usual and the smallest size, at 100% and 200%.
#[test]
fn the_switch_profile_list_lights_the_row_under_the_pointer() {
    for (name, size) in SIZES {
        for ppp in [1.0, 2.0] {
            let what = format!("{name} at {ppp}");
            let size = egui::vec2(size[0], size[1]);
            let (ctx, mut f, out) = settled(Kind::Connected, size, ppp);
            let lit = switch_list_hovered(&ctx, &mut f, size, &out, "Customer demo");
            let name_at = crate::menu_bar::tests::last_text_rect(&lit, "Customer demo").unwrap();
            let rows: Vec<_> = theme::checks::fills(&lit).into_iter().filter(|(_, c, _)| *c == theme::MENU_HOVER).collect();
            assert_eq!(rows.len(), 1, "{what}: {rows:?}");
            let (row, _, radius) = rows[0];
            assert!(row.contains(name_at.center()) && row.width() >= theme::PROFILE_LIST_WIDTH - 0.5 && row.height() >= theme::MENU_ROW_HEIGHT && radius == theme::RADIUS_SM, "{what}: {row:?}");
            assert_eq!(lit.platform_output.cursor_icon, egui::CursorIcon::PointingHand, "{what}");
            let active = crate::menu_bar::tests::last_text_rect(&lit, profiles::ACTIVE).unwrap_or_else(|| panic!("{what}: Active"));
            assert!(theme::checks::fills(&lit).iter().any(|(r, c, _)| *c == theme::SELECTION && r.contains(active.center())), "{what}: the active row");
            crate::profiles_ui::tests::assert_active_bar(&lit, "Sandbox test", &what);
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            // The list stands over the page: its own texts are checked, from its frame on.
            let frame = lit.shapes.iter().position(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.fill == theme::RAISED && r.corner_radius.nw == theme::RADIUS_LG && r.rect.contains(name_at.center()))).expect("the list's frame");
            let list = egui::FullOutput { shapes: lit.shapes[frame..].to_vec(), ..Default::default() };
            theme::checks::assert_no_overlaps(&list, &what);
            assert!(theme::checks::assert_texts_fit(&list, &what) >= 4, "{what}");
            assert_inside_across(&list, window, &what);
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

/// The Tours page with profiles at the smallest window, at 100% and 200%: the profile dropdown
/// ends before Refresh, nothing runs over anything, and every control stays in the window.
#[test]
fn the_tours_header_never_overlaps_at_the_smallest_window() {
    let book = {
        let p = |id: &str, name: &str| Profile { id: id.into(), name: name.into(), server: "https://tours.example.com".into(), account: Some(ACCOUNT.into()), workspace: Some("Sandbox test".into()), identity: None, state: State::Ready, named: true };
        Book { list: vec![p("pa", "Sandbox test with a long profile name"), p("pb", "Customer demo")], active: Some("pa".into()), legacy: None }
    };
    for ppp in [1.0, 2.0] {
        let what = format!("Tours at {ppp}");
        let ctx = crate::theme::test_context();
        ctx.set_pixels_per_point(ppp);
        let mut f = fixture(Kind::Add);
        f.s.tours = crate::tours_ui::tests::page_with(Vec::new());
        f.s.nav.request(Page::Tours);
        let size = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]);
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let tours = crate::tours_ui::tests::conn(TokenStatus::Saved);
        let out = crate::publish_ui::with_test_book(book.clone(), || {
            let mut last = None;
            for _ in 0..3 {
                let input = egui::RawInput { screen_rect: Some(window), ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, None));
                out.textures_delta.clear();
                last = Some(out);
            }
            last.unwrap()
        });
        let record = holder(&out, crate::tours_ui::RECORD_A_TOUR);
        let dropdown = shapes_of(&out)
            .into_iter()
            .filter_map(|s| match s {
                egui::Shape::Rect(r) if r.rect.right() < record.left() && r.rect.left() > NAV_WIDTH && (r.fill.a() > 0 || r.stroke.width > 0.0) && (r.rect.height() - theme::CONTROL_HEIGHT).abs() < 1.0 && (r.rect.center().y - record.center().y).abs() < 2.0 => Some(r.rect),
                _ => None,
            })
            .max_by(|a, b| a.width().total_cmp(&b.width()))
            .expect("the dropdown");
        let refresh_left = record.left() - theme::ITEM_GAP - theme::CONTROL_HEIGHT;
        assert!(dropdown.right() <= refresh_left + 0.5, "{what}: the dropdown {dropdown:?} runs under Refresh (from {refresh_left})");
        theme::checks::assert_no_overlaps(&out, &what);
        theme::checks::assert_texts_fit(&out, &what);
        assert_inside_across(&out, window, &what);
    }
}

/// A row keeps where its buttons stand when something appears above it: the width they took is
/// remembered by the row's key, not by its place.
#[test]
fn a_row_keeps_its_buttons_in_place_when_something_appears_above() {
    let ctx = crate::theme::test_context();
    let draw = |extra: bool| {
        let mut o = ctx.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0))), ..Default::default() }, |ui| {
            theme::page(ui, |ui| {
                if extra {
                    ui.label("A line that was not there before");
                }
                theme::card(ui, |rows| rows.row("Site", |ui| drop(theme::value(ui, "tours.example.com")), |ui| drop(ui.button("Open site"))));
            });
        });
        o.textures_delta.clear();
        o
    };
    draw(false);
    let before = holder(&draw(false), "Open site");
    let after = holder(&draw(true), "Open site");
    assert!((before.left() - after.left()).abs() < 0.5, "{before:?} then {after:?}");
}

/// The parts of a row stand on one line: Site version's value and Test connection; the Updates
/// checkbox has its helper text a line's gap under it; the tray words stand level with the first
/// choice; the add form's Site address stands level with its field; and the card with no
/// profile keeps its badge's place, empty.
#[test]
fn the_parts_of_a_row_stand_on_one_line() {
    let (_ctx, _f, out) = settled(Kind::Connected, egui::vec2(SIZE[0], 1100.0), 1.0);
    let (version, test) = (text_rect(&out, "0.4.2").unwrap(), holder(&out, workspace_ui::TEST_CONNECTION));
    assert!((version.center().y - test.center().y).abs() < 1.5, "{version:?} {test:?}");
    assert!(text_rect(&out, workspace_ui::CHECKED_NOW).unwrap().top() > version.bottom(), "the result stays under the version");
    for size in SIZES.map(|(_, s)| egui::vec2(s[0], 1100.0)) {
        let (_ctx, _f, out) = settled(Kind::Add, size, 1.0);
        let label = on_page(&out, profiles::SITE_ADDRESS);
        let field = holder(&out, "https://tours.example.com");
        assert!((label.center().y - field.center().y).abs() < 1.5, "{label:?} {field:?}");
    }
    let ctx = crate::theme::test_context();
    let mut f = fixture(Kind::Add);
    f.s.nav.request(Page::Preferences);
    f.s.prefs.state.tab = settings_ui::Tab::General;
    let size = egui::vec2(SIZE[0], 1100.0);
    frame(&ctx, &mut f, size);
    let out = frame(&ctx, &mut f, size);
    let (check, helper) = (text_rect(&out, crate::update_ui::CHECKBOX).unwrap(), text_rect(&out, crate::update_ui::HELPER).unwrap());
    assert!(helper.top() - check.bottom() <= theme::ITEM_GAP + 1.0, "{check:?} {helper:?}");
    let (words, first) = (text_rect(&out, "Left-click on the tray icon").unwrap(), text_rect(&out, "Open Mythicshot").unwrap());
    assert!((words.center().y - first.center().y).abs() < 1.5, "{words:?} {first:?}");
    let (_ctx, _f, out) = settled(Kind::NoActive, egui::vec2(SIZE[0], SIZE[1]), 1.0);
    let placeholder = shapes_of(&out).into_iter().any(|s| matches!(s, egui::Shape::Circle(c) if c.fill == theme::CHIP && (c.radius - theme::BADGE_SIZE / 2.0).abs() < 0.5));
    assert!(placeholder, "the badge's place is kept");
}


/// Where the window cannot be tall enough for the Workspace page (1080p at 200%: 437 points high),
/// the page scrolls, and scrolled to its end it shows the Sign out row whole inside the window,
/// with the page's own margin under it and nothing overlapping.
#[test]
fn the_workspace_page_scrolls_to_sign_out_where_the_window_is_too_low() {
    let ppp = 2.0;
    let size = super::super::opening::opening(None, Some(super::super::opening::tests::work(1920, 1080, ppp)), 1.0).size;
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    let (ctx, mut f, out) = settled(Kind::Connected, size, ppp);
    assert!(text_rect(&out, workspace_ui::SIGN_OUT).is_none_or(|r| !window.contains_rect(r)), "{size:?}: Sign out needs scrolling to");
    let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -200.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
    let over_page = egui::pos2(NAV_WIDTH + 200.0, size.y / 2.0);
    let tours = crate::tours_ui::tests::conn(TokenStatus::Missing);
    let mut draw = |events: Vec<egui::Event>| {
        let input = egui::RawInput { screen_rect: Some(window), events, time: Some(ctx.input(|i| i.time) + 0.5), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, Some(&f.given)));
        out.textures_delta.clear();
        out
    };
    draw(vec![egui::Event::PointerMoved(over_page)]);
    for _ in 0..10 {
        draw(vec![wheel.clone()]);
    }
    draw(vec![]);
    let out = draw(vec![]);
    let button = holder(&out, workspace_ui::SIGN_OUT);
    assert!(window.contains_rect(button), "{size:?}: {button:?}");
    assert!(size.y - button.bottom() >= theme::PAGE_PAD - 0.5, "{size:?}: {} points under {button:?}", size.y - button.bottom());
    assert!(button.contains_rect(text_rect(&out, workspace_ui::SIGN_OUT).unwrap()));
    // Its row's note too, at the left.
    let note = text_rect(&out, workspace_ui::TOKEN_NOTE).unwrap();
    assert!(window.contains_rect(note) && (note.center().y - button.center().y).abs() < 2.0, "{note:?}");
    theme::checks::assert_no_overlaps(&out, "Workspace scrolled at 1080p 200%");
    assert_inside_across(&out, window, "Workspace scrolled at 1080p 200%");
}

/// The lowest thing drawn on the page beside the menu: text, or a box, a line or a control.
fn page_bottom(out: &egui::FullOutput) -> f32 {
    let mut low = 0.0f32;
    for c in &out.shapes {
        let mut all = Vec::new();
        fn collect<'a>(s: &'a egui::Shape, into: &mut Vec<&'a egui::Shape>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                s => into.push(s),
            }
        }
        collect(&c.shape, &mut all);
        for s in all {
            match s {
                egui::Shape::Text(t) if t.pos.x > NAV_WIDTH && !t.galley.text().trim().is_empty() => low = low.max(t.visual_bounding_rect().bottom()),
                egui::Shape::Rect(r) if r.rect.left() > NAV_WIDTH && r.rect.height() < 1000.0 && (r.fill.a() > 0 || r.stroke.width > 0.0) => low = low.max(r.rect.bottom()),
                _ => {}
            }
        }
    }
    low
}

/// Draw a page of a fixture with no profile (or `page` Workspace with a connected one) at
/// `size` and `ppp`, settled.
fn page_out(page: Page, tab: Option<settings_ui::Tab>, size: egui::Vec2, ppp: f32) -> egui::FullOutput {
    if page == Page::Workspace {
        return settled(Kind::Connected, size, ppp).2;
    }
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
    let mut last = None;
    for _ in 0..4 {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), time: Some(ctx.input(|i| i.time) + 0.5), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| window_ui_given(ui, &mut f.s, &tours, None));
        out.textures_delta.clear();
        last = Some(out);
    }
    last.unwrap()
}

/// At the default size every page shows whole without scrolling (drawn far taller, nothing goes
/// below the default's bottom), except Preferences' Output and Hotkeys tabs, which hold more than
/// any window of about this size can show. The Workspace page of a connected profile keeps about
/// 150 points clear under its last row, as in the owner's picture.
#[test]
fn every_page_but_two_preferences_tabs_shows_whole_at_the_default_size() {
    let tall = egui::vec2(SIZE[0], 3000.0);
    let mut pages: Vec<(Page, Option<settings_ui::Tab>)> = vec![(Page::Home, None), (Page::Tours, None), (Page::Workspace, None)];
    pages.extend(settings_ui::Tab::ALL.map(|t| (Page::Preferences, Some(t))));
    for (page, tab) in pages {
        let bottom = page_bottom(&page_out(page, tab, tall, 1.0));
        let scrolls = matches!(tab, Some(settings_ui::Tab::Output | settings_ui::Tab::Hotkeys));
        assert_eq!(bottom > SIZE[1] - theme::PAGE_PAD, scrolls, "{page:?} {tab:?}: its content ends at {bottom}, the window at {}", SIZE[1]);
        if page == Page::Workspace {
            assert!((140.0..=170.0).contains(&(SIZE[1] - bottom)), "{} points clear under Sign out", SIZE[1] - bottom);
        }
    }
}

/// The Workspace page of a connected profile, at the size the window opens at on a 1080p and on
/// a 4K monitor, at 100%, 125%, 150% and 200%: Sign out, its last row, drawn whole inside the
/// window with space under it, and nothing overlapping. On 1080p at 200% (a work area of
/// 960×492 points) the window cannot be tall enough, and Sign out is reached by scrolling.
#[test]
fn the_workspace_page_shows_sign_out_whole_at_the_opening_size_on_1080p_and_4k() {
    for ppp in [1.0f32, 1.25, 1.5, 2.0] {
        for (name, w, h) in [("1080p", 1920u32, 1080u32), ("4K", 3840, 2160)] {
            let work = super::super::opening::tests::work(w, h, ppp);
            let size = super::super::opening::opening(None, Some(work), 1.0).size;
            let what = format!("{name} at {ppp}: {size:?}");
            let (_ctx, _f, out) = settled(Kind::Connected, size, ppp);
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            theme::checks::assert_no_overlaps(&out, &what);
            assert_inside_across(&out, window, &what);
            let sign_out = text_rect(&out, workspace_ui::SIGN_OUT).filter(|r| window.contains_rect(*r));
            if name == "1080p" && ppp == 2.0 {
                // Too low to show the page whole: see the test below, which scrolls to Sign out.
                assert!(size.y < 589.0, "{what}: too low to show the page whole");
                continue;
            }
            let sign_out = sign_out.unwrap_or_else(|| panic!("{what}: Sign out is not drawn inside the window"));
            let button = holder(&out, workspace_ui::SIGN_OUT);
            assert!(window.contains_rect(button), "{what}: {button:?}");
            assert!(size.y - button.bottom() >= theme::PAGE_PAD, "{what}: {} points under {button:?}", size.y - button.bottom());
            assert!(button.contains_rect(sign_out));
        }
    }
}

/// Pictures of every page at the window's default size at 100%, the Workspace page with a
/// connected profile, and the Workspace page at the size the window opens at on a 1080p monitor
/// at 150%, at that scale. Run with
/// `MYTHICSHOT_SNAPSHOTS=<absolute folder> cargo test -p mythicshot window_size_renders -- --ignored`.
#[test]
#[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
fn window_size_renders() {
    let default = egui::vec2(SIZE[0], SIZE[1]);
    let fitted = super::super::opening::opening(None, Some(super::super::opening::tests::work(1920, 1080, 1.5)), 1.0).size;
    let missing = crate::tours_ui::tests::conn(TokenStatus::Missing);
    for (label, size, ppp) in [("size-workspace-default-1x", default, 1.0f32), ("size-workspace-1080p-150", fitted, 1.5)] {
        let (_ctx, mut f, _) = settled(Kind::Connected, size, ppp);
        let shot = crate::snapshot::shot(&crate::theme::test_context(), size, ppp, 3, |ui| {
            let ws = f.given.clone();
            window_ui_given(ui, &mut f.s, &missing, Some(&ws));
        });
        assert_eq!(crate::snapshot::save(&shot, label), 0, "{label}");
    }
    let tours_conn = crate::tours_ui::tests::conn(TokenStatus::Saved);
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
        let shot = crate::snapshot::shot(&crate::theme::test_context(), default, 1.0, 3, |ui| window_ui_given(ui, &mut f.s, &tours_conn, None));
        assert_eq!(crate::snapshot::save(&shot, &format!("size-{label}-default-1x")), 0, "{label}");
    }
}
