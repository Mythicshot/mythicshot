//! The Workspace page of the main window: the tour site Mythicshot publishes to. The site
//! address, whether this computer is connected to it and to which workspace, Sign in (the
//! browser confirms a short code and the site gives this computer its own token), a token
//! pasted by hand instead, Sign out, and what the site says about itself: how many tours it
//! holds and its version.
//!
//! Everything that talks to the site or to the credential store runs on a background
//! thread. The token field is only ever a masked input. It is emptied, wiped and given a
//! new id (dropping its undo history) on Save, on Sign out, on Cancel and when the page is
//! left or the window closes, and a saved token is never read back into it.
//!
//! The token belongs to the address it was saved for, and is sent to no other. With another
//! address in the field the page says whose token it is and offers to use it here too (an
//! explicit click) or to replace it; until then nothing is sent. A token from Sign in belongs
//! to the address the sign-in was started for, even when the field was changed meanwhile.
//!
//! The site is asked on its own only when the page is opened and after a token is saved,
//! and only at the address as it stood then. While the address is being typed nothing is
//! sent: every half-typed address would otherwise be handed the token. An edited address
//! is checked when "Test connection" is pressed. Sign in sends no token, and only on a click,
//! to an address the address rule accepts.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use eframe::egui::{self, RichText};

use crate::theme;
use crate::publish::connect::{self, Clock, ConnectError};
use crate::publish::{self, RemoteTour};
use crate::publish_ui::{self, TokenStatus};
use crate::sign_in_ui::{self, Code, Opener, Reporter, SignInAction, SignInDialog, SignInJob, Starter};
use crate::tours_ui::Site;


pub const NOT_CONNECTED: &str = "Not connected";
pub const CHECKING: &str = "Checking…";
pub const TOKEN_NOTE: &str = "The token is kept in Windows Credential Manager, not in the settings file.";
pub const PRESS_TEST: &str = "Press Test connection to check this address.";
pub const SIGN_IN: &str = "Sign in";
pub const SIGN_IN_HINT: &str = "Sign in opens your browser. You confirm a short code there, and this computer is connected.";
pub const PASTE_INSTEAD: &str = "Paste a token instead";
pub const THIS_COMPUTER: &str = "This computer";
pub const SIGN_OUT: &str = "Sign out";
/// After "Signed in as …": the same as Sign out, for a code someone else confirmed.
pub const NOT_YOU: &str = "Not you? Sign out";
/// The most characters of an account name "Signed in as" shows.
pub const ACCOUNT_SHOWN_MAX: usize = 30;

/// "Signed in as alex.": the account that confirmed the code, cleaned as plain text and at most
/// [`ACCOUNT_SHOWN_MAX`] characters. None when there is nothing to show.
pub fn signed_in_as(account: &str) -> Option<String> {
    let name = publish::share::plain_text(account);
    (!name.is_empty()).then(|| format!("Signed in as {}.", publish_ui::short_title(&name, ACCOUNT_SHOWN_MAX)))
}
/// What a row shows when there is nothing to say yet.
const NOTHING: &str = "—";

/// What the page is told about the token each frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Conn {
    pub token: TokenStatus,
    /// Changes whenever the token is saved, replaced or forgotten.
    pub epoch: u64,
    /// Debug builds: the address from `MYTHICSHOT_TEST_PUBLISH`, shown instead of the saved one.
    pub test_server: Option<String>,
    /// The address the saved token belongs to. It is sent to no other.
    pub bound: Option<String>,
    /// The site refused the saved token the last time it was sent ([`publish_ui::token_refused`]).
    pub refused: bool,
    /// The active profile when the frame was drawn: a check is pinned to it, and its answer is
    /// kept for it alone.
    pub profile: Option<String>,
}

impl Conn {
    pub fn current() -> Conn {
        Conn { token: publish_ui::token_status(), epoch: publish_ui::token_epoch(), test_server: publish_ui::test_server(), bound: publish_ui::token_site(), refused: publish_ui::token_refused(), profile: publish_ui::pin().profile }
    }

    /// Why the saved token may not go to `address`, when it may not.
    fn binding_problem(&self, address: &str) -> Option<String> {
        if self.token != TokenStatus::Saved {
            return None;
        }
        publish_ui::binding_problem(self.bound.as_deref(), address)
    }

    /// The site at `address`: only with a usable address and a token saved for that address.
    fn site(&self, address: &str) -> Option<Site> {
        if self.token != TokenStatus::Saved || self.binding_problem(address).is_some() {
            return None;
        }
        Some(Site { profile: self.profile.clone(), server: publish::check_server(address).ok()?, epoch: self.epoch })
    }
}

/// How many tours the workspace holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub public: usize,
    pub draft: usize,
}

impl Counts {
    pub fn of(tours: &[RemoteTour]) -> Counts {
        let public = tours.iter().filter(|t| t.is_public()).count();
        Counts { public, draft: tours.len() - public }
    }
}

/// What the site said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub workspace: String,
    /// None for a site too old to tell.
    pub version: Option<String>,
    /// None when the tour list could not be read.
    pub tours: Option<Counts>,
}

/// The "Tours" row: "2 (1 public, 1 draft)".
pub fn tours_summary(c: Counts) -> String {
    match c.public + c.draft {
        0 => "0".to_string(),
        total => format!("{total} ({} public, {} draft)", c.public, c.draft),
    }
}

/// The "Site version" row: the version, or "unknown" for a site that does not tell.
pub fn version_label(version: Option<&str>) -> String {
    version.map_or_else(|| "unknown".to_string(), str::to_string)
}

/// The "Status" row: "Checking…", "Connected to <workspace>" or "Not connected". `workspace`
/// is the name the site gave, only while it is connected.
pub fn status(token: &TokenStatus, checking: bool, workspace: Option<&str>) -> String {
    if *token == TokenStatus::Checking || checking {
        CHECKING.to_string()
    } else if let Some(name) = workspace {
        format!("Connected to {}", publish_ui::short_title(&publish::share::plain_text(name), publish_ui::ROW_TITLE_MAX))
    } else {
        NOT_CONNECTED.to_string()
    }
}

/// Whether the "Status" row says connected.
pub fn is_connected_status(text: &str) -> bool {
    text.starts_with("Connected to ")
}

/// What the "Token" row holds.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenRow {
    Checking,
    /// "Paste a token instead": the paste field is one click away.
    Link,
    /// The paste field with Save, and Cancel when it replaces a saved token.
    Paste { replacing: bool },
    /// The credential store's message, the paste field, and Forget so that what cannot be read can be removed.
    Unavailable(String),
}

/// The "Token" row: `pasting` is whether "Paste a token instead" was clicked.
pub fn token_row(token: &TokenStatus, pasting: bool) -> TokenRow {
    match token {
        TokenStatus::Checking => TokenRow::Checking,
        TokenStatus::Saved | TokenStatus::Missing if !pasting => TokenRow::Link,
        TokenStatus::Saved => TokenRow::Paste { replacing: true },
        TokenStatus::Missing => TokenRow::Paste { replacing: false },
        TokenStatus::Unavailable(e) => TokenRow::Unavailable(e.clone()),
    }
}

/// Which rows the page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rows {
    /// "This computer", with its name and Sign out: a token is saved.
    pub this_computer: bool,
    /// "Sign in" and "Token", and the hint under the page: there is no token for this address, or
    /// the site refused it. Not for a saved token whose site is merely unreachable or failing:
    /// signing in again would make a second connection on the site.
    pub sign_in: bool,
    /// "Tours" and "Site version": connected.
    pub details: bool,
}

/// The rows for this token: `connected` when the site answered for it, `checking` while it is
/// asked, `refused` when the site refused it (401), `for_here` when it belongs to this address.
pub fn rows(token: &TokenStatus, connected: bool, checking: bool, refused: bool, for_here: bool) -> Rows {
    let saved = *token == TokenStatus::Saved;
    let sign_in = match token {
        _ if connected => false,
        TokenStatus::Checking => false,
        // While it is being checked the page waits for the answer instead of offering to sign in.
        TokenStatus::Saved => !for_here || (refused && !checking),
        TokenStatus::Missing | TokenStatus::Unavailable(_) => true,
    };
    Rows { this_computer: saved, sign_in, details: connected }
}

/// Whether "Sign in" can be pressed: nothing else is running, no sign-in is open, and the
/// address is one the address rule accepts (the same check a saved token's address gets).
pub fn sign_in_enabled(busy: bool, signing_in: bool, address: &str) -> bool {
    !busy && !signing_in && publish::check_server(address).is_ok()
}

/// The address "Open site" opens: the site address when it is one, by the link rule.
pub fn site_link(address: &str) -> Option<String> {
    publish::check_server(address).ok().filter(|url| mythicshot_destinations::is_web_link(url))
}

/// The last answer of the site, and whether it is being asked.
#[derive(Default)]
struct Check {
    generation: u64,
    running: bool,
    done: Option<(Site, Result<Info, String>)>,
}

impl Check {
    fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.running = true;
        self.generation
    }

    /// The answer of request `generation`; one that a newer request has overtaken is dropped.
    fn finish(&mut self, generation: u64, site: Site, result: Result<Info, String>) {
        if generation == self.generation {
            self.running = false;
            self.done = Some((site, result));
        }
    }

    /// What the site said, when it was `site` that was asked and it answered.
    fn info_for(&self, site: Option<&Site>) -> Option<&Info> {
        match (&self.done, site) {
            (Some((asked, Ok(info))), Some(site)) if asked == site => Some(info),
            _ => None,
        }
    }

    fn was_asked(&self, site: &Site) -> bool {
        self.done.as_ref().is_some_and(|(asked, _)| asked == site)
    }

    /// Whether `site` refused the token (401) when it was last asked.
    fn rejected(&self, site: Option<&Site>) -> bool {
        matches!((&self.done, site), (Some((asked, Err(e))), Some(site)) if asked == site && e == publish::REJECTED_TOKEN)
    }
}

/// Where a check gets the token for the address it is about to ask.
type TokenSource = Arc<dyn Fn(&crate::profiles::Pin) -> Result<String, String> + Send + Sync>;
/// Ties the saved token to an address (None: to none).
type Bind = Arc<dyn Fn(Option<String>) + Send + Sync>;

/// Stores a token in the credential store; the error is the store's own sentence.
type Store = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
/// Removes the token from the credential store.
type Forget = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The credential store as the page uses it: store and forget the token, and look it up again
/// after either. The real one is Windows Credential Manager; tests use a fake.
#[derive(Clone)]
pub struct Vault {
    pub store: Store,
    pub forget: Forget,
    pub refresh: Arc<dyn Fn() + Send + Sync>,
}

impl Default for Vault {
    fn default() -> Self {
        // A token is stored for a profile only through its check (the add form); Sign out signs
        // out of the active profile, which then waits for a new sign-in.
        Vault { store: Arc::new(publish_ui::store_without_check), forget: Arc::new(publish_ui::forget_active_token), refresh: Arc::new(publish_ui::refresh_token_now) }
    }
}

/// What Save shows, and what it ties the token to: the address in the field when the
/// token was stored, and nothing when it was not. A failed save changes no tie.
fn after_save(stored: Result<(), String>, site: Option<String>, bind: &Bind) -> Result<String, String> {
    stored?;
    bind(site);
    Ok("Token saved.".to_string())
}

/// The same for Sign out: with no token, there is nothing to tie.
fn after_forget(forgotten: Result<(), String>, bind: &Bind) -> Result<String, String> {
    forgotten?;
    bind(None);
    Ok("Token removed from Windows Credential Manager.".to_string())
}

/// What Sign in does on its own thread, for the site address `server` (as the address rule writes
/// it): ask for a code, show it, wait for it to be confirmed, then store the token through the
/// same path as Save, tied to `server`. A cancel that comes before the token is stored stores
/// nothing. The token never leaves this thread.
fn sign_in_work(server: String, vault: Vault, bind: Bind, clock: Arc<dyn Clock + Send + Sync>) -> impl FnOnce(&Reporter) -> Result<Option<String>, ConnectError> + Send + 'static {
    move |r| {
        let started = connect::start(&server, &connect::device_name())?;
        r.code(Code::of(&started));
        let connected = connect::wait(&started, &|| r.cancelled(), &*clock)?;
        if r.cancelled() {
            return Err(ConnectError::Cancelled);
        }
        // The token belongs to the address the sign-in was started for, whatever the field holds now.
        let result = after_save((vault.store)(connected.token.expose()), Some(started.server.clone()), &bind);
        let account = connected.account.clone();
        drop(connected);
        (vault.refresh)();
        result.map(|_| account).map_err(ConnectError::Failed)
    }
}

/// The page's own state.
pub struct WorkspaceState {
    token_input: String,
    /// Part of the token field's id; a new value gives the field a fresh, empty state.
    field: u64,
    /// The token field's id as last drawn, so its stored state can be removed.
    field_id: Option<egui::Id>,
    /// "Paste a token instead" was clicked: the paste field shows.
    pasting: bool,
    /// The last Save, Sign out or Test connection result.
    message: Arc<Mutex<Option<Outcome>>>,
    /// A Save, Sign out or Test connection is running on its background thread.
    busy: Arc<Mutex<bool>>,
    check: Arc<Mutex<Check>>,
    /// The site that was last asked without a click, or whose address was typed: it is not asked again on its own.
    auto: Option<Site>,
    /// The "Sign in" window, while it is open. Dropping it cancels its sign-in.
    sign_in: Option<SignInDialog>,
    /// After a sign-in: the account the site said confirmed the code, and the token epoch it
    /// belongs to (taken on the first frame after the sign-in). Shown until the page is left or the token changes; never stored.
    account: Option<(String, Option<u64>)>,
    token: TokenSource,
    bind: Bind,
    vault: Vault,
    open: Opener,
    clock: Arc<dyn Clock + Send + Sync>,
    /// The page shows the active profile: its address cannot be edited here, and signing in
    /// (again) and the profile's token are the profiles panel's above (see `profiles_ui`).
    pub profile_mode: bool,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            token_input: String::new(),
            field: 0,
            field_id: None,
            pasting: false,
            message: Arc::default(),
            busy: Arc::default(),
            check: Arc::default(),
            auto: None,
            sign_in: None,
            account: None,
            token: Arc::new(publish_ui::token_for_pin),
            bind: Arc::new(publish_ui::bind_token),
            vault: Vault::default(),
            open: Arc::new(sign_in_ui::open_in_browser),
            clock: Arc::new(connect::RealClock::new()),
            profile_mode: false,
        }
    }
}

/// A finished Save, Sign out or Test connection.
#[derive(Clone)]
struct Outcome {
    text: String,
    ok: bool,
    /// For Test connection: the address it tested. The result shows only while the field still holds it.
    server: Option<String>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ask the site for its name and version, then count its tours. A list that cannot be read
/// leaves the count unknown; the connection itself still stands.
fn ask(site: &Site, token: &str) -> Result<Info, String> {
    let server = site.server.as_str();
    let ws = publish::workspace(server, token)?;
    // Kept for the profile the check was pinned to, whichever is active by now.
    if let Some(id) = &site.profile {
        publish_ui::note_check(id, server, &ws);
    }
    let tours = publish::list_tours(server, token).ok().map(|t| Counts::of(&t));
    Ok(Info { workspace: ws.name, version: ws.version, tours })
}

impl WorkspaceState {
    /// A page whose checks get the token from `token` instead of the credential store.
    #[cfg(test)]
    fn with_token(token: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Self {
        Self { token: Arc::new(move |pin: &crate::profiles::Pin| token(&pin.server)), ..Default::default() }
    }

    fn is_busy(&self) -> bool {
        *lock(&self.busy)
    }

    /// Empty the token field: wipe its text, remove its stored state with the undo
    /// history, and give it a new id.
    pub fn clear_token_field(&mut self, ctx: &egui::Context) {
        publish_ui::wipe(&mut self.token_input);
        if let Some(id) = self.field_id.take() {
            ctx.data_mut(|d| d.remove::<egui::text_edit::TextEditState>(id));
        }
        self.field += 1;
    }

    /// The page was left, or the window closed: a token typed but not saved is wiped, a
    /// running sign-in is cancelled, and the next visit asks the site again.
    pub fn leave(&mut self, ctx: &egui::Context) {
        self.clear_token_field(ctx);
        self.pasting = false;
        self.auto = None;
        self.sign_in = None;
        self.account = None;
    }

    /// Whether the "Sign in" window is open.
    #[cfg(test)]
    pub fn is_signing_in(&self) -> bool {
        self.sign_in.is_some()
    }

    /// The typed token for Save; the field is cleared as it is taken.
    fn take_for_save(&mut self, ctx: &egui::Context) -> String {
        let entered = self.token_input.clone();
        self.clear_token_field(ctx);
        entered
    }

    /// Run `work` on a background thread and show what it returns. `server` is the
    /// address a Test connection checks.
    fn run(&self, server: Option<String>, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        *lock(&self.busy) = true;
        *lock(&self.message) = None;
        let (busy, message) = (self.busy.clone(), self.message.clone());
        std::thread::spawn(move || {
            let (text, ok) = match work() {
                Ok(text) => (text, true),
                Err(text) => (text, false),
            };
            *lock(&message) = Some(Outcome { text, ok, server });
            *lock(&busy) = false;
        });
    }

    /// Ask `site` about itself on a background thread. `announce` is the address as typed
    /// when this is a Test connection, whose result is also shown as a sentence.
    fn spawn_check(&self, site: Site, announce: Option<String>) {
        let generation = lock(&self.check).begin();
        let (check, token) = (self.check.clone(), self.token.clone());
        let work = move || {
            let result = token(&site.pin()).and_then(|token| ask(&site, &token));
            lock(&check).finish(generation, site, result.clone());
            result.map(|info| format!("Connected to workspace \"{}\"", info.workspace))
        };
        match announce {
            Some(typed) => self.run(Some(typed), work),
            None => {
                std::thread::spawn(work);
            }
        }
    }

    /// What starts a sign-in for an address: the work above, with this page's store and clock.
    fn starter(&self) -> Starter {
        let (vault, bind, clock) = (self.vault.clone(), self.bind.clone(), self.clock.clone());
        Arc::new(move |server| SignInJob::spawn(sign_in_work(server, vault.clone(), bind.clone(), clock.clone())))
    }

    /// "Sign in": open the window for `address`, when the address rule accepts it.
    fn begin_sign_in(&mut self, ctx: &egui::Context, address: &str) {
        let Ok(site) = publish::check_server(address) else { return };
        self.clear_token_field(ctx);
        self.pasting = false;
        self.clear_message();
        self.sign_in = Some(SignInDialog::open(site, self.starter(), self.open.clone()));
    }

    /// The result to show beside Test connection, if it still applies to `server`.
    fn current_message(&self, server: &str) -> Option<(String, bool)> {
        let m = lock(&self.message).clone()?;
        match &m.server {
            Some(tested) if tested.trim() != server.trim() => None,
            _ => Some((m.text, m.ok)),
        }
    }

    fn clear_message(&self) {
        *lock(&self.message) = None;
    }

    /// Whether a token has been typed and not saved.
    #[cfg(test)]
    pub fn has_unsaved_token(&self) -> bool {
        !self.token_input.is_empty()
    }

    #[cfg(test)]
    pub fn type_token_for_test(&mut self, token: &str) {
        self.token_input = token.to_string();
    }
}

/// The masked token field. Egui's undo history for it is dropped every frame, so an
/// earlier text can never be brought back with Ctrl+Z.
fn token_field(ui: &mut egui::Ui, p: &mut WorkspaceState, hint: &str, width: f32) -> egui::Response {
    let edit = egui::TextEdit::singleline(&mut p.token_input).id_salt(("publish-token", p.field)).password(true).hint_text(hint).desired_width(width);
    let resp = ui.add(edit);
    p.field_id = Some(resp.id);
    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), resp.id) {
        state.clear_undoer();
        state.store(ui.ctx(), resp.id);
    }
    resp
}

/// Draw the page. `server` is the site address in the settings; returns whether it changed.
pub fn ui(ui: &mut egui::Ui, p: &mut WorkspaceState, server: &mut String, conn: &Conn) -> bool {
    let busy = p.is_busy();
    let mut changed = false;
    // The address this page works with: the test hook's when set, else the saved one.
    let effective = conn.test_server.clone().unwrap_or_else(|| server.clone());
    let site = conn.site(&effective);
    let problem = conn.binding_problem(&effective);

    // On opening the page, and after a token was saved: ask the site once, without a click.
    if let Some(site) = &site
        && p.auto.as_ref() != Some(site)
        && !busy
    {
        p.auto = Some(site.clone());
        p.spawn_check(site.clone(), None);
    }
    let (checking, info, asked) = {
        let c = lock(&p.check);
        (c.running, c.info_for(site.as_ref()).cloned(), site.as_ref().is_some_and(|s| c.was_asked(s)))
    };
    // A token the site refused is not a connection, whatever an earlier answer said.
    let refused = conn.refused || lock(&p.check).rejected(site.as_ref());
    let info = info.filter(|_| !refused);
    let connected = info.is_some();
    let mut rows = rows(&conn.token, connected, checking, refused, problem.is_none());
    if p.profile_mode {
        // A profile is signed in (again) only through its check, in the panel above.
        rows.sign_in = false;
    }
    // "Signed in as" belongs to the token the sign-in saved, and goes when it changes.
    if conn.token != TokenStatus::Checking
        && let Some((_, epoch)) = p.account.as_mut()
    {
        match epoch {
            None if conn.token == TokenStatus::Saved => *epoch = Some(conn.epoch),
            Some(e) if *e == conn.epoch && conn.token == TokenStatus::Saved => {}
            _ => p.account = None,
        }
    }
    if busy || checking || conn.token == TokenStatus::Checking {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    if !p.profile_mode {
        ui.heading("Workspace");
        ui.add_space(12.0);
    }

    let row = |ui: &mut egui::Ui, name: &str| {
        ui.label(RichText::new(name).color(theme::MUTED));
    };
    let value = |ui: &mut egui::Ui, text: Option<String>| {
        ui.label(text.unwrap_or_else(|| NOTHING.to_string()));
    };
    egui::Grid::new("workspace-rows").num_columns(2).spacing([18.0, 12.0]).show(ui, |ui| {
        row(ui, "Site");
        ui.vertical(|ui| {
            match &conn.test_server {
                Some(t) => {
                    let mut shown = t.clone();
                    ui.add_enabled(false, egui::TextEdit::singleline(&mut shown).desired_width(340.0));
                    ui.label(RichText::new("Debug build: set by MYTHICSHOT_TEST_PUBLISH for this run, not saved.").color(theme::MUTED).size(12.0));
                }
                None if p.profile_mode => {
                    // The address is the profile's own.
                    let mut shown = server.clone();
                    ui.add_enabled(false, egui::TextEdit::singleline(&mut shown).desired_width(340.0));
                }
                None => {
                    if ui.add(egui::TextEdit::singleline(server).hint_text("https://tours.example.com").desired_width(340.0)).changed() {
                        changed = true;
                        p.clear_message();
                        // A typed address is never asked on its own; see the module's first lines.
                        if let Some(typed) = conn.site(server) {
                            p.auto = Some(typed);
                        }
                    }
                }
            }
            if !effective.trim().is_empty()
                && let Err(e) = publish::check_server(&effective)
            {
                theme::caution_line(ui, e, theme::SMALL_SIZE, theme::Fit::Wrap);
            }
        });
        ui.end_row();

        row(ui, "Status");
        ui.vertical(|ui| {
            let label = status(&conn.token, checking && conn.token == TokenStatus::Saved, info.as_ref().map(|i| i.workspace.as_str()));
            let (fg, bg) = if is_connected_status(&label) { (theme::SUCCESS_TEXT, theme::SUCCESS_TINT) } else { (theme::MUTED, theme::CHIP) };
            egui::Frame::new().fill(bg).corner_radius(egui::CornerRadius::same(theme::RADIUS_FULL)).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
                ui.label(RichText::new(label).color(fg));
            });
            if refused && conn.token == TokenStatus::Saved {
                theme::caution_line(ui, publish::REJECTED_TOKEN, theme::SMALL_SIZE, theme::Fit::Wrap);
            }
            if let Some(line) = p.account.as_ref().and_then(|(name, _)| signed_in_as(name)) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(line).color(theme::MUTED).size(12.0));
                    if ui.add_enabled(!busy, egui::Link::new(RichText::new(NOT_YOU).size(12.0))).clicked() {
                        sign_out(ui.ctx(), p);
                    }
                });
            }
        });
        ui.end_row();

        if rows.this_computer {
            row(ui, THIS_COMPUTER);
            ui.horizontal(|ui| {
                ui.label(connect::device_name());
                ui.label(RichText::new("·").color(theme::MUTED));
                // Today's "forget the token": it is removed from this computer; nothing changes on the site.
                if ui.add_enabled(!busy, egui::Link::new(SIGN_OUT)).clicked() {
                    sign_out(ui.ctx(), p);
                }
            });
            ui.end_row();
        }

        if rows.sign_in && conn.token != TokenStatus::Checking {
            row(ui, SIGN_IN);
            if ui.add_enabled(sign_in_enabled(busy, p.sign_in.is_some(), &effective), egui::Button::new(SIGN_IN)).clicked() {
                p.begin_sign_in(ui.ctx(), &effective);
            }
            ui.end_row();
        }

        if (rows.sign_in || conn.token == TokenStatus::Checking) && !p.profile_mode {
            row(ui, "Token");
            ui.vertical(|ui| token_ui(ui, p, conn, &effective, busy));
            ui.end_row();
        }

        if rows.details {
            row(ui, "Tours");
            value(ui, info.as_ref().and_then(|i| i.tours).map(tours_summary));
            ui.end_row();

            row(ui, "Site version");
            value(ui, info.as_ref().map(|i| version_label(i.version.as_deref())));
            ui.end_row();
        }
    });

    // The token was saved for another address: say so, and send nothing until the user decides.
    if let Some(problem) = problem.as_ref().filter(|_| !p.profile_mode) {
        ui.add_space(10.0);
        theme::caution_line(ui, problem.as_str(), theme::BODY_SIZE, theme::Fit::Wrap);
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new(publish_ui::USE_IT_HERE)).clicked() {
                (p.bind)(publish::check_server(&effective).ok());
                p.clear_message();
            }
        });
        ui.add(egui::Label::new(RichText::new(publish_ui::USE_IT_HERE_MEANS).color(theme::MUTED).size(12.0)).wrap());
    }

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy && !checking, egui::Button::new("Test connection")).clicked() {
            let tested = effective.clone();
            match publish::check_server(&tested) {
                Ok(_) if problem.is_some() => *lock(&p.message) = Some(Outcome { text: problem.clone().unwrap_or_default(), ok: false, server: Some(tested) }),
                Ok(checked) => {
                    let site = Site { profile: conn.profile.clone(), server: checked, epoch: conn.epoch };
                    p.auto = Some(site.clone());
                    p.spawn_check(site, Some(tested));
                }
                Err(e) => *lock(&p.message) = Some(Outcome { text: e, ok: false, server: Some(tested) }),
            }
        }
        let link = site_link(&effective);
        if ui.add_enabled(link.is_some(), egui::Button::new("Open site")).clicked()
            && let Some(link) = link
            && let Err(e) = mythicshot_destinations::open_url(&link)
        {
            *lock(&p.message) = Some(Outcome { text: format!("Could not open the site: {e}."), ok: false, server: None });
        }
        if busy || checking {
            ui.add(egui::Spinner::new());
        } else if let Some((text, ok)) = p.current_message(&effective) {
            if ok {
                ui.add(egui::Label::new(RichText::new(text).color(theme::SUCCESS_TEXT)).wrap());
            } else {
                theme::caution_line(ui, text, theme::BODY_SIZE, theme::Fit::Wrap);
            }
        } else if site.is_some() && !asked {
            ui.label(RichText::new(PRESS_TEST).color(theme::MUTED).size(12.0));
        }
    });
    ui.add_space(6.0);
    if rows.sign_in && conn.token != TokenStatus::Checking {
        ui.add(egui::Label::new(RichText::new(SIGN_IN_HINT).color(theme::MUTED).size(12.0)).wrap());
    }
    if conn.token == TokenStatus::Saved || (p.pasting && !p.profile_mode) {
        ui.label(RichText::new(TOKEN_NOTE).color(theme::MUTED).size(12.0));
    }

    // The "Sign in" window, over the page.
    if let Some(d) = p.sign_in.as_mut() {
        match d.show(ui.ctx()) {
            None => {}
            Some(SignInAction::Closed) => p.sign_in = None,
            // The token is saved and tied to the address the sign-in was for: the page asks the site again by itself.
            Some(SignInAction::Connected(account)) => {
                p.sign_in = None;
                // The token was looked up again before the window heard of it: the next frame's epoch is the new token's.
                p.account = account.map(|name| (name, None));
            }
        }
    }
    changed
}

/// Sign out (and "Not you? Sign out"): today's "forget the token". It is removed from this
/// computer and untied from its address; nothing changes on the site.
fn sign_out(ctx: &egui::Context, p: &mut WorkspaceState) {
    p.clear_token_field(ctx);
    p.pasting = false;
    p.account = None;
    let (bind, vault) = (p.bind.clone(), p.vault.clone());
    p.run(None, move || {
        let result = after_forget((vault.forget)(), &bind);
        (vault.refresh)();
        result
    });
}

/// The "Token" row: the way to paste a token, or the field to paste one into.
fn token_ui(ui: &mut egui::Ui, p: &mut WorkspaceState, conn: &Conn, address: &str, busy: bool) {
    let paste = |ui: &mut egui::Ui, p: &mut WorkspaceState, hint: &str| {
        token_field(ui, p, hint, 240.0);
        if ui.add_enabled(!busy && !p.token_input.trim().is_empty(), egui::Button::new("Save")).clicked() {
            // The field is emptied now; the token goes to the background thread and nowhere else.
            let mut entered = p.take_for_save(ui.ctx());
            p.pasting = false;
            // The token belongs to the address that is in the field now.
            let (bind, vault, site) = (p.bind.clone(), p.vault.clone(), publish::check_server(address).ok());
            p.run(None, move || {
                let result = after_save((vault.store)(&entered), site, &bind);
                publish_ui::wipe(&mut entered);
                (vault.refresh)();
                result
            });
        }
    };
    match token_row(&conn.token, p.pasting) {
        TokenRow::Checking => {
            ui.label(RichText::new("Checking for a saved token…").color(theme::MUTED));
        }
        TokenRow::Link => {
            if ui.add_enabled(!busy, egui::Link::new(PASTE_INSTEAD)).clicked() {
                p.pasting = true;
            }
        }
        TokenRow::Paste { replacing } => {
            ui.horizontal(|ui| {
                paste(ui, p, if replacing { "Paste the new token" } else { "Paste the token from the site's setup page" });
                if replacing && ui.button("Cancel").clicked() {
                    p.clear_token_field(ui.ctx());
                    p.pasting = false;
                }
            });
            if replacing {
                ui.label(RichText::new("The saved token stays until the new one is saved.").color(theme::MUTED).size(12.0));
            }
        }
        TokenRow::Unavailable(e) => {
            theme::caution_line(ui, e.as_str(), theme::SMALL_SIZE, theme::Fit::Wrap);
            ui.horizontal(|ui| {
                paste(ui, p, "Paste the token from the site's setup page");
                // Forget stays available when the store holds something it cannot read, so it can be removed.
                if ui.add_enabled(!busy, egui::Button::new("Forget")).clicked() {
                    p.clear_token_field(ui.ctx());
                    p.pasting = false;
                    let (bind, vault) = (p.bind.clone(), p.vault.clone());
                    p.run(None, move || {
                        let result = after_forget((vault.forget)(), &bind);
                        (vault.refresh)();
                        result
                    });
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::tests::{Stub, reply, stub};
    use std::time::Instant;

    const TOKEN: &str = "dp_0123456789abcdef0123456789abcdef01234567";

    /// One frame of just the token field, `t` seconds in, with these input events.
    fn frame(ctx: &egui::Context, p: &mut WorkspaceState, t: f64, events: Vec<egui::Event>) {
        let input = egui::RawInput { time: Some(t), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            token_field(ui, p, "", 300.0);
        });
        out.textures_delta.clear();
    }

    fn focus(ctx: &egui::Context, p: &WorkspaceState) {
        let id = p.field_id.expect("drawn");
        ctx.memory_mut(|m| m.request_focus(id));
    }

    fn undo() -> egui::Event {
        egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }
    }

    #[test]
    fn after_save_the_token_cannot_be_brought_back_by_undo() {
        let ctx = crate::theme::test_context();
        let mut p = WorkspaceState::default();
        frame(&ctx, &mut p, 0.0, vec![]);
        focus(&ctx, &p);
        frame(&ctx, &mut p, 1.0, vec![]);
        frame(&ctx, &mut p, 2.0, vec![egui::Event::Text(TOKEN.into())]);
        for t in 3..8 {
            frame(&ctx, &mut p, t as f64 * 2.0, vec![]);
        }
        assert_eq!(p.token_input, TOKEN);
        let old = p.field_id.unwrap();
        let taken = p.take_for_save(&ctx);
        assert_eq!(taken, TOKEN);
        assert!(p.token_input.is_empty());
        assert!(egui::text_edit::TextEditState::load(&ctx, old).is_none(), "the old field's state is gone");
        frame(&ctx, &mut p, 20.0, vec![]);
        assert_ne!(p.field_id, Some(old), "the field has a fresh id");
        focus(&ctx, &p);
        frame(&ctx, &mut p, 21.0, vec![]);
        frame(&ctx, &mut p, 22.0, vec![undo()]);
        frame(&ctx, &mut p, 23.0, vec![undo()]);
        assert!(p.token_input.is_empty(), "undo brought back {:?}", p.token_input);
    }

    #[test]
    fn undo_never_restores_a_token_even_in_the_same_field() {
        let ctx = crate::theme::test_context();
        let mut p = WorkspaceState::default();
        frame(&ctx, &mut p, 0.0, vec![]);
        focus(&ctx, &p);
        frame(&ctx, &mut p, 1.0, vec![]);
        frame(&ctx, &mut p, 2.0, vec![egui::Event::Text(TOKEN.into())]);
        for t in 3..8 {
            frame(&ctx, &mut p, t as f64 * 2.0, vec![]);
        }
        // Select all and delete, then undo.
        let key = |k, m| egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m };
        frame(&ctx, &mut p, 20.0, vec![key(egui::Key::A, egui::Modifiers::COMMAND), key(egui::Key::Backspace, egui::Modifiers::NONE)]);
        assert!(p.token_input.is_empty());
        frame(&ctx, &mut p, 22.0, vec![]);
        frame(&ctx, &mut p, 24.0, vec![undo()]);
        assert!(p.token_input.is_empty(), "undo brought back {:?}", p.token_input);
    }

    #[test]
    fn closing_or_leaving_the_page_clears_an_unsaved_token() {
        let ctx = crate::theme::test_context();
        let mut p = WorkspaceState { token_input: TOKEN.into(), pasting: true, ..Default::default() };
        let before = p.field;
        p.clear_token_field(&ctx);
        assert!(p.token_input.is_empty());
        assert_ne!(p.field, before);
        // Leaving the page does the same, and closes the paste field.
        let mut p = WorkspaceState { token_input: TOKEN.into(), pasting: true, auto: Some(site("https://a.example.com")), ..Default::default() };
        p.leave(&ctx);
        assert!(!p.has_unsaved_token() && !p.pasting && p.auto.is_none());
    }

    #[test]
    fn a_test_result_only_shows_for_the_address_it_tested() {
        let p = WorkspaceState::default();
        *p.message.lock().unwrap() = Some(Outcome { text: "Connected to workspace \"Acme\"".into(), ok: true, server: Some("https://a.example.com".into()) });
        assert!(p.current_message("https://a.example.com").is_some());
        assert!(p.current_message(" https://a.example.com ").is_some());
        assert!(p.current_message("https://b.example.com").is_none());
        // Save and Forget results are not tied to an address.
        *p.message.lock().unwrap() = Some(Outcome { text: "Token saved.".into(), ok: true, server: None });
        assert!(p.current_message("https://b.example.com").is_some());
    }

    fn site(server: &str) -> Site {
        Site { profile: None, server: server.into(), epoch: 1 }
    }

    /// A connection whose token belongs to whatever address the page holds: the tie is not what these tests are about.
    fn conn(token: TokenStatus) -> Conn {
        Conn { token, epoch: 1, test_server: None, bound: None, refused: false, profile: None }
    }

    fn bound_to(stub: &Stub, token: TokenStatus) -> Conn {
        Conn { bound: Some(stub.base.clone()), ..conn(token) }
    }

    #[test]
    fn the_rows_say_what_the_site_said_and_unknown_for_an_older_site() {
        assert_eq!(tours_summary(Counts { public: 1, draft: 1 }), "2 (1 public, 1 draft)");
        assert_eq!(tours_summary(Counts { public: 3, draft: 0 }), "3 (3 public, 0 draft)");
        assert_eq!(tours_summary(Counts { public: 0, draft: 1 }), "1 (0 public, 1 draft)");
        assert_eq!(tours_summary(Counts { public: 0, draft: 0 }), "0");
        assert_eq!(version_label(Some("0.1.1")), "0.1.1");
        assert_eq!(version_label(None), "unknown");
        let t = |status: &str| RemoteTour { id: "t".into(), title: "T".into(), status: status.into(), chapters: 1, steps: 1, link: None, updated_at: None, published_at: None, unpublished_changes: false };
        assert_eq!(Counts::of(&[t("published"), t("draft"), t("draft")]), Counts { public: 1, draft: 2 });
        assert_eq!(Counts::of(&[]), Counts { public: 0, draft: 0 });
    }

    #[test]
    fn the_status_the_rows_and_the_token_row_follow_the_saved_state() {
        let saved = TokenStatus::Saved;
        assert_eq!(status(&saved, false, Some("Cowfee Raid Tools")), "Connected to Cowfee Raid Tools");
        assert!(is_connected_status(&status(&saved, false, Some("Acme"))));
        assert_eq!(status(&saved, true, Some("Acme")), CHECKING);
        assert_eq!(status(&saved, true, None), CHECKING);
        assert_eq!(status(&saved, false, None), NOT_CONNECTED);
        assert_eq!(status(&TokenStatus::Checking, false, None), CHECKING);
        assert_eq!(status(&TokenStatus::Missing, false, None), NOT_CONNECTED);
        assert_eq!(status(&TokenStatus::Unavailable("locked".into()), false, None), NOT_CONNECTED);
        // A name that hides text, or a very long one, is shown plain and short.
        assert_eq!(status(&saved, false, Some("Acme\u{202e}\u{200b}")), "Connected to Acme");
        assert_eq!(status(&saved, false, Some(&"x".repeat(80))), format!("Connected to {}…", "x".repeat(39)));
        assert!(!is_connected_status(NOT_CONNECTED) && !is_connected_status(CHECKING));

        // rows(token, connected, checking, refused, for this address)
        // Connected: this computer, and the site's details; no Sign in.
        assert_eq!(rows(&saved, true, false, false, true), Rows { this_computer: true, sign_in: false, details: true });
        // Refused by the site, or saved for another address: Sign in again, or Sign out.
        assert_eq!(rows(&saved, false, false, true, true), Rows { this_computer: true, sign_in: true, details: false });
        assert_eq!(rows(&saved, false, false, false, false), Rows { this_computer: true, sign_in: true, details: false });
        // Saved for this address but the site is unreachable or failing: no Sign in, which would make a second connection.
        assert_eq!(rows(&saved, false, false, false, true), Rows { this_computer: true, sign_in: false, details: false });
        // While it is checked, the page waits.
        assert_eq!(rows(&saved, false, true, true, true), Rows { this_computer: true, sign_in: false, details: false });
        assert_eq!(rows(&saved, false, true, false, true), Rows { this_computer: true, sign_in: false, details: false });
        assert_eq!(rows(&TokenStatus::Missing, false, false, false, true), Rows { this_computer: false, sign_in: true, details: false });
        assert_eq!(rows(&TokenStatus::Unavailable("locked".into()), false, false, false, true), Rows { this_computer: false, sign_in: true, details: false });
        assert_eq!(rows(&TokenStatus::Checking, false, false, false, true), Rows { this_computer: false, sign_in: false, details: false });

        assert_eq!(token_row(&saved, false), TokenRow::Link);
        assert_eq!(token_row(&saved, true), TokenRow::Paste { replacing: true });
        assert_eq!(token_row(&TokenStatus::Missing, false), TokenRow::Link);
        assert_eq!(token_row(&TokenStatus::Missing, true), TokenRow::Paste { replacing: false });
        assert_eq!(token_row(&TokenStatus::Checking, true), TokenRow::Checking);
        assert_eq!(token_row(&TokenStatus::Unavailable("locked".into()), false), TokenRow::Unavailable("locked".into()));
    }

    #[test]
    fn open_site_only_opens_a_plain_web_address() {
        assert_eq!(site_link(" https://Tours.Example.com/ ").as_deref(), Some("https://tours.example.com"));
        assert_eq!(site_link("http://localhost:8787").as_deref(), Some("http://localhost:8787"));
        for bad in ["", "tours.example.com", "http://tours.example.com", "file:///C:/x", "https://a.example.com/?q=1", "https://user@a.example.com", "javascript:alert(1)"] {
            assert_eq!(site_link(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_answer_counts_only_for_the_site_that_was_asked() {
        let info = Info { workspace: "Acme".into(), version: Some("0.1.1".into()), tours: Some(Counts { public: 1, draft: 1 }) };
        let mut c = Check::default();
        let a = site("https://a.example.com");
        let first = c.begin();
        assert!(c.running);
        c.finish(first, a.clone(), Ok(info.clone()));
        assert!(!c.running);
        assert_eq!(c.info_for(Some(&a)), Some(&info));
        // Another address, another token, or no site at all: not connected.
        assert_eq!(c.info_for(Some(&site("https://b.example.com"))), None);
        assert_eq!(c.info_for(Some(&Site { epoch: 2, ..a.clone() })), None);
        assert_eq!(c.info_for(None), None);
        assert!(c.was_asked(&a) && !c.was_asked(&site("https://b.example.com")));
        // A slow answer that a newer request overtook is dropped.
        let slow = c.begin();
        let newer = c.begin();
        c.finish(slow, a.clone(), Err("Could not check the server: the server did not answer in time.".into()));
        assert!(c.running);
        assert_eq!(c.info_for(Some(&a)), Some(&info));
        c.finish(newer, a.clone(), Err("The server rejected the API token.".into()));
        assert!(!c.running);
        assert_eq!(c.info_for(Some(&a)), None, "a failure is not a connection");
        assert!(c.was_asked(&a));
    }

    // ---- With a site behind it ----

    /// Every piece of text one frame of the page draws, and whether the address changed.
    fn drawn(ctx: &egui::Context, p: &mut WorkspaceState, server: &mut String, conn: &Conn) -> String {
        fn collect(shape: &egui::Shape, into: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => into.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                _ => {}
            }
        }
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(590.0, 520.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            super::ui(ui, p, server, conn);
        });
        out.textures_delta.clear();
        let mut texts = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        texts.join("\n")
    }

    fn until(ctx: &egui::Context, p: &mut WorkspaceState, server: &mut String, conn: &Conn, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let text = drawn(ctx, p, server, conn);
            if done(&text) {
                return text;
            }
            assert!(Instant::now() < deadline, "the page did not settle:\n{text}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn has_line(text: &str, line: &str) -> bool {
        text.lines().any(|l| l == line)
    }

    /// The page says it is connected, to whichever workspace.
    fn connected(text: &str) -> bool {
        text.lines().any(is_connected_status)
    }

    fn page() -> WorkspaceState {
        WorkspaceState::with_token(|_| Ok(TOKEN.to_string()))
    }

    const LIST: &str = r#"{"tours":[{"id":"tour_a","title":"A","status":"published","slug":"abc","url":"/t/abc","chapters":2,"steps":22,"updated_at":1,"published_at":2},{"id":"tour_b","title":"B","status":"draft","slug":"def","url":null,"chapters":1,"steps":9,"updated_at":1,"published_at":null}]}"#;

    fn site_stub(me: &'static str) -> Stub {
        stub(move |req, _| match req.line.as_str() {
            "GET /api/v1/me" => reply(200, me),
            "GET /api/v1/tours" => reply(200, LIST),
            _ => reply(404, r#"{"error":"not_found"}"#),
        })
    }

    #[test]
    fn a_check_is_pinned_to_the_profile_active_when_the_page_asked() {
        let s = site_stub(r#"{"workspace":"Acme"}"#);
        let asked: Arc<Mutex<Vec<crate::profiles::Pin>>> = Arc::default();
        let record = asked.clone();
        let mut p = WorkspaceState {
            token: Arc::new(move |pin: &crate::profiles::Pin| {
                record.lock().unwrap().push(pin.clone());
                Ok(TOKEN.to_string())
            }),
            ..page()
        };
        let c = Conn { profile: Some("pa".into()), ..bound_to(&s, TokenStatus::Saved) };
        assert_eq!(c.site(&s.base).map(|site| site.pin()), Some(crate::profiles::Pin { profile: Some("pa".into()), server: s.base.clone() }));
        let (ctx, mut server) = (crate::theme::test_context(), s.base.clone());
        until(&ctx, &mut p, &mut server, &c, connected);
        assert_eq!(*asked.lock().unwrap(), vec![crate::profiles::Pin { profile: Some("pa".into()), server: s.base.clone() }]);
    }

    #[test]
    fn a_connected_page_shows_the_workspace_the_tours_and_the_version() {
        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let (ctx, mut p, mut server, c) = (crate::theme::test_context(), page(), String::new(), bound_to(&s, TokenStatus::Saved));
        server.clone_from(&s.base);
        // Opening the page asks the site without a click.
        let first = drawn(&ctx, &mut p, &mut server, &c);
        // The status says so, unless the stub already answered.
        assert!(has_line(&first, "Workspace") && (has_line(&first, CHECKING) || has_line(&first, "Connected to Acme")), "{first}");
        let text = until(&ctx, &mut p, &mut server, &c, connected);
        let computer = connect::device_name();
        for word in ["Workspace", "Site", "Status", "Connected to Acme", THIS_COMPUTER, computer.as_str(), SIGN_OUT, "Tours", "2 (1 public, 1 draft)", "Site version", "0.1.1", "Test connection", "Open site", TOKEN_NOTE] {
            assert!(has_line(&text, word), "the page is missing {word:?} in:\n{text}");
        }
        // Connected: no Sign in, no token field, nothing of the token.
        for word in [SIGN_IN, PASTE_INSTEAD, "Token", "Save", SIGN_IN_HINT, NOT_CONNECTED] {
            assert!(!has_line(&text, word), "the page shows {word:?} in:\n{text}");
        }
        assert!(!text.contains("dp_"), "{text}");
        // It asked once, not once per frame.
        for _ in 0..5 {
            drawn(&ctx, &mut p, &mut server, &c);
        }
        assert_eq!(s.lines(), ["GET /api/v1/me", "GET /api/v1/tours"]);
        // Leaving and coming back asks again.
        p.leave(&ctx);
        drawn(&ctx, &mut p, &mut server, &c);
        until(&ctx, &mut p, &mut server, &c, |_| s.lines().len() == 4);
        // While connected the paste field stays hidden, even when it was opened earlier: Sign out comes first.
        p.pasting = true;
        let text = until(&ctx, &mut p, &mut server, &c, connected);
        assert!(!has_line(&text, "Save") && has_line(&text, SIGN_OUT), "{text}");
    }

    #[test]
    fn an_older_site_is_connected_with_its_version_unknown() {
        let s = site_stub(r#"{"workspace":"Acme"}"#);
        let (ctx, mut p, mut server, c) = (crate::theme::test_context(), page(), s.base.clone(), bound_to(&s, TokenStatus::Saved));
        let text = until(&ctx, &mut p, &mut server, &c, connected);
        assert!(has_line(&text, "Connected to Acme") && has_line(&text, "unknown") && has_line(&text, "2 (1 public, 1 draft)"), "{text}");
    }

    #[test]
    fn a_page_without_a_token_or_with_a_failing_site_is_not_connected_and_asks_nothing_more() {
        let ctx = crate::theme::test_context();
        // No token: Sign in, the way to paste a token instead, the hint, and no request to anyone.
        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let (mut p, mut server) = (page(), s.base.clone());
        let text = drawn(&ctx, &mut p, &mut server, &conn(TokenStatus::Missing));
        for word in ["Workspace", "Site", "Status", NOT_CONNECTED, SIGN_IN, "Token", PASTE_INSTEAD, "Test connection", "Open site", SIGN_IN_HINT] {
            assert!(has_line(&text, word), "the page is missing {word:?} in:\n{text}");
        }
        assert_eq!(text.lines().filter(|l| *l == SIGN_IN).count(), 2, "the row and its button: {text}");
        for word in ["Save", SIGN_OUT, THIS_COMPUTER, "Forget", "Tours", "Site version", "Acme", "unknown", TOKEN_NOTE] {
            assert!(!has_line(&text, word), "the page shows {word:?} in:\n{text}");
        }
        // "Paste a token instead": today's paste field, unchanged, and the note on where it is kept.
        p.pasting = true;
        let text = drawn(&ctx, &mut p, &mut server, &conn(TokenStatus::Missing));
        assert!(has_line(&text, "Save") && !has_line(&text, PASTE_INSTEAD) && !has_line(&text, "Cancel") && has_line(&text, TOKEN_NOTE), "{text}");
        p.pasting = false;
        // The token is being looked up.
        let text = drawn(&ctx, &mut p, &mut server, &conn(TokenStatus::Checking));
        assert!(has_line(&text, CHECKING) && has_line(&text, "Checking for a saved token…"), "{text}");
        // A credential store that cannot be read: its sentence, and Forget to clear it.
        let text = drawn(&ctx, &mut p, &mut server, &conn(TokenStatus::Unavailable("Could not use the credential store: the stored token could not be read.".into())));
        assert!(has_line(&text, NOT_CONNECTED) && text.replace('\n', " ").contains("the stored token could not be read") && has_line(&text, "Forget") && has_line(&text, "Save"), "{text}");
        std::thread::sleep(Duration::from_millis(50));
        assert!(s.lines().is_empty(), "nothing is sent without a saved token");

        // A site that refuses the token: not connected, no rows filled, and no request loop.
        let s = stub(|_, _| reply(401, r#"{"error":"unauthorized"}"#));
        let (mut p, mut server, c) = (page(), s.base.clone(), bound_to(&s, TokenStatus::Saved));
        drawn(&ctx, &mut p, &mut server, &c);
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, NOT_CONNECTED));
        assert!(!has_line(&text, "unknown") && !has_line(&text, PRESS_TEST), "{text}");
        // Sign in again, paste another, or Sign out.
        assert!(has_line(&text, SIGN_IN) && has_line(&text, PASTE_INSTEAD) && has_line(&text, SIGN_OUT), "{text}");
        for _ in 0..5 {
            drawn(&ctx, &mut p, &mut server, &c);
        }
        assert_eq!(s.lines(), ["GET /api/v1/me"]);
        // A tour list that cannot be read leaves the count open; the connection stands.
        let s = stub(|req, _| if req.line == "GET /api/v1/me" { reply(200, r#"{"workspace":"Acme","version":"0.1.1"}"#) } else { reply(503, "") });
        let (mut p, mut server, c) = (page(), s.base.clone(), bound_to(&s, TokenStatus::Saved));
        let text = until(&ctx, &mut p, &mut server, &c, connected);
        assert!(has_line(&text, "Connected to Acme") && !text.contains("public"), "{text}");

        // A token the site refused elsewhere (a publish or a share got 401): not connected, with
        // the existing sentence, even though an earlier answer named the workspace.
        let refused = Conn { refused: true, ..c.clone() };
        let text = drawn(&ctx, &mut p, &mut server, &refused);
        assert!(has_line(&text, NOT_CONNECTED) && has_line(&text, publish::REJECTED_TOKEN) && has_line(&text, SIGN_IN) && !connected(&text), "{text}");
        assert!(!has_line(&text, "Tours") && !has_line(&text, "Site version"), "{text}");
    }

    #[test]
    fn an_address_being_typed_is_never_asked_on_its_own() {
        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let (ctx, mut p, c) = (crate::theme::test_context(), page(), bound_to(&s, TokenStatus::Saved));
        // The address in the field is the token's own, typed just now: what the field's change handler does.
        let mut server = s.base.clone();
        if let Some(typed) = c.site(&server) {
            p.auto = Some(typed);
        }
        let text = drawn(&ctx, &mut p, &mut server, &c);
        assert!(has_line(&text, NOT_CONNECTED) && has_line(&text, PRESS_TEST), "{text}");
        std::thread::sleep(Duration::from_millis(50));
        drawn(&ctx, &mut p, &mut server, &c);
        assert!(s.lines().is_empty(), "the typed address got a request: {:?}", s.lines());
        // Test connection asks it, and says so.
        p.spawn_check(c.site(&server).unwrap(), Some(server.clone()));
        let text = until(&ctx, &mut p, &mut server, &c, connected);
        assert!(text.replace('\n', " ").contains("Connected to workspace \"Acme\"") && has_line(&text, "Connected to Acme"), "{text}");
        assert_eq!(s.lines(), ["GET /api/v1/me", "GET /api/v1/tours"]);
    }

    #[test]
    fn a_token_saved_for_another_address_is_not_sent_until_the_user_says_so() {
        let home = site_stub(r#"{"workspace":"Home"}"#);
        let typo = site_stub(r#"{"workspace":"Somebody else"}"#);
        let ctx = crate::theme::test_context();
        // Which address the token is tied to, as the page's button would set it.
        let bound = Arc::new(Mutex::new(Some(home.base.clone())));
        let record = bound.clone();
        let mut p = WorkspaceState { bind: Arc::new(move |site| *record.lock().unwrap() = site), ..page() };
        let c = |bound: &Mutex<Option<String>>| Conn { bound: bound.lock().unwrap().clone(), ..conn(TokenStatus::Saved) };
        let mut server = home.base.clone();
        until(&ctx, &mut p, &mut server, &c(&bound), connected);
        // The address changes to another host. Opening the page again must not hand it the token.
        server.clone_from(&typo.base);
        p.leave(&ctx);
        let text = drawn(&ctx, &mut p, &mut server, &c(&bound));
        let flat = text.replace('\n', " ");
        assert!(has_line(&text, NOT_CONNECTED) && flat.contains("The saved token belongs to ") && flat.contains("so it is not sent to this address."), "{text}");
        assert!(has_line(&text, publish_ui::USE_IT_HERE) && has_line(&text, PASTE_INSTEAD) && has_line(&text, SIGN_IN) && !connected(&text) && !has_line(&text, PRESS_TEST), "{text}");
        // The button says what it does: the token moves, it is not shared between the two addresses.
        assert_eq!(publish_ui::USE_IT_HERE, "Use it for this address instead");
        assert!(flat.contains("The token will then be used only for this address, and no longer for the one it was saved for.") && !flat.contains("address too"), "{text}");
        for _ in 0..5 {
            drawn(&ctx, &mut p, &mut server, &c(&bound));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(typo.lines().is_empty(), "the other address got a request: {:?}", typo.lines());
        assert_eq!(c(&bound).site(&server), None);
        assert_eq!(c(&bound).binding_problem(&home.base), None);
        // A token tied to nothing is not sent either.
        let unbound = conn(TokenStatus::Saved);
        let text = drawn(&ctx, &mut p, &mut server, &unbound).replace('\n', " ");
        assert!(text.contains("The saved token is not tied to a site address yet") && text.contains(publish_ui::USE_IT_HERE), "{text}");
        assert!(typo.lines().is_empty());
        // "Use it for this address instead": the tie moves, and only then is the site asked.
        (p.bind)(publish::check_server(&server).ok());
        assert_eq!(bound.lock().unwrap().as_deref(), Some(typo.base.as_str()));
        let text = until(&ctx, &mut p, &mut server, &c(&bound), connected);
        assert!(has_line(&text, "Connected to Somebody else") && !has_line(&text, publish_ui::USE_IT_HERE), "{text}");
        assert_eq!(home.lines().len(), 2, "the first site was asked once, when the page opened");
    }

    #[test]
    fn saving_ties_the_token_to_the_address_and_a_failed_save_ties_nothing() {
        let ties: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let record = ties.clone();
        let bind: Bind = Arc::new(move |site| record.lock().unwrap().push(site));
        assert_eq!(after_save(Ok(()), Some("https://tours.example.com".into()), &bind), Ok("Token saved.".into()));
        assert_eq!(*ties.lock().unwrap(), [Some("https://tours.example.com".to_string())]);
        // Saved with no usable address in the field: tied to none, so it goes nowhere yet.
        assert!(after_save(Ok(()), None, &bind).is_ok());
        assert_eq!(ties.lock().unwrap().last(), Some(&None));
        // The store refused it: the token that was saved before keeps its own address.
        let refused = Err("That is not an API token. A token starts with dp_ and comes from the server's setup page.".to_string());
        assert_eq!(after_save(refused.clone(), Some("https://other.example.com".into()), &bind), refused.map(|()| String::new()));
        assert_eq!(ties.lock().unwrap().len(), 2);
        // Forget unties; a Forget that failed leaves the tie.
        assert!(after_forget(Err("Could not use the credential store: the credential store is locked or not reachable.".into()), &bind).is_err());
        assert_eq!(ties.lock().unwrap().len(), 2);
        assert_eq!(after_forget(Ok(()), &bind), Ok("Token removed from Windows Credential Manager.".into()));
        assert_eq!(ties.lock().unwrap().last(), Some(&None));
    }

    #[test]
    fn typing_in_the_address_field_reports_a_change_and_sends_nothing() {
        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let (ctx, mut p, c) = (crate::theme::test_context(), page(), bound_to(&s, TokenStatus::Saved));
        let mut server = String::new();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(590.0, 520.0));
        let run = |p: &mut WorkspaceState, server: &mut String, events: Vec<egui::Event>| {
            let mut changed = false;
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| changed = super::ui(ui, p, server, &c));
            out.textures_delta.clear();
            changed
        };
        // Click into the address field (the first field on the page), then paste the address.
        let at = egui::pos2(260.0, 52.0);
        let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        for events in [vec![], vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![]] {
            assert!(!run(&mut p, &mut server, events));
        }
        assert!(run(&mut p, &mut server, vec![egui::Event::Text(s.base.clone())]), "the page reports the new address");
        assert_eq!(server, s.base);
        for _ in 0..5 {
            assert!(!run(&mut p, &mut server, vec![]));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(s.lines().is_empty(), "typing sent a request: {:?}", s.lines());
    }

    // ---- Sign in ----

    /// A token in the platform's form that only these tests hand out.
    const NEW_TOKEN: &str = "dp_c0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ff";
    const CODE: &str = "K7QM-2HXD";

    /// A clock that moves only when slept on, so no test waits for an interval.
    #[derive(Default)]
    struct FakeClock(Mutex<Duration>);

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, d: Duration) {
            *self.0.lock().unwrap() += d;
            // A little real time too, so a code check is never asked in a tight loop.
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// What the fake credential store and the tie were asked to do.
    #[derive(Default)]
    struct Record {
        stored: Mutex<Vec<String>>,
        ties: Mutex<Vec<Option<String>>>,
        refreshed: Mutex<usize>,
        opened: Mutex<Vec<String>>,
    }

    /// A page whose store, tie, browser and clock are fakes that write into the record.
    fn signing_page(record: &Arc<Record>) -> WorkspaceState {
        let (a, b, c, d) = (record.clone(), record.clone(), record.clone(), record.clone());
        let vault = Vault {
            store: Arc::new(move |t| {
                a.stored.lock().unwrap().push(t.to_string());
                Ok(())
            }),
            forget: Arc::new(|| Ok(())),
            refresh: Arc::new(move || *b.refreshed.lock().unwrap() += 1),
        };
        WorkspaceState {
            vault,
            bind: Arc::new(move |site| c.ties.lock().unwrap().push(site)),
            open: Arc::new(move |url: &str| {
                d.opened.lock().unwrap().push(url.to_string());
                Ok(())
            }),
            clock: Arc::new(FakeClock::default()),
            ..page()
        }
    }

    /// The stand-in site for Sign in: a code whose page is `/connections`, then `asks` for each code check.
    fn connect_stub(asks: impl Fn(usize) -> crate::publish::tests::Answer + Send + 'static) -> Stub {
        stub(move |req, seen| match req.line.as_str() {
            "POST /api/v1/connect/start" => reply(201, &format!(r#"{{"device_code":"dpd_0123456789abcdef0123456789abcdef01234567","user_code":"{CODE}","verify_url":"/connections","expires_in":600,"interval":5}}"#)),
            "POST /api/v1/connect/token" => asks(seen),
            _ => reply(404, r#"{"error":"not_found"}"#),
        })
    }

    fn connected_answer() -> crate::publish::tests::Answer {
        reply(200, &format!(r#"{{"status":"connected","token":"{NEW_TOKEN}","workspace":{{"name":"Cowfee"}}}}"#))
    }

    #[test]
    fn sign_in_shows_the_code_opens_the_page_and_saves_the_token_for_that_address() {
        let s = connect_stub(|seen| if seen == 0 { reply(200, r#"{"status":"pending"}"#) } else { connected_answer() });
        let record = Arc::new(Record::default());
        let (ctx, mut p, c) = (crate::theme::test_context(), signing_page(&record), conn(TokenStatus::Missing));
        let mut server = s.base.clone();
        drawn(&ctx, &mut p, &mut server, &c);
        p.begin_sign_in(&ctx, &server);
        assert!(p.is_signing_in());
        // The window, until the site has answered; then it closes by itself.
        let mut seen_code = String::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while p.is_signing_in() {
            let text = drawn(&ctx, &mut p, &mut server, &c);
            if text.contains(CODE) {
                seen_code = text.clone();
            }
            assert!(Instant::now() < deadline, "the sign-in did not end:\n{text}");
            std::thread::sleep(Duration::from_millis(5));
        }
        // While it waited, the window said what the mockup says, with the page's address.
        let address = format!("{}/connections", s.base.trim_start_matches("http://"));
        for line in [sign_in_ui::HEADING, sign_in_ui::OPENED, "Waiting for you to confirm… The code works for 10 minutes.", "Browser did not open?", sign_in_ui::OPEN_THE_PAGE, &format!("or go to {address}."), CODE, "Cancel"] {
            assert!(has_line(&seen_code, line), "the window is missing {line:?} in:\n{seen_code}");
        }
        // The token went to the store through the path Save uses, tied to the address the sign-in was for.
        assert_eq!(*record.stored.lock().unwrap(), [NEW_TOKEN]);
        assert_eq!(*record.ties.lock().unwrap(), [Some(s.base.clone())]);
        assert_eq!(*record.refreshed.lock().unwrap(), 1);
        assert_eq!(*record.opened.lock().unwrap(), [format!("{}/connections", s.base)]);
        // No request carried a credential, and the page never showed the token or the device code.
        assert!(s.requests().iter().all(|r| r.auth.is_none()), "{:?}", s.requests().iter().map(|r| &r.line).collect::<Vec<_>>());
        assert!(!seen_code.contains("dp_") && !seen_code.contains("dpd_"), "{seen_code}");
        let text = drawn(&ctx, &mut p, &mut server, &c);
        assert!(!text.contains("dp_") && !text.contains("dpd_") && !text.contains(CODE), "{text}");
    }

    #[test]
    fn a_token_from_sign_in_belongs_to_the_address_it_was_started_for() {
        // The site waits until the test lets it answer.
        let go = Arc::new(Mutex::new(false));
        let g = go.clone();
        let s = connect_stub(move |_| if *g.lock().unwrap() { connected_answer() } else { reply(200, r#"{"status":"pending"}"#) });
        let record = Arc::new(Record::default());
        let (ctx, mut p, c) = (crate::theme::test_context(), signing_page(&record), conn(TokenStatus::Missing));
        let mut server = s.base.clone();
        p.begin_sign_in(&ctx, &server);
        until(&ctx, &mut p, &mut server, &c, |t| t.contains(CODE));
        // The address field changes meanwhile.
        server = "https://somewhere-else.example.com".to_string();
        *go.lock().unwrap() = true;
        let deadline = Instant::now() + Duration::from_secs(20);
        while p.is_signing_in() {
            drawn(&ctx, &mut p, &mut server, &c);
            assert!(Instant::now() < deadline, "the sign-in did not end");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*record.ties.lock().unwrap(), [Some(s.base.clone())], "the token is tied to the address it was issued by");
        assert_eq!(*record.stored.lock().unwrap(), [NEW_TOKEN]);
    }

    #[test]
    fn leaving_the_page_or_cancel_stops_the_sign_in_and_stores_nothing() {
        let s = connect_stub(|_| reply(200, r#"{"status":"pending"}"#));
        let record = Arc::new(Record::default());
        let (ctx, mut p, c) = (crate::theme::test_context(), signing_page(&record), conn(TokenStatus::Missing));
        let mut server = s.base.clone();
        p.begin_sign_in(&ctx, &server);
        until(&ctx, &mut p, &mut server, &c, |t| t.contains(CODE));
        // Closing the window (or leaving the page) drops the window, which cancels its sign-in.
        p.leave(&ctx);
        assert!(!p.is_signing_in());
        std::thread::sleep(Duration::from_millis(200));
        let asked = s.lines().len();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.lines().len(), asked, "it went on asking after the cancel");
        assert!(record.stored.lock().unwrap().is_empty() && record.ties.lock().unwrap().is_empty());

        // Cancel in the window does the same.
        let s = connect_stub(|_| reply(200, r#"{"status":"pending"}"#));
        let mut server = s.base.clone();
        p.begin_sign_in(&ctx, &server);
        until(&ctx, &mut p, &mut server, &c, |t| t.contains(CODE));
        assert_eq!(p.sign_in.as_mut().unwrap().press(sign_in_ui::Button::Cancel), Some(SignInAction::Closed));
        p.sign_in = None;
        std::thread::sleep(Duration::from_millis(200));
        let asked = s.lines().len();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.lines().len(), asked);
        assert!(record.stored.lock().unwrap().is_empty());
    }

    #[test]
    fn an_older_site_says_it_has_no_sign_in_and_nothing_is_stored() {
        let s = stub(|_, _| reply(404, r#"{"error":"not_found","detail":"Cannot POST /api/v1/connect/start"}"#));
        let record = Arc::new(Record::default());
        let (ctx, mut p, c) = (crate::theme::test_context(), signing_page(&record), conn(TokenStatus::Missing));
        let mut server = s.base.clone();
        p.begin_sign_in(&ctx, &server);
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, connect::NOT_OFFERED));
        assert!(has_line(&text, connect::NOT_CONNECTED_HEADING) && has_line(&text, "Close") && !has_line(&text, "Try again") && !text.contains("Cannot POST"), "{text}");
        assert!(record.stored.lock().unwrap().is_empty() && record.opened.lock().unwrap().is_empty());
        assert_eq!(s.lines(), ["POST /api/v1/connect/start"]);
        assert!(s.requests().iter().all(|r| r.auth.is_none()));
    }

    #[test]
    fn sign_in_needs_an_address_the_address_rule_accepts() {
        assert!(sign_in_enabled(false, false, "https://tours.example.com"));
        assert!(sign_in_enabled(false, false, "http://127.0.0.1:8787"));
        for half_typed in ["", "https://", "tours.example.com", "http://tours.example.com", "https://tours.example.com/?x", "https://user@tours.example.com"] {
            assert!(!sign_in_enabled(false, false, half_typed), "{half_typed:?}");
        }
        assert!(!sign_in_enabled(true, false, "https://tours.example.com"), "not while a save runs");
        assert!(!sign_in_enabled(false, true, "https://tours.example.com"), "not twice");
        // Pressed anyway with such an address: nothing opens, nothing is sent.
        let record = Arc::new(Record::default());
        let (ctx, mut p) = (crate::theme::test_context(), signing_page(&record));
        p.begin_sign_in(&ctx, "https://");
        assert!(!p.is_signing_in());
    }

    #[test]
    fn sign_in_is_offered_without_a_token_or_for_a_refused_one_but_not_for_an_unreachable_site() {
        let ctx = crate::theme::test_context();
        // 1. No token.
        let s = site_stub(r#"{"workspace":"Acme"}"#);
        let (mut p, mut server) = (page(), s.base.clone());
        let text = drawn(&ctx, &mut p, &mut server, &conn(TokenStatus::Missing));
        assert!(has_line(&text, SIGN_IN) && has_line(&text, PASTE_INSTEAD) && has_line(&text, SIGN_IN_HINT), "{text}");
        // 2. A saved token the site refuses.
        let s = stub(|_, _| reply(401, r#"{"error":"unauthorized"}"#));
        let (mut p, mut server, c) = (page(), s.base.clone(), bound_to(&s, TokenStatus::Saved));
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, SIGN_IN));
        assert!(has_line(&text, NOT_CONNECTED) && has_line(&text, publish::REJECTED_TOKEN) && has_line(&text, SIGN_OUT), "{text}");
        // 3. A saved token whose site answers errors, or cannot be reached: as before, Not connected
        // with Sign out and Test connection, and no Sign in that would make a second connection.
        for s in [stub(|_, _| reply(503, r#"{"error":"down"}"#)), stub(|_, _| crate::publish::tests::Answer::Drop)] {
            let (mut p, mut server, c) = (page(), s.base.clone(), bound_to(&s, TokenStatus::Saved));
            let check = p.check.clone();
            drawn(&ctx, &mut p, &mut server, &c);
            until(&ctx, &mut p, &mut server, &c, |_| !lock(&check).running && lock(&check).done.is_some());
            let text = drawn(&ctx, &mut p, &mut server, &c);
            assert!(has_line(&text, NOT_CONNECTED) && has_line(&text, SIGN_OUT) && has_line(&text, "Test connection"), "{text}");
            for word in [SIGN_IN, PASTE_INSTEAD, SIGN_IN_HINT, publish::REJECTED_TOKEN] {
                assert!(!has_line(&text, word), "an unreachable site shows {word:?}:\n{text}");
            }
        }
    }

    #[test]
    fn signed_in_as_names_the_account_until_the_token_changes_and_not_you_signs_out() {
        assert_eq!(signed_in_as("alex").as_deref(), Some("Signed in as alex."));
        assert_eq!(signed_in_as(" mallory\u{202e}\u{200b}\n").as_deref(), Some("Signed in as mallory."));
        assert_eq!(signed_in_as(&"a".repeat(40)).as_deref(), Some(format!("Signed in as {}….", "a".repeat(29)).as_str()));
        assert_eq!(signed_in_as(&"a".repeat(30)).as_deref(), Some(format!("Signed in as {}.", "a".repeat(30)).as_str()));
        assert_eq!(signed_in_as(" \u{200b}"), None);
        assert_eq!(NOT_YOU, "Not you? Sign out");

        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let forgot = Arc::new(Mutex::new(0usize));
        let f = forgot.clone();
        let vault = Vault { store: Arc::new(|_| Ok(())), forget: Arc::new(move || { *f.lock().unwrap() += 1; Ok(()) }), refresh: Arc::new(|| {}) };
        let (ctx, mut p, c) = (crate::theme::test_context(), WorkspaceState { vault, bind: Arc::new(|_| {}), ..page() }, bound_to(&s, TokenStatus::Saved));
        let mut server = s.base.clone();
        // What the window hands the page after a sign-in the site named an account for.
        p.account = Some(("alex".into(), None));
        let text = until(&ctx, &mut p, &mut server, &c, connected);
        assert!(has_line(&text, "Signed in as alex.") && has_line(&text, NOT_YOU), "{text}");
        // Still there on later frames with the same token.
        let text = drawn(&ctx, &mut p, &mut server, &c);
        assert!(has_line(&text, "Signed in as alex."), "{text}");
        // Another token (saved, replaced or forgotten): gone.
        let other = Conn { epoch: 2, ..c.clone() };
        let text = drawn(&ctx, &mut p, &mut server, &other);
        assert!(!text.contains("Signed in as") && !has_line(&text, NOT_YOU), "{text}");
        // Leaving the page: gone too.
        p.account = Some(("alex".into(), None));
        drawn(&ctx, &mut p, &mut server, &c);
        p.leave(&ctx);
        let text = drawn(&ctx, &mut p, &mut server, &c);
        assert!(!text.contains("Signed in as"), "{text}");
        // No account named: nothing extra.
        assert!(!drawn(&ctx, &mut p, &mut server, &c).contains("Not you"));
        // "Not you? Sign out" is Sign out.
        p.account = Some(("mallory".into(), None));
        drawn(&ctx, &mut p, &mut server, &c);
        sign_out(&ctx, &mut p);
        assert!(p.account.is_none());
        let deadline = Instant::now() + Duration::from_secs(10);
        while *forgot.lock().unwrap() == 0 {
            assert!(Instant::now() < deadline, "the token was not forgotten");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_sign_in_whose_answer_named_an_account_shows_it_on_the_page() {
        let s = connect_stub(|_| reply(200, &format!(r#"{{"status":"connected","token":"{NEW_TOKEN}","workspace":{{"name":"Cowfee"}},"account":{{"username":"alex"}}}}"#)));
        let record = Arc::new(Record::default());
        let (ctx, mut p, c) = (crate::theme::test_context(), signing_page(&record), conn(TokenStatus::Missing));
        let mut server = s.base.clone();
        p.begin_sign_in(&ctx, &server);
        let deadline = Instant::now() + Duration::from_secs(20);
        while p.is_signing_in() {
            drawn(&ctx, &mut p, &mut server, &c);
            assert!(Instant::now() < deadline, "the sign-in did not end");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(p.account.as_ref().map(|(n, _)| n.as_str()), Some("alex"));
        // The page now holds the saved token (the fake store does not change the status, so the test does).
        let saved = Conn { bound: Some(s.base.clone()), ..conn(TokenStatus::Saved) };
        let text = drawn(&ctx, &mut p, &mut server, &saved);
        assert!(has_line(&text, "Signed in as alex.") && has_line(&text, NOT_YOU), "{text}");
    }
}
