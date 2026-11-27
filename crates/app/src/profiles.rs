//! Profiles: each one a name the user chose, a site address, and its own token in the OS
//! credential store, in an entry of its own keyed by the profile's id ([`publish::profile_key`]).
//! The settings file holds the list (id, name, address, the last account and workspace the site
//! named, the account's identity on the site, and whether the profile needs a new sign-in) and
//! which one is active; never a token.
//!
//! Everything here works on a [`Book`] and a [`KeyStore`] that are handed in, so every step is
//! tested with a fake store and no window: the app hands in the settings' list and the OS store.
//!
//! The rules this module keeps:
//! - A profile is saved only after its sign-in (or a pasted token) passed the check: the site
//!   answered for the new token with its workspace ([`checked`]). Until then the token is only in
//!   memory; a failed or cancelled attempt leaves nothing behind, and a save that cannot be
//!   confirmed in the store is undone ([`save_new`]).
//! - Each profile records who it is on the site ([`Identity`]: the workspace and the account, by
//!   their ids when the site gives ids) at every check that passes with its own token. One
//!   profile per site and identity: a second sign-in into the same one is offered as "Switch to
//!   it" ([`find_same`]). A profile whose identity is not (fully) recorded is never matched that
//!   way; only its own "Sign in again" renews it, and records the identity.
//! - A profile whose token the site refused (401) is kept and marked [`State::SignInAgain`]; no
//!   token of it is handed out until a new sign-in for the same identity passed the check
//!   ([`renew`]). A refusal counts only for the profile, and the lookup, its request was made
//!   with ([`Live`]). A site that is only unreachable changes nothing.
//! - A token is handed out only for its own profile's address ([`token_for`]).
//! - A token this computer lets go of, by a removal ([`remove`]) or a "Sign in again" that
//!   replaced it ([`renew`]), is handed back to be ended at its own profile's site once the change
//!   is done ([`Ending`]); never one that the old entry or another profile still holds
//!   ([`held_elsewhere`]). No other change ends a token.
//! - The one connection of the versions before profiles becomes the first profile without a new
//!   sign-in ([`migrate`]); its old credential entry goes once the settings that say so were
//!   written ([`remove_legacy`]). Should the first profile turn out to be an account another
//!   profile already has, it is joined to that one ([`twin_of`], [`attach`]).

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::publish::connect::Secret;
use crate::publish::ending::Outcome;
use crate::publish::{self, KeyStore, LEGACY_KEY, Workspace, profile_key};
use crate::settings::Settings;

pub mod holds;

/// The id of the profile the one connection of earlier versions became.
pub const MIGRATED_ID: &str = "first";
/// The longest profile name, in characters.
pub const NAME_MAX: usize = 60;

// ---- What the windows say (the approved wording) ----

pub const ACTIVE: &str = "Active";
pub const SIGN_IN_AGAIN: &str = "Sign in again";
pub const ADD_PROFILE: &str = "+ Add profile…";
/// The Workspace page's button beside its title: what "+ Add profile…" does.
pub const ADD_PROFILE_BUTTON: &str = "Add profile";
/// The profile card's button that opens the profile list.
pub const SWITCH_PROFILE: &str = "Switch profile";
pub const MANAGE_PROFILES: &str = "Manage profiles…";
pub const SITE_ADDRESS: &str = "Site address";
pub const SIGN_IN: &str = "Sign in";
pub const ADDRESS_HINT: &str = "Mythicshot checks the address first, then opens your browser to confirm a code.";
pub const CANCEL: &str = "Cancel";
pub const SAVE_PROFILE: &str = "Save profile";
pub const PROFILE_NAME: &str = "Profile name";
/// The first sentence of every failed add or sign-in again.
pub const NOT_SAVED_LEAD: &str = "This profile was not saved.";
/// What follows it when the site refused the new token.
pub const NOT_ACCEPTED: &str = "The site did not accept the sign-in.";
pub const NOT_SAVED: &str = "This profile was not saved. The site did not accept the sign-in.";
pub const SWITCH_TO_IT: &str = "Switch to it";
pub const REMOVE_PROFILE: &str = "Remove profile";
pub const UPLOAD_AS_NEW: &str = "Upload as a new tour";
pub const REMOVE: &str = "Remove";

/// "This profile was not saved." and why, in the app's existing words for that failure.
pub fn not_saved(why: &str) -> String {
    let why = why.trim();
    if why.is_empty() { NOT_SAVED_LEAD.to_string() } else { format!("{NOT_SAVED_LEAD} {why}") }
}

/// "Connected. Signed in as customer@mythicshot.test, workspace "customer1"."
pub fn connected(account: Option<&str>, workspace: &str) -> String {
    match account {
        Some(account) => format!("Connected. Signed in as {account}, workspace \"{workspace}\"."),
        // Not in the approved wording: an older site, or a pasted token, names no account.
        None => format!("Connected. Workspace \"{workspace}\"."),
    }
}

/// "You already have a profile for this account: "Robin · mythicshot.com"."
pub fn exists(name: &str) -> String {
    format!("You already have a profile for this account: \"{name}\".")
}

/// The button beside "Switch to it" that signs in afresh.
pub const TRY_DIFFERENT_ACCOUNT: &str = "Try a different account";

/// The second sentence under [`exists`], saying why the sign-in came back as an account that
/// has a profile.
pub fn exists_why() -> String {
    "The browser was signed in as that account. To add a different user, start again and choose \"Use a different account\" in the browser.".to_string()
}

/// ""customer1 test" needs you to sign in again. Nothing can be uploaded with it until you do."
pub fn needs_sign_in(name: &str) -> String {
    format!("\"{name}\" needs you to sign in again. Nothing can be uploaded with it until you do.")
}

/// The refusal of an update from another profile than the tour's.
pub fn other_profile(name: &str) -> String {
    format!("This recording belongs to a tour in \"{name}\". Switch to that profile to update it, or upload it here as a new tour.")
}

/// "Switch to Sandbox admin".
pub fn switch_to(name: &str) -> String {
    format!("Switch to {name}")
}

/// Not in the approved wording: the profile the recording's tour belongs to was removed.
pub const OWNER_REMOVED: &str = "This recording belongs to a tour in a profile that was removed from this computer. Upload it here as a new tour.";

/// The question before a profile is removed.
pub fn remove_question(name: &str) -> String {
    format!("Remove \"{name}\" from this computer? Your tours and account on the site are not touched.")
}

/// Said once after the update that adds profiles made the existing connection a profile.
pub fn migrated(name: &str) -> String {
    format!("Your existing connection is now the profile \"{name}\".")
}

// ---- The list ----

/// Whether a profile can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Ready,
    /// The site refused its token: kept, shown amber, and not used until a new sign-in passes.
    SignInAgain,
}

/// Who a profile is on its site, from the site's answer to the check of a token (`GET /api/v1/me`):
/// the workspace by its id (`id:ws_…`) when the site sends `workspace_id`, else by its name
/// (`name:…`, today's sites); and the account by its id (`id:…`) only, when the site sends
/// `account_id`. A user name is never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub workspace: String,
    /// The account's id; None while the site sends none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl Identity {
    /// From the site's ids: the workspace's, and the account's. Only such an identity matches
    /// another sign-in, and only exactly.
    pub fn complete(&self) -> bool {
        self.by_ids() && self.account.is_some()
    }

    /// The workspace is known by its id (not only by its name).
    pub fn by_ids(&self) -> bool {
        self.workspace.starts_with("id:")
    }

    /// From an answer: the ids when it has them, else the workspace's name.
    fn of(ws: &Workspace, name: &str) -> Option<Identity> {
        let workspace = key(ws.id.as_deref(), Some(name))?;
        let account = if ws.id.is_some() { ws.account.as_deref().map(|a| format!("id:{a}")) } else { None };
        Some(Identity { workspace, account })
    }
}

fn key(id: Option<&str>, name: Option<&str>) -> Option<String> {
    match (id, name.map(str::trim).filter(|n| !n.is_empty())) {
        (Some(id), _) => Some(format!("id:{id}")),
        (None, Some(name)) => Some(format!("name:{name}")),
        (None, None) => None,
    }
}

/// One profile as the settings file holds it. No secret is in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable for the profile's life: its credential entry is named after it.
    pub id: String,
    pub name: String,
    /// The site address, as the address rule writes it. Its token is sent nowhere else.
    pub server: String,
    /// The account's name the site gave at the last sign-in, for the windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The workspace's name the site gave the last time it answered, for the windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Who it is on the site, recorded at every check that passed with its own token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
    #[serde(default)]
    pub state: State,
    /// The name was chosen (or kept) by the user. Only the migrated profile starts without one:
    /// it is named after its workspace once the site has said which that is.
    #[serde(default)]
    pub named: bool,
}

impl Profile {
    /// The line under the name in the dropdown: "robin's workspace · app.mythicshot.com".
    pub fn detail(&self) -> String {
        let site = site_label(&self.server);
        match &self.workspace {
            Some(ws) if !ws.is_empty() => format!("{ws} · {site}"),
            _ => site,
        }
    }
}

/// The profiles, which one is active, and which one the connection of earlier versions became.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    pub list: Vec<Profile>,
    pub active: Option<String>,
    /// The profile recordings linked before profiles belong to; None when there was no connection
    /// to carry over (they then belong to nobody yet).
    pub legacy: Option<String>,
}

impl Book {
    pub fn of(settings: &Settings) -> Book {
        Book { list: settings.profiles.clone(), active: settings.active_profile.clone(), legacy: settings.legacy_profile.clone() }
    }

    pub fn write_to(&self, settings: &mut Settings) {
        settings.profiles = self.list.clone();
        settings.active_profile = self.active.clone();
        settings.legacy_profile = self.legacy.clone();
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.list.iter().find(|p| p.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Profile> {
        self.list.iter_mut().find(|p| p.id == id)
    }

    /// The active profile; None when there is none, or the id names none.
    pub fn active(&self) -> Option<&Profile> {
        self.get(self.active.as_deref()?)
    }

    /// Make `id` the active profile; false when there is no such profile.
    pub fn switch(&mut self, id: &str) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        self.active = Some(id.to_string());
        true
    }

    /// The active one after `id` went: one that can be used first, else any, else none.
    fn pick_active_after(&mut self, id: &str) {
        if self.active.as_deref() == Some(id) || self.active().is_none() {
            self.active = self.list.iter().find(|p| p.state == State::Ready).or(self.list.first()).map(|p| p.id.clone());
        }
    }
}

/// The site as the dropdown writes it: the address without `https://` and its path
/// ("app.mythicshot.com", "localhost:3000").
pub fn site_label(server: &str) -> String {
    let s = publish::check_server(server).unwrap_or_else(|_| server.trim().to_string());
    let rest = s.split_once("://").map_or(s.as_str(), |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// The site's address as the Workspace page shows it: [`site_label`]'s host, and the path when
/// the site lives under one ("tours.example.com/demo").
pub fn site_address_label(server: &str) -> String {
    let s = publish::check_server(server).unwrap_or_else(|_| server.trim().to_string());
    let rest = s.split_once("://").map_or(s.as_str(), |(_, r)| r);
    rest.trim_end_matches('/').to_string()
}

/// The site as a default profile name writes it: [`site_label`] without a leading `www.` or
/// `app.` ("mythicshot.com" for app.mythicshot.com).
pub fn name_host(server: &str) -> String {
    let label = site_label(server);
    for prefix in ["www.", "app."] {
        if let Some(rest) = label.strip_prefix(prefix)
            && rest.contains('.')
        {
            return rest.to_string();
        }
    }
    label
}

/// The name a new profile is offered: "customer1 · mythicshot.com".
pub fn default_name(workspace: &str, server: &str) -> String {
    clean_name(&format!("{} · {}", publish::share::plain_text(workspace), name_host(server)))
}

/// A name as it is saved: plain text, trimmed, at most [`NAME_MAX`] characters.
pub fn clean_name(name: &str) -> String {
    let plain = publish::share::plain_text(name);
    plain.chars().take(NAME_MAX).collect::<String>().trim_end().to_string()
}

pub fn same_site(a: &str, b: &str) -> bool {
    matches!((publish::check_server(a), publish::check_server(b)), (Ok(a), Ok(b)) if a == b)
}

// ---- Signing in ----

/// A sign-in that passed the check: the site answered for its token. The token is only in
/// memory until [`save_new`] or [`renew`] stores it, and is wiped when this is dropped.
pub struct Candidate {
    pub server: String,
    /// The account's name, for the windows.
    pub account: Option<String>,
    /// The workspace's name, for the windows.
    pub workspace: String,
    pub identity: Identity,
    token: Secret,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate").field("server", &self.server).field("account", &self.account).field("workspace", &self.workspace).field("identity", &self.identity).finish_non_exhaustive()
    }
}

impl Candidate {
    /// A candidate as today's sites answer: the workspace by its name only.
    #[cfg(test)]
    pub fn for_test(server: &str, account: Option<&str>, workspace: &str, token: &str) -> Candidate {
        let identity = Identity { workspace: format!("name:{workspace}"), account: None };
        Candidate { server: server.to_string(), account: account.map(str::to_string), workspace: workspace.to_string(), identity, token: Secret::new(token.to_string()) }
    }

    /// A candidate as a site that sends ids answers.
    #[cfg(test)]
    pub fn with_ids(server: &str, workspace: &str, workspace_id: &str, account_id: &str, token: &str) -> Candidate {
        let identity = Identity { workspace: format!("id:{workspace_id}"), account: Some(format!("id:{account_id}")) };
        Candidate { server: server.to_string(), account: None, workspace: workspace.to_string(), identity, token: Secret::new(token.to_string()) }
    }

    /// The name this sign-in is offered under.
    pub fn default_name(&self) -> String {
        default_name(&self.workspace, &self.server)
    }
}

/// What the site named for the account at sign-in, beside the token: its user name, for the
/// windows only (it is not part of the identity).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignedIn {
    pub name: Option<String>,
}

/// The check every sign-in and every pasted token passes before anything is saved: the site at
/// `server` is asked with the new token who it belongs to. Nothing is stored here, and the token
/// goes when this fails. The error is "This profile was not saved." and why, in the app's
/// existing words: "The site did not accept the sign-in." only when the site refused the token.
pub fn checked(server: &str, signed_in: SignedIn, token: Secret) -> Result<Candidate, String> {
    let server = publish::check_server(server).map_err(|e| not_saved(&e))?;
    match publish::workspace(&server, token.expose()) {
        Ok(ws) => {
            let workspace = publish::share::plain_text(&ws.name);
            let account = signed_in.name.map(|a| publish::share::plain_text(&a)).filter(|a| !a.is_empty());
            let identity = Identity::of(&ws, &workspace).unwrap_or(Identity { workspace: String::new(), account: None });
            Ok(Candidate { server, account, workspace, identity, token })
        }
        Err(e) if e == publish::REJECTED_TOKEN => Err(not_saved(NOT_ACCEPTED)),
        Err(e) => Err(not_saved(&e)),
    }
}

/// The profile this sign-in already has: same site and the same complete identity. A profile
/// whose identity is not fully recorded is never matched.
pub fn find_same<'a>(book: &'a Book, c: &Candidate) -> Option<&'a Profile> {
    book.list.iter().find(|p| same_site(&p.server, &c.server) && p.identity.as_ref().is_some_and(|i| i.complete() && *i == c.identity))
}

/// Store `token` in entry `key` and read it back: an error unless the store holds exactly it.
fn write_confirmed(store: &dyn KeyStore, key: &str, token: &str) -> Result<(), String> {
    publish::store_token_at(store, key, token)?;
    match publish::load_token_at(store, key)? {
        Some(back) if back == token => Ok(()),
        _ => Err("Could not use the credential store: the token could not be saved.".to_string()),
    }
}

static IDS: AtomicU64 = AtomicU64::new(0);

/// A new profile id: "p" and 16 hex characters, none the book has.
fn new_id(book: &Book) -> String {
    loop {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let seed = format!("{nanos}-{}-{}-{}", std::process::id(), IDS.fetch_add(1, Ordering::Relaxed), book.list.len());
        let id = format!("p{}", &publish::token_fingerprint(&seed)[..16]);
        if book.get(&id).is_none() {
            return id;
        }
    }
}

/// "Save profile": store the token in a new entry, confirm it is there, then add the profile
/// under `name` (the default name when empty) and make it the active one. When the store cannot
/// take or confirm the token, the entry is removed again and no profile is added.
pub fn save_new(book: &mut Book, store: &dyn KeyStore, c: Candidate, name: &str) -> Result<String, String> {
    if let Some(p) = find_same(book, &c) {
        return Err(exists(&p.name));
    }
    let id = new_id(book);
    let key = profile_key(&id);
    if let Err(e) = write_confirmed(store, &key, c.token.expose()) {
        let _ = store.delete(&key);
        return Err(e);
    }
    let name = match clean_name(name) {
        n if n.is_empty() => c.default_name(),
        n => n,
    };
    book.list.push(Profile { id: id.clone(), name, server: c.server.clone(), account: c.account.clone(), workspace: Some(c.workspace.clone()), identity: Some(c.identity.clone()), state: State::Ready, named: true });
    book.active = Some(id.clone());
    Ok(id)
}

/// What a new sign-in for an existing profile came to.
#[derive(Debug)]
pub enum Renewal {
    /// The same account: its new token is stored, and the profile can be used again. The token it
    /// replaced, to end at the profile's site, unless it is still held elsewhere or was the same.
    Renewed(Option<Ending>),
    /// Another account: nothing was stored, and the old profile is as it was. The sign-in can be
    /// saved as a profile of its own.
    OtherAccount(Candidate),
}

/// Whether profile `p`'s own "Sign in again" may take sign-in `c`, the identity being recorded
/// from `c` then. Same site, and:
/// - none recorded: any;
/// - recorded by the workspace's name (today's sites): the same workspace name, from the same
///   answer that gives `c`'s ids if any (so a site that starts sending ids upgrades it here); and
///   when the profile's account name and the sign-in's are both known, the same account name,
///   whatever its case (the newest spelling is kept);
/// - recorded by ids: the same workspace id, and the same account id when one is recorded. It
///   never matches by name: an answer without ids is another account.
fn renews(p: &Profile, c: &Candidate) -> bool {
    if !same_site(&p.server, &c.server) {
        return false;
    }
    match &p.identity {
        None => true,
        Some(i) if i.by_ids() => c.identity.workspace == i.workspace && (i.account.is_none() || c.identity.account == i.account),
        Some(i) => i.workspace == format!("name:{}", c.workspace) && !matches!((&p.account, &c.account), (Some(a), Some(b)) if a.to_lowercase() != b.to_lowercase()),
    }
}

/// Whether profile `id`'s own "Sign in again" would take sign-in `c` (see [`renew`]).
pub fn would_renew(book: &Book, id: &str, c: &Candidate) -> bool {
    book.get(id).is_some_and(|p| renews(p, c))
}

/// "Sign in again" for profile `id`, from that profile's own action: only a sign-in for the same
/// account replaces its token, and the identity is recorded. The token it replaced is handed
/// back to end at the profile's site, only once the new one is stored and confirmed, and not
/// when it is the new one or anything else here still holds it.
pub fn renew(book: &mut Book, store: &dyn KeyStore, id: &str, c: Candidate) -> Result<Renewal, String> {
    let Some(p) = book.get(id) else { return Ok(Renewal::OtherAccount(c)) };
    if !renews(p, &c) {
        return Ok(Renewal::OtherAccount(c));
    }
    // Read before it is overwritten; handed back only after the new one is confirmed. The old
    // entry may still hold it for now: see below.
    let old = ending_of(book, store, id, Some(c.token.expose()), true);
    write_confirmed(store, &profile_key(id), c.token.expose())?;
    if let Some(p) = book.get_mut(id) {
        p.state = State::Ready;
        if c.account.is_some() {
            p.account = c.account.clone();
        }
        p.identity = Some(c.identity.clone());
        name_after(p, &c.workspace);
    }
    // The old entry's copy of the replaced token (the first profile, before the cleanup) is
    // stale now: no profile holds that token any more, and [`remove_legacy`] would never take
    // it. It goes here, and only once it is gone is the token ended.
    let replaced = old.filter(|e| match legacy_holds(store, e.token.expose()) {
        Held::No => true,
        Held::Yes => store.delete(LEGACY_KEY).is_ok(),
        Held::Unknown => false,
    });
    Ok(Renewal::Renewed(replaced))
}

/// A check of profile `id` with its own token, at `server`, passed: the site's answer is kept as
/// its workspace label and its identity, and a profile not named yet is named after the
/// workspace. An answer for another address than the profile's changes nothing. True when
/// anything changed.
pub fn note_check(book: &mut Book, id: &str, server: &str, ws: &Workspace) -> bool {
    let Some(p) = book.get_mut(id) else { return false };
    if !same_site(&p.server, server) {
        return false;
    }
    let before = p.clone();
    let name = publish::share::plain_text(&ws.name);
    // The profile's own token answered: what the answer says is the profile. Ids replace a
    // name-based identity (a site that starts sending them); an answer without ids never takes
    // the place of ids; an account id the answer leaves out stays as recorded.
    if let Some(mut seen) = Identity::of(ws, &name) {
        let stored = p.identity.clone();
        match &stored {
            Some(old) if old.by_ids() && !seen.by_ids() => seen = old.clone(),
            Some(old) if old.workspace == seen.workspace && seen.account.is_none() => seen.account = old.account.clone(),
            _ => {}
        }
        p.identity = Some(seen);
    }
    name_after(p, &name);
    *p != before
}

fn name_after(p: &mut Profile, workspace: &str) {
    let ws = publish::share::plain_text(workspace);
    if !ws.is_empty() {
        p.workspace = Some(ws.clone());
        if !p.named {
            p.name = default_name(&ws, &p.server);
            p.named = true;
        }
    }
}

/// The site refused profile `id`'s token (401): it needs a new sign-in. True when that is new.
pub fn mark_refused(book: &mut Book, id: &str) -> bool {
    match book.get_mut(id) {
        Some(p) if p.state != State::SignInAgain => {
            p.state = State::SignInAgain;
            true
        }
        _ => false,
    }
}

/// "Sign out" of profile `id`: its token is removed from this computer, and the profile stays,
/// needing a new sign-in.
pub fn sign_out(book: &mut Book, store: &dyn KeyStore, id: &str) -> Result<(), String> {
    store.delete(&profile_key(id))?;
    mark_refused(book, id);
    Ok(())
}

// ---- Ending a token at its site ----

/// A token a change let go of, to end at its own site once that change is done: whose it was,
/// the profile's own address (as the address rule writes it) and the token, wiped when this is
/// dropped. Only [`remove`] and [`renew`] hand one over, and never for a token anything else on
/// this computer still holds ([`held_elsewhere`]).
pub struct Ending {
    pub id: String,
    pub name: String,
    pub server: String,
    token: Secret,
}

impl std::fmt::Debug for Ending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ending").field("id", &self.id).field("name", &self.name).field("server", &self.server).finish_non_exhaustive()
    }
}

impl Ending {
    #[cfg(test)]
    pub fn token(&self) -> &str {
        self.token.expose()
    }

    /// Whether anything in `book` and `store` holds this token now, but the entry of the profile
    /// it was let go of by ([`held_elsewhere`]).
    pub fn held_in(&self, book: &Book, store: &dyn KeyStore) -> bool {
        held_elsewhere(book, store, &self.id, self.token.expose())
    }

    /// Ask the profile's own site to end the token, once. Blocks for at most
    /// [`publish::ending::END_TIMEOUT`]: never on the UI thread.
    pub fn send(&self) -> Outcome {
        publish::ending::end_token(&self.server, self.token.expose())
    }
}

/// Whether `token` is still held on this computer by anything but profile `id`'s own entry: the
/// entry of the versions before profiles, or the entry of any other profile (a twin after a
/// join, or any other whose token is byte for byte the same). An entry that cannot be read
/// counts as holding it: a token is ended only when nothing else can still need it.
pub fn held_elsewhere(book: &Book, store: &dyn KeyStore, id: &str, token: &str) -> bool {
    legacy_holds(store, token) != Held::No || held_by_other_profiles(book, store, id, token)
}

/// Whether an entry holds a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    No,
    Yes,
    /// The entry cannot be read: it may.
    Unknown,
}

fn holds(store: &dyn KeyStore, key: &str, token: &str) -> Held {
    match publish::load_token_at(store, key) {
        Ok(Some(mut copy)) => {
            let same = copy == token;
            crate::publish_ui::wipe(&mut copy);
            if same { Held::Yes } else { Held::No }
        }
        Ok(None) => Held::No,
        Err(_) => Held::Unknown,
    }
}

fn legacy_holds(store: &dyn KeyStore, token: &str) -> Held {
    holds(store, LEGACY_KEY, token)
}

fn held_by_other_profiles(book: &Book, store: &dyn KeyStore, id: &str, token: &str) -> bool {
    book.list.iter().filter(|p| p.id != id).any(|p| holds(store, &profile_key(&p.id), token) != Held::No)
}

/// The token in profile `id`'s own entry, to end at the profile's own address, unless another
/// profile still holds it, it is `except` (the token that replaces it), or the old entry holds
/// it while `legacy_goes` is false (when it is true the caller removes that copy first, and ends
/// the token only once it is gone). The address is the profile's, as the address rule writes
/// it: a token is only ever sent to its own profile's site, as [`token_for`] hands it out.
fn ending_of(book: &Book, store: &dyn KeyStore, id: &str, except: Option<&str>, legacy_goes: bool) -> Option<Ending> {
    let p = book.get(id)?;
    let server = publish::check_server(&p.server).ok()?;
    let token = Secret::new(publish::load_token_at(store, &profile_key(id)).ok()??);
    if except == Some(token.expose()) || held_by_other_profiles(book, store, id, token.expose()) {
        return None;
    }
    match legacy_holds(store, token.expose()) {
        Held::No => {}
        Held::Yes if legacy_goes => {}
        Held::Yes | Held::Unknown => return None,
    }
    Some(Ending { id: p.id.clone(), name: p.name.clone(), server, token })
}

impl Candidate {
    /// The connection a sign-in made, which is not kept (another account was tried, the form was
    /// cancelled, or the profile that is there already was switched to): to end at the site the
    /// sign-in was made at. Its token goes with it, and is wiped when it is dropped. It belongs
    /// to no profile, so whether anything here holds it is asked of every profile and of the
    /// entry of the versions before profiles ([`Ending::held_in`]).
    pub fn into_ending(self) -> Option<Ending> {
        let server = publish::check_server(&self.server).ok()?;
        Some(Ending { id: String::new(), name: self.workspace, server, token: self.token })
    }
}

/// The log line after the connection of a sign-in that was not kept was to be ended; it never
/// holds the token.
pub fn abandoned_log_line(server: &str, outcome: &Outcome) -> String {
    let site = site_label(server);
    match outcome {
        Outcome::Ended => format!("Profiles: the connection of a sign-in that was not kept was ended at {site}."),
        Outcome::AlreadyEnded => format!("Profiles: the connection of a sign-in that was not kept was no longer valid at {site}."),
        Outcome::SiteTooOld => format!("Profiles: {site} cannot end the connection of a sign-in that was not kept yet; it lapses there by itself."),
        Outcome::NotAConnection => format!("Profiles: {site} keeps the token of a sign-in that was not kept, because it is not a sign-in connection."),
        Outcome::NotReached(why) => format!("Profiles: the connection of a sign-in that was not kept could not be ended at {site}: {why}."),
    }
}

/// What a removal says when the site did not end its token; nothing when it did, or the token
/// was not valid any more.
pub fn removal_notice(name: &str, outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Ended | Outcome::AlreadyEnded | Outcome::NotAConnection => None,
        Outcome::SiteTooOld | Outcome::NotReached(_) => Some(not_ended(name)),
    }
}

/// The approved line: the profile was removed here, and its site did not end its token (an
/// older site, or no answer).
pub fn not_ended(name: &str) -> String {
    format!("Removed \"{name}\" from this computer. The site could not be reached, so this computer is still listed under Connections there until you remove it.")
}

/// The app's log line for the end of the token a "Sign in again" replaced, at `server`. It never
/// holds any of the token.
pub fn renewal_log_line(server: &str, outcome: &Outcome) -> String {
    let site = site_label(server);
    match outcome {
        Outcome::Ended => format!("Profiles: the token a new sign-in replaced was ended at {site}."),
        Outcome::AlreadyEnded => format!("Profiles: the token a new sign-in replaced was no longer valid at {site}."),
        Outcome::SiteTooOld => format!("Profiles: {site} cannot end the token a new sign-in replaced yet; it lapses there by itself."),
        Outcome::NotAConnection => format!("Profiles: {site} keeps the token a new sign-in replaced, because it is not a sign-in connection."),
        Outcome::NotReached(why) => format!("Profiles: the token a new sign-in replaced could not be ended at {site}: {why}."),
    }
}

/// Remove profile `id` and its token from this computer; nothing is sent to the site here. When
/// it was the active one, another becomes active (one that can be used first), or none. The
/// token, read before its entry goes, is handed back to end at the profile's site once the
/// removal is done ([`Ending`]); not when the old entry or any other profile still holds it.
pub fn remove(book: &mut Book, store: &dyn KeyStore, id: &str) -> Result<Option<Ending>, String> {
    // What is left of the old entry goes with it only when it is this profile's own copy.
    let legacy_is_its_copy = book.legacy.as_deref() == Some(id)
        && match publish::load_token_at(store, &profile_key(id)) {
            Ok(Some(mut own)) => {
                let same = legacy_holds(store, &own) == Held::Yes;
                crate::publish_ui::wipe(&mut own);
                same
            }
            _ => false,
        };
    let mut ending = ending_of(book, store, id, None, legacy_is_its_copy);
    store.delete(&profile_key(id))?;
    // The token is ended only when that copy went too.
    if legacy_is_its_copy && store.delete(LEGACY_KEY).is_err() {
        ending = None;
    }
    book.list.retain(|p| p.id != id);
    book.pick_active_after(id);
    Ok(ending)
}

/// Another profile on the same site with the same complete identity as profile `id`: the
/// profile it is a second copy of.
pub fn twin_of(book: &Book, id: &str) -> Option<String> {
    let p = book.get(id)?;
    let i = p.identity.as_ref().filter(|i| i.complete())?;
    book.list.iter().find(|o| o.id != id && same_site(&o.server, &p.server) && o.identity.as_ref() == Some(i)).map(|o| o.id.clone())
}

/// Join profile `from` (the migrated one) to `to`, the same account by complete id-based
/// identities on both sides: recordings linked before profiles belong to `to`, and `to` is active
/// where `from` was. `from`'s token is the one a passing check has just proved (a twin's token
/// may be revoked however it looks), so it always moves into `to`: written, read back, and only
/// then removed from `from`, and `to` is ready again. When that cannot be confirmed, nothing is
/// joined. The old entry is left for [`remove_legacy`].
pub fn attach(book: &mut Book, store: &dyn KeyStore, from: &str, to: &str) -> Result<(), String> {
    let (Some(f), Some(t)) = (book.get(from), book.get(to)) else { return Ok(()) };
    let same = matches!((&f.identity, &t.identity), (Some(a), Some(b)) if a.complete() && b.complete() && a == b);
    if from == to || !same || !same_site(&f.server, &t.server) {
        return Ok(());
    }
    let Some(mut token) = publish::load_token_at(store, &profile_key(from))? else { return Ok(()) };
    let moved = write_confirmed(store, &profile_key(to), &token);
    crate::publish_ui::wipe(&mut token);
    moved?;
    if let Some(t) = book.get_mut(to) {
        t.state = State::Ready;
    }
    store.delete(&profile_key(from))?;
    let was_active = book.active.as_deref() == Some(from);
    book.list.retain(|p| p.id != from);
    if book.legacy.as_deref() == Some(from) || book.legacy.is_none() {
        book.legacy = Some(to.to_string());
    }
    if was_active {
        book.active = Some(to.to_string());
    }
    Ok(())
}

// ---- Which token lookup a refusal belongs to ----

/// The token looked up last: whose, by which lookup, and its fingerprint (never the token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Looked {
    pub profile: String,
    pub generation: u64,
    pub fingerprint: String,
}

/// The active profile's token as the windows know it: each switch or change of the profiles
/// starts a new lookup, and what was looked up before it no longer counts. A refusal is only
/// ever put on the profile, and the lookup, its token was looked up for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Live {
    pub generation: u64,
    pub looked: Option<Looked>,
}

impl Live {
    /// A new lookup for the active profile of `book`: the earlier one stops counting at once.
    pub fn begin(&mut self, book: &Book) -> (u64, Option<String>) {
        self.generation += 1;
        self.looked = None;
        (self.generation, book.active().map(|p| p.id.clone()))
    }

    /// The lookup `generation` for `profile` found a token with this fingerprint. A lookup an
    /// newer one overtook is dropped; false then.
    pub fn finish(&mut self, generation: u64, profile: Option<String>, fingerprint: Option<String>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.looked = profile.zip(fingerprint).map(|(profile, fingerprint)| Looked { profile, generation, fingerprint });
        true
    }

    /// The site refused the looked-up token: its own profile needs a new sign-in. True when the
    /// book changed.
    pub fn attribute(&self, book: &mut Book, refused: &dyn Fn(&str) -> bool) -> bool {
        match &self.looked {
            Some(l) if l.generation == self.generation && refused(&l.fingerprint) => mark_refused(book, &l.profile),
            _ => false,
        }
    }

    /// Whether the active profile cannot be used now: it waits for a sign-in, or the site
    /// refused its own looked-up token.
    pub fn active_refused(&self, book: &Book, refused: &dyn Fn(&str) -> bool) -> bool {
        let Some(active) = book.active() else { return false };
        active.state == State::SignInAgain || self.looked.as_ref().is_some_and(|l| l.profile == active.id && l.generation == self.generation && refused(&l.fingerprint))
    }
}

/// The book after a change that ran on a copy, `new`, made from `before`, while `current` may
/// have been changed meanwhile (a switch, a refusal, a check): what the change did wins where it
/// changed something, and what happened meanwhile is kept everywhere else.
pub fn merge(before: &Book, current: &Book, new: &Book) -> Book {
    let mut out = new.clone();
    for p in out.list.iter_mut() {
        // Field by field: what the change left alone takes what happened meanwhile (an identity
        // recorded, a refusal); what the change set (a renewal's Ready, say) stays.
        if let (Some(was), Some(now)) = (before.get(&p.id), current.get(&p.id)) {
            macro_rules! keep {
                ($($f:ident),*) => {$(if p.$f == was.$f { p.$f = now.$f.clone(); })*};
            }
            keep!(name, server, account, workspace, identity, state, named);
        }
    }
    if new.active == before.active && current.active != before.active && current.active.as_deref().is_some_and(|id| out.get(id).is_some()) {
        out.active = current.active.clone();
    }
    if new.legacy == before.legacy {
        out.legacy = current.legacy.clone();
    }
    out
}

// ---- Which profile a job uses ----

/// The profile a job was started with, and the address it sends to. A job keeps it to the end,
/// whatever profile is switched to meanwhile.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pin {
    pub profile: Option<String>,
    pub server: String,
}

/// The token of the profile `pin` names, for a job about to send it to `pin.server`: refused for
/// any other address than the profile's own, for a profile that needs a new sign-in, and for one
/// that is gone.
pub fn token_for(book: &Book, store: &dyn KeyStore, pin: &Pin) -> Result<String, String> {
    let p = usable(book, pin.profile.as_deref())?;
    if let Some(problem) = crate::publish_ui::binding_problem(Some(&p.server), &pin.server) {
        return Err(problem);
    }
    if !same_site(&p.server, &pin.server) {
        return Err(crate::publish_ui::ADD_SERVER_AND_TOKEN.to_string());
    }
    publish::load_token_at(store, &profile_key(&p.id))?.ok_or_else(|| needs_sign_in(&p.name))
}

/// The debug builds' test hook: the profile's token for the hook's address `hook` only, and only
/// while the profile can be used.
pub fn token_for_hook(book: &Book, store: &dyn KeyStore, pin: &Pin, hook: &str) -> Result<String, String> {
    if let Some(problem) = crate::publish_ui::binding_problem(publish::check_server(hook).ok().as_deref(), &pin.server) {
        return Err(format!("{problem} {}", crate::publish_ui::FIX_ON_WORKSPACE));
    }
    let p = usable(book, pin.profile.as_deref())?;
    publish::load_token_at(store, &profile_key(&p.id))?.ok_or_else(|| needs_sign_in(&p.name))
}

fn usable<'a>(book: &'a Book, id: Option<&str>) -> Result<&'a Profile, String> {
    let p = id.and_then(|id| book.get(id)).ok_or_else(|| crate::publish_ui::ADD_SERVER_AND_TOKEN.to_string())?;
    if p.state == State::SignInAgain {
        return Err(needs_sign_in(&p.name));
    }
    Ok(p)
}

// ---- Which profile a recording's tour belongs to ----

/// Whose tour a recording was linked to, seen from the active profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// The active profile's: it can be updated.
    Active,
    /// Another profile on this computer: switch to it to update.
    Other { id: String, name: String },
    /// A profile that was removed: only a new tour.
    Removed,
    /// Linked before profiles, with no connection carried over: nobody's yet. It is updated by
    /// the active profile when the addresses match, and is that profile's from then on.
    Unowned,
    /// No profile is active: as before profiles, when not connected.
    NoProfile,
}

/// Whose tour it is: `recorded` is the profile id the recording was saved with; an empty one
/// (saved before profiles) is the profile the old connection became, or nobody's.
pub fn owner_of(book: &Book, recorded: &str) -> Owner {
    let owner = match (recorded, &book.legacy) {
        ("", Some(legacy)) => legacy.as_str(),
        ("", None) => return if book.active().is_some() { Owner::Unowned } else { Owner::NoProfile },
        (id, _) => id,
    };
    match book.active() {
        None => Owner::NoProfile,
        Some(a) if a.id == owner => Owner::Active,
        Some(_) => match book.get(owner) {
            Some(p) => Owner::Other { id: p.id.clone(), name: p.name.clone() },
            None => Owner::Removed,
        },
    }
}

// ---- The update that adds profiles ----

/// Make the one connection of the versions before profiles the first profile, once. Its token is
/// copied into the profile's own entry and read back before the profile is added; the old entry
/// is left for [`remove_legacy`], after the settings were saved. Safe to run again: a profile it
/// made is never made twice, and a start cut off at any step is finished by the next one. Ok(Some
/// name) when it made the profile; an error (the store is locked, say) changes nothing, and the
/// next start tries again.
pub fn migrate(settings: &mut Settings, store: &dyn KeyStore) -> Result<Option<String>, String> {
    if settings.profiles_migrated {
        return Ok(None);
    }
    let server = settings.publish_saved_for.as_deref().filter(|s| !s.trim().is_empty()).and_then(|s| publish::check_server(s).ok());
    let Some(server) = server else {
        // No address the token was tied to: there was no connection to carry over, and an old
        // entry can never be used; it goes once this is saved.
        settings.profiles_migrated = true;
        return Ok(None);
    };
    let key = profile_key(MIGRATED_ID);
    match publish::load_token_at(store, LEGACY_KEY)? {
        Some(mut token) => {
            let written = write_confirmed(store, &key, &token);
            crate::publish_ui::wipe(&mut token);
            written?;
        }
        // A start cut off after the old entry went: the copy is what is left.
        None if publish::load_token_at(store, &key)?.is_some() => {}
        None => {
            settings.profiles_migrated = true;
            return Ok(None);
        }
    }
    let mut book = Book::of(settings);
    if book.get(MIGRATED_ID).is_none() {
        book.list.insert(0, Profile { id: MIGRATED_ID.to_string(), name: name_host(&server), server, account: None, workspace: None, identity: None, state: State::Ready, named: false });
    }
    if book.active().is_none() {
        book.active = Some(MIGRATED_ID.to_string());
    }
    book.legacy = Some(MIGRATED_ID.to_string());
    book.write_to(settings);
    settings.profiles_migrated = true;
    Ok(book.get(MIGRATED_ID).map(|p| p.name.clone()))
}

/// Remove the old credential entry once the settings that say what became of it were saved, and
/// only while a profile's own entry holds exactly the same token (a confirmed copy of it), or when
/// it could never be used (there was no address to tie it to). Anything else (an entry an older
/// version wrote again, another token, no copy left anywhere) is left untouched, silently. True
/// when it removed one. Run after every start's save: it finishes a migration a start cut off.
pub fn remove_legacy(settings: &Settings, store: &dyn KeyStore) -> Result<bool, String> {
    if !settings.profiles_migrated || store.get(LEGACY_KEY)?.is_none() {
        return Ok(false);
    }
    let unusable = settings.legacy_profile.is_none() && settings.publish_saved_for.as_deref().is_none_or(|s| publish::check_server(s).is_err());
    let copied = match publish::load_token_at(store, LEGACY_KEY) {
        Ok(Some(mut old)) => {
            let mut held = false;
            for p in &settings.profiles {
                if let Ok(Some(mut copy)) = publish::load_token_at(store, &profile_key(&p.id)) {
                    held |= copy == old;
                    crate::publish_ui::wipe(&mut copy);
                }
            }
            crate::publish_ui::wipe(&mut old);
            held
        }
        _ => false,
    };
    if !copied && !unusable {
        return Ok(false);
    }
    store.delete(LEGACY_KEY)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests;
