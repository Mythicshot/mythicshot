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

/// The identity today's sites give: the workspace's name only.
pub(crate) fn named(workspace: &str) -> Identity {
    Identity { workspace: format!("name:{workspace}"), account: None }
}

/// The identity a site that sends ids gives.
pub(crate) fn ided(workspace_id: &str, account_id: &str) -> Identity {
    Identity { workspace: format!("id:{workspace_id}"), account: Some(format!("id:{account_id}")) }
}

const WS_ID: &str = "ws_0123456789abcdef01234567";

/// A profile; its identity is recorded from the names when the workspace is given.
fn profile(id: &str, name: &str, server: &str, account: Option<&str>, workspace: Option<&str>) -> Profile {
    Profile { id: id.into(), name: name.into(), server: server.into(), account: account.map(str::to_string), workspace: workspace.map(str::to_string), identity: workspace.map(named), state: State::Ready, named: true }
}

fn book(list: Vec<Profile>, active: &str) -> Book {
    Book { list, active: Some(active.into()), legacy: None }
}

fn ws(name: &str, id: Option<&str>, account: Option<&str>) -> Workspace {
    Workspace { name: name.into(), version: None, id: id.map(str::to_string), account: account.map(str::to_string) }
}

// ---- 5. The update that adds profiles ----

#[test]
fn the_old_connection_becomes_the_first_profile_without_a_new_sign_in() {
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD)]);
    let mut s = old_settings();
    assert_eq!(migrate(&mut s, &store), Ok(Some("tours.example.com".into())));
    assert!(s.profiles_migrated);
    assert_eq!(s.profiles, vec![Profile { id: MIGRATED_ID.into(), name: "tours.example.com".into(), server: SITE.into(), account: None, workspace: None, identity: None, state: State::Ready, named: false }]);
    assert_eq!((s.active_profile.as_deref(), s.legacy_profile.as_deref()), (Some(MIGRATED_ID), Some(MIGRATED_ID)));
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
        // The old entry is never removed while the migration has not run.
        assert!(!remove_legacy(&s, &store).unwrap_or(false));
        assert_eq!(store.entries.borrow().get(LEGACY_KEY).map(String::as_str), Some(OLD));
    }
}

#[test]
fn without_an_old_connection_no_profile_is_made_and_an_unusable_old_entry_goes() {
    // No address the token was tied to: nothing to carry over, and the old entry, which can
    // never be used, goes once the settings were saved.
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD)]);
    let mut s = old_settings();
    s.publish_saved_for = Some(String::new());
    assert_eq!(migrate(&mut s, &store), Ok(None));
    assert!(s.profiles_migrated && s.profiles.is_empty() && s.active_profile.is_none() && s.legacy_profile.is_none());
    assert_eq!(remove_legacy(&s, &store), Ok(true));
    assert!(store.entries.borrow().is_empty());
    // An address, but no token.
    let store = FakeKeys::default();
    let mut s = old_settings();
    assert_eq!(migrate(&mut s, &store), Ok(None));
    assert!(s.profiles_migrated && s.profiles.is_empty() && s.legacy_profile.is_none());
    assert_eq!(remove_legacy(&s, &store), Ok(false));
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
    assert!(!old.profiles_migrated && old.profiles.is_empty() && old.active_profile.is_none() && old.legacy_profile.is_none());
}

// ---- 10. A migration that turns out to be an account another profile has ----

#[test]
fn the_first_profile_is_joined_to_a_profile_of_the_same_identity() {
    let store = FakeKeys::with(&[(LEGACY_KEY, OLD), (&first_key(), OLD), ("profile-pa", TOKEN_A)]);
    let mut b = Book {
        list: vec![
            Profile { identity: None, workspace: None, named: false, ..profile(MIGRATED_ID, "tours.example.com", SITE, None, None) },
            Profile { identity: Some(ided(WS_ID, "42")), ..profile("pa", "Acme", SITE, Some("alex"), Some("Acme")) },
        ],
        active: Some(MIGRATED_ID.into()),
        legacy: Some(MIGRATED_ID.into()),
    };
    // Before its first check nothing can be said: no identity, no twin.
    assert_eq!(twin_of(&b, MIGRATED_ID), None);
    // An answer that names the workspace only: still not enough.
    note_check(&mut b, MIGRATED_ID, SITE, &ws("Acme", None, None));
    assert_eq!(twin_of(&b, MIGRATED_ID), None);
    // One from a site that sends ids, naming the same account: it is the same, and is joined to it.
    note_check(&mut b, MIGRATED_ID, SITE, &ws("Acme", Some(WS_ID), Some("42")));
    assert_eq!(twin_of(&b, MIGRATED_ID).as_deref(), Some("pa"));
    attach(&mut b, &store, MIGRATED_ID, "pa").unwrap();
    assert_eq!(b.list.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["pa"]);
    assert_eq!((b.active.as_deref(), b.legacy.as_deref()), (Some("pa"), Some("pa")));
    assert_eq!(store.keys(), vec!["profile-pa".to_string()], "the copy and the old entry go");
    // Recordings linked before profiles belong to it now.
    assert_eq!(owner_of(&b, ""), Owner::Active);
}

// ---- 3. Adding a profile ----

fn me_stub(status: u16, body: &'static str) -> Stub {
    stub(move |req, _| if req.line == "GET /api/v1/me" { reply(status, body) } else { reply(404, "") })
}

fn signed_in(name: &str) -> SignedIn {
    SignedIn { name: Some(name.into()) }
}

#[test]
fn the_check_asks_the_site_with_the_new_token_and_says_why_it_failed() {
    let s = me_stub(200, r#"{"workspace":"customer1","version":"0.1.9"}"#);
    let c = checked(&s.base, signed_in("customer@mythicshot.test"), Secret::new(TOKEN_A.into())).unwrap();
    assert_eq!((c.server.as_str(), c.account.as_deref(), c.workspace.as_str()), (s.base.as_str(), Some("customer@mythicshot.test"), "customer1"));
    assert_eq!(c.identity, named("customer1"), "today's sites: the workspace's name only, never the user name");
    assert_eq!(s.requests()[0].auth.as_deref(), Some(format!("Bearer {TOKEN_A}").as_str()));
    assert!(!format!("{c:?}").contains(TOKEN_A), "the token is never in a Debug print");
    // Only a refusal says the site did not accept it.
    let refused = me_stub(401, "");
    assert_eq!(checked(&refused.base, SignedIn::default(), Secret::new(TOKEN_A.into())).unwrap_err(), NOT_SAVED);
    // Anything else says what the app says of that failure, after the first sentence.
    let not_a_site = me_stub(404, "<html>");
    let page = me_stub(200, "<html>hello</html>");
    for (server, rest) in [
        (not_a_site.base.as_str(), "Could not check the server: the server answered HTTP 404."),
        (page.base.as_str(), "Could not check the server: the server's answer was not understood."),
        ("http://tours.example.com", "The server address must start with https:// (http:// is only for localhost)."),
    ] {
        let e = checked(server, SignedIn::default(), Secret::new(TOKEN_A.into())).unwrap_err();
        assert_eq!(e, format!("{NOT_SAVED_LEAD} {rest}"), "{server}");
        assert!(!e.contains(NOT_ACCEPTED) && !e.contains("dp_"));
    }
    let e = checked("http://127.0.0.1:1", SignedIn::default(), Secret::new(TOKEN_A.into())).unwrap_err();
    assert!(e.starts_with(&format!("{NOT_SAVED_LEAD} Could not check the server: ")) && !e.contains(NOT_ACCEPTED), "{e}");
}

#[test]
fn the_identity_is_the_sites_ids_when_it_gives_them() {
    // The coming platform version: `workspace_id` and `account_id` beside the workspace's name.
    let s = me_stub(200, r#"{"workspace":"customer1","version":"0.2.0","workspace_id":"ws_0123456789abcdef01234567","account_id":"acc-9f"}"#);
    let c = checked(&s.base, signed_in("customer"), Secret::new(TOKEN_A.into())).unwrap();
    assert_eq!(c.identity, ided(WS_ID, "acc-9f"));
    assert_eq!((c.workspace.as_str(), c.account.as_deref()), ("customer1", Some("customer")));
    // Today's answer, with its addresses: the name only.
    let s = me_stub(200, r#"{"workspace":"customer1","version":"0.1.9","addresses":{"tours":"https://t.example.com","app":"https://a.example.com"}}"#);
    assert_eq!(checked(&s.base, signed_in("customer"), Secret::new(TOKEN_A.into())).unwrap().identity, named("customer1"));
    // An account id without a workspace id is not taken: the name decides then.
    let s = me_stub(200, r#"{"workspace":"customer1","account_id":"acc-9f"}"#);
    assert_eq!(checked(&s.base, SignedIn::default(), Secret::new(TOKEN_A.into())).unwrap().identity, named("customer1"));
}

#[test]
fn save_profile_stores_the_token_confirms_it_and_only_then_adds_the_profile() {
    let store = FakeKeys::default();
    let mut b = Book::default();
    let id = save_new(&mut b, &store, Candidate::for_test("http://localhost:3000", Some("customer"), "customer1", TOKEN_A), "  customer1 test ").unwrap();
    assert_eq!(b.active.as_deref(), Some(id.as_str()));
    assert_eq!(b.list, vec![Profile { id: id.clone(), name: "customer1 test".into(), server: "http://localhost:3000".into(), account: Some("customer".into()), workspace: Some("customer1".into()), identity: Some(named("customer1")), state: State::Ready, named: true }]);
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
fn a_second_profile_for_the_same_site_and_account_is_not_made_once_the_site_sends_ids() {
    let store = FakeKeys::default();
    let app = "https://app.mythicshot.com";
    let mut b = book(vec![Profile { identity: Some(ided(WS_ID, "acc-alex")), ..profile("pa", "Alex · mythicshot.com", app, Some("alex"), Some("alex's workspace")) }], "pa");
    let c = Candidate::with_ids("https://app.mythicshot.com/", "alex's workspace", WS_ID, "acc-alex", TOKEN_A);
    assert_eq!(find_same(&b, &c).map(|p| p.id.as_str()), Some("pa"));
    assert_eq!(save_new(&mut b, &store, c, "again").unwrap_err(), exists("Alex · mythicshot.com"));
    assert_eq!(b.list.len(), 1);
    assert!(store.entries.borrow().is_empty());
    // Another account, another workspace or another site is another profile; and the same names
    // without ids never match an identity that has ids.
    for c in [
        Candidate::with_ids(app, "alex's workspace", WS_ID, "acc-customer", TOKEN_A),
        Candidate::with_ids(app, "alex's workspace", "ws_ffffffffffffffffffffffff", "acc-alex", TOKEN_A),
        Candidate::with_ids("http://localhost:3000", "alex's workspace", WS_ID, "acc-alex", TOKEN_A),
        Candidate::for_test(app, Some("alex"), "alex's workspace", TOKEN_A),
    ] {
        assert!(find_same(&b, &c).is_none(), "{c:?}");
    }
}

#[test]
fn two_accounts_of_one_site_are_two_profiles() {
    let store = FakeKeys::default();
    let mut b = Book::default();
    let a = save_new(&mut b, &store, Candidate::for_test(SITE, Some("alex"), "Acme", TOKEN_A), "").unwrap();
    let c = save_new(&mut b, &store, Candidate::for_test(SITE, Some("customer"), "Acme", TOKEN_B), "").unwrap();
    assert_ne!(a, c);
    assert_eq!(b.list.len(), 2);
    assert_eq!((store.at(&profile_key(&a)).as_deref(), store.at(&profile_key(&c)).as_deref()), (Some(TOKEN_A), Some(TOKEN_B)));
    // Today's sites cannot say who an account is: the same account again is a third profile,
    // never claimed as a match.
    let again = Candidate::for_test(SITE, Some("alex"), "Acme", TOKEN_A);
    assert!(find_same(&b, &again).is_none());
    save_new(&mut b, &store, again, "").unwrap();
    assert_eq!(b.list.len(), 3);
}

#[test]
fn a_profile_without_a_full_identity_is_never_matched_by_a_new_sign_in() {
    let store = FakeKeys::with(&[(&first_key(), OLD)]);
    // The migrated profile before any check, and one whose site never named the account.
    for identity in [None, Some(named("Acme"))] {
        let mut b = book(vec![Profile { identity: identity.clone(), state: State::SignInAgain, ..profile(MIGRATED_ID, "Alex", SITE, None, None) }], MIGRATED_ID);
        let c = Candidate::for_test(SITE, Some("alex"), "Acme", TOKEN_A);
        assert!(find_same(&b, &c).is_none(), "{identity:?}");
        // "Add profile" makes a profile of its own; the waiting one keeps its token.
        let id = save_new(&mut b, &store, c, "").unwrap();
        assert_ne!(id, MIGRATED_ID);
        assert_eq!(store.at(&first_key()).as_deref(), Some(OLD));
        assert_eq!(b.get(MIGRATED_ID).unwrap().state, State::SignInAgain);
    }
    // Its own "Sign in again" renews it and records the identity.
    let store = FakeKeys::with(&[(&first_key(), OLD)]);
    let mut b = book(vec![Profile { identity: Some(named("Acme")), state: State::SignInAgain, ..profile(MIGRATED_ID, "Alex", SITE, None, None) }], MIGRATED_ID);
    assert!(matches!(renew(&mut b, &store, MIGRATED_ID, Candidate::for_test(SITE, Some("alex"), "Acme", TOKEN_A)).unwrap(), Renewal::Renewed));
    assert_eq!(b.get(MIGRATED_ID).unwrap().identity, Some(named("Acme")));
    // Only in its own workspace.
    let mut b = book(vec![Profile { identity: Some(named("Acme")), state: State::SignInAgain, ..profile(MIGRATED_ID, "Alex", SITE, None, None) }], MIGRATED_ID);
    assert!(matches!(renew(&mut b, &store, MIGRATED_ID, Candidate::for_test(SITE, Some("alex"), "Other", TOKEN_A)).unwrap(), Renewal::OtherAccount(_)));
}

#[test]
fn a_check_with_the_profiles_own_token_records_its_identity_and_only_for_that_profile() {
    let mut b = book(vec![Profile { identity: None, workspace: None, named: false, ..profile(MIGRATED_ID, "mythicshot.com", "https://app.mythicshot.com", None, None) }, profile("pb", "B", "https://app.mythicshot.com", Some("b"), Some("B ws"))], "pb");
    let pb = b.get("pb").unwrap().clone();
    // The answer to a request made for the first profile, arriving while B is active.
    assert!(note_check(&mut b, MIGRATED_ID, "https://app.mythicshot.com", &ws("Alex", Some("w1"), Some("a1"))));
    let first = b.get(MIGRATED_ID).unwrap();
    assert_eq!((first.name.as_str(), first.identity.clone()), ("Alex · mythicshot.com", Some(Identity { workspace: "id:w1".into(), account: Some("id:a1".into()) })));
    assert_eq!(b.get("pb"), Some(&pb), "the active profile is not touched");
    // An answer from another address than the profile's changes nothing.
    assert!(!note_check(&mut b, "pb", "https://other.example.com", &ws("X", Some("x"), None)));
    // A later answer without the account keeps the recorded one; another workspace drops it.
    assert!(!note_check(&mut b, MIGRATED_ID, "https://app.mythicshot.com", &ws("Alex", Some("w1"), None)));
    note_check(&mut b, MIGRATED_ID, "https://app.mythicshot.com", &ws("Alex", Some("w2"), None));
    assert_eq!(b.get(MIGRATED_ID).unwrap().identity, Some(Identity { workspace: "id:w2".into(), account: None }));
    // A name once given stays when the workspace is renamed.
    note_check(&mut b, MIGRATED_ID, "https://app.mythicshot.com", &ws("Alex & co", Some("w2"), None));
    assert_eq!(b.get(MIGRATED_ID).unwrap().name, "Alex · mythicshot.com");
}

#[test]
fn todays_sites_renew_by_the_workspace_name_and_anything_else_is_another_account() {
    let store = FakeKeys::with(&[("profile-pc", OLD)]);
    let waiting = || book(vec![Profile { state: State::SignInAgain, ..profile("pc", "c", SITE, Some("customer"), Some("Acme")) }], "pc");
    // Another workspace name: another account, nothing stored.
    let mut b = waiting();
    assert!(matches!(renew(&mut b, &store, "pc", Candidate::for_test(SITE, Some("customer"), "Other", TOKEN_A)).unwrap(), Renewal::OtherAccount(_)));
    assert_eq!(store.at("profile-pc").as_deref(), Some(OLD));
    // The same workspace name, whoever signs in (today's sites cannot say more): renewed.
    let mut b = waiting();
    assert!(matches!(renew(&mut b, &store, "pc", Candidate::for_test(SITE, Some("someone"), "Acme", TOKEN_A)).unwrap(), Renewal::Renewed));
    assert_eq!(b.get("pc").unwrap().identity, Some(named("Acme")));
}

#[test]
fn a_site_that_starts_sending_ids_upgrades_a_name_based_identity_in_place() {
    // At the next check with its own token.
    let mut b = book(vec![profile("pa", "Acme", SITE, Some("alex"), Some("Acme")), profile("pb", "Acme too", SITE, Some("bo"), Some("Acme"))], "pa");
    assert!(note_check(&mut b, "pa", SITE, &ws("Acme", Some(WS_ID), Some("acc-alex"))));
    assert_eq!((b.get("pa").unwrap().identity.clone(), b.get("pa").unwrap().state), (Some(ided(WS_ID, "acc-alex")), State::Ready));
    // A later answer without ids (an older copy of the site) never downgrades it.
    assert!(!note_check(&mut b, "pa", SITE, &ws("Acme", None, None)));
    assert_eq!(b.get("pa").unwrap().identity, Some(ided(WS_ID, "acc-alex")));
    // A renewal against a name-based identity compares the workspace name of the same answer,
    // and records the ids on a match.
    let store = FakeKeys::with(&[("profile-pb", OLD)]);
    mark_refused(&mut b, "pb");
    assert!(matches!(renew(&mut b, &store, "pb", Candidate::with_ids(SITE, "Acme", WS_ID, "acc-bo", TOKEN_B)).unwrap(), Renewal::Renewed));
    assert_eq!(b.get("pb").unwrap().identity, Some(ided(WS_ID, "acc-bo")));
    let mut c = b.clone();
    mark_refused(&mut c, "pb");
    let renamed = Profile { identity: Some(named("Old name")), ..c.get("pb").unwrap().clone() };
    c.list[1] = renamed;
    assert!(matches!(renew(&mut c, &store, "pb", Candidate::with_ids(SITE, "Acme", WS_ID, "acc-bo", TOKEN_B)).unwrap(), Renewal::OtherAccount(_)));
}

#[test]
fn two_profiles_that_turn_out_to_be_one_account_stay_two_and_keep_their_own_tours() {
    let mut b = book(vec![profile("pa", "A", SITE, None, Some("Acme")), profile("pb", "B", SITE, None, Some("Acme"))], "pa");
    note_check(&mut b, "pa", SITE, &ws("Acme", Some(WS_ID), Some("acc-1")));
    note_check(&mut b, "pb", SITE, &ws("Acme", Some(WS_ID), Some("acc-1")));
    assert_eq!(b.list.len(), 2, "no automatic merge");
    assert_eq!(b.get("pa").unwrap().identity, b.get("pb").unwrap().identity);
    // A recording of B's tour is still refused while A is active, by profile id.
    assert_eq!(owner_of(&b, "pb"), Owner::Other { id: "pb".into(), name: "B".into() });
    // A new sign-in into that account names the first of them.
    assert_eq!(find_same(&b, &Candidate::with_ids(SITE, "Acme", WS_ID, "acc-1", TOKEN_A)).map(|p| p.id.as_str()), Some("pa"));
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
    let mut b = book(vec![Profile { identity: Some(ided(WS_ID, "acc-customer")), ..profile("pc", "customer1 test", "http://localhost:3000", Some("customer"), Some("customer1")) }], "pc");
    assert!(mark_refused(&mut b, "pc"));
    assert!(!mark_refused(&mut b, "pc"), "said once");
    assert_eq!(b.get("pc").unwrap().state, State::SignInAgain);
    // Nothing of it is handed out meanwhile.
    let pin = Pin { profile: Some("pc".into()), server: "http://localhost:3000".into() };
    assert_eq!(token_for(&b, &store, &pin).unwrap_err(), needs_sign_in("customer1 test"));
    // Another account, even in the same workspace: saved over nothing.
    let other = Candidate::with_ids("http://localhost:3000", "customer1", WS_ID, "acc-intruder", TOKEN_B);
    match renew(&mut b, &store, "pc", other).unwrap() {
        Renewal::OtherAccount(c) => assert_eq!(c.identity, ided(WS_ID, "acc-intruder")),
        Renewal::Renewed => panic!("another account renewed the profile"),
    }
    // An answer without ids never matches ids by name.
    assert!(matches!(renew(&mut b, &store, "pc", Candidate::for_test("http://localhost:3000", Some("customer"), "customer1", TOKEN_B)).unwrap(), Renewal::OtherAccount(_)));
    assert_eq!((b.get("pc").unwrap().state, store.at("profile-pc").as_deref()), (State::SignInAgain, Some(OLD)));
    // Exactly the same identity: its new token is kept and the profile works again.
    let same = Candidate::with_ids("http://localhost:3000/", "customer1", WS_ID, "acc-customer", TOKEN_A);
    assert!(matches!(renew(&mut b, &store, "pc", same).unwrap(), Renewal::Renewed));
    assert_eq!(b.get("pc").unwrap().state, State::Ready);
    assert_eq!(token_for(&b, &store, &pin).as_deref(), Ok(TOKEN_A));
}

#[test]
fn a_renewal_the_store_cannot_confirm_keeps_the_profile_waiting() {
    let store = FakeKeys { garble: true, ..FakeKeys::with(&[("profile-pc", OLD)]) };
    let mut b = book(vec![Profile { state: State::SignInAgain, ..profile("pc", "c", SITE, Some("a"), Some("Acme")) }], "pc");
    assert!(renew(&mut b, &store, "pc", Candidate::for_test(SITE, Some("a"), "Acme", TOKEN_A)).is_err());
    assert_eq!(b.get("pc").unwrap().state, State::SignInAgain);
}

#[test]
fn sign_out_removes_the_token_and_keeps_the_profile_waiting_for_a_sign_in() {
    let store = FakeKeys::with(&[("profile-pa", TOKEN_A)]);
    let mut b = book(vec![profile("pa", "A", SITE, None, Some("Acme"))], "pa");
    sign_out(&mut b, &store, "pa").unwrap();
    assert!(store.entries.borrow().is_empty());
    assert_eq!((b.list.len(), b.get("pa").unwrap().state), (1, State::SignInAgain));
}

// ---- 1. A refusal belongs to the profile its token was looked up for ----

/// A and B, with A's token looked up and refused by the site.
fn refused_a() -> (Book, Live, impl Fn(&str) -> bool) {
    let b = book(vec![profile("pa", "A", SITE, Some("a"), Some("Acme")), profile("pb", "B", "http://localhost:3000", Some("b"), Some("Bee"))], "pa");
    let mut live = Live::default();
    let (generation, profile) = live.begin(&b);
    assert!(live.finish(generation, profile, Some("fp-a".into())));
    (b, live, |fp: &str| fp == "fp-a")
}

#[test]
fn a_refusal_of_one_profiles_token_never_lands_on_the_profile_switched_to() {
    let (mut b, mut live, refused) = refused_a();
    assert!(live.active_refused(&b, &refused));
    // The switch, and in the same frame the page asks whether the active profile is refused.
    b.switch("pb");
    live.begin(&b);
    assert!(!live.active_refused(&b, &refused));
    assert!(!live.attribute(&mut b, &refused));
    assert_eq!(b.get("pb").unwrap().state, State::Ready);
    // A lookup for A that ends after the switch is dropped, not taken for B's.
    let (mut b2, mut live2, refused2) = refused_a();
    let (old_generation, old_profile) = (live2.generation, Some("pa".to_string()));
    b2.switch("pb");
    let (generation, profile) = live2.begin(&b2);
    assert!(!live2.finish(old_generation, old_profile, Some("fp-a".into())));
    assert!(live2.finish(generation, profile, Some("fp-b".into())));
    assert!(!live2.attribute(&mut b2, &refused2));
    assert_eq!(b2.get("pb").unwrap().state, State::Ready);
}

#[test]
fn a_refusal_lands_on_its_own_profile_only() {
    let (mut b, live, refused) = refused_a();
    assert!(live.attribute(&mut b, &refused));
    assert_eq!((b.get("pa").unwrap().state, b.get("pb").unwrap().state), (State::SignInAgain, State::Ready));
}

#[test]
fn a_change_of_the_profiles_starts_a_new_lookup_that_inherits_no_refusal() {
    let (mut b, mut live, refused) = refused_a();
    // A is removed while its refused fingerprint is still the last one looked up: B becomes active.
    let store = FakeKeys::with(&[("profile-pa", TOKEN_A), ("profile-pb", TOKEN_B)]);
    remove(&mut b, &store, "pa").unwrap();
    live.begin(&b);
    assert!(!live.active_refused(&b, &refused));
    assert!(!live.attribute(&mut b, &refused));
    assert_eq!(b.get("pb").unwrap().state, State::Ready);
}

#[test]
fn a_renewed_profile_is_not_marked_again_by_its_old_tokens_refusal() {
    let (mut b, mut live, refused) = refused_a();
    assert!(live.attribute(&mut b, &refused));
    let store = FakeKeys::with(&[("profile-pa", TOKEN_A)]);
    assert!(matches!(renew(&mut b, &store, "pa", Candidate::for_test(SITE, Some("a"), "Acme", TOKEN_B)).unwrap(), Renewal::Renewed));
    // Before the new token is looked up, and after: the old refusal does not count.
    let (generation, profile) = live.begin(&b);
    assert!(!live.attribute(&mut b, &refused) && !live.active_refused(&b, &refused));
    live.finish(generation, profile, Some("fp-new".into()));
    assert!(!live.attribute(&mut b, &refused) && !live.active_refused(&b, &refused));
    assert_eq!(b.get("pa").unwrap().state, State::Ready);
}

// ---- 8. A change made on a copy, outside the lock ----

#[test]
fn a_change_made_on_a_copy_keeps_what_happened_meanwhile() {
    let before = book(vec![profile("pa", "A", SITE, None, Some("Acme")), profile("pb", "B", SITE, None, Some("Bee"))], "pa");
    // Meanwhile: a switch to B and a refusal of A.
    let mut current = before.clone();
    current.switch("pb");
    mark_refused(&mut current, "pa");
    // The change: B's sign-out on the copy.
    let mut new = before.clone();
    mark_refused(&mut new, "pb");
    let merged = merge(&before, &current, &new);
    assert_eq!(merged.active.as_deref(), Some("pb"));
    assert_eq!((merged.get("pa").unwrap().state, merged.get("pb").unwrap().state), (State::SignInAgain, State::SignInAgain));
    // A removal on the copy wins, and an active one that is gone is not brought back.
    let mut new = before.clone();
    new.list.retain(|p| p.id != "pb");
    new.active = Some("pa".into());
    let merged = merge(&before, &current, &new);
    assert_eq!(merged.list.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["pa"]);
    assert_eq!(merged.active.as_deref(), Some("pa"));
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
    b.legacy = Some(MIGRATED_ID.into());
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
fn the_test_hook_respects_a_profile_that_needs_a_new_sign_in() {
    let store = FakeKeys::with(&[("profile-pa", TOKEN_A)]);
    let hook = "http://127.0.0.1:4000";
    let mut b = book(vec![profile("pa", "A", "https://a.example.com", None, None)], "pa");
    let pin = Pin { profile: Some("pa".into()), server: hook.into() };
    assert_eq!(token_for_hook(&b, &store, &pin, hook).as_deref(), Ok(TOKEN_A));
    assert!(token_for_hook(&b, &store, &Pin { server: "https://a.example.com".into(), ..pin.clone() }, hook).is_err(), "the hook's address only");
    mark_refused(&mut b, "pa");
    assert_eq!(token_for_hook(&b, &store, &pin, hook).unwrap_err(), needs_sign_in("A"));
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
    let mut b = book(vec![profile(MIGRATED_ID, "Alex · mythicshot.com", SITE, None, None), profile("pb", "Sandbox admin", "http://localhost:3000", None, None)], "pb");
    b.legacy = Some(MIGRATED_ID.into());
    assert_eq!(owner_of(&b, "pb"), Owner::Active);
    assert_eq!(owner_of(&b, MIGRATED_ID), Owner::Other { id: MIGRATED_ID.into(), name: "Alex · mythicshot.com".into() });
    assert_eq!(owner_of(&b, ""), Owner::Other { id: MIGRATED_ID.into(), name: "Alex · mythicshot.com".into() }, "linked before profiles");
    assert_eq!(owner_of(&b, "pgone"), Owner::Removed);
    let mut first = b.clone();
    first.switch(MIGRATED_ID);
    assert_eq!(owner_of(&first, ""), Owner::Active);
    assert_eq!(owner_of(&Book::default(), "pb"), Owner::NoProfile);
    // The first profile was removed: old recordings say so.
    let mut gone = b.clone();
    gone.list.retain(|p| p.id != MIGRATED_ID);
    assert_eq!(owner_of(&gone, ""), Owner::Removed);
}

#[test]
fn without_a_carried_over_connection_old_recordings_belong_to_nobody_yet() {
    let b = book(vec![profile("pb", "Sandbox admin", "http://localhost:3000", None, None)], "pb");
    assert_eq!(b.legacy, None);
    assert_eq!(owner_of(&b, ""), Owner::Unowned, "not \"removed\"");
    assert_eq!(owner_of(&Book::default(), ""), Owner::NoProfile);
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
    assert_eq!(NOT_SAVED, format!("{NOT_SAVED_LEAD} {NOT_ACCEPTED}"));
}
