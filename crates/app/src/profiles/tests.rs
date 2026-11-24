use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;
use crate::publish::tests::{Stub, reply, stub};

pub(crate) const TOKEN_A: &str = "dp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(crate) const TOKEN_B: &str = "dp_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OLD: &str = "dp_0123456789abcdef0123456789abcdef01234567";
const SITE: &str = "https://tours.example.com";

/// A credential store in memory. It can be locked, refuse writes to one entry, or keep
/// something else than what it was given; it logs every write and delete.
#[derive(Default)]
pub(crate) struct FakeKeys {
    pub(crate) entries: RefCell<BTreeMap<String, String>>,
    pub(crate) locked: bool,
    pub(crate) refuse_write: Option<String>,
    pub(crate) refuse_delete: bool,
    pub(crate) garble: bool,
    pub(crate) log: RefCell<Vec<String>>,
}

impl FakeKeys {
    pub(crate) fn with(entries: &[(&str, &str)]) -> FakeKeys {
        let f = FakeKeys::default();
        for (k, v) in entries {
            f.entries.borrow_mut().insert(k.to_string(), v.to_string());
        }
        f
    }
    pub(crate) fn at(&self, key: &str) -> Option<String> {
        self.entries.borrow().get(key).cloned()
    }
    pub(crate) fn keys(&self) -> Vec<String> {
        self.entries.borrow().keys().cloned().collect()
    }
}

const LOCKED: &str = "Could not use the credential store: the credential store is locked or not reachable.";

impl KeyStore for FakeKeys {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        if self.locked {
            return Err(LOCKED.into());
        }
        Ok(self.at(key))
    }
    fn set(&self, key: &str, secret: &str) -> Result<(), String> {
        if self.locked || self.refuse_write.as_deref() == Some(key) {
            return Err(LOCKED.into());
        }
        self.log.borrow_mut().push(format!("set {key}"));
        let kept = if self.garble { "dp_ffffffffffffffffffffffffffffffffffffffff".to_string() } else { secret.to_string() };
        self.entries.borrow_mut().insert(key.to_string(), kept);
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        if self.locked || self.refuse_delete {
            return Err(LOCKED.into());
        }
        self.log.borrow_mut().push(format!("delete {key}"));
        self.entries.borrow_mut().remove(key);
        Ok(())
    }
}

fn first_key() -> String {
    profile_key(MIGRATED_ID)
}

/// Settings as a version before profiles left them: an address, and the token tied to it.
fn old_settings() -> Settings {
    let mut s = Settings::default();
    (s.publish_server, s.publish_saved_for, s.publish_binding_settled) = (SITE.into(), Some(SITE.into()), true);
    s
}

fn profile(id: &str, name: &str, server: &str, account: Option<&str>, workspace: Option<&str>) -> Profile {
    Profile { id: id.into(), name: name.into(), server: server.into(), account: account.map(str::to_string), workspace: workspace.map(str::to_string), state: State::Ready, named: true }
}

fn book(list: Vec<Profile>, active: &str) -> Book {
    Book { list, active: Some(active.into()) }
}

// ---- 5. The update that adds profiles ----

#[test]
fn the_old_connection_becomes_the_first_profile_without_a_new_sign_in() {
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD)]);
    let mut s = old_settings();
    assert_eq!(migrate(&mut s, &store), Ok(Some("tours.example.com".into())));
    assert!(s.profiles_migrated);
    assert_eq!(s.profiles, vec![Profile { id: MIGRATED_ID.into(), name: "tours.example.com".into(), server: SITE.into(), account: None, workspace: None, state: State::Ready, named: false }]);
    assert_eq!(s.active_profile.as_deref(), Some(MIGRATED_ID));
    assert_eq!(store.at(&first_key()).as_deref(), Some(OLD), "the token is in the profile's own entry");
    assert_eq!(store.at(LEGACY_KEY).as_deref(), Some(OLD), "the old entry stays until the settings are saved");
    // The app saved the settings: now the old entry goes, and only then.
    assert_eq!(remove_legacy(&s, &store), Ok(true));
    assert_eq!(store.keys(), vec![first_key()]);
    assert_eq!(store.at(&first_key()).as_deref(), Some(OLD));
}

#[test]
fn the_migration_is_safe_to_run_twice() {
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD)]);
    let mut s = old_settings();
    migrate(&mut s, &store).unwrap();
    let after_first = (s.profiles.clone(), s.active_profile.clone());
    let writes = store.log.borrow().len();
    assert_eq!(migrate(&mut s, &store), Ok(None));
    assert_eq!((s.profiles.clone(), s.active_profile.clone()), after_first);
    assert_eq!(store.log.borrow().len(), writes, "a second run writes nothing");
    assert_eq!(remove_legacy(&s, &store), Ok(true));
    assert_eq!(remove_legacy(&s, &store), Ok(false));
    assert_eq!(store.at(&first_key()).as_deref(), Some(OLD));
}

#[test]
fn a_migration_cut_off_before_the_settings_were_saved_is_finished_by_the_next_start() {
    // Cut off after the copy: the settings on disk still say nothing of profiles.
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD)]);
    let on_disk = old_settings();
    let mut lost = on_disk.clone();
    migrate(&mut lost, &store).unwrap();
    let mut s = on_disk.clone();
    assert_eq!(migrate(&mut s, &store), Ok(Some("tours.example.com".into())));
    assert_eq!(s.profiles.len(), 1);
    assert_eq!(store.at(&first_key()).as_deref(), Some(OLD));

    // Cut off with the old entry already gone (a save that was lost after it): the copy is used.
    let store = FakeKeys::with(&[(&first_key(), OLD)]);
    let mut s = on_disk.clone();
    assert_eq!(migrate(&mut s, &store), Ok(Some("tours.example.com".into())));
    assert_eq!((s.profiles.len(), s.active_profile.as_deref()), (1, Some(MIGRATED_ID)));
    assert_eq!(store.at(&first_key()).as_deref(), Some(OLD));

    // The settings were saved, the old entry not yet removed: the next start removes it.
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD), (&first_key(), OLD)]);
    let mut s = on_disk;
    migrate(&mut s, &store).unwrap();
    assert_eq!(migrate(&mut s, &store), Ok(None));
    assert_eq!(remove_legacy(&s, &store), Ok(true));
    assert_eq!(store.keys(), vec![first_key()]);
}

#[test]
fn a_migration_that_cannot_confirm_the_copy_changes_nothing_and_keeps_the_old_token() {
    for store in [FakeKeys { refuse_write: Some(first_key()), ..FakeKeys::with(&[(LEGACY_KEY, OLD)]) }, FakeKeys { garble: true, ..FakeKeys::with(&[(LEGACY_KEY, OLD)]) }, FakeKeys { locked: true, ..FakeKeys::with(&[(LEGACY_KEY, OLD)]) }] {
        let mut s = old_settings();
        let before = s.clone();
        let e = migrate(&mut s, &store).unwrap_err();
        assert!(e.starts_with("Could not use the credential store"), "{e}");
        assert!(!e.contains(OLD));
        assert!(!s.profiles_migrated && s.profiles.is_empty() && s.active_profile.is_none());
        assert_eq!(toml::to_string(&s).unwrap(), toml::to_string(&before).unwrap());
        assert_eq!(store.entries.borrow().get(LEGACY_KEY).map(String::as_str), Some(OLD), "the old token is never lost");
        // The old entry is never removed while there is no confirmed copy.
        assert!(!remove_legacy(&s, &store).unwrap_or(false));
        assert_eq!(store.entries.borrow().get(LEGACY_KEY).map(String::as_str), Some(OLD));
    }
}

#[test]
fn without_an_old_connection_the_migration_makes_no_profile() {
    // No address the token was tied to: nothing to carry over, and the old entry is left alone.
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD)]);
    let mut s = old_settings();
    s.publish_saved_for = Some(String::new());
    assert_eq!(migrate(&mut s, &store), Ok(None));
    assert!(s.profiles_migrated && s.profiles.is_empty() && s.active_profile.is_none());
    assert_eq!(remove_legacy(&s, &store), Ok(false));
    assert_eq!(store.at(LEGACY_KEY).as_deref(), Some(OLD));
    // An address, but no token.
    let store = FakeKeys::default();
    let mut s = old_settings();
    assert_eq!(migrate(&mut s, &store), Ok(None));
    assert!(s.profiles_migrated && s.profiles.is_empty());
    assert!(store.log.borrow().is_empty());
}

#[test]
fn the_settings_file_holds_the_profiles_and_no_token() {
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD)]);
    let mut s = old_settings();
    migrate(&mut s, &store).unwrap();
    let mut b = Book::of(&s);
    b.list.push(Profile { state: State::SignInAgain, ..profile("pabc", "customer1 test", "http://localhost:3000", Some("customer"), Some("customer1")) });
    b.write_to(&mut s);
    let text = toml::to_string_pretty(&s).unwrap();
    assert!(!text.to_ascii_lowercase().contains("token") && !text.contains(OLD) && !text.contains("dp_"), "{text}");
    assert!(text.contains("sign_in_again"), "{text}");
    let back: Settings = toml::from_str(&text).unwrap();
    assert_eq!(Book::of(&back), b);
    assert!(back.profiles_migrated);
    // A file from before profiles reads as not migrated, with none.
    let old: Settings = toml::from_str("publish_server = \"https://tours.example.com\"\n").unwrap();
    assert!(!old.profiles_migrated && old.profiles.is_empty() && old.active_profile.is_none());
}

// ---- 3. Adding a profile ----

fn me_stub(status: u16, body: &'static str) -> Stub {
    stub(move |req, _| if req.line == "GET /api/v1/me" { reply(status, body) } else { reply(404, "") })
}

#[test]
fn the_check_asks_the_site_with_the_new_token_and_any_failure_is_not_saved() {
    let s = me_stub(200, r#"{"workspace":"customer1","version":"0.1.9"}"#);
    let c = checked(&s.base, Some("customer@mythicshot.test".into()), Secret::new(TOKEN_A.into())).unwrap();
    assert_eq!((c.server.as_str(), c.account.as_deref(), c.workspace.as_str()), (s.base.as_str(), Some("customer@mythicshot.test"), "customer1"));
    assert_eq!(s.requests()[0].auth.as_deref(), Some(format!("Bearer {TOKEN_A}").as_str()));
    assert!(!format!("{c:?}").contains(TOKEN_A), "the token is never in a Debug print");
    // Refused, not a Mythicshot site, a web page, no answer at all, an address that is not one.
    let refused = me_stub(401, "");
    let not_a_site = me_stub(404, "<html>");
    let page = me_stub(200, "<html>hello</html>");
    for server in [refused.base.as_str(), not_a_site.base.as_str(), page.base.as_str(), "http://127.0.0.1:1", "http://tours.example.com"] {
        assert_eq!(checked(server, None, Secret::new(TOKEN_A.into())).unwrap_err(), NOT_SAVED, "{server}");
    }
}

#[test]
fn save_profile_stores_the_token_confirms_it_and_only_then_adds_the_profile() {
    let store = FakeKeys::default();
    let mut b = Book::default();
    let id = save_new(&mut b, &store, Candidate::for_test("http://localhost:3000", Some("customer"), "customer1", TOKEN_A), "  customer1 test ").unwrap();
    assert_eq!(b.active.as_deref(), Some(id.as_str()));
    assert_eq!(b.list, vec![Profile { id: id.clone(), name: "customer1 test".into(), server: "http://localhost:3000".into(), account: Some("customer".into()), workspace: Some("customer1".into()), state: State::Ready, named: true }]);
    assert_eq!(store.at(&profile_key(&id)).as_deref(), Some(TOKEN_A));
    assert!(id.starts_with('p') && id.len() == 17 && id != MIGRATED_ID);
    // An empty name is the default one.
    let id2 = save_new(&mut b, &store, Candidate::for_test("https://app.mythicshot.com", None, "customer1", TOKEN_B), "").unwrap();
    assert_ne!(id, id2);
    assert_eq!(b.get(&id2).unwrap().name, "customer1 · mythicshot.com");
}

#[test]
fn a_save_the_store_cannot_confirm_leaves_no_profile_and_no_token() {
    let mut b = Book::default();
    for store in [FakeKeys { garble: true, ..FakeKeys::default() }, FakeKeys { locked: true, ..FakeKeys::default() }] {
        let e = save_new(&mut b, &store, Candidate::for_test(SITE, None, "Acme", TOKEN_A), "Acme").unwrap_err();
        assert!(e.starts_with("Could not use the credential store") && !e.contains(TOKEN_A), "{e}");
        assert!(b.list.is_empty() && b.active.is_none());
        assert!(store.entries.borrow().is_empty(), "a token stored half-way is removed again");
    }
}

#[test]
fn a_second_profile_for_the_same_site_and_account_is_not_made() {
    let store = FakeKeys::default();
    let mut b = book(vec![profile("pa", "Alex · mythicshot.com", "https://app.mythicshot.com", Some("alex"), Some("alex's workspace"))], "pa");
    let c = Candidate::for_test("https://app.mythicshot.com/", Some("alex"), "alex's workspace", TOKEN_A);
    assert_eq!(find_same(&b, &c).map(|p| p.id.as_str()), Some("pa"));
    assert_eq!(save_new(&mut b, &store, c, "again").unwrap_err(), exists("Alex · mythicshot.com"));
    assert_eq!(b.list.len(), 1);
    assert!(store.entries.borrow().is_empty());
    // Another account, another workspace or another site is another profile.
    for c in [
        Candidate::for_test("https://app.mythicshot.com", Some("customer"), "alex's workspace", TOKEN_A),
        Candidate::for_test("https://app.mythicshot.com", Some("alex"), "customer1", TOKEN_A),
        Candidate::for_test("http://localhost:3000", Some("alex"), "alex's workspace", TOKEN_A),
    ] {
        assert!(find_same(&b, &c).is_none(), "{c:?}");
    }
}

#[test]
fn a_default_name_is_the_workspace_and_the_site() {
    assert_eq!(default_name("customer1", "https://app.mythicshot.com"), "customer1 · mythicshot.com");
    assert_eq!(default_name("Acme", "https://www.acme.com/tours/"), "Acme · acme.com");
    assert_eq!(default_name("Sandbox", "http://localhost:3000"), "Sandbox · localhost:3000");
    assert_eq!(default_name("Me", "https://app.io"), "Me · app.io");
    assert_eq!(clean_name(&"x".repeat(100)).chars().count(), NAME_MAX);
    assert_eq!(clean_name(" a\u{202e}b\n "), "ab");
    assert_eq!(profile("p1", "n", "https://app.mythicshot.com", None, Some("alex's workspace")).detail(), "alex's workspace · app.mythicshot.com");
    assert_eq!(profile("p1", "n", "http://localhost:3000", None, None).detail(), "localhost:3000");
}

// ---- 4. A token that stops working ----

#[test]
fn a_refused_profile_is_kept_until_a_sign_in_for_the_same_account_passes() {
    let store = FakeKeys::with(&[("profile-pc", OLD)]);
    let mut b = book(vec![profile("pc", "customer1 test", "http://localhost:3000", Some("customer"), Some("customer1"))], "pc");
    assert!(mark_refused(&mut b, "pc"));
    assert!(!mark_refused(&mut b, "pc"), "said once");
    assert_eq!(b.get("pc").unwrap().state, State::SignInAgain);
    // Nothing of it is handed out meanwhile.
    let pin = Pin { profile: Some("pc".into()), server: "http://localhost:3000".into() };
    assert_eq!(token_for(&b, &store, &pin).unwrap_err(), needs_sign_in("customer1 test"));
    // Another account: saved over nothing, the old profile stays as it was.
    let other = Candidate::for_test("http://localhost:3000", Some("intruder"), "customer1", TOKEN_B);
    match renew(&mut b, &store, "pc", other).unwrap() {
        Renewal::OtherAccount(c) => assert_eq!(c.account.as_deref(), Some("intruder")),
        Renewal::Renewed => panic!("another account renewed the profile"),
    }
    assert_eq!((b.get("pc").unwrap().state, store.at("profile-pc").as_deref()), (State::SignInAgain, Some(OLD)));
    // The same account: its new token is kept and the profile works again.
    let same = Candidate::for_test("http://localhost:3000/", Some("customer"), "customer1", TOKEN_A);
    assert!(matches!(renew(&mut b, &store, "pc", same).unwrap(), Renewal::Renewed));
    assert_eq!(b.get("pc").unwrap().state, State::Ready);
    assert_eq!(token_for(&b, &store, &pin).as_deref(), Ok(TOKEN_A));
}

#[test]
fn a_renewal_the_store_cannot_confirm_keeps_the_profile_waiting() {
    let store = FakeKeys { garble: true, ..FakeKeys::with(&[("profile-pc", OLD)]) };
    let mut b = book(vec![Profile { state: State::SignInAgain, ..profile("pc", "c", SITE, None, Some("Acme")) }], "pc");
    assert!(renew(&mut b, &store, "pc", Candidate::for_test(SITE, None, "Acme", TOKEN_A)).is_err());
    assert_eq!(b.get("pc").unwrap().state, State::SignInAgain);
}

#[test]
fn the_migrated_profile_takes_its_workspace_and_name_from_the_first_answer() {
    let mut b = book(vec![Profile { id: MIGRATED_ID.into(), name: "mythicshot.com".into(), server: "https://app.mythicshot.com".into(), account: None, workspace: None, state: State::Ready, named: false }], MIGRATED_ID);
    assert!(note_workspace(&mut b, MIGRATED_ID, "Alex"));
    assert_eq!((b.active().unwrap().name.as_str(), b.active().unwrap().workspace.as_deref()), ("Alex · mythicshot.com", Some("Alex")));
    assert!(!note_workspace(&mut b, MIGRATED_ID, "Alex"));
    // A name once given stays when the workspace is renamed.
    assert!(note_workspace(&mut b, MIGRATED_ID, "Alex & co"));
    assert_eq!(b.active().unwrap().name, "Alex · mythicshot.com");
    // Before the site ever answered, the first sign-in on its site is its account.
    let mut fresh = book(vec![Profile { state: State::SignInAgain, ..profile(MIGRATED_ID, "tours.example.com", SITE, None, None) }], MIGRATED_ID);
    let store = FakeKeys::default();
    assert!(matches!(renew(&mut fresh, &store, MIGRATED_ID, Candidate::for_test(SITE, Some("alex"), "Acme", TOKEN_A)).unwrap(), Renewal::Renewed));
    assert_eq!(fresh.active().unwrap().account.as_deref(), Some("alex"));
}

#[test]
fn sign_out_removes_the_token_and_keeps_the_profile_waiting_for_a_sign_in() {
    let store = FakeKeys::with(&[("profile-pa", TOKEN_A)]);
    let mut b = book(vec![profile("pa", "A", SITE, None, Some("Acme"))], "pa");
    sign_out(&mut b, &store, "pa").unwrap();
    assert!(store.entries.borrow().is_empty());
    assert_eq!((b.list.len(), b.get("pa").unwrap().state), (1, State::SignInAgain));
}

// ---- 7. Removing a profile ----

#[test]
fn remove_deletes_only_that_profile_and_its_token_and_picks_another() {
    let store = FakeKeys::with(&[("profile-pa", TOKEN_A), ("profile-pb", TOKEN_B), ("profile-pc", OLD)]);
    let mut b = book(
        vec![
            Profile { state: State::SignInAgain, ..profile("pc", "C", SITE, None, None) },
            profile("pa", "A", SITE, None, None),
            profile("pb", "B", "http://localhost:3000", None, None),
        ],
        "pa",
    );
    remove(&mut b, &store, "pa").unwrap();
    assert_eq!(store.keys(), vec!["profile-pb".to_string(), "profile-pc".to_string()]);
    assert_eq!(b.active.as_deref(), Some("pb"), "one that can be used comes first");
    remove(&mut b, &store, "pc").unwrap();
    assert_eq!(b.active.as_deref(), Some("pb"), "removing another leaves the active one");
    remove(&mut b, &store, "pb").unwrap();
    assert_eq!((b.list.len(), b.active.as_deref()), (0, None), "no profile: as when not connected");
    assert!(store.entries.borrow().is_empty());
    assert!(store.log.borrow().iter().all(|l| l.starts_with("delete profile-")));
}

#[test]
fn removing_the_migrated_profile_takes_what_is_left_of_the_old_entry_too() {
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD), (&first_key(), OLD)]);
    let mut b = book(vec![profile(MIGRATED_ID, "A", SITE, None, None)], MIGRATED_ID);
    remove(&mut b, &store, MIGRATED_ID).unwrap();
    assert!(store.entries.borrow().is_empty());
}

#[test]
fn a_profile_whose_token_cannot_be_removed_is_kept() {
    let store = FakeKeys { refuse_delete: true, ..FakeKeys::with(&[("profile-pa", TOKEN_A)]) };
    let mut b = book(vec![profile("pa", "A", SITE, None, None)], "pa");
    assert!(remove(&mut b, &store, "pa").is_err());
    assert_eq!((b.list.len(), b.active.as_deref()), (1, Some("pa")));
}

// ---- 8. A token goes to its own profile's site only ----

#[test]
fn a_token_is_handed_out_only_for_its_own_profiles_address() {
    let store = FakeKeys::with(&[("profile-pa", TOKEN_A), ("profile-pb", TOKEN_B)]);
    let b = book(vec![profile("pa", "A", "https://a.example.com", None, None), profile("pb", "B", "https://b.example.com", None, None)], "pb");
    let pin = |id: &str, server: &str| Pin { profile: Some(id.into()), server: server.into() };
    assert_eq!(token_for(&b, &store, &pin("pa", "https://a.example.com/")).as_deref(), Ok(TOKEN_A));
    assert_eq!(token_for(&b, &store, &pin("pb", "https://b.example.com")).as_deref(), Ok(TOKEN_B));
    for (id, server) in [("pa", "https://b.example.com"), ("pb", "https://a.example.com"), ("pa", "http://a.example.com"), ("pa", ""), ("gone", "https://a.example.com")] {
        let e = token_for(&b, &store, &pin(id, server)).unwrap_err();
        assert!(!e.contains("dp_"), "{e}");
    }
    assert!(token_for(&b, &store, &Pin { profile: None, server: "https://a.example.com".into() }).is_err());
}

#[test]
fn a_job_pinned_to_one_profile_sends_its_token_to_that_site_only_after_a_switch() {
    let site_a = me_stub(200, r#"{"workspace":"A"}"#);
    let site_b = me_stub(200, r#"{"workspace":"B"}"#);
    let store = FakeKeys::with(&[("profile-pa", TOKEN_A), ("profile-pb", TOKEN_B)]);
    let mut b = book(vec![profile("pa", "A", &site_a.base, None, None), profile("pb", "B", &site_b.base, None, None)], "pa");
    // The job starts on A …
    let pin = Pin { profile: b.active.clone(), server: b.active().unwrap().server.clone() };
    // … the user switches to B while it runs …
    assert!(b.switch("pb"));
    // … and the job's request still goes to A, with A's token.
    let token = token_for(&b, &store, &pin).unwrap();
    publish::workspace(&pin.server, &token).unwrap();
    assert_eq!(site_a.requests().iter().map(|r| r.auth.clone()).collect::<Vec<_>>(), vec![Some(format!("Bearer {TOKEN_A}"))]);
    assert!(site_b.requests().is_empty(), "B's site never sees A's token");
    // A pin that names A with B's address gets no token at all.
    let crossed = Pin { profile: Some("pa".into()), server: site_b.base.clone() };
    assert!(token_for(&b, &store, &crossed).is_err());
    assert!(site_b.requests().is_empty());
}

// ---- 6. Whose tour a recording belongs to ----

#[test]
fn a_recording_belongs_to_the_profile_it_was_uploaded_with_and_old_ones_to_the_first() {
    let b = book(vec![profile(MIGRATED_ID, "Alex · mythicshot.com", SITE, None, None), profile("pb", "Sandbox admin", "http://localhost:3000", None, None)], "pb");
    assert_eq!(owner_of(&b, "pb"), Owner::Active);
    assert_eq!(owner_of(&b, MIGRATED_ID), Owner::Other { id: MIGRATED_ID.into(), name: "Alex · mythicshot.com".into() });
    assert_eq!(owner_of(&b, ""), Owner::Other { id: MIGRATED_ID.into(), name: "Alex · mythicshot.com".into() }, "linked before profiles");
    assert_eq!(owner_of(&b, "pgone"), Owner::Removed);
    let mut first = b.clone();
    first.switch(MIGRATED_ID);
    assert_eq!(owner_of(&first, ""), Owner::Active);
    assert_eq!(owner_of(&Book::default(), "pb"), Owner::NoProfile);
}

#[test]
fn the_sentences_are_the_approved_ones() {
    assert_eq!(connected(Some("customer@mythicshot.test"), "customer1"), "Connected. Signed in as customer@mythicshot.test, workspace \"customer1\".");
    assert_eq!(exists("Alex · mythicshot.com"), "You already have a profile for this account: \"Alex · mythicshot.com\".");
    assert_eq!(needs_sign_in("customer1 test"), "\"customer1 test\" needs you to sign in again. Nothing can be uploaded with it until you do.");
    assert_eq!(other_profile("Sandbox admin"), "This recording belongs to a tour in \"Sandbox admin\". Switch to that profile to update it, or upload it here as a new tour.");
    assert_eq!(switch_to("Sandbox admin"), "Switch to Sandbox admin");
    assert_eq!(remove_question("Sandbox admin"), "Remove \"Sandbox admin\" from this computer? Your tours and account on the site are not touched.");
    assert_eq!(migrated("Alex · mythicshot.com"), "Your existing connection is now the profile \"Alex · mythicshot.com\".");
}
