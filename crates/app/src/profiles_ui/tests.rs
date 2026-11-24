use std::time::{Duration, Instant};

use super::*;
use crate::profiles::tests::{FakeKeys, TOKEN_A, TOKEN_B};
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
}

impl TestShelf {
    fn with(book: Book, keys: FakeKeys) -> Arc<TestShelf> {
        Arc::new(TestShelf { inner: Mutex::new((book, keys)), switched: Mutex::default() })
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
    Profile { id: id.into(), name: name.into(), server: server.into(), account: account.map(str::to_string), workspace: workspace.map(str::to_string), state, named: true }
}

// ---- 2. The dropdown ----

#[test]
fn the_dropdown_lists_every_profile_with_its_state_and_the_active_one() {
    let book = Book {
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
fn a_failed_or_unanswered_sign_in_leaves_no_profile_and_no_token() {
    let not_a_site = stub(|_, _| reply(404, "<html>"));
    let turned_down = site(|_| reply(410, r#"{"error":"expired_token"}"#), (200, r#"{"workspace":"customer1"}"#));
    let refused = site(|_| connected(ACCOUNT), (401, ""));
    let not_an_answer = site(|_| connected(ACCOUNT), (200, "<html>not a site</html>"));
    for address in [not_a_site.base.clone(), turned_down.base.clone(), refused.base.clone(), not_an_answer.base.clone(), "http://127.0.0.1:1".to_string()] {
        let shelf = TestShelf::with(Book::default(), FakeKeys::default());
        let mut f = flow(&shelf, Purpose::New, &address);
        sign_in(&mut f);
        match f.sign_in_dialog().map(|d| d.stage()) {
            Some(Stage::Failed { text, retry }) => assert_eq!((text.as_str(), *retry), (profiles::NOT_SAVED, true), "{address}"),
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
    let s = working_site();
    let book = Book { list: vec![profile("pa", "Alex · mythicshot.com", "https://app.mythicshot.com", None, None, State::Ready), profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::Ready)], active: Some("pa".into()) };
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

#[test]
fn switch_to_it_for_a_profile_waiting_for_a_sign_in_renews_it() {
    let s = working_site();
    let book = Book { list: vec![profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain)], active: Some("pc".into()) };
    let shelf = TestShelf::with(book, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::New, &s.base);
    sign_in(&mut f);
    f.switch_to_it();
    settle(&mut f);
    assert!(f.is_finished());
    assert_eq!(shelf.book().get("pc").unwrap().state, State::Ready);
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), NEW_TOKEN.to_string())]);
}

// ---- 4. Sign in again ----

#[test]
fn sign_in_again_renews_the_same_account_and_offers_another_as_a_new_profile() {
    let s = working_site();
    let waiting = Book { list: vec![profile("pc", "customer1 test", &s.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain)], active: Some("pc".into()) };
    let shelf = TestShelf::with(waiting, FakeKeys::with(&[("profile-pc", TOKEN_B)]));
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &s.base);
    sign_in(&mut f);
    settle(&mut f);
    assert!(f.is_finished());
    assert_eq!(shelf.book().get("pc").unwrap().state, State::Ready);
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), NEW_TOKEN.to_string())]);
    assert_eq!(*lock(&shelf.switched), ["pc"]);

    // Another account signs in: the old profile stays waiting with its token, and the new one
    // is offered under its own name.
    let other = site(|_| connected("intruder"), (200, r#"{"workspace":"customer1"}"#));
    let book = Book { list: vec![profile("pc", "customer1 test", &other.base, Some(ACCOUNT), Some("customer1"), State::SignInAgain)], active: Some("pc".into()) };
    let shelf = TestShelf::with(book.clone(), FakeKeys::with(&[("profile-pc", TOKEN_A)]));
    let mut f = flow(&shelf, Purpose::Renew("pc".into()), &other.base);
    sign_in(&mut f);
    settle(&mut f);
    let Step::Checked { candidate, .. } = f.step() else { panic!("not offered as a new profile") };
    assert_eq!(candidate.account.as_deref(), Some("intruder"));
    assert_eq!(f.purpose, Purpose::New);
    assert_eq!(shelf.book(), book);
    assert_eq!(shelf.keys(), vec![("profile-pc".to_string(), TOKEN_A.to_string())]);
}

// ---- 7. Removing a profile ----

#[test]
fn remove_asks_first_and_takes_only_that_profile_and_its_token() {
    let book = Book { list: vec![profile(MIGRATED_ID, "Alex · mythicshot.com", "https://app.mythicshot.com", None, None, State::Ready), profile("pb", "Sandbox admin", "http://localhost:3000", None, None, State::Ready)], active: Some("pb".into()) };
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
    let shelf = TestShelf::with(Book { list: vec![profile("pa", "A", "http://127.0.0.1:1", None, None, State::Ready)], active: Some("pa".into()) }, FakeKeys::default());
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
