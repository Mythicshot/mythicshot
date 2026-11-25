use std::time::{Duration, Instant};

use super::*;
use crate::profiles::tests::{FakeKeys, TOKEN_A, TOKEN_B, ided, named};
use crate::profiles::{MIGRATED_ID, Profile};
use crate::publish::tests::{Answer, Stub, reply, stub};
use crate::sign_in_ui::Stage;

const NEW_TOKEN: &str = "dp_c0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ff";
const CODE: &str = "K7QM-2HXD";
const ACCOUNT: &str = "customer@mythicshot.test";

/// A clock that moves only when slept on, so no test waits for an interval.
#[derive(Default)]
struct FakeClock(Mutex<Duration>);

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        *lock(&self.0)
    }
    fn sleep(&self, d: Duration) {
        *lock(&self.0) += d;
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Profiles and a credential store in memory, and the switches asked for.
#[derive(Default)]
struct TestShelf {
    inner: Mutex<(Book, FakeKeys)>,
    switched: Mutex<Vec<String>>,
    /// The sites a token was to be ended at, in order.
    ended_at: Mutex<Vec<String>>,
    /// The jobs' holds on tokens, of this shelf only.
    holds: Holds,
}

impl TestShelf {
    fn with(book: Book, keys: FakeKeys) -> Arc<TestShelf> {
        Arc::new(TestShelf { inner: Mutex::new((book, keys)), ..TestShelf::default() })
    }
    fn keys(&self) -> Vec<(String, String)> {
        lock(&self.inner).1.entries.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
    fn writes(&self) -> Vec<String> {
        lock(&self.inner).1.log.borrow().clone()
    }
}

impl Shelf for TestShelf {
    fn book(&self) -> Book {
        lock(&self.inner).0.clone()
    }
    fn change(&self, change: Change) -> Done {
        let mut g = lock(&self.inner);
        let (book, keys) = &mut *g;
        change(book, &*keys)
    }
    fn switch(&self, id: &str) -> bool {
        lock(&self.switched).push(id.to_string());
        lock(&self.inner).0.switch(id)
    }
    /// The real call, but only ever to a stand-in site on 127.0.0.1: a profile of a test that
    /// names another address sends nothing.
    fn end(&self, ending: &Ending) -> Outcome {
        lock(&self.ended_at).push(ending.server.clone());
        if ending.server.starts_with("http://127.0.0.1:") { ending.send() } else { Outcome::NotReached("not sent from a test".into()) }
    }
    fn holds(&self) -> Holds {
        self.holds.clone()
    }
    fn held(&self, ending: &Ending) -> bool {
        let g = lock(&self.inner);
        ending.held_in(&g.0, &g.1)
    }
}

/// The stand-in site: a code, `token` for each code check, and `me` for the check of the new token.
fn site(token: impl Fn(usize) -> Answer + Send + 'static, me: (u16, &'static str)) -> Stub {
    stub(move |req, seen| match req.line.as_str() {
        "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
        "POST /api/v1/connect/token" => token(seen),
        "GET /api/v1/me" => reply(me.0, me.1),
        _ => reply(404, r#"{"error":"not_found"}"#),
    })
}

fn connected(account: &str) -> Answer {
    reply(200, &format!(r#"{{"status":"connected","token":"{NEW_TOKEN}","workspace":{{"name":"customer1"}},"account":{{"username":"{account}"}}}}"#))
}

const WS_ID: &str = "ws_0123456789abcdef01234567";

/// A site of the coming version: the check names the workspace and the account by their ids.
fn ids_site(account_id: &'static str) -> Stub {
    let me: &'static str = Box::leak(format!(r#"{{"workspace":"customer1","workspace_id":"{WS_ID}","account_id":"{account_id}"}}"#).into_boxed_str());
    site(|seen| if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected(ACCOUNT) }, (200, me))
}

fn working_site() -> Stub {
    site(|seen| if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected(ACCOUNT) }, (200, r#"{"workspace":"customer1"}"#))
}

fn flow(shelf: &Arc<TestShelf>, purpose: Purpose, address: &str) -> AddFlow {
    let shelf: Arc<dyn Shelf> = shelf.clone();
    AddFlow::new(purpose, address.to_string(), shelf, Arc::new(|_: &str| Ok(())), Arc::new(FakeClock::default()))
}

/// Drive the sign-in until its window closed or says why it failed.
fn sign_in(f: &mut AddFlow) {
    f.begin_sign_in();
    assert!(f.is_signing_in(), "Sign in did not start");
    let deadline = Instant::now() + Duration::from_secs(20);
    while f.is_signing_in() && !matches!(f.sign_in_dialog().map(|d| d.stage()), Some(Stage::Failed { .. })) {
        assert!(Instant::now() < deadline, "the sign-in did not end");
        f.poll_sign_in();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait for a save or renewal to end.
fn settle(f: &mut AddFlow) {
    let deadline = Instant::now() + Duration::from_secs(10);
    f.poll();
    while f.is_working() {
        assert!(Instant::now() < deadline, "the save did not end");
        std::thread::sleep(Duration::from_millis(5));
        f.poll();
    }
}

fn profile(id: &str, name: &str, server: &str, account: Option<&str>, workspace: Option<&str>, state: State) -> Profile {
    Profile { id: id.into(), name: name.into(), server: server.into(), account: account.map(str::to_string), workspace: workspace.map(str::to_string), identity: workspace.map(named), state, named: true }
}

// ---- 2. The dropdown ----

#[test]
fn the_dropdown_lists_every_profile_with_its_state_and_the_active_one() {
    let book = Book {
        legacy: None,
        list: vec![
            profile("pa", "Alex · mythicshot.com", "https://app.mythicshot.com", None, Some("alex's workspace"), State::Ready),
            profile("pb", "Sandbox admin", "http://localhost:3000", None, Some("alex's workspace"), State::Ready),
            profile("pc", "customer1 test", "http://localhost:3000", None, Some("customer1's workspace"), State::SignInAgain),
        ],
        active: Some("pa".into()),
    };
    let rows = entries(&book, false);
    assert_eq!(
        rows,
        vec![
            Entry { id: "pa".into(), name: "Alex · mythicshot.com".into(), detail: "alex's workspace · app.mythicshot.com".into(), active: true, waiting: false },
            Entry { id: "pb".into(), name: "Sandbox admin".into(), detail: "alex's workspace · localhost:3000".into(), active: false, waiting: false },
            Entry { id: "pc".into(), name: "customer1 test".into(), detail: "customer1's workspace · localhost:3000".into(), active: false, waiting: true },
        ]
    );
    // A refusal of the active one's token shows on it at once.
    assert!(entries(&book, true)[0].waiting);
    // Without any profile it draws nothing and picks nothing.
    let ctx = crate::theme::test_context();
    let mut out = None;
    let mut full = ctx.run_ui(egui::RawInput::default(), |ui| {
        out = Some(dropdown_with(ui, "none", &[], true));
    });
    full.textures_delta.clear();
    assert_eq!(out, Some(None));
}

/// The profile list open from the dropdown in a window `size` big, a frame for each of `steps`'
/// events: the last frame and what was picked.
fn dropdown_frames(ctx: &egui::Context, rows: &[Entry], size: egui::Vec2, steps: Vec<Vec<egui::Event>>) -> (egui::FullOutput, Option<Pick>) {
    let mut last = None;
    let mut picked = None;
    for events in steps {
        // Half a second apart, so the list has faded in.
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, time: Some(ctx.input(|i| i.time) + 0.5), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            crate::theme::page(ui, |ui| {
                if let Some(p) = dropdown_with(ui, "hover", rows, true) {
                    picked = Some(p);
                }
            });
        });
        out.textures_delta.clear();
        last = Some(out);
    }
    (last.unwrap(), picked)
}

/// The active profile's row, the one holding `name`, has its bar: [`theme::ACTIVE_BAR_WIDTH`]
/// wide in primary-text at the row's left edge, inside the row's rounded shape (clear of its
/// corners), and no other row has one.
pub(crate) fn assert_active_bar(out: &egui::FullOutput, name: &str, what: &str) {
    let at = crate::menu_bar::tests::last_text_rect(out, name).unwrap_or_else(|| panic!("{what}: {name}")).center();
    let fills = crate::theme::checks::fills(out);
    let row = fills.iter().filter(|(r, c, _)| *c == theme::SELECTION && r.contains(at)).map(|(r, ..)| *r).min_by(|a, b| a.area().total_cmp(&b.area())).unwrap_or_else(|| panic!("{what}: the active row"));
    let bars: Vec<egui::Rect> = fills.iter().filter(|(r, c, _)| *c == theme::PRIMARY_TEXT && (r.width() - theme::ACTIVE_BAR_WIDTH).abs() < 0.01).map(|(r, ..)| *r).collect();
    assert_eq!(bars.len(), 1, "{what}: one bar, on the active row: {bars:?}");
    let bar = bars[0];
    let radius = f32::from(theme::RADIUS_SM);
    assert!((bar.left() - row.left()).abs() < 0.01, "{what}: at the row's left edge: {bar:?} {row:?}");
    assert!(bar.top() >= row.top() + radius - 0.01 && bar.bottom() <= row.bottom() - radius + 0.01, "{what}: clear of the rounded corners: {bar:?} {row:?}");
    assert!(bar.height() >= row.height() - 2.0 * radius - 0.01, "{what}: as high as the row allows: {bar:?} {row:?}");
}

fn press(at: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

fn hover_rows() -> Vec<Entry> {
    vec![
        Entry { id: "pa".into(), name: "Sandbox test".into(), detail: "Sandbox test · tours.example.com".into(), active: true, waiting: false },
        Entry { id: "pb".into(), name: "Customer demo".into(), detail: "Customer demo · tours.example.com".into(), active: false, waiting: false },
        Entry { id: "pc".into(), name: "customer1 test".into(), detail: "customer1's workspace · localhost:3000".into(), active: false, waiting: true },
    ]
}

/// The profile list lights the row under the pointer across the list's width, with the hand,
/// and a click anywhere on the row (not only on the name) picks it. The active row keeps its dot,
/// its "Active" and the selection fill, hovered or not; "+ Add profile…" and "Manage profiles…"
/// light up the same way. Nothing overlaps or sticks out, at 100% and 200%, in a window as small
/// as the main window's smallest.
#[test]
fn the_profile_list_lights_the_row_under_the_pointer_and_takes_a_click_anywhere_on_it() {
    let rows = hover_rows();
    for ppp in [1.0, 2.0] {
        for size in [egui::vec2(900.0, 640.0), egui::vec2(crate::main_window::MIN_SIZE[0], crate::main_window::MIN_SIZE[1])] {
            let what = format!("{size:?} at {ppp}");
            let ctx = crate::theme::test_context();
            ctx.set_pixels_per_point(ppp);
            let (out, _) = dropdown_frames(&ctx, &rows, size, vec![vec![], vec![]]);
            let button = crate::share_ui::tests::text_rect(&out, "Sandbox test").unwrap().center();
            let (open, _) = dropdown_frames(&ctx, &rows, size, vec![vec![egui::Event::PointerMoved(button)], vec![press(button, true)], vec![press(button, false)], vec![], vec![]]);
            let name = crate::menu_bar::tests::last_text_rect(&open, "Customer demo").unwrap_or_else(|| panic!("{what}: the list is not open"));
            // The pointer on the row's right end, past its words.
            let list = crate::theme::checks::fills(&open).into_iter().filter(|(r, c, _)| *c == theme::RAISED && r.contains(name.center())).min_by(|a, b| a.0.area().total_cmp(&b.0.area())).unwrap_or_else(|| panic!("the list frame around {name:?}: {:?}", crate::theme::checks::fills(&open))).0;
            let at = egui::pos2(list.right() - 12.0, name.center().y);
            let (lit, _) = dropdown_frames(&ctx, &rows, size, vec![vec![egui::Event::PointerMoved(at)], vec![], vec![]]);
            let hovered: Vec<_> = crate::theme::checks::fills(&lit).into_iter().filter(|(_, c, _)| *c == theme::MENU_HOVER).collect();
            assert_eq!(hovered.len(), 1, "{what}: one row is lit at {at:?}: {hovered:?} {:?}", crate::theme::checks::fills(&lit));
            let (row, _, radius) = hovered[0];
            let detail = crate::menu_bar::tests::last_text_rect(&lit, "Customer demo · tours.example.com").unwrap();
            assert!(row.contains(name.center()) && row.contains(detail.center()), "{what}: the whole row, name and detail, is lit: {row:?}");
            assert!(row.width() >= theme::PROFILE_LIST_WIDTH - 0.5 && row.left() >= list.left() && row.right() <= list.right(), "{what}: across the list: {row:?} in {list:?}");
            assert!(row.height() >= theme::MENU_ROW_HEIGHT, "{what}: {row:?}");
            assert_eq!(radius, theme::RADIUS_SM, "{what}");
            assert_eq!(lit.platform_output.cursor_icon, egui::CursorIcon::PointingHand, "{what}");
            // The active row: its dot and "Active", on the selection fill, not the hover's.
            let active_name = crate::menu_bar::tests::last_text_rect(&lit, "Sandbox test").unwrap();
            let active = crate::menu_bar::tests::last_text_rect(&lit, profiles::ACTIVE).unwrap_or_else(|| panic!("{what}: the active row says so"));
            let selected = crate::theme::checks::fills(&lit).into_iter().find(|(r, c, _)| *c == theme::SELECTION && r.contains(active_name.center()) && r.contains(active.center()));
            assert!(selected.is_some(), "{what}: the active row is on the selection fill");
            assert!(!row.intersects(selected.unwrap().0.shrink(1.0)), "{what}: the lit row and the active one are apart");
            assert_active_bar(&lit, "Sandbox test", &what);
            // Nothing overlaps or sticks out of the window.
            theme::checks::assert_no_overlaps(&lit, &what);
            theme::checks::assert_texts_fit(&lit, &what);
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            assert!(window.contains_rect(list), "{what}: the list {list:?} is inside the window");
            // "+ Add profile…" and "Manage profiles…" light up under the pointer too.
            for word in [profiles::ADD_PROFILE, profiles::MANAGE_PROFILES] {
                let at = crate::menu_bar::tests::last_text_rect(&lit, word).unwrap().center() + egui::vec2(60.0, 0.0);
                let (o, _) = dropdown_frames(&ctx, &rows, size, vec![vec![egui::Event::PointerMoved(at)], vec![], vec![]]);
                let w = crate::menu_bar::tests::last_text_rect(&o, word).unwrap();
                assert!(crate::theme::checks::fills(&o).iter().any(|(r, c, _)| *c == theme::MENU_HOVER && r.contains(w.center()) && r.width() >= theme::PROFILE_LIST_WIDTH - 0.5), "{what}: {word} is lit");
            }
            // A click on the lit row's right end picks that profile.
            let (_, picked) = dropdown_frames(&ctx, &rows, size, vec![vec![egui::Event::PointerMoved(at)], vec![press(at, true)], vec![press(at, false)], vec![]]);
            assert_eq!(picked, Some(Pick::Switch("pb".into())), "{what}");
        }
    }
}

/// A disabled dropdown (a job of the window runs) opens no list, so no row lights up.
#[test]
fn a_disabled_dropdown_lights_nothing() {
    let rows = hover_rows();
    let ctx = crate::theme::test_context();
    let size = egui::vec2(900.0, 640.0);
    let draw = |events: Vec<egui::Event>| {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| crate::theme::page(ui, |ui| drop(dropdown_with(ui, "off", &rows, false))));
        out.textures_delta.clear();
        out
    };
    let at = crate::share_ui::tests::text_rect(&draw(vec![]), "Sandbox test").unwrap().center();
    for events in [vec![egui::Event::PointerMoved(at)], vec![press(at, true)], vec![press(at, false)], vec![]] {
        draw(events);
    }
    let out = draw(vec![]);
    assert!(crate::menu_bar::tests::last_text_rect(&out, profiles::MANAGE_PROFILES).is_none(), "no list opens");
    assert!(crate::theme::checks::fills(&out).iter().all(|(_, c, _)| *c != theme::MENU_HOVER));
    assert_ne!(out.platform_output.cursor_icon, egui::CursorIcon::PointingHand);
}

// ---- 6. The update of another profile's tour ----

#[test]
fn an_update_from_another_profile_is_refused_with_the_three_choices() {
    let p = OwnerPrompt { owner: Owner::Other { id: "pb".into(), name: "Sandbox admin".into() }, title: "Chapter one".into() };
    assert_eq!(p.view(), ("This recording belongs to a tour in \"Sandbox admin\". Switch to that profile to update it, or upload it here as a new tour.".to_string(), vec!["Switch to Sandbox admin".to_string(), "Upload as a new tour".to_string(), "Cancel".to_string()]));
    assert_eq!((p.choice(0), p.choice(1), p.choice(2)), (OwnerChoice::Switch("pb".into()), OwnerChoice::NewTour, OwnerChoice::Cancel));
    // The profile was removed: said so, and only a new tour.
    let gone = OwnerPrompt { owner: Owner::Removed, title: "Chapter one".into() };
    assert_eq!(gone.view(), (profiles::OWNER_REMOVED.to_string(), vec!["Upload as a new tour".to_string(), "Cancel".to_string()]));
    assert_eq!((gone.choice(0), gone.choice(1)), (OwnerChoice::NewTour, OwnerChoice::Cancel));
}

// ---- 3. Adding a profile ----

#[test]
fn save_profile_waits_for_the_check_and_a_good_sign_in_is_saved_only_then() {
    let s = working_site();
    let shelf = TestShelf::with(Book::default(), FakeKeys::default());
    let mut f = flow(&shelf, Purpose::New, "");
    assert!(!f.can_sign_in() && !f.can_save(), "nothing to sign in to yet");
    for bad in ["tours.example.com", "http://tours.example.com", "https://tours.example.com/?x"] {
        f.address = bad.into();
        assert!(!f.can_sign_in(), "{bad}");
    }
    f.address = s.base.clone();
    assert!(f.can_sign_in() && !f.can_save());
    f.save();
    assert!(shelf.keys().is_empty(), "Save before the check does nothing");
    sign_in(&mut f);
    // Checked: the site named the account and the workspace for the new token.
    let Step::Checked { candidate, name } = f.step() else { panic!("not checked") };
    assert_eq!(profiles::connected(candidate.account.as_deref(), &candidate.workspace), format!("Connected. Signed in as {ACCOUNT}, workspace \"customer1\"."));
    assert_eq!(*name, format!("customer1 · {}", s.base.trim_start_matches("http://")));
    assert!(f.can_save());
    // Nothing is stored before Save, and the check was the only request with the token.
    assert!(shelf.keys().is_empty() && shelf.book().list.is_empty());
    let with_token: Vec<_> = s.requests().into_iter().filter(|r| r.auth.is_some()).collect();
    let bearer = format!("Bearer {NEW_TOKEN}");
    assert_eq!(with_token.iter().map(|r| (r.line.as_str(), r.auth.as_deref())).collect::<Vec<_>>(), vec![("GET /api/v1/me", Some(bearer.as_str()))]);
    *f.name_mut().unwrap() = "customer1 test".into();
    f.save();
    settle(&mut f);
    assert!(f.is_finished());
    let book = shelf.book();
    let p = book.active().unwrap();
    assert_eq!((p.name.as_str(), p.server.as_str(), p.account.as_deref(), p.workspace.as_deref(), p.state), ("customer1 test", s.base.as_str(), Some(ACCOUNT), Some("customer1"), State::Ready));
    assert_eq!(shelf.keys(), vec![(publish::profile_key(&p.id), NEW_TOKEN.to_string())]);
}

#[test]
fn a_failed_or_unanswered_sign_in_leaves_no_profile_and_no_token_and_says_why() {
    let not_a_site = stub(|_, _| reply(404, "<html>"));
    let turned_down = site(|_| reply(410, r#"{"error":"expired_token"}"#), (200, r#"{"workspace":"customer1"}"#));
    let refused = site(|_| connected(ACCOUNT), (401, ""));
    let not_an_answer = site(|_| connected(ACCOUNT), (200, "<html>not a site</html>"));
    let lead = profiles::NOT_SAVED_LEAD;
    let cases = [
        (not_a_site.base.clone(), format!("{lead} {}", connect::NOT_OFFERED)),
        (turned_down.base.clone(), format!("{lead} {}", connect::NOT_CONNECTED_TEXT)),
        (refused.base.clone(), profiles::NOT_SAVED.to_string()),
        (not_an_answer.base.clone(), format!("{lead} Could not check the server: the server's answer was not understood.")),
        ("http://127.0.0.1:1".to_string(), format!("{lead} Could not sign in: ")),
    ];
    for (address, said) in cases {
        let shelf = TestShelf::with(Book::default(), FakeKeys::default());
        let mut f = flow(&shelf, Purpose::New, &address);
        sign_in(&mut f);
        match f.sign_in_dialog().map(|d| d.stage()) {
            Some(Stage::Failed { text, .. }) => {
                assert!(text.starts_with(&said), "{address}: {text}");
                // Only a refusal of the new token says the site did not accept it.
                assert_eq!(text.contains(profiles::NOT_ACCEPTED), said == profiles::NOT_SAVED, "{address}: {text}");
                assert!(!text.contains("dp_"));
            }
            other => panic!("{address}: {other:?}"),
        }
        assert!(matches!(f.step(), Step::Address) && !f.can_save(), "{address}");
        assert!(shelf.book().list.is_empty() && shelf.keys().is_empty() && shelf.writes().is_empty(), "{address}: something was stored");
        // Closing it leaves the form as it was.
        f.on_sign_in(SignInAction::Closed);
        assert!(!f.is_signing_in() && !f.can_save());
        assert!(shelf.keys().is_empty());
    }
}

/// Run the check of the pasted token until it ended.
fn check_paste(f: &mut AddFlow, token: &str) {
    f.begin_paste();
    f.type_paste_for_test(token);
    assert!(f.can_check_paste());
    f.check_paste();
    let deadline = Instant::now() + Duration::from_secs(10);
    while f.is_checking_paste() {
        assert!(Instant::now() < deadline, "the check did not end");
        std::thread::sleep(Duration::from_millis(5));
        f.poll();
    }
}

fn me_stub_ok(workspace: &'static str) -> Stub {
    stub(move |req, _| if req.line == "GET /api/v1/me" { reply(200, &format!(r#"{{"workspace":"{workspace}"}}"#)) } else { reply(404, "") })
}

#[test]
fn a_pasted_token_for_a_site_without_sign_in_is_checked_before_anything_is_saved() {
    // The site has no Sign in (404), and answers only for the token from its setup page.
    let good = format!("Bearer {TOKEN_A}");
    let s = stub(move |req, _| if req.line == "GET /api/v1/me" { if req.auth.as_deref() == Some(good.as_str()) { reply(200, r#"{"workspace":"Old site"}"#) } else { reply(401, "") } } else { reply(404, "") });
    let shelf = TestShelf::with(Book::default(), FakeKeys::default());
    let mut f = flow(&shelf, Purpose::New, &s.base);
    // Not a token, a token the site refuses: said, and nothing is kept.
    check_paste(&mut f, "hello");
    assert_eq!(f.error.as_deref(), Some(format!("{} {}", profiles::NOT_SAVED_LEAD, publish::NOT_A_TOKEN).as_str()));
    check_paste(&mut f, TOKEN_B);
    assert_eq!(f.error.as_deref(), Some(profiles::NOT_SAVED));
    assert!(!f.can_save() && shelf.keys().is_empty() && shelf.writes().is_empty());
    assert!(s.requests().iter().all(|r| r.line == "GET /api/v1/me"), "nothing but the check is asked");
    // The right one: checked, offered under its name, and saved only on Save profile.
    check_paste(&mut f, TOKEN_A);
    let Step::Checked { candidate, .. } = f.step() else { panic!("not checked: {:?}", f.error) };
    assert_eq!((candidate.workspace.as_str(), candidate.account.as_deref()), ("Old site", None));
    assert!(shelf.keys().is_empty());
    f.save();
    settle(&mut f);
    assert!(f.is_finished());
    let id = shelf.book().active.clone().unwrap();
    assert_eq!(shelf.keys(), vec![(publish::profile_key(&id), TOKEN_A.to_string())]);
}

#[test]
fn a_pasted_token_renews_only_its_own_profile() {
    let s = me_stub_ok("Old site");
    let book = Book { legacy: None, list: vec![profile("po", "Old", &s.base, None, Some("Old site"), State::SignInAgain)], active: Some("po".into()) };
    let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-po", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::Renew("po".into()), &s.base);
    check_paste(&mut f, TOKEN_A);
    settle(&mut f);
    assert!(!f.is_finished() && f.can_save(), "the renewal waits for its confirmation");
    assert_eq!(shelf.keys(), vec![("profile-po".to_string(), TOKEN_B.to_string())], "nothing stored before the confirmation");
    f.save();
    settle(&mut f);
    assert!(f.is_finished());
    assert_eq!(shelf.book().get("po").unwrap().state, State::Ready);
    assert_eq!(shelf.keys(), vec![("profile-po".to_string(), TOKEN_A.to_string())]);
}

#[test]
fn cancelling_at_any_step_leaves_no_profile_and_no_token() {
    // While the code waits in the browser.
    let s = site(|_| reply(200, r#"{"status":"pending"}"#), (200, r#"{"workspace":"customer1"}"#));
    let shelf = TestShelf::with(Book::default(), FakeKeys::default());
    let mut f = flow(&shelf, Purpose::New, &s.base);
    f.begin_sign_in();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !s.lines().iter().any(|l| l == "POST /api/v1/connect/token") {
        assert!(Instant::now() < deadline);
        f.poll_sign_in();
        std::thread::sleep(Duration::from_millis(5));
    }
    f.cancel();
    assert!(f.is_finished() && !f.is_signing_in());
    std::thread::sleep(Duration::from_millis(300));
    assert!(shelf.book().list.is_empty() && shelf.keys().is_empty());
    // After the check passed, before Save.
    let s = working_site();
    let shelf = TestShelf::with(Book::default(), FakeKeys::default());
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    assert!(f.can_save());
    f.cancel();
    assert!(f.is_finished() && !f.can_save());
    f.save();
    assert!(shelf.book().list.is_empty() && shelf.keys().is_empty() && shelf.writes().is_empty());
}

#[test]
fn cancel_waits_while_save_profile_runs() {
    let s = working_site();
    let shelf = TestShelf::with(Book::default(), FakeKeys::default());
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    // The save runs while the shelf is held: it cannot end before the test lets it.
    let hold = lock(&shelf.inner);
    f.save();
    assert!(f.is_working() && !f.can_cancel());
    f.cancel();
    assert!(!f.is_finished(), "Cancel does nothing while the save runs");
    drop(hold);
    settle(&mut f);
    assert!(f.is_finished());
    assert_eq!(shelf.book().list.len(), 1, "the save went through, and nothing was left half-done");
}

#[test]
fn a_save_the_store_cannot_confirm_says_why_and_leaves_nothing() {
    let s = working_site();
    let shelf = TestShelf::with(Book::default(), FakeKeys { garble: true, ..FakeKeys::default() });
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    f.save();
    settle(&mut f);
    assert!(!f.is_finished() && matches!(f.step(), Step::Address));
    assert!(f.error.as_deref().is_some_and(|e| e.starts_with("Could not use the credential store") && !e.contains("dp_")), "{:?}", f.error);
    assert!(shelf.book().list.is_empty() && shelf.keys().is_empty());
}

#[test]
fn a_sign_in_into_a_profile_that_is_there_offers_switch_to_it() {
    let s = ids_site("acc-c");
    let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::Ready) };
    let book = Book { legacy: None, list: vec![profile("pa", "Alex · mythicshot.com", "https://app.mythicshot.com", None, None, State::Ready), pc], active: Some("pa".into()) };
    let shelf = TestShelf::with(book.clone(), FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    let Step::Same { name, .. } = f.step() else { panic!("no Switch to it") };
    assert_eq!(profiles::exists(name), "You already have a profile for this account: \"customer1 test\".");
    assert!(!f.can_save());
    f.switch_to_it();
    settle(&mut f);
    assert!(f.is_finished());
    assert_eq!(*lock(&shelf.switched), ["pc"]);
    assert_eq!(shelf.book().list, book.list, "no second profile");
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), TOKEN_B.to_string())], "a profile that works keeps its token");
    assert!(shelf.writes().is_empty());
}

/// Every text one frame of the add form draws.
fn form_texts(f: &mut AddFlow) -> Vec<String> {
    fn collect(shape: &egui::Shape, into: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => into.push(t.galley.text().replace('\n', " ")),
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
            _ => {}
        }
    }
    let ctx = crate::theme::test_context();
    let mut texts = Vec::new();
    for _ in 0..2 {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 500.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| f.ui(ui));
        out.textures_delta.clear();
        texts.clear();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
    }
    texts
}

/// The browser was still signed in as an account that has a profile: the form says so and why,
/// offers "Switch to it" first and a fresh sign-in second.
#[test]
fn the_profile_is_there_already_form_says_why_and_offers_a_different_account() {
    let s = ids_site("acc-c");
    let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::Ready) };
    let shelf = TestShelf::with(Book { legacy: None, list: vec![pc], active: Some("pc".into()) }, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    assert!(matches!(f.step(), Step::Same { .. }));
    let texts = form_texts(&mut f);
    let joined = texts.join(" ");
    assert!(joined.contains("You already have a profile for this account: \"customer1 test\"."), "{texts:?}");
    assert!(joined.contains(&profiles::exists_why()), "{texts:?}");
    assert_eq!(profiles::exists_why(), "The browser was signed in as that account. To add a different user, start again and choose \"Use a different account\" in the browser.");
    let switch = texts.iter().position(|t| t == profiles::SWITCH_TO_IT).expect("Switch to it");
    let other = texts.iter().position(|t| t == profiles::TRY_DIFFERENT_ACCOUNT).expect("Try a different account");
    assert!(switch < other, "Switch to it comes first: {texts:?}");
    assert_eq!(profiles::TRY_DIFFERENT_ACCOUNT, "Try a different account");
}

#[test]
fn try_a_different_account_signs_in_again_at_the_same_address_and_stores_nothing() {
    // The first sign-in is the account that has a profile; the second another one.
    let me = |seen: usize| if seen == 0 { format!(r#"{{"workspace":"customer1","workspace_id":"{WS_ID}","account_id":"acc-c"}}"#) } else { format!(r#"{{"workspace":"customer1","workspace_id":"{WS_ID}","account_id":"acc-d"}}"#) };
    let s = stub(move |req, seen| match req.line.as_str() {
        "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
        "POST /api/v1/connect/token" => connected(ACCOUNT),
        "GET /api/v1/me" => reply(200, &me(seen)),
        _ => reply(404, r#"{"error":"not_found"}"#),
    });
    let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::Ready) };
    let book = Book { legacy: None, list: vec![pc], active: Some("pc".into()) };
    let shelf = TestShelf::with(book.clone(), FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = opened.clone();
    let dyn_shelf: Arc<dyn Shelf> = shelf.clone();
    let mut f = AddFlow::new(Purpose::New, s.base.clone(), dyn_shelf, Arc::new(move |url: &str| { lock(&record).push(url.to_string()); Ok(()) }), Arc::new(FakeClock::default()));
    sign_in(&mut f);
    assert!(matches!(f.step(), Step::Same { .. }));
    f.try_different_account();
    assert!(f.is_signing_in(), "a fresh sign-in started");
    assert!(matches!(f.step(), Step::Address), "the form waits again");
    assert_eq!(f.sign_in_dialog().map(|d| d.server().to_string()), Some(s.base.clone()), "for the same address");
    assert_eq!(shelf.book().list, book.list, "nothing stored from the first sign-in");
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), TOKEN_B.to_string())]);
    // The second sign-in comes back as another account: the name step, as usual.
    let deadline = Instant::now() + Duration::from_secs(20);
    while f.is_signing_in() {
        assert!(Instant::now() < deadline, "the second sign-in did not end");
        f.poll_sign_in();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(f.step(), Step::Checked { .. }), "on to the name");
    assert_eq!(lock(&opened).len(), 2, "the browser was opened twice");
    assert_eq!(s.lines().iter().filter(|l| *l == "POST /api/v1/connect/start").count(), 2, "a new code");
    assert!(shelf.writes().is_empty());
}

#[test]
fn try_a_different_account_that_comes_back_as_the_same_one_shows_the_same_dead_end() {
    let s = site(|_| connected(ACCOUNT), (200, Box::leak(format!(r#"{{"workspace":"customer1","workspace_id":"{WS_ID}","account_id":"acc-c"}}"#).into_boxed_str())));
    let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::Ready) };
    let shelf = TestShelf::with(Book { legacy: None, list: vec![pc], active: Some("pc".into()) }, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    f.try_different_account();
    let deadline = Instant::now() + Duration::from_secs(20);
    while f.is_signing_in() {
        assert!(Instant::now() < deadline, "the second sign-in did not end");
        f.poll_sign_in();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(f.step(), Step::Same { .. }), "the same account again");
    assert!(shelf.writes().is_empty() && shelf.book().list.len() == 1);
}

#[test]
fn switch_to_it_for_a_profile_waiting_for_a_sign_in_renews_it() {
    let s = ids_site("acc-c");
    let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain) };
    let book = Book { legacy: None, list: vec![pc], active: Some("pc".into()) };
    let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    f.switch_to_it();
    settle(&mut f);
    assert!(!f.is_finished() && f.can_save(), "the renewal waits for its confirmation");
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), TOKEN_B.to_string())], "nothing stored before the confirmation");
    f.save();
    settle(&mut f);
    assert!(f.is_finished());
    assert_eq!(shelf.book().get("pc").unwrap().state, State::Ready);
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), NEW_TOKEN.to_string())]);
}

// ---- 4. Sign in again ----

#[test]
fn sign_in_again_renews_the_same_account_and_offers_another_as_a_new_profile() {
    let s = working_site();
    let waiting = Book { legacy: None, list: vec![profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain)], active: Some("pc".into()) };
    let shelf = TestShelf::with(waiting, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    assert!(!f.is_finished() && f.can_save(), "the renewal waits for its confirmation");
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), TOKEN_B.to_string())], "nothing stored before the confirmation");
    f.save();
    settle(&mut f);
    assert!(f.is_finished());
    assert_eq!(shelf.book().get("pc").unwrap().state, State::Ready);
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), NEW_TOKEN.to_string())]);
    assert_eq!(*lock(&shelf.switched), ["pc"]);

    // Another account signs in: the old profile stays waiting with its token, and the new one
    // is offered under its own name.
    let other = ids_site("acc-intruder");
    let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &other.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain) };
    let book = Book { legacy: None, list: vec![pc], active: Some("pc".into()) };
    let shelf = TestShelf::with(book.clone(), FakeKeys::with(&[("profile-pc", TOKEN_A)]));
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &other.base);
    sign_in(&mut f);
    settle(&mut f);
    let Step::Checked { candidate, .. } = f.step() else { panic!("not offered as a new profile") };
    assert_eq!(candidate.identity, ided(WS_ID, "acc-intruder"));
    assert_eq!(f.purpose, Purpose::New);
    assert_eq!(shelf.book(), book);
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), TOKEN_A.to_string())]);
}

#[test]
fn cancel_at_the_renewal_confirmation_leaves_the_profile_as_it_was() {
    let s = working_site();
    let waiting = Book { legacy: None, list: vec![profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain)], active: Some("pc".into()) };
    let shelf = TestShelf::with(waiting.clone(), FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    // What the confirmation shows: the approved line for the sign-in it would take.
    let Step::Renew { candidate, .. } = f.step() else { panic!("no confirmation") };
    assert_eq!(profiles::connected(candidate.account.as_deref(), &candidate.workspace), format!("Connected. Signed in as {ACCOUNT}, workspace \"customer1\"."));
    f.cancel();
    assert!(f.is_finished());
    assert_eq!(shelf.book(), waiting);
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), TOKEN_B.to_string())]);
    assert!(shelf.writes().is_empty());
}

#[test]
fn a_profile_with_no_identity_is_renewed_after_the_confirmation_and_records_what_it_got() {
    let s = working_site();
    let bare = Profile { identity: None, workspace: None, account: None, ..profile("pf", "first", &s.base, None, None, State::SignInAgain) };
    let shelf = TestShelf::with(Book { legacy: None, list: vec![bare], active: Some("pf".into()) }, FakeKeys::with(&[("profile-pf", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::Renew("pf".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    assert!(!f.is_finished() && f.can_save());
    assert_eq!(shelf.keys(), vec![("profile-pf".to_string(), TOKEN_B.to_string())]);
    f.save();
    settle(&mut f);
    let p = shelf.book().get("pf").unwrap().clone();
    assert_eq!((p.state, p.identity, p.account.as_deref()), (State::Ready, Some(named("customer1")), Some(ACCOUNT)));
    assert_eq!(shelf.keys(), vec![("profile-pf".to_string(), NEW_TOKEN.to_string())]);
}

// ---- 7. Removing a profile ----

#[test]
fn remove_asks_first_and_takes_only_that_profile_and_its_token() {
    let book = Book { legacy: None, list: vec![profile(MIGRATED_ID, "Alex · mythicshot.com", "https://app.mythicshot.com", None, None, State::Ready), profile("pb", "Sandbox admin", "http://localhost:3000", None, None, State::Ready)], active: Some("pb".into()) };
    let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-first", TOKEN_A), ("profile-pb", TOKEN_B)]));
    let make_shelf: Arc<dyn Shelf> = shelf.clone();
    let mut panel = Panel::with(make_shelf, Arc::new(|_, _| panic!("no form here")));
    panel.ask_remove("pb");
    assert_eq!(panel.remove_question().as_deref(), Some("Remove \"Sandbox admin\" from this computer? Your tours and account on the site are not touched."));
    panel.cancel_remove();
    assert_eq!(panel.remove_question(), None);
    assert_eq!(shelf.book().list.len(), 2);
    panel.ask_remove("pb");
    panel.confirm_remove();
    let deadline = Instant::now() + Duration::from_secs(10);
    while panel.is_working() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
        panel.poll(None);
    }
    let book = shelf.book();
    assert_eq!(book.list.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), [MIGRATED_ID]);
    assert_eq!(book.active.as_deref(), Some(MIGRATED_ID), "another profile becomes active");
    assert_eq!(shelf.keys(), vec![("profile-first".to_string(), TOKEN_A.to_string())]);
}

#[test]
fn a_request_from_a_dialog_opens_the_form_or_the_list() {
    let shelf = TestShelf::with(Book { legacy: None, list: vec![profile("pa", "A", "http://127.0.0.1:1", None, None, State::Ready)], active: Some("pa".into()) }, FakeKeys::default());
    let for_flow = shelf.clone();
    let make: Arc<dyn Fn(Purpose, String) -> AddFlow + Send + Sync> = Arc::new(move |purpose, address| flow(&for_flow, purpose, &address));
    let make_shelf: Arc<dyn Shelf> = shelf.clone();
    let mut panel = Panel::with(make_shelf, make);
    panel.poll(Some(ProfilesRequest::Manage));
    assert!(matches!(panel.mode, Mode::Manage));
    panel.poll(Some(ProfilesRequest::Add));
    assert!(matches!(&panel.mode, Mode::Add(f) if f.purpose == Purpose::New && !f.can_save()));
    // Sign in again opens the browser window for the profile's own address at once.
    panel.sign_in_again("pa");
    assert!(matches!(&panel.mode, Mode::Add(f) if f.purpose == Purpose::Renew("pa".into()) && f.address == "http://127.0.0.1:1" && f.is_signing_in()));
}

// ---- 9. Ending a token at its site ----

const END: &str = "DELETE /api/v1/token";

/// A site that answers the end of a token with `end`, and nothing else.
fn ending_site(end: impl Fn() -> Answer + Send + 'static) -> Stub {
    stub(move |req, _| if req.line == END { end() } else { reply(404, r#"{"error":"not_found"}"#) })
}

fn panel_for(shelf: &Arc<TestShelf>) -> Panel {
    let shelf: Arc<dyn Shelf> = shelf.clone();
    Panel::with(shelf, Arc::new(|_, _| panic!("no form here")))
}

/// Confirm the removal of `id` and wait for its local part; how long that took.
fn remove_now(panel: &mut Panel, id: &str) -> Duration {
    let started = Instant::now();
    panel.ask_remove(id);
    panel.confirm_remove();
    panel.poll(None);
    while panel.is_working() {
        assert!(started.elapsed() < Duration::from_secs(10), "the removal did not end");
        std::thread::sleep(Duration::from_millis(5));
        panel.poll(None);
    }
    started.elapsed()
}

/// Wait until the site was asked, or given up on, and what came of it was taken in.
fn wait_ended(panel: &mut Panel) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while panel.is_ending() {
        assert!(Instant::now() < deadline, "the site was never given up on");
        std::thread::sleep(Duration::from_millis(5));
    }
    panel.poll(None);
}

fn ends(s: &Stub) -> Vec<Option<String>> {
    s.requests().into_iter().filter(|r| r.line == END).map(|r| r.auth).collect()
}

fn bearer(token: &str) -> Option<String> {
    Some(format!("Bearer {token}"))
}

#[test]
fn a_removal_ends_the_token_at_its_own_site_and_shows_nothing_more() {
    let s = ending_site(|| reply(204, ""));
    let book = Book { legacy: None, list: vec![profile("pa", "A", &s.base, None, None, State::Ready), profile("pb", "B", "http://127.0.0.1:1", None, None, State::Ready)], active: Some("pa".into()) };
    let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-pa", TOKEN_A), ("profile-pb", TOKEN_B)]));
    let mut panel = panel_for(&shelf);
    remove_now(&mut panel, "pa");
    assert_eq!(shelf.book().list.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["pb"]);
    assert_eq!(shelf.keys(), vec![("profile-pb".to_string(), TOKEN_B.to_string())]);
    wait_ended(&mut panel);
    assert_eq!(ends(&s), vec![bearer(TOKEN_A)], "once, with its own token, at its own site");
    assert_eq!(*lock(&shelf.ended_at), vec![s.base.clone()]);
    assert!(panel.notices().is_empty());
    assert_eq!(panel.error(), None);
}

#[test]
fn a_profile_waiting_for_a_sign_in_is_still_tried_once_and_already_ended_shows_nothing() {
    let s = ending_site(|| reply(401, r#"{"error":"unauthorized"}"#));
    let book = Book { legacy: None, list: vec![profile("pc", "C", &s.base, None, None, State::SignInAgain)], active: Some("pc".into()) };
    let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-pc", TOKEN_A)]));
    let mut panel = panel_for(&shelf);
    remove_now(&mut panel, "pc");
    wait_ended(&mut panel);
    assert_eq!(ends(&s), vec![bearer(TOKEN_A)]);
    assert!(shelf.book().list.is_empty() && shelf.keys().is_empty());
    assert!(panel.notices().is_empty());
}

#[test]
fn an_older_or_unreachable_site_gets_one_line_and_the_profile_still_goes() {
    let answers: Vec<(&str, Box<dyn Fn() -> Answer + Send>)> = vec![
        ("404", Box::new(|| reply(404, r#"{"error":"not_found"}"#))),
        ("405", Box::new(|| reply(405, ""))),
        ("500", Box::new(|| reply(500, ""))),
        ("redirect", Box::new(|| Answer::Reply(302, String::new(), vec![("Location", "http://127.0.0.1:1/api/v1/token".into())]))),
        ("dropped", Box::new(|| Answer::Drop)),
    ];
    for (what, answer) in answers {
        let s = ending_site(answer);
        let book = Book { legacy: None, list: vec![profile("pa", "Sandbox admin", &s.base, None, None, State::Ready)], active: Some("pa".into()) };
        let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-pa", TOKEN_A)]));
        let mut panel = panel_for(&shelf);
        remove_now(&mut panel, "pa");
        wait_ended(&mut panel);
        assert!(shelf.book().list.is_empty() && shelf.keys().is_empty(), "{what}: removed locally all the same");
        assert_eq!(ends(&s).len(), 1, "{what}: one call, no retry");
        assert_eq!(panel.notices(), [profiles::not_ended("Sandbox admin")], "{what}");
        assert_eq!(panel.error(), None, "{what}");
        // The next Remove starts without it.
        panel.ask_remove("gone");
        assert!(panel.notices().is_empty());
    }
    // Nothing listening at all.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let server = format!("http://127.0.0.1:{port}");
    let shelf = TestShelf::with(Book { legacy: None, list: vec![profile("pa", "A", &server, None, None, State::Ready)], active: Some("pa".into()) }, FakeKeys::with(&[("profile-pa", TOKEN_A)]));
    let mut panel = panel_for(&shelf);
    remove_now(&mut panel, "pa");
    wait_ended(&mut panel);
    assert_eq!(panel.notices(), [profiles::not_ended("A")]);
}

#[test]
fn a_slow_site_never_holds_up_the_local_removal() {
    let s = ending_site(|| Answer::Hang);
    let book = Book { legacy: None, list: vec![profile("pa", "A", &s.base, None, None, State::Ready)], active: Some("pa".into()) };
    let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-pa", TOKEN_A)]));
    let mut panel = panel_for(&shelf);
    let took = remove_now(&mut panel, "pa");
    assert!(took < Duration::from_secs(2), "the removal waited for the site: {took:?}");
    assert!(panel.is_ending(), "the site is still being asked");
    assert!(crate::profiles_ui::endings_in_flight(), "an app update counts an ending being sent as running work");
    assert!(shelf.book().list.is_empty() && shelf.keys().is_empty(), "the list is updated already");
    assert!(panel.notices().is_empty());
    let started = Instant::now();
    wait_ended(&mut panel);
    assert!(started.elapsed() < Duration::from_secs(7), "given up at the timeout: {:?}", started.elapsed());
    assert_eq!(panel.notices(), [profiles::not_ended("A")]);
}

#[test]
fn a_removal_never_ends_a_token_another_profile_or_the_old_entry_still_holds() {
    let s = ending_site(|| reply(204, ""));
    // The old entry holds it (not as the profile's own copy); another profile with the same
    // token; a twin after a join that could not delete the first profile's entry.
    let first = profile(MIGRATED_ID, "first", &s.base, None, None, State::Ready);
    let cases = [
        (Book { legacy: None, list: vec![first.clone()], active: Some(MIGRATED_ID.into()) }, FakeKeys::with(&[(publish::LEGACY_KEY, TOKEN_A), ("profile-first", TOKEN_A)]), MIGRATED_ID),
        (Book { legacy: None, list: vec![profile("pa", "A", &s.base, None, None, State::Ready), profile("pb", "B", &s.base, None, None, State::Ready)], active: Some("pa".into()) }, FakeKeys::with(&[("profile-pa", TOKEN_A), ("profile-pb", TOKEN_A)]), "pa"),
        (
            Book { legacy: None, list: vec![Profile { identity: Some(ided(WS_ID, "acc-a")), ..first.clone() }, Profile { identity: Some(ided(WS_ID, "acc-a")), ..profile("pa", "A", &s.base, None, None, State::Ready) }], active: Some("pa".into()) },
            FakeKeys::with(&[("profile-first", TOKEN_A), ("profile-pa", TOKEN_A)]),
            MIGRATED_ID,
        ),
    ];
    for (book, keys, id) in cases {
        let shelf = TestShelf::with(book, keys);
        let mut panel = panel_for(&shelf);
        remove_now(&mut panel, id);
        wait_ended(&mut panel);
        assert!(shelf.book().get(id).is_none(), "{id}: removed locally");
        assert!(lock(&shelf.ended_at).is_empty(), "{id}: nothing to end");
        assert!(panel.notices().is_empty());
    }
    assert!(ends(&s).is_empty());
}

/// Wait until the end of a replaced token was sent, or given up on.
fn wait_flow_ended(f: &AddFlow) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while f.is_ending() {
        assert!(Instant::now() < deadline, "the site was never given up on");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn sign_in_again_ends_the_old_token_only_after_the_new_one_is_stored() {
    // What the store held when the site was asked to end the old token.
    let shelf_slot: Arc<std::sync::OnceLock<Arc<TestShelf>>> = Arc::default();
    let held_then: Arc<Mutex<Vec<Vec<(String, String)>>>> = Arc::default();
    let (slot, held) = (shelf_slot.clone(), held_then.clone());
    let s = stub(move |req, seen| match req.line.as_str() {
        "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
        "POST /api/v1/connect/token" => if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected(ACCOUNT) },
        "GET /api/v1/me" => reply(200, r#"{"workspace":"customer1"}"#),
        END => {
            lock(&held).push(slot.get().map(|s| s.keys()).unwrap_or_default());
            reply(204, "")
        }
        _ => reply(404, r#"{"error":"not_found"}"#),
    });
    let waiting = Book { legacy: None, list: vec![profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain)], active: Some("pc".into()) };
    let shelf = TestShelf::with(waiting, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let _ = shelf_slot.set(shelf.clone());
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    assert!(ends(&s).is_empty(), "nothing is ended before the confirmation");
    f.save();
    settle(&mut f);
    assert!(f.is_finished(), "the form does not wait for the site");
    wait_flow_ended(&f);
    assert_eq!(ends(&s), vec![bearer(TOKEN_B)], "once, with the old token");
    assert_eq!(*lock(&held_then), vec![vec![("profile-pc".to_string(), NEW_TOKEN.to_string())]], "the new token was stored first");
    assert_eq!(shelf.book().get("pc").unwrap().state, State::Ready);
}

#[test]
fn sign_in_again_says_nothing_whatever_the_site_answers_about_the_old_token() {
    for status in [401, 404, 409, 500] {
        let s = stub(move |req, seen| match req.line.as_str() {
            "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
            "POST /api/v1/connect/token" => if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected(ACCOUNT) },
            "GET /api/v1/me" => reply(200, r#"{"workspace":"customer1"}"#),
            _ => reply(status, ""),
        });
        let waiting = Book { legacy: None, list: vec![profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain)], active: Some("pc".into()) };
        let shelf = TestShelf::with(waiting, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
        let mut f = flow(&shelf, Purpose::Renew("pc".into()), &s.base);
        sign_in(&mut f);
        settle(&mut f);
        f.save();
        settle(&mut f);
        wait_flow_ended(&f);
        assert!(f.is_finished() && f.error.is_none(), "HTTP {status}");
        assert_eq!(ends(&s), vec![bearer(TOKEN_B)], "HTTP {status}");
        assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), NEW_TOKEN.to_string())]);
    }
}

#[test]
fn sign_in_again_ends_nothing_when_the_token_stays_or_the_save_fails() {
    // The site handed back the very token the profile has.
    let s = working_site();
    let waiting = || Book { legacy: None, list: vec![profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain)], active: Some("pc".into()) };
    let shelf = TestShelf::with(waiting(), FakeKeys::with(&[("profile-pc", NEW_TOKEN)]));
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    f.save();
    settle(&mut f);
    wait_flow_ended(&f);
    assert!(f.is_finished());
    // The store cannot take the new token: the old one stays, and is not ended.
    let shelf2 = TestShelf::with(waiting(), FakeKeys { refuse_write: Some("profile-pc".into()), ..FakeKeys::with(&[("profile-pc", TOKEN_B)]) });
    let mut f = flow(&shelf2, Purpose::Renew("pc".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    f.save();
    settle(&mut f);
    wait_flow_ended(&f);
    assert!(f.error.is_some() && !f.is_finished());
    assert_eq!(shelf2.keys(), vec![("profile-pc".to_string(), TOKEN_B.to_string())]);
    assert!(ends(&s).is_empty());
    assert!(lock(&shelf.ended_at).is_empty() && lock(&shelf2.ended_at).is_empty());
}

#[test]
fn no_other_path_ends_a_token() {
    // One site for every path; it would answer the end of a token, and logs every request.
    let s = stub(move |req, seen| match req.line.as_str() {
        "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
        "POST /api/v1/connect/token" => if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected(ACCOUNT) },
        "GET /api/v1/me" => reply(200, &format!(r#"{{"workspace":"customer1","workspace_id":"{WS_ID}","account_id":"acc-new"}}"#)),
        END => reply(204, ""),
        _ => reply(404, r#"{"error":"not_found"}"#),
    });
    let refused = site(|_| connected(ACCOUNT), (401, ""));
    let mut shelves = Vec::new();

    // Another account at "Sign in again": offered as a new profile, the old one untouched.
    let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain) };
    let shelf = TestShelf::with(Book { legacy: None, list: vec![pc.clone()], active: Some("pc".into()) }, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    assert!(matches!(f.step(), Step::Checked { .. }), "offered as a new profile");
    // ...and its save fails: the store cannot take it.
    let failing = TestShelf::with(Book { legacy: None, list: vec![pc.clone()], active: Some("pc".into()) }, FakeKeys { garble: true, ..FakeKeys::with(&[("profile-pc", TOKEN_B)]) });
    let mut g = flow(&failing, Purpose::New, &s.base);
    sign_in(&mut g);
    settle(&mut g);
    g.save();
    settle(&mut g);
    assert!(g.error.is_some(), "the save failed");
    wait_flow_ended(&f);
    wait_flow_ended(&g);
    shelves.extend([shelf.clone(), failing.clone()]);

    // A refused sign-in.
    let shelf = TestShelf::with(Book { legacy: None, list: vec![pc.clone()], active: Some("pc".into()) }, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &refused.base);
    sign_in(&mut f);
    assert!(matches!(f.sign_in_dialog().map(|d| d.stage()), Some(Stage::Failed { .. })));
    shelves.push(shelf);

    // A switch in the dropdown. ("Switch to it" ends the connection of the sign-in it leaves
    // unkept: see `every_way_of_leaving_a_passed_sign_in_unkept_ends_its_connection_once`.)
    let ready = Profile { identity: Some(ided(WS_ID, "acc-new")), state: State::Ready, ..pc.clone() };
    let other = profile("pd", "D", &s.base, None, None, State::Ready);
    let shelf = TestShelf::with(Book { legacy: None, list: vec![ready, other], active: Some("pd".into()) }, FakeKeys::with(&[("profile-pc", TOKEN_B), ("profile-pd", TOKEN_A)]));
    assert!(shelf.switch("pc") && shelf.switch("pd"));
    shelves.push(shelf.clone());

    // The update that adds profiles, the join of twins, the old entry's cleanup and Sign out.
    // The app runs these on the OS credential store (`main.rs` at start, and
    // `publish_ui::note_check`, `publish_ui::forget_active_token` through `change_profiles`), so
    // their own entry points cannot be driven here. What makes an end impossible on them is the
    // type: a token is only ever ended from an `Ending`, by one of two routes. `spawn_change`
    // ends one for a `Done::ThenEnd`, which is only made from the `Ending` that `profiles::remove`
    // and `profiles::renew` hand back; `spawn_abandon` ends the connection of a sign-in the add
    // form leaves unkept, from `Candidate::into_ending`, which only a candidate the form holds
    // can make. These four return no `Ending` and hold no candidate, and the lines below pin
    // that: one of them returning one no longer compiles. They are run on the stub's address all
    // the same, and the stub sees nothing.
    let _: fn(&mut crate::settings::Settings, &dyn KeyStore) -> Result<Option<String>, String> = profiles::migrate;
    let _: fn(&mut Book, &dyn KeyStore, &str, &str) -> Result<(), String> = profiles::attach;
    let _: fn(&crate::settings::Settings, &dyn KeyStore) -> Result<bool, String> = profiles::remove_legacy;
    let _: fn(&mut Book, &dyn KeyStore, &str) -> Result<(), String> = profiles::sign_out;
    let store = FakeKeys::with(&[(publish::LEGACY_KEY, TOKEN_A), ("profile-pt", TOKEN_B)]);
    let mut settings = crate::settings::Settings::default();
    settings.publish_saved_for = Some(s.base.clone());
    assert!(profiles::migrate(&mut settings, &store).unwrap().is_some());
    let mut book = Book::of(&settings);
    for p in book.list.iter_mut() {
        p.identity = Some(ided(WS_ID, "acc-a"));
    }
    book.list.push(Profile { identity: Some(ided(WS_ID, "acc-a")), ..profile("pt", "Twin", &s.base, None, Some("customer1"), State::SignInAgain) });
    assert_eq!(profiles::twin_of(&book, MIGRATED_ID).as_deref(), Some("pt"));
    profiles::attach(&mut book, &store, MIGRATED_ID, "pt").unwrap();
    book.write_to(&mut settings);
    assert!(profiles::remove_legacy(&settings, &store).unwrap());
    // Signing out of a profile only forgets its token here.
    profiles::sign_out(&mut book, &store, "pt").unwrap();
    assert!(store.entries.borrow().is_empty());

    assert!(ends(&s).is_empty() && ends(&refused).is_empty(), "no token was ended");
    for shelf in shelves {
        assert!(lock(&shelf.ended_at).is_empty());
    }
}

// ---- 10. A token a running job holds ----

/// A site whose `GET /api/v1/me` answers for TOKEN_A until it was asked to end it, and 401 after.
fn job_site(end: u16) -> Stub {
    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    stub(move |req, _| match req.line.as_str() {
        END => {
            ended.store(true, Ordering::SeqCst);
            reply(end, "")
        }
        "GET /api/v1/me" if ended.load(Ordering::SeqCst) => reply(401, r#"{"error":"unauthorized"}"#),
        "GET /api/v1/me" => reply(200, r#"{"workspace":"A"}"#),
        _ => reply(404, r#"{"error":"not_found"}"#),
    })
}

fn shelf_on(s: &Stub, ids: &[(&str, &str, &str)]) -> Arc<TestShelf> {
    let list = ids.iter().map(|(id, name, _)| profile(id, name, &s.base, None, None, State::Ready)).collect();
    let keys: Vec<(String, &str)> = ids.iter().map(|(id, _, token)| (publish::profile_key(id), *token)).collect();
    let keys: Vec<(&str, &str)> = keys.iter().map(|(k, t)| (k.as_str(), *t)).collect();
    TestShelf::with(Book { legacy: None, list, active: Some(ids[0].0.into()) }, FakeKeys::with(&keys))
}

/// Nothing more reaches the site for a while.
fn still_no_end(s: &Stub, ending: bool) {
    std::thread::sleep(Duration::from_millis(200));
    assert!(ends(s).is_empty(), "the token was ended under a running job");
    assert!(ending);
}

#[test]
fn a_job_in_flight_runs_to_its_end_before_its_token_is_ended() {
    let s = job_site(204);
    let shelf = shelf_on(&s, &[("pa", "A", TOKEN_A)]);
    // The job takes its hold and reads its token, as the app's jobs do, then waits for the go.
    let held = shelf.holds.hold("pa");
    let token = shelf.keys()[0].1.clone();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let base = s.base.clone();
    let job = std::thread::spawn(move || {
        let _held = held;
        wait.recv().unwrap();
        (0..3)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(50));
                publish::workspace(&base, &token).map(|w| w.name)
            })
            .collect::<Vec<_>>()
    });
    let mut panel = panel_for(&shelf);
    let took = remove_now(&mut panel, "pa");
    assert!(took < Duration::from_secs(2), "the local removal waited: {took:?}");
    assert!(shelf.book().list.is_empty() && shelf.keys().is_empty(), "removed here at once");
    still_no_end(&s, panel.is_ending());
    go.send(()).unwrap();
    let answers = job.join().unwrap();
    assert_eq!(answers, vec![Ok("A".to_string()); 3], "every request of the job went through");
    wait_ended(&mut panel);
    assert_eq!(ends(&s), vec![bearer(TOKEN_A)], "then exactly once");
    assert_eq!(s.lines().last().map(String::as_str), Some(END), "after the job's last request");
    assert!(panel.notices().is_empty());
}

#[test]
fn a_job_that_fails_is_cancelled_or_panics_still_lets_go() {
    for how in ["fails", "cancelled", "panics"] {
        let s = job_site(204);
        let shelf = shelf_on(&s, &[("pa", "A", TOKEN_A)]);
        let held = shelf.holds.hold("pa");
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let job = std::thread::spawn(move || -> Result<(), String> {
            let _held = held;
            wait.recv().unwrap();
            match how {
                "fails" => Err("Could not publish the tour: the server answered HTTP 500.".into()),
                "cancelled" => Err(publish::CANCELLED.into()),
                _ => panic!("the job broke"),
            }
        });
        let mut panel = panel_for(&shelf);
        remove_now(&mut panel, "pa");
        still_no_end(&s, panel.is_ending());
        go.send(()).unwrap();
        let _ = job.join();
        wait_ended(&mut panel);
        assert_eq!(ends(&s), vec![bearer(TOKEN_A)], "{how}");
    }
}

#[test]
fn two_jobs_on_one_profile_both_run_to_their_end_first() {
    let s = job_site(204);
    let shelf = shelf_on(&s, &[("pa", "A", TOKEN_A)]);
    let (first, second) = (shelf.holds.hold("pa"), shelf.holds.hold("pa"));
    let mut panel = panel_for(&shelf);
    remove_now(&mut panel, "pa");
    still_no_end(&s, panel.is_ending());
    drop(first);
    still_no_end(&s, panel.is_ending());
    drop(second);
    wait_ended(&mut panel);
    assert_eq!(ends(&s), vec![bearer(TOKEN_A)]);
}

#[test]
fn sign_in_again_waits_for_a_job_holding_the_old_token_too() {
    let s = stub(move |req, seen| match req.line.as_str() {
        "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
        "POST /api/v1/connect/token" => if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected(ACCOUNT) },
        "GET /api/v1/me" => reply(200, r#"{"workspace":"customer1"}"#),
        END => reply(204, ""),
        _ => reply(404, r#"{"error":"not_found"}"#),
    });
    let ready = Book { legacy: None, list: vec![profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::Ready)], active: Some("pc".into()) };
    let shelf = TestShelf::with(ready, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let held = shelf.holds.hold("pc");
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    f.save();
    settle(&mut f);
    assert!(f.is_finished(), "the form does not wait for the job");
    std::thread::sleep(Duration::from_millis(200));
    assert!(ends(&s).is_empty() && f.is_ending());
    drop(held);
    wait_flow_ended(&f);
    assert_eq!(ends(&s), vec![bearer(TOKEN_B)]);
}

#[test]
fn a_token_a_profile_was_saved_with_meanwhile_is_not_ended() {
    let s = job_site(204);
    let shelf = shelf_on(&s, &[("pa", "A", TOKEN_A)]);
    let held = shelf.holds.hold("pa");
    let mut panel = panel_for(&shelf);
    remove_now(&mut panel, "pa");
    // Meanwhile the same token is pasted into a new profile.
    {
        let mut g = lock(&shelf.inner);
        g.0.list.push(profile("pz", "Z", &s.base, None, None, State::Ready));
        g.1.entries.borrow_mut().insert("profile-pz".into(), TOKEN_A.into());
    }
    drop(held);
    wait_ended(&mut panel);
    assert!(ends(&s).is_empty(), "held again: skipped");
    assert!(lock(&shelf.ended_at).is_empty());
    assert!(panel.notices().is_empty(), "silently");
}

#[test]
fn two_removals_whose_sites_did_not_end_the_tokens_keep_both_lines() {
    let s = job_site(404);
    let shelf = shelf_on(&s, &[("pa", "A", TOKEN_A), ("pb", "B", TOKEN_B)]);
    let (ha, hb) = (shelf.holds.hold("pa"), shelf.holds.hold("pb"));
    let mut panel = panel_for(&shelf);
    remove_now(&mut panel, "pa");
    remove_now(&mut panel, "pb");
    drop(ha);
    drop(hb);
    wait_ended(&mut panel);
    let mut lines = panel.notices().to_vec();
    lines.sort();
    assert_eq!(lines, vec![profiles::not_ended("A"), profiles::not_ended("B")]);
    assert_eq!(ends(&s).len(), 2);
    // Cleared as before: by the next Remove.
    panel.ask_remove("gone");
    assert!(panel.notices().is_empty());
}

#[test]
fn removing_the_migrated_profile_ends_its_token_once_its_old_entry_copy_went_too() {
    let s = job_site(204);
    let first = profile(MIGRATED_ID, "first", &s.base, None, None, State::Ready);
    let shelf = TestShelf::with(Book { legacy: Some(MIGRATED_ID.into()), list: vec![first], active: Some(MIGRATED_ID.into()) }, FakeKeys::with(&[(publish::LEGACY_KEY, TOKEN_A), ("profile-first", TOKEN_A)]));
    let mut panel = panel_for(&shelf);
    remove_now(&mut panel, MIGRATED_ID);
    wait_ended(&mut panel);
    assert!(shelf.keys().is_empty());
    assert_eq!(ends(&s), vec![bearer(TOKEN_A)]);
}

// ---- 11. A setup token the site keeps, and repaints ----

#[test]
fn a_setup_token_the_site_keeps_on_purpose_is_removed_here_and_nothing_is_said() {
    let s = ending_site(|| reply(409, r#"{"error":"not_a_connection"}"#));
    let book = Book { legacy: None, list: vec![profile("pa", "Pasted", &s.base, None, None, State::Ready)], active: Some("pa".into()) };
    let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-pa", TOKEN_A)]));
    let mut panel = panel_for(&shelf);
    remove_now(&mut panel, "pa");
    wait_ended(&mut panel);
    assert_eq!(ends(&s), vec![bearer(TOKEN_A)], "asked once");
    assert!(shelf.book().list.is_empty() && shelf.keys().is_empty(), "removed here as always");
    assert!(panel.notices().is_empty(), "no line");
    assert_eq!(panel.error(), None);
}

#[test]
fn an_ending_that_waits_for_a_job_asks_for_no_repaint_and_wakes_the_page_once_for_its_line() {
    let s = job_site(404);
    let shelf = shelf_on(&s, &[("pa", "A", TOKEN_A)]);
    let held = shelf.holds.hold("pa");
    let mut panel = panel_for(&shelf);
    let woken = Arc::new(AtomicUsize::new(0));
    let count = woken.clone();
    panel.set_waker(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    remove_now(&mut panel, "pa");
    std::thread::sleep(Duration::from_millis(200));
    assert!(panel.is_ending(), "still waiting for the job");
    assert!(crate::profiles_ui::endings_in_flight(), "an app update counts it as running work");
    assert!(!panel.wants_repaint(), "an idle page does not repaint while only waiting");
    assert_eq!(woken.load(Ordering::SeqCst), 0);
    drop(held);
    wait_ended(&mut panel);
    assert_eq!(panel.notices(), [profiles::not_ended("A")]);
    assert_eq!(woken.load(Ordering::SeqCst), 1, "woken once, when the line arrived");
    assert!(!panel.wants_repaint());
}

// ---- 11. The connection an abandoned sign-in made ----

/// A site that signs in (as `account_id`, by ids) and answers the end of a token with `end`.
fn abandoning_site(account_id: &'static str, end: u16) -> Stub {
    let me: &'static str = Box::leak(format!(r#"{{"workspace":"customer1","workspace_id":"{WS_ID}","account_id":"{account_id}"}}"#).into_boxed_str());
    stub(move |req, seen| match req.line.as_str() {
        "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
        "POST /api/v1/connect/token" => if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected(ACCOUNT) },
        "GET /api/v1/me" => reply(200, me),
        END => reply(end, if end == 204 { "" } else { r#"{"error":"x"}"# }),
        _ => reply(404, r#"{"error":"not_found"}"#),
    })
}

/// A shelf holding profile `pc` for account `acc-c` at the stub's address, `state` as given.
fn shelf_with_pc(s: &Stub, state: State) -> Arc<TestShelf> {
    let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), state) };
    TestShelf::with(Book { legacy: None, list: vec![pc], active: Some("pc".into()) }, FakeKeys::with(&[("profile-pc", TOKEN_B)]))
}

/// Wait until the stub has seen `n` ends, and a little longer, so a second would show.
fn ended_once(s: &Stub, f: &AddFlow) -> Vec<Option<String>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while ends(s).is_empty() {
        assert!(Instant::now() < deadline, "the connection was never ended");
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_flow_ended(f);
    std::thread::sleep(Duration::from_millis(50));
    ends(s)
}

/// "Try a different account", Cancel after the sign-in passed, and "Switch to it" each end the
/// connection the sign-in made, once, at its own site, with its own token.
#[test]
fn every_way_of_leaving_a_passed_sign_in_unkept_ends_its_connection_once() {
    // Try a different account.
    let s = abandoning_site("acc-c", 204);
    let shelf = shelf_with_pc(&s, State::Ready);
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    assert!(matches!(f.step(), Step::Same { .. }));
    f.try_different_account();
    assert_eq!(ended_once(&s, &f), vec![bearer(NEW_TOKEN)], "Try a different account");
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), TOKEN_B.to_string())]);
    // Cancel at the name, and at the renewal's confirmation.
    for (purpose, account) in [(Purpose::New, "acc-new"), (Purpose::Renew("pc".into()), "acc-c")] {
        let s = abandoning_site(account, 204);
        let shelf = shelf_with_pc(&s, State::SignInAgain);
        let mut f = flow(&shelf, purpose.clone(), &s.base);
        sign_in(&mut f);
        assert!(matches!(f.step(), Step::Checked { .. } | Step::Renew { .. }), "{purpose:?}");
        f.cancel();
        assert_eq!(ended_once(&s, &f), vec![bearer(NEW_TOKEN)], "Cancel, {purpose:?}");
        assert!(f.error.is_none() && shelf.writes().is_empty());
    }
    // Switch to it, for a profile that is ready.
    let s = abandoning_site("acc-c", 204);
    let shelf = shelf_with_pc(&s, State::Ready);
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    f.switch_to_it();
    assert!(f.is_finished());
    assert_eq!(ended_once(&s, &f), vec![bearer(NEW_TOKEN)], "Switch to it");
    assert_eq!(*lock(&shelf.switched), ["pc"]);
}

/// A sign-in that is saved, or renews its profile, ends nothing; nor does a pasted token
/// (the site's own setup token), whatever becomes of it.
#[test]
fn a_kept_sign_in_or_a_pasted_token_ends_nothing() {
    let s = abandoning_site("acc-new", 204);
    let shelf = shelf_with_pc(&s, State::Ready);
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    f.save();
    settle(&mut f);
    assert!(f.is_finished());
    let s2 = abandoning_site("acc-c", 204);
    let shelf2 = shelf_with_pc(&s2, State::SignInAgain);
    let mut g = flow(&shelf2, Purpose::Renew("pc".into()), &s2.base);
    sign_in(&mut g);
    g.save();
    settle(&mut g);
    assert!(g.is_finished());
    // The pasted token, checked, then the form cancelled.
    let s3 = abandoning_site("acc-new", 204);
    let shelf3 = shelf_with_pc(&s3, State::Ready);
    let mut h = flow(&shelf3, Purpose::New, &s3.base);
    check_paste(&mut h, TOKEN_A);
    assert!(matches!(h.step(), Step::Checked { .. }), "{:?}", h.error);
    h.cancel();
    std::thread::sleep(Duration::from_millis(200));
    // The save ends nothing of the new token (the renewal ends only the old one it replaced).
    assert!(ends(&s).is_empty() && ends(&s3).is_empty(), "{:?} {:?}", ends(&s), ends(&s3));
    wait_flow_ended(&g);
    assert!(!ends(&s2).contains(&bearer(NEW_TOKEN)), "{:?}", ends(&s2));
}

/// Whatever the site answers, nothing is said: the form shows no error and no line.
#[test]
fn ending_an_abandoned_connection_is_silent_on_every_answer() {
    for code in [401, 404, 409, 500] {
        let s = abandoning_site("acc-new", code);
        let shelf = shelf_with_pc(&s, State::Ready);
        let mut f = flow(&shelf, Purpose::New, &s.base);
        sign_in(&mut f);
        f.cancel();
        assert_eq!(ended_once(&s, &f), vec![bearer(NEW_TOKEN)], "{code}");
        f.poll();
        assert!(f.error.is_none() && lock(&f.endings.notices).is_empty(), "{code}: {:?}", f.error);
        assert!(form_texts(&mut f).iter().all(|t| !t.contains("site") || t.contains("Site address") || t.contains("browser")), "{code}");
    }
}

/// The token is never ended while anything on this computer holds it: a saved profile, or the
/// entry of the versions before profiles.
#[test]
fn an_abandoned_connection_held_by_a_saved_profile_or_the_old_entry_is_not_ended() {
    for key in ["profile-pd", publish::LEGACY_KEY] {
        let s = abandoning_site("acc-new", 204);
        let pd = profile("pd", "D", &s.base, None, None, State::Ready);
        let pc = Profile { identity: Some(ided(WS_ID, "acc-c")), ..profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::Ready) };
        let shelf = TestShelf::with(Book { legacy: None, list: vec![pc, pd], active: Some("pc".into()) }, FakeKeys::with(&[("profile-pc", TOKEN_B), (key, NEW_TOKEN)]));
        let mut f = flow(&shelf, Purpose::New, &s.base);
        sign_in(&mut f);
        f.cancel();
        wait_flow_ended(&f);
        std::thread::sleep(Duration::from_millis(100));
        assert!(ends(&s).is_empty(), "{key}: {:?}", ends(&s));
    }
}

/// While the site is asked to end an abandoned connection, an app update counts it as running
/// work; the form itself is not held up.
#[test]
fn an_abandoned_connection_being_ended_counts_as_running_work() {
    let me: &'static str = Box::leak(format!(r#"{{"workspace":"customer1","workspace_id":"{WS_ID}","account_id":"acc-new"}}"#).into_boxed_str());
    let s = stub(move |req, seen| match req.line.as_str() {
        "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
        "POST /api/v1/connect/token" => if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected(ACCOUNT) },
        "GET /api/v1/me" => reply(200, me),
        END => Answer::Hang,
        _ => reply(404, r#"{"error":"not_found"}"#),
    });
    let shelf = shelf_with_pc(&s, State::Ready);
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    f.cancel();
    assert!(f.is_finished(), "Cancel does not wait for the site");
    assert!(f.is_ending() && crate::profiles_ui::endings_in_flight());
    wait_flow_ended(&f);
    assert!(f.error.is_none());
}
