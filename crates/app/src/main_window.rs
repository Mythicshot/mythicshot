//! The main window: a left menu (Home, Tours, Workspace, Preferences) and the page it selects.
//!
//! Like every window opened from the tray it is a deferred viewport, so it draws on its own
//! schedule: it changes [`Shared`], and the app applies what it queued on its next frame. Closing
//! it only closes the window; the app stays in the tray.
//!
//! Adding a page: give it a module with a `ui` function, and call it from [`page_ui`]. A page
//! reports what it wants through [`PageOut`], so it never needs the app itself.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::publish_ui::{self, TokenStatus};
use crate::settings_ui::{self, UiAction};
use crate::tray::Command;

const MUTED: Color32 = Color32::from_rgb(163, 170, 184);

pub const TITLE: &str = "Mythicshot";
pub const SIZE: [f32; 2] = [760.0, 520.0];
const MIN_SIZE: [f32; 2] = [640.0, 420.0];
const NAV_WIDTH: f32 = 170.0;

/// How long after the window is closed a capture started from it waits, so the window is off the
/// screen (and the window that had focus before it has it back) when the picture is taken.
pub const HIDE_BEFORE_CAPTURE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Home,
    Tours,
    Workspace,
    Preferences,
}

impl Page {
    /// The menu, top to bottom.
    pub const ALL: [Page; 4] = [Page::Home, Page::Tours, Page::Workspace, Page::Preferences];

    pub fn label(self) -> &'static str {
        match self {
            Page::Home => "Home",
            Page::Tours => "Tours",
            Page::Workspace => "Workspace",
            Page::Preferences => "Preferences",
        }
    }

    /// The one sentence a page that is not built yet shows; `None` for a page that is.
    pub fn coming_soon(self) -> Option<&'static str> {
        match self {
            Page::Tours => Some("The Tours page is coming in a later update."),
            Page::Workspace => Some("The Workspace page is coming in a later update."),
            Page::Home | Page::Preferences => None,
        }
    }
}

/// The page a command opens the main window on, for the commands that open it.
pub fn page_for(cmd: Command) -> Option<Page> {
    match cmd {
        Command::OpenApp => Some(Page::Home),
        Command::Preferences => Some(Page::Preferences),
        _ => None,
    }
}

/// The page to open at start-up: Preferences for `--settings`, otherwise the app starts in the tray.
pub fn start_page(args: impl IntoIterator<Item = String>) -> Option<Page> {
    args.into_iter().any(|a| a == "--settings").then_some(Page::Preferences)
}

/// Which page is up and whether the window is open, without any drawing.
#[derive(Debug, Default)]
pub struct Nav {
    open: bool,
    page: Page,
    focus: bool,
    left_preferences: bool,
}

impl Nav {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// Open the window on `page`. When it is already open this switches the page and brings the
    /// same window to the front; there is never a second one.
    pub fn request(&mut self, page: Page) {
        self.go(page);
        self.open = true;
        self.focus = true;
    }

    /// Switch page from inside the window: no focus change.
    pub fn go(&mut self, page: Page) {
        if self.open && self.page == Page::Preferences && page != Page::Preferences {
            self.left_preferences = true;
        }
        self.page = page;
    }

    /// Close the window. The page stays, so the next plain open of the window is where it was left.
    pub fn close(&mut self) {
        if self.open && self.page == Page::Preferences {
            self.left_preferences = true;
        }
        self.open = false;
        self.focus = false;
    }

    /// Whether the window should be brought to the front now; true once per request.
    pub fn take_focus(&mut self) -> bool {
        std::mem::take(&mut self.focus)
    }

    /// Whether the Preferences page was left (by switching page or closing the window) since the
    /// last call. Its unfinished input is dropped then, see [`Shared::settle`].
    pub fn take_left_preferences(&mut self) -> bool {
        std::mem::take(&mut self.left_preferences)
    }
}

/// What a page asks the window or the app to do.
#[derive(Debug, Default)]
pub struct PageOut {
    /// Switch to another page.
    pub go_to: Option<Page>,
    /// Hide the window, then run this capture command.
    pub capture: Option<Command>,
}

/// State shared between the app and the window.
pub struct Shared {
    pub nav: Nav,
    /// The Preferences page: the settings being edited and what the page queued for the app.
    pub prefs: settings_ui::Shared,
    /// A capture a Home tile asked for; the app hides the window, then runs it.
    pub capture: Option<Command>,
    /// A capture or recording is going on: the tiles are greyed out.
    pub busy: bool,
}

impl Shared {
    pub fn new(prefs: settings_ui::Shared) -> Self {
        Self { nav: Nav::default(), prefs, capture: None, busy: false }
    }

    /// Drop the Preferences page's unfinished input once the page has been left. Settings
    /// themselves are saved as they are changed, so the only things that can be left half done are
    /// a token typed but not saved (wiped, exactly as when the settings window closed) and a
    /// shortcut waiting for its keys (cancelled).
    pub fn settle(&mut self, ctx: &egui::Context) {
        if self.nav.take_left_preferences() {
            self.prefs.state.publishing.clear_token_field(ctx);
            self.prefs.actions.push(UiAction::StopListening);
        }
    }
}

/// A capture asked for from the window: it runs once the window has been off the screen for
/// [`HIDE_BEFORE_CAPTURE`].
#[derive(Debug, Default)]
pub struct HideThenRun {
    waiting: Option<(Command, Instant)>,
}

impl HideThenRun {
    /// The window was closed at `now`; run `cmd` after the wait. A second request replaces the first.
    pub fn start(&mut self, cmd: Command, now: Instant) {
        self.waiting = Some((cmd, now + HIDE_BEFORE_CAPTURE));
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// The command, once, when its time has come.
    pub fn take_due(&mut self, now: Instant) -> Option<Command> {
        match self.waiting {
            Some((cmd, at)) if now >= at => {
                self.waiting = None;
                Some(cmd)
            }
            _ => None,
        }
    }

    /// How long until the command is due, to wake the UI then.
    pub fn due_in(&self, now: Instant) -> Option<Duration> {
        self.waiting.map(|(_, at)| at.saturating_duration_since(now))
    }
}

/// The host name for the "Connected to" line: the saved server's, when a token is saved too.
/// Nothing is asked of the server; this only says what is set up.
pub fn connected_host(server: &str, token: &TokenStatus) -> Option<String> {
    if *token != TokenStatus::Saved {
        return None;
    }
    let url = crate::publish::check_server(server).ok()?;
    let authority = url.split_once("://")?.1.split('/').next()?;
    let host = match authority.strip_prefix('[') {
        // An IPv6 address keeps its brackets.
        Some(rest) => &authority[..rest.find(']')? + 2],
        None => authority.rsplit_once(':').map_or(authority, |(host, _port)| host),
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("main-window")
}

/// Show the window. `page_request` opens it on that page, or, when it is already open, switches
/// to the page and brings the window to the front. Without a request, an open window is kept
/// drawn and a closed one stays closed.
pub fn show(ctx: &egui::Context, shared: &Arc<Mutex<Shared>>, icon: Arc<egui::IconData>, page_request: Option<Page>) {
    {
        let mut s = shared.lock().expect("main window state");
        if let Some(page) = page_request {
            s.nav.request(page);
        }
        s.settle(ctx);
        if !s.nav.is_open() {
            return;
        }
    }
    let builder = egui::ViewportBuilder::default().with_title(TITLE).with_inner_size(SIZE).with_min_inner_size(MIN_SIZE).with_icon(icon);
    let shared = shared.clone();
    ctx.show_viewport_deferred(viewport_id(), builder, move |ui, _class| {
        let mut guard = shared.lock().expect("main window state");
        let s = &mut *guard;
        if s.nav.take_focus() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            // Only the window goes; the app stays in the tray.
            s.nav.close();
        }
        window_ui(ui, s);
        s.settle(ui.ctx());
        // The app applies what was queued on its own frame: wake it instead of waiting for its slow tick.
        if !s.nav.is_open() || s.capture.is_some() || !s.prefs.actions.is_empty() {
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    });
    ctx.request_repaint_of(viewport_id());
}

/// The window's contents: the menu on the left, the selected page beside it.
pub fn window_ui(ui: &mut egui::Ui, s: &mut Shared) {
    let mut out = PageOut::default();
    egui::Panel::left("main-nav").resizable(false).exact_size(NAV_WIDTH).show(ui, |ui| nav_ui(ui, s.nav.page(), &mut out));
    egui::CentralPanel::default().show(ui, |ui| page_ui(ui, s, &mut out));
    if let Some(page) = out.go_to {
        s.nav.go(page);
    }
    if out.capture.is_some() {
        s.capture = out.capture;
    }
}

fn nav_ui(ui: &mut egui::Ui, current: Page, out: &mut PageOut) {
    ui.add_space(8.0);
    for page in Page::ALL {
        if ui.add_sized([ui.available_width(), 34.0], egui::Button::selectable(current == page, page.label())).clicked() {
            out.go_to = Some(page);
        }
    }
    // Whether a site is set up, at the bottom of the menu.
    let token = publish_ui::token_status();
    if token == TokenStatus::Checking {
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }
    let host = connected_host(&publish_ui::server(), &token);
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.add_space(8.0);
        match host {
            Some(host) => {
                ui.add(egui::Label::new(RichText::new(host).size(12.0).color(MUTED)).truncate());
                ui.label(RichText::new("Connected to").size(12.0).color(MUTED));
            }
            None => {
                ui.label(RichText::new("Not connected").size(12.0).color(MUTED));
            }
        }
    });
}

/// Draw the selected page. Each page gets the part of [`Shared`] it owns and a [`PageOut`].
fn page_ui(ui: &mut egui::Ui, s: &mut Shared, _out: &mut PageOut) {
    match s.nav.page() {
        // The Home page comes with the next change.
        Page::Home => {
            ui.heading(Page::Home.label());
        }
        // Not built yet: Tours (`tours_ui`) and Workspace (`workspace_ui`) replace these two arms.
        page @ (Page::Tours | Page::Workspace) => placeholder(ui, page),
        Page::Preferences => {
            let p = &mut s.prefs;
            let view = settings_ui::View { settings: &mut p.settings, errors: &p.errors, snipping_tool: p.snipping_tool, autostart: p.autostart };
            let actions = settings_ui::ui(ui, &mut p.state, view);
            p.actions.extend(actions);
        }
    }
}

/// A page that is not built yet: its heading and one sentence.
fn placeholder(ui: &mut egui::Ui, page: Page) {
    ui.heading(page.label());
    ui.add_space(12.0);
    ui.label(RichText::new(page.coming_soon().unwrap_or_default()).color(MUTED));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn opening_an_open_window_focuses_it_and_switches_page() {
        let mut nav = Nav::default();
        assert!(!nav.is_open() && !nav.take_focus());
        nav.request(Page::Home);
        assert!(nav.is_open());
        assert_eq!(nav.page(), Page::Home);
        assert!(nav.take_focus(), "a newly opened window comes to the front");
        assert!(!nav.take_focus(), "and is brought forward once, not every frame");
        // Asked again for another page while open: the same window, in front, on that page.
        nav.request(Page::Preferences);
        assert!(nav.is_open());
        assert_eq!(nav.page(), Page::Preferences);
        assert!(nav.take_focus());
        // Asked for the page it is already on: still brought forward.
        nav.request(Page::Preferences);
        assert!(nav.take_focus());
    }

    #[test]
    fn switching_page_inside_the_window_does_not_take_focus() {
        let mut nav = Nav::default();
        nav.request(Page::Home);
        nav.take_focus();
        for page in Page::ALL {
            nav.go(page);
            assert_eq!(nav.page(), page);
            assert!(nav.is_open() && !nav.take_focus());
        }
    }

    #[test]
    fn closing_keeps_the_page_and_never_asks_for_focus() {
        let mut nav = Nav::default();
        nav.request(Page::Tours);
        nav.close();
        assert!(!nav.is_open());
        assert!(!nav.take_focus(), "a closed window is never raised");
        assert_eq!(nav.page(), Page::Tours);
        nav.request(Page::Home);
        assert!(nav.is_open() && nav.take_focus());
    }

    #[test]
    fn leaving_preferences_is_noticed_once_however_it_is_left() {
        // By switching page.
        let mut nav = Nav::default();
        nav.request(Page::Preferences);
        assert!(!nav.take_left_preferences());
        nav.go(Page::Home);
        assert!(nav.take_left_preferences());
        assert!(!nav.take_left_preferences());
        // By closing the window.
        nav.request(Page::Preferences);
        nav.close();
        assert!(nav.take_left_preferences());
        // By a request for another page.
        nav.request(Page::Preferences);
        nav.request(Page::Home);
        assert!(nav.take_left_preferences());
        // Staying on it, or moving between other pages, is not leaving it.
        nav.request(Page::Preferences);
        nav.go(Page::Preferences);
        nav.request(Page::Preferences);
        assert!(!nav.take_left_preferences());
        nav.go(Page::Tours);
        nav.take_left_preferences();
        nav.go(Page::Workspace);
        nav.close();
        assert!(!nav.take_left_preferences());
    }

    #[test]
    fn every_way_in_to_preferences_opens_the_main_window_on_that_page() {
        // The tray's "Preferences…" item and the debug test hook both run this command.
        assert_eq!(page_for(Command::Preferences), Some(Page::Preferences));
        assert_eq!(page_for(Command::OpenApp), Some(Page::Home));
        // The `--settings` argument.
        assert_eq!(start_page(["mythicshot.exe".to_string(), "--settings".to_string()]), Some(Page::Preferences));
        assert_eq!(start_page(["mythicshot.exe".to_string()]), None);
        // Nothing else opens the window.
        for cmd in [Command::Region, Command::Window, Command::FullScreen, Command::Record, Command::OpenFolder, Command::Exit, Command::ToggleMagnifier] {
            assert_eq!(page_for(cmd), None, "{cmd:?}");
        }
    }

    #[test]
    fn a_capture_from_the_window_waits_until_the_window_is_gone() {
        let t0 = Instant::now();
        let mut h = HideThenRun::default();
        assert!(!h.is_waiting());
        assert_eq!(h.take_due(t0), None);
        assert_eq!(h.due_in(t0), None);
        h.start(Command::Window, t0);
        assert!(h.is_waiting());
        assert_eq!(h.take_due(t0), None, "not in the same frame the window closes");
        assert_eq!(h.take_due(t0 + HIDE_BEFORE_CAPTURE - Duration::from_millis(1)), None);
        assert_eq!(h.due_in(t0 + Duration::from_millis(100)), Some(HIDE_BEFORE_CAPTURE - Duration::from_millis(100)));
        assert_eq!(h.take_due(t0 + HIDE_BEFORE_CAPTURE), Some(Command::Window));
        assert_eq!(h.take_due(t0 + HIDE_BEFORE_CAPTURE * 2), None, "it runs once");
        assert!(!h.is_waiting());
    }

    #[test]
    fn the_connection_line_names_the_host_only_when_a_server_and_token_are_saved() {
        let saved = TokenStatus::Saved;
        assert_eq!(connected_host("https://tours.example.com", &saved).as_deref(), Some("tours.example.com"));
        assert_eq!(connected_host(" https://Tours.Example.com/demo/ ", &saved).as_deref(), Some("tours.example.com"));
        assert_eq!(connected_host("http://localhost:8787", &saved).as_deref(), Some("localhost"));
        assert_eq!(connected_host("http://127.0.0.1:8787/x", &saved).as_deref(), Some("127.0.0.1"));
        // No address, or one the app would refuse to use.
        assert_eq!(connected_host("", &saved), None);
        assert_eq!(connected_host("http://tours.example.com", &saved), None);
        assert_eq!(connected_host("tours.example.com", &saved), None);
        // No token, not known yet, or a store that cannot be read.
        for token in [TokenStatus::Missing, TokenStatus::Checking, TokenStatus::Unavailable("The credential store is locked.".into())] {
            assert_eq!(connected_host("https://tours.example.com", &token), None, "{token:?}");
        }
    }

    #[test]
    fn the_two_pages_not_built_yet_say_so_in_one_sentence() {
        for page in [Page::Tours, Page::Workspace] {
            let s = page.coming_soon().expect("a placeholder");
            assert!(s.starts_with(&format!("The {} page", page.label())) && s.ends_with('.') && s.matches('.').count() == 1 && !s.contains('!'), "{s}");
        }
        assert_eq!((Page::Home.coming_soon(), Page::Preferences.coming_soon()), (None, None));
        assert_eq!(Page::ALL.map(Page::label), ["Home", "Tours", "Workspace", "Preferences"]);
    }

    fn shared(dir: &std::path::Path) -> Shared {
        let mut settings = Settings::default();
        settings.output_dir = dir.to_path_buf();
        Shared::new(settings_ui::Shared { settings, errors: Default::default(), snipping_tool: None, autostart: false, state: Default::default(), actions: Vec::new() })
    }

    /// Every piece of text one frame of the window draws.
    fn drawn(ctx: &egui::Context, s: &mut Shared) -> String {
        fn collect(shape: &egui::Shape, into: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => into.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                _ => {}
            }
        }
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIZE[0], SIZE[1]))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| window_ui(ui, s));
        out.textures_delta.clear();
        let mut texts = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        texts.join("\n")
    }

    #[test]
    fn each_page_draws_its_wording_beside_the_menu() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        let ctx = egui::Context::default();
        s.nav.request(Page::Home);
        let home = drawn(&ctx, &mut s);
        for word in ["Home", "Tours", "Workspace", "Preferences", "Not connected"] {
            assert!(home.lines().any(|l| l == word), "Home is missing {word:?} in:\n{home}");
        }
        for page in [Page::Tours, Page::Workspace] {
            s.nav.go(page);
            let text = drawn(&ctx, &mut s);
            assert!(text.contains(page.coming_soon().unwrap()), "{text}");
        }
        s.nav.go(Page::Preferences);
        let prefs = drawn(&ctx, &mut s);
        for word in ["General", "Output", "Hotkeys", "Left-click on the tray icon", "Open Mythicshot", "Capture a region"] {
            assert!(prefs.lines().any(|l| l == word), "Preferences is missing {word:?} in:\n{prefs}");
        }
    }

    #[test]
    fn leaving_preferences_wipes_an_unsaved_token_and_stops_a_shortcut_recording() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        for leave in [|n: &mut Nav| n.go(Page::Home), |n: &mut Nav| n.close(), |n: &mut Nav| n.request(Page::Tours)] {
            let mut s = shared(tmp.path());
            s.nav.request(Page::Preferences);
            s.prefs.state.publishing.type_token_for_test("dp_0123456789abcdef0123456789abcdef01234567");
            s.settle(&ctx);
            assert!(s.prefs.state.publishing.has_unsaved_token(), "nothing is dropped while the page is up");
            assert!(s.prefs.actions.is_empty());
            leave(&mut s.nav);
            s.settle(&ctx);
            assert!(!s.prefs.state.publishing.has_unsaved_token());
            assert!(matches!(s.prefs.actions.as_slice(), [UiAction::StopListening]));
            // Once is enough.
            s.settle(&ctx);
            assert_eq!(s.prefs.actions.len(), 1);
        }
    }
}
