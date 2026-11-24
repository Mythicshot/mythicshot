//! Profiles: each one a name the user chose, a site address, and its own token in the OS
//! credential store, in an entry of its own keyed by the profile's id ([`publish::profile_key`]).
//! The settings file holds the list (id, name, address, the last account and workspace the site
//! named, and whether the profile needs a new sign-in) and which one is active; never a token.
//!
//! Everything here works on a [`Book`] and a [`KeyStore`] that are handed in, so every step is
//! tested with a fake store and no window: the app hands in the settings' list and the OS store.
//!
//! The rules this module keeps:
//! - A profile is saved only after its sign-in passed the check: the site answered for the new
//!   token with its workspace ([`checked`]). Until then the token is only in memory; a failed or
//!   cancelled attempt leaves nothing behind, and a save that cannot be confirmed in the store is
//!   undone ([`save_new`]).
//! - One profile per site and account: a second sign-in into the same one is offered as
//!   "Switch to it" ([`find_same`]).
//! - A profile whose token the site refused (401) is kept and marked [`State::SignInAgain`]; no
//!   token of it is handed out until a new sign-in for the same account passed the check
//!   ([`renew`]). A site that is only unreachable changes nothing.
//! - A token is handed out only for its own profile's address ([`token_for`]).
//! - The one connection of the versions before profiles becomes the first profile without a new
//!   sign-in ([`migrate`]); its old credential entry goes only once the new one is confirmed and
//!   the settings that name the profile were written ([`remove_legacy`]).

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::publish::connect::Secret;
use crate::publish::{self, KeyStore, LEGACY_KEY, profile_key};
use crate::settings::Settings;

/// The id of the profile the one connection of earlier versions became. Recordings linked to a
/// tour before profiles existed belong to it.
pub const MIGRATED_ID: &str = "first";
/// The longest profile name, in characters.
pub const NAME_MAX: usize = 60;

// ---- What the windows say (the approved wording) ----

pub const ACTIVE: &str = "Active";
pub const SIGN_IN_AGAIN: &str = "Sign in again";
pub const ADD_PROFILE: &str = "+ Add profile…";
pub const MANAGE_PROFILES: &str = "Manage profiles…";
pub const SITE_ADDRESS: &str = "Site address";
pub const SIGN_IN: &str = "Sign in";
pub const ADDRESS_HINT: &str = "Mythicshot checks the address first, then opens your browser to confirm a code.";
pub const CANCEL: &str = "Cancel";
pub const SAVE_PROFILE: &str = "Save profile";
pub const PROFILE_NAME: &str = "Profile name";
pub const NOT_SAVED: &str = "This profile was not saved. The site did not accept the sign-in.";
pub const SWITCH_TO_IT: &str = "Switch to it";
pub const REMOVE_PROFILE: &str = "Remove profile";
pub const UPLOAD_AS_NEW: &str = "Upload as a new tour";
pub const REMOVE: &str = "Remove";

/// "Connected. Signed in as customer@mythicshot.test, workspace "customer1"."
pub fn connected(account: Option<&str>, workspace: &str) -> String {
    match account {
        Some(account) => format!("Connected. Signed in as {account}, workspace \"{workspace}\"."),
        // Not in the approved wording: an older site names no account.
        None => format!("Connected. Workspace \"{workspace}\"."),
    }
}

/// "You already have a profile for this account: "Alex · mythicshot.com"."
pub fn exists(name: &str) -> String {
    format!("You already have a profile for this account: \"{name}\".")
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

/// One profile as the settings file holds it. No secret is in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable for the profile's life: its credential entry is named after it.
    pub id: String,
    pub name: String,
    /// The site address, as the address rule writes it. Its token is sent nowhere else.
    pub server: String,
    /// The account the site named at the last sign-in, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The workspace the site named the last time it answered for the token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default)]
    pub state: State,
    /// The name was chosen (or kept) by the user. Only the migrated profile starts without one:
    /// it is named after its workspace once the site has said which that is.
    #[serde(default)]
    pub named: bool,
}

impl Profile {
    /// The line under the name in the dropdown: "alex's workspace · app.mythicshot.com".
    pub fn detail(&self) -> String {
        let site = site_label(&self.server);
        match &self.workspace {
            Some(ws) if !ws.is_empty() => format!("{ws} · {site}"),
            _ => site,
        }
    }
}

/// The profiles and which one is active.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    pub list: Vec<Profile>,
    pub active: Option<String>,
}

impl Book {
    pub fn of(settings: &Settings) -> Book {
        Book { list: settings.profiles.clone(), active: settings.active_profile.clone() }
    }

    pub fn write_to(&self, settings: &mut Settings) {
        settings.profiles = self.list.clone();
        settings.active_profile = self.active.clone();
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
}

/// The site as the dropdown writes it: the address without `https://` and its path
/// ("app.mythicshot.com", "localhost:3000").
pub fn site_label(server: &str) -> String {
    let s = publish::check_server(server).unwrap_or_else(|_| server.trim().to_string());
    let rest = s.split_once("://").map_or(s.as_str(), |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
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

fn same_site(a: &str, b: &str) -> bool {
    matches!((publish::check_server(a), publish::check_server(b)), (Ok(a), Ok(b)) if a == b)
}

// ---- Signing in ----

/// A sign-in that passed the check: the site answered for its token. The token is only in
/// memory until [`save_new`] or [`renew`] stores it, and is wiped when this is dropped.
pub struct Candidate {
    pub server: String,
    pub account: Option<String>,
    pub workspace: String,
    token: Secret,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate").field("server", &self.server).field("account", &self.account).field("workspace", &self.workspace).finish_non_exhaustive()
    }
}

impl Candidate {
    #[cfg(test)]
    pub fn for_test(server: &str, account: Option<&str>, workspace: &str, token: &str) -> Candidate {
        Candidate { server: server.to_string(), account: account.map(str::to_string), workspace: workspace.to_string(), token: Secret::new(token.to_string()) }
    }

    /// The name this sign-in is offered under.
    pub fn default_name(&self) -> String {
        default_name(&self.workspace, &self.server)
    }
}

/// The check every sign-in passes before anything is saved: the site at `server` is asked with
/// the new token who it belongs to. Any failure is [`NOT_SAVED`]: nothing was stored, and the
/// token goes when this returns.
pub fn checked(server: &str, account: Option<String>, token: Secret) -> Result<Candidate, String> {
    let server = publish::check_server(server).map_err(|_| NOT_SAVED.to_string())?;
    match publish::workspace(&server, token.expose()) {
        Ok(ws) => {
            let workspace = publish::share::plain_text(&ws.name);
            let account = account.map(|a| publish::share::plain_text(&a)).filter(|a| !a.is_empty());
            Ok(Candidate { server, account, workspace, token })
        }
        Err(_) => Err(NOT_SAVED.to_string()),
    }
}

/// Whether the sign-in `c` is the account of profile `p`: the same site, the same workspace when
/// the profile knows it, and the same account when both name one. A profile that knows neither
/// (the migrated one, before the site ever answered for it) takes the first that passes on its site.
pub fn same_account(p: &Profile, c: &Candidate) -> bool {
    if !same_site(&p.server, &c.server) {
        return false;
    }
    if let Some(ws) = &p.workspace
        && *ws != c.workspace
    {
        return false;
    }
    match (&p.account, &c.account) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// The profile this sign-in already has, if any.
pub fn find_same<'a>(book: &'a Book, c: &Candidate) -> Option<&'a Profile> {
    book.list.iter().find(|p| same_account(p, c))
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
    book.list.push(Profile { id: id.clone(), name, server: c.server.clone(), account: c.account.clone(), workspace: Some(c.workspace.clone()), state: State::Ready, named: true });
    book.active = Some(id.clone());
    Ok(id)
}

/// What a new sign-in for an existing profile came to.
#[derive(Debug)]
pub enum Renewal {
    /// The same account: its new token is stored, and the profile can be used again.
    Renewed,
    /// Another account: nothing was stored, and the old profile is as it was. The sign-in can be
    /// saved as a profile of its own.
    OtherAccount(Candidate),
}

/// "Sign in again" for profile `id`: only a sign-in for the same account replaces its token.
pub fn renew(book: &mut Book, store: &dyn KeyStore, id: &str, c: Candidate) -> Result<Renewal, String> {
    let Some(p) = book.get(id) else { return Ok(Renewal::OtherAccount(c)) };
    if !same_account(p, &c) {
        return Ok(Renewal::OtherAccount(c));
    }
    write_confirmed(store, &profile_key(id), c.token.expose())?;
    if let Some(p) = book.get_mut(id) {
        p.state = State::Ready;
        if c.account.is_some() {
            p.account = c.account.clone();
        }
        note_workspace_in(p, &c.workspace);
    }
    Ok(Renewal::Renewed)
}

/// The site answered for profile `id`'s token and named its workspace: kept as the profile's
/// label, and a profile not named yet is named after it. True when anything changed.
pub fn note_workspace(book: &mut Book, id: &str, workspace: &str) -> bool {
    match book.get_mut(id) {
        Some(p) => note_workspace_in(p, workspace),
        None => false,
    }
}

fn note_workspace_in(p: &mut Profile, workspace: &str) -> bool {
    let ws = publish::share::plain_text(workspace);
    let before = p.clone();
    if !ws.is_empty() {
        p.workspace = Some(ws.clone());
        if !p.named {
            p.name = default_name(&ws, &p.server);
            p.named = true;
        }
    }
    *p != before
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

/// Remove profile `id` and its token from this computer; nothing is sent to the site. When it
/// was the active one, another becomes active (one that can be used first), or none.
pub fn remove(book: &mut Book, store: &dyn KeyStore, id: &str) -> Result<(), String> {
    store.delete(&profile_key(id))?;
    if id == MIGRATED_ID {
        // What the migration left of the old entry, if a start was cut off before it went.
        let _ = store.delete(LEGACY_KEY);
    }
    book.list.retain(|p| p.id != id);
    if book.active.as_deref() == Some(id) || book.active().is_none() {
        book.active = book.list.iter().find(|p| p.state == State::Ready).or(book.list.first()).map(|p| p.id.clone());
    }
    Ok(())
}

// ---- Which profile a job uses ----

/// The profile a job was started with, and the address it sends to. A job keeps it to the end,
/// whatever profile is switched to meanwhile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub profile: Option<String>,
    pub server: String,
}

/// The token of the profile `pin` names, for a job about to send it to `pin.server`: refused for
/// any other address than the profile's own, for a profile that needs a new sign-in, and for one
/// that is gone.
pub fn token_for(book: &Book, store: &dyn KeyStore, pin: &Pin) -> Result<String, String> {
    let Some(p) = pin.profile.as_deref().and_then(|id| book.get(id)) else { return Err(crate::publish_ui::ADD_SERVER_AND_TOKEN.to_string()) };
    if let Some(problem) = crate::publish_ui::binding_problem(Some(&p.server), &pin.server) {
        return Err(problem);
    }
    if !same_site(&p.server, &pin.server) {
        return Err(crate::publish_ui::ADD_SERVER_AND_TOKEN.to_string());
    }
    if p.state == State::SignInAgain {
        return Err(needs_sign_in(&p.name));
    }
    publish::load_token_at(store, &profile_key(&p.id))?.ok_or_else(|| needs_sign_in(&p.name))
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
    /// No profile is active: as before profiles, when not connected.
    NoProfile,
}

/// Whose tour it is: `recorded` is the profile id the recording was saved with; an empty one
/// (saved before profiles) is the migrated profile's.
pub fn owner_of(book: &Book, recorded: &str) -> Owner {
    let owner = if recorded.is_empty() { MIGRATED_ID } else { recorded };
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
        // No address the token was tied to: there was no connection to carry over.
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
        book.list.insert(0, Profile { id: MIGRATED_ID.to_string(), name: name_host(&server), server, account: None, workspace: None, state: State::Ready, named: false });
    }
    if book.active().is_none() {
        book.active = Some(MIGRATED_ID.to_string());
    }
    book.write_to(settings);
    settings.profiles_migrated = true;
    Ok(book.get(MIGRATED_ID).map(|p| p.name.clone()))
}

/// Remove the old credential entry, once the settings that name the migrated profile were saved
/// and its own entry holds a token. True when it removed one. Run after every start's save: it
/// finishes a migration a start cut off.
pub fn remove_legacy(settings: &Settings, store: &dyn KeyStore) -> Result<bool, String> {
    if !settings.profiles_migrated || !settings.profiles.iter().any(|p| p.id == MIGRATED_ID) {
        return Ok(false);
    }
    if store.get(LEGACY_KEY)?.is_none() {
        return Ok(false);
    }
    if publish::load_token_at(store, &profile_key(MIGRATED_ID))?.is_none() {
        return Ok(false);
    }
    store.delete(LEGACY_KEY)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests;
