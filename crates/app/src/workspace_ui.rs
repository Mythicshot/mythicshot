//! The Workspace page of the main window: the tour site Mythicshot publishes to. The site
//! address, the API token (saved, replaced or forgotten), and what the site says about
//! itself: the workspace's name, how many tours it holds and its version.
//!
//! Everything that talks to the site or to the credential store runs on a background
//! thread. The token field is only ever a masked input. It is emptied, wiped and given a
//! new id (dropping its undo history) on Save, on Forget, on Cancel and when the page is
//! left or the window closes, and a saved token is never read back into it.
//!
//! The site is asked on its own only when the page is opened and after a token is saved,
//! and only at the address as it stood then. While the address is being typed nothing is
//! sent: every half-typed address would otherwise be handed the token. An edited address
//! is checked when "Test connection" is pressed.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};

use crate::publish::{self, RemoteTour};
use crate::publish_ui::{self, TokenStatus};
use crate::tours_ui::Site;

const WARN_TEXT: Color32 = Color32::from_rgb(255, 196, 107);
const MUTED: Color32 = Color32::from_rgb(163, 170, 184);
const OK_TEXT: Color32 = Color32::from_rgb(125, 211, 140);
const OK_BG: Color32 = Color32::from_rgb(24, 52, 34);
const IDLE_BG: Color32 = Color32::from_rgb(44, 50, 62);

pub const CONNECTED: &str = "Connected";
pub const NOT_CONNECTED: &str = "Not connected";
pub const CHECKING: &str = "Checking…";
pub const TOKEN_SAVED: &str = "Saved on this computer";
pub const TOKEN_NOTE: &str = "The token is kept in Windows Credential Manager, not in the settings file.";
pub const PRESS_TEST: &str = "Press Test connection to check this address.";
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
}

impl Conn {
    pub fn current() -> Conn {
        Conn { token: publish_ui::token_status(), epoch: publish_ui::token_epoch(), test_server: publish_ui::test_server() }
    }

    /// The site at `address`: only with a usable address and a saved token.
    fn site(&self, address: &str) -> Option<Site> {
        if self.token != TokenStatus::Saved {
            return None;
        }
        Some(Site { server: publish::check_server(address).ok()?, epoch: self.epoch })
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

/// The badge beside the heading.
pub fn badge(token: &TokenStatus, checking: bool, connected: bool) -> &'static str {
    if *token == TokenStatus::Checking || checking {
        CHECKING
    } else if connected {
        CONNECTED
    } else {
        NOT_CONNECTED
    }
}

/// What the "Token" row holds.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenRow {
    Checking,
    /// "Saved on this computer", with Replace and Forget.
    Saved,
    /// The paste field with Save, and Cancel when it replaces a saved token.
    Paste { replacing: bool },
    /// The credential store's message, the paste field, and Forget so that what cannot be read can be removed.
    Unavailable(String),
}

pub fn token_row(token: &TokenStatus, replacing: bool) -> TokenRow {
    match token {
        TokenStatus::Checking => TokenRow::Checking,
        TokenStatus::Saved if replacing => TokenRow::Paste { replacing: true },
        TokenStatus::Saved => TokenRow::Saved,
        TokenStatus::Missing => TokenRow::Paste { replacing: false },
        TokenStatus::Unavailable(e) => TokenRow::Unavailable(e.clone()),
    }
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
}

type TokenSource = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

/// The page's own state.
pub struct WorkspaceState {
    token_input: String,
    /// Part of the token field's id; a new value gives the field a fresh, empty state.
    field: u64,
    /// The token field's id as last drawn, so its stored state can be removed.
    field_id: Option<egui::Id>,
    /// "Replace" was pressed: the paste field shows although a token is saved.
    replacing: bool,
    /// The last Save, Forget or Test connection result.
    message: Arc<Mutex<Option<Outcome>>>,
    /// A Save, Forget or Test connection is running on its background thread.
    busy: Arc<Mutex<bool>>,
    check: Arc<Mutex<Check>>,
    /// The site that was last asked without a click, or whose address was typed: it is not asked again on its own.
    auto: Option<Site>,
    token: TokenSource,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            token_input: String::new(),
            field: 0,
            field_id: None,
            replacing: false,
            message: Arc::default(),
            busy: Arc::default(),
            check: Arc::default(),
            auto: None,
            token: Arc::new(publish_ui::token_for_job),
        }
    }
}

/// A finished Save, Forget or Test connection.
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
fn ask(server: &str, token: &str) -> Result<Info, String> {
    let ws = publish::workspace(server, token)?;
    let tours = publish::list_tours(server, token).ok().map(|t| Counts::of(&t));
    Ok(Info { workspace: ws.name, version: ws.version, tours })
}

impl WorkspaceState {
    /// A page whose checks get the token from `token` instead of the credential store.
    #[cfg(test)]
    fn with_token(token: impl Fn() -> Result<String, String> + Send + Sync + 'static) -> Self {
        Self { token: Arc::new(token), ..Default::default() }
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

    /// The page was left, or the window closed: a token typed but not saved is wiped, and
    /// the next visit asks the site again.
    pub fn leave(&mut self, ctx: &egui::Context) {
        self.clear_token_field(ctx);
        self.replacing = false;
        self.auto = None;
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
            let result = token().and_then(|token| ask(&site.server, &token));
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
    if busy || checking || conn.token == TokenStatus::Checking {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    ui.horizontal(|ui| {
        ui.heading("Workspace");
        let label = badge(&conn.token, checking, info.is_some());
        let (fg, bg) = if label == CONNECTED { (OK_TEXT, OK_BG) } else { (MUTED, IDLE_BG) };
        egui::Frame::new().fill(bg).corner_radius(egui::CornerRadius::same(9)).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
            ui.label(RichText::new(label).color(fg).size(11.0));
        });
    });
    ui.add_space(12.0);

    let row = |ui: &mut egui::Ui, name: &str| {
        ui.label(RichText::new(name).color(MUTED));
    };
    let value = |ui: &mut egui::Ui, text: Option<String>| {
        ui.label(text.unwrap_or_else(|| NOTHING.to_string()));
    };
    egui::Grid::new("workspace-rows").num_columns(2).spacing([18.0, 12.0]).show(ui, |ui| {
        row(ui, "Site address");
        ui.vertical(|ui| {
            match &conn.test_server {
                Some(t) => {
                    let mut shown = t.clone();
                    ui.add_enabled(false, egui::TextEdit::singleline(&mut shown).desired_width(340.0));
                    ui.label(RichText::new("Debug build: set by MYTHICSHOT_TEST_PUBLISH for this run, not saved.").color(MUTED).size(12.0));
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
                ui.label(RichText::new(e).color(WARN_TEXT).size(12.0));
            }
        });
        ui.end_row();

        row(ui, "Workspace");
        value(ui, info.as_ref().map(|i| i.workspace.clone()));
        ui.end_row();

        row(ui, "Token");
        ui.vertical(|ui| token_ui(ui, p, conn, busy));
        ui.end_row();

        row(ui, "Tours");
        value(ui, info.as_ref().and_then(|i| i.tours).map(tours_summary));
        ui.end_row();

        row(ui, "Site version");
        value(ui, info.as_ref().map(|i| version_label(i.version.as_deref())));
        ui.end_row();
    });

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy && !checking, egui::Button::new("Test connection")).clicked() {
            let tested = effective.clone();
            match publish::check_server(&tested) {
                Ok(checked) => {
                    let site = Site { server: checked, epoch: conn.epoch };
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
            ui.add(egui::Label::new(RichText::new(text).color(if ok { OK_TEXT } else { WARN_TEXT })).wrap());
        } else if site.is_some() && !asked {
            ui.label(RichText::new(PRESS_TEST).color(MUTED).size(12.0));
        }
    });
    ui.add_space(6.0);
    ui.label(RichText::new(TOKEN_NOTE).color(MUTED).size(12.0));
    changed
}

/// The "Token" row: what is saved, or the field to paste one into.
fn token_ui(ui: &mut egui::Ui, p: &mut WorkspaceState, conn: &Conn, busy: bool) {
    let forget = |ui: &mut egui::Ui, p: &mut WorkspaceState| {
        if ui.add_enabled(!busy, egui::Button::new("Forget")).clicked() {
            p.clear_token_field(ui.ctx());
            p.replacing = false;
            p.run(None, || {
                let forgotten = publish::forget_token();
                publish_ui::refresh_token_now();
                forgotten.map(|()| "Token removed from Windows Credential Manager.".to_string())
            });
        }
    };
    let paste = |ui: &mut egui::Ui, p: &mut WorkspaceState, hint: &str| {
        token_field(ui, p, hint, 240.0);
        if ui.add_enabled(!busy && !p.token_input.trim().is_empty(), egui::Button::new("Save")).clicked() {
            // The field is emptied now; the token goes to the background thread and nowhere else.
            let mut entered = p.take_for_save(ui.ctx());
            p.replacing = false;
            p.run(None, move || {
                let saved = publish::store_token(&entered);
                publish_ui::wipe(&mut entered);
                publish_ui::refresh_token_now();
                saved.map(|()| "Token saved.".to_string())
            });
        }
    };
    match token_row(&conn.token, p.replacing) {
        TokenRow::Checking => {
            ui.label(RichText::new("Checking for a saved token…").color(MUTED));
        }
        TokenRow::Saved => {
            ui.horizontal(|ui| {
                ui.label(TOKEN_SAVED);
                if ui.add_enabled(!busy, egui::Button::new("Replace")).clicked() {
                    p.replacing = true;
                }
                forget(ui, p);
            });
        }
        TokenRow::Paste { replacing } => {
            ui.horizontal(|ui| {
                paste(ui, p, if replacing { "Paste the new token" } else { "Paste the token from the site's setup page" });
                if replacing && ui.button("Cancel").clicked() {
                    p.clear_token_field(ui.ctx());
                    p.replacing = false;
                }
            });
            if replacing {
                ui.label(RichText::new("The saved token stays until the new one is saved.").color(MUTED).size(12.0));
            }
        }
        TokenRow::Unavailable(e) => {
            ui.label(RichText::new(e).color(WARN_TEXT).size(12.0));
            ui.horizontal(|ui| {
                paste(ui, p, "Paste the token from the site's setup page");
                // Forget stays available when the store holds something it cannot read, so it can be removed.
                forget(ui, p);
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
        let ctx = egui::Context::default();
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
        let ctx = egui::Context::default();
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
        let ctx = egui::Context::default();
        let mut p = WorkspaceState { token_input: TOKEN.into(), replacing: true, ..Default::default() };
        let before = p.field;
        p.clear_token_field(&ctx);
        assert!(p.token_input.is_empty());
        assert_ne!(p.field, before);
        // Leaving the page does the same, and closes "Replace".
        let mut p = WorkspaceState { token_input: TOKEN.into(), replacing: true, auto: Some(site("https://a.example.com")), ..Default::default() };
        p.leave(&ctx);
        assert!(!p.has_unsaved_token() && !p.replacing && p.auto.is_none());
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
        Site { server: server.into(), epoch: 1 }
    }

    fn conn(token: TokenStatus) -> Conn {
        Conn { token, epoch: 1, test_server: None }
    }

    #[test]
    fn the_rows_say_what_the_site_said_and_unknown_for_an_older_site() {
        assert_eq!(tours_summary(Counts { public: 1, draft: 1 }), "2 (1 public, 1 draft)");
        assert_eq!(tours_summary(Counts { public: 3, draft: 0 }), "3 (3 public, 0 draft)");
        assert_eq!(tours_summary(Counts { public: 0, draft: 1 }), "1 (0 public, 1 draft)");
        assert_eq!(tours_summary(Counts { public: 0, draft: 0 }), "0");
        assert_eq!(version_label(Some("0.1.1")), "0.1.1");
        assert_eq!(version_label(None), "unknown");
        let t = |status: &str| RemoteTour { id: "t".into(), title: "T".into(), status: status.into(), chapters: 1, steps: 1, link: None, updated_at: None, published_at: None };
        assert_eq!(Counts::of(&[t("published"), t("draft"), t("draft")]), Counts { public: 1, draft: 2 });
        assert_eq!(Counts::of(&[]), Counts { public: 0, draft: 0 });
    }

    #[test]
    fn the_badge_and_the_token_row_follow_the_saved_state() {
        let saved = TokenStatus::Saved;
        assert_eq!(badge(&saved, false, true), CONNECTED);
        assert_eq!(badge(&saved, true, true), CHECKING);
        assert_eq!(badge(&saved, true, false), CHECKING);
        assert_eq!(badge(&saved, false, false), NOT_CONNECTED);
        assert_eq!(badge(&TokenStatus::Checking, false, false), CHECKING);
        assert_eq!(badge(&TokenStatus::Missing, false, false), NOT_CONNECTED);
        assert_eq!(badge(&TokenStatus::Unavailable("locked".into()), false, false), NOT_CONNECTED);

        assert_eq!(token_row(&saved, false), TokenRow::Saved);
        assert_eq!(token_row(&saved, true), TokenRow::Paste { replacing: true });
        assert_eq!(token_row(&TokenStatus::Missing, false), TokenRow::Paste { replacing: false });
        // "Replace" means nothing without a saved token.
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

    fn page() -> WorkspaceState {
        WorkspaceState::with_token(|| Ok(TOKEN.to_string()))
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
    fn a_connected_page_shows_the_workspace_the_tours_and_the_version() {
        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let (ctx, mut p, mut server, c) = (egui::Context::default(), page(), String::new(), conn(TokenStatus::Saved));
        server.clone_from(&s.base);
        // Opening the page asks the site without a click.
        let first = drawn(&ctx, &mut p, &mut server, &c);
        // The badge says so, unless the stub already answered.
        assert!(has_line(&first, "Workspace") && (has_line(&first, CHECKING) || has_line(&first, CONNECTED)), "{first}");
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, CONNECTED));
        for word in ["Workspace", CONNECTED, "Site address", "Acme", "Token", TOKEN_SAVED, "Replace", "Forget", "Tours", "2 (1 public, 1 draft)", "Site version", "0.1.1", "Test connection", "Open site", TOKEN_NOTE] {
            assert!(has_line(&text, word), "the page is missing {word:?} in:\n{text}");
        }
        assert!(!text.contains("dp_") && !has_line(&text, "Save"), "{text}");
        // It asked once, not once per frame.
        for _ in 0..5 {
            drawn(&ctx, &mut p, &mut server, &c);
        }
        assert_eq!(s.lines(), ["GET /api/v1/me", "GET /api/v1/tours"]);
        // Leaving and coming back asks again.
        p.leave(&ctx);
        drawn(&ctx, &mut p, &mut server, &c);
        until(&ctx, &mut p, &mut server, &c, |_| s.lines().len() == 4);
        // A saved token being replaced: the paste field with Save and Cancel; the rest stays.
        p.replacing = true;
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, CONNECTED));
        assert!(has_line(&text, "Save") && has_line(&text, "Cancel") && !has_line(&text, TOKEN_SAVED) && has_line(&text, "Acme"), "{text}");
    }

    #[test]
    fn an_older_site_is_connected_with_its_version_unknown() {
        let s = site_stub(r#"{"workspace":"Acme"}"#);
        let (ctx, mut p, mut server, c) = (egui::Context::default(), page(), s.base.clone(), conn(TokenStatus::Saved));
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, CONNECTED));
        assert!(has_line(&text, "Acme") && has_line(&text, "unknown") && has_line(&text, "2 (1 public, 1 draft)"), "{text}");
    }

    #[test]
    fn a_page_without_a_token_or_with_a_failing_site_is_not_connected_and_asks_nothing_more() {
        let ctx = egui::Context::default();
        // No token: the paste field with Save, and no request to anyone.
        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let (mut p, mut server) = (page(), s.base.clone());
        let text = drawn(&ctx, &mut p, &mut server, &conn(TokenStatus::Missing));
        for word in ["Workspace", NOT_CONNECTED, "Site address", "Token", "Save", "Tours", "Site version", "Test connection", "Open site"] {
            assert!(has_line(&text, word), "the page is missing {word:?} in:\n{text}");
        }
        assert!(!has_line(&text, "Replace") && !has_line(&text, "Forget") && !has_line(&text, "Acme") && !has_line(&text, "unknown"), "{text}");
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
        let (mut p, mut server, c) = (page(), s.base.clone(), conn(TokenStatus::Saved));
        drawn(&ctx, &mut p, &mut server, &c);
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, NOT_CONNECTED));
        assert!(!has_line(&text, "unknown") && !has_line(&text, PRESS_TEST), "{text}");
        for _ in 0..5 {
            drawn(&ctx, &mut p, &mut server, &c);
        }
        assert_eq!(s.lines(), ["GET /api/v1/me"]);
        // A tour list that cannot be read leaves the count open; the connection stands.
        let s = stub(|req, _| if req.line == "GET /api/v1/me" { reply(200, r#"{"workspace":"Acme","version":"0.1.1"}"#) } else { reply(503, "") });
        let (mut p, mut server) = (page(), s.base.clone());
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, CONNECTED));
        assert!(has_line(&text, "Acme") && !text.contains("public"), "{text}");
    }

    #[test]
    fn an_address_being_typed_is_never_asked_on_its_own() {
        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let (ctx, mut p, c) = (egui::Context::default(), page(), conn(TokenStatus::Saved));
        // The page was opened on another address and has asked it.
        let other = site_stub(r#"{"workspace":"Other"}"#);
        let mut server = other.base.clone();
        until(&ctx, &mut p, &mut server, &c, |t| has_line(t, CONNECTED));
        // Typing: what the field's change handler does for each new text.
        server.clone_from(&s.base);
        if let Some(typed) = c.site(&server) {
            p.auto = Some(typed);
        }
        let text = drawn(&ctx, &mut p, &mut server, &c);
        assert!(has_line(&text, NOT_CONNECTED) && has_line(&text, PRESS_TEST) && !has_line(&text, "Other"), "the old site's answer is not shown for the new address:\n{text}");
        std::thread::sleep(Duration::from_millis(50));
        drawn(&ctx, &mut p, &mut server, &c);
        assert!(s.lines().is_empty(), "the typed address got a request: {:?}", s.lines());
        // Test connection asks it, and says so.
        p.spawn_check(c.site(&server).unwrap(), Some(server.clone()));
        let text = until(&ctx, &mut p, &mut server, &c, |t| has_line(t, CONNECTED));
        assert!(text.replace('\n', " ").contains("Connected to workspace \"Acme\"") && has_line(&text, "Acme"), "{text}");
        assert_eq!(s.lines(), ["GET /api/v1/me", "GET /api/v1/tours"]);
    }

    #[test]
    fn typing_in_the_address_field_reports_a_change_and_sends_nothing() {
        let s = site_stub(r#"{"workspace":"Acme","version":"0.1.1"}"#);
        let (ctx, mut p, c) = (egui::Context::default(), page(), conn(TokenStatus::Saved));
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
}
