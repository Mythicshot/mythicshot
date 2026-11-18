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

use crate::home_ui::{self, HomeState};
use crate::platform::{WindowId, WindowKind};
use crate::publish_ui::{self, TokenStatus};
use crate::settings_ui::{self, UiAction};
use crate::tours_ui::{self, ToursState};
use crate::workspace_ui::{self, WorkspaceState};
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

}

/// A request to show the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// A tray click or "Open Mythicshot": a closed window opens on Home; an open one comes to the
    /// front as it is, on the page it shows, with whatever is typed there left alone.
    Open,
    /// "Preferences…" or a link from another page: open on this page, or switch to it.
    Page(Page),
}

/// The request a command makes of the main window, for the commands that open it.
pub fn request_for(cmd: Command) -> Option<Request> {
    match cmd {
        Command::OpenApp => Some(Request::Open),
        Command::Preferences => Some(Request::Page(Page::Preferences)),
        _ => None,
    }
}

/// What to open at start-up: Preferences for `--settings`, otherwise the app starts in the tray.
pub fn start_request(args: impl IntoIterator<Item = String>) -> Option<Request> {
    args.into_iter().any(|a| a == "--settings").then_some(Request::Page(Page::Preferences))
}

/// Which page is up and whether the window is open, without any drawing.
#[derive(Debug, Default)]
pub struct Nav {
    open: bool,
    page: Page,
    focus: bool,
    left_preferences: bool,
    left_workspace: bool,
}

impl Nav {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// Apply a request to show the window.
    pub fn apply(&mut self, request: Request) {
        match request {
            Request::Open => self.open_or_raise(),
            Request::Page(page) => self.request(page),
        }
    }

    /// A closed window opens on Home. An open one only comes to the front: its page stays, so
    /// nothing typed on it is lost.
    pub fn open_or_raise(&mut self) {
        if !self.open {
            self.page = Page::Home;
            self.open = true;
        }
        self.focus = true;
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
        if self.open && self.page != page {
            self.note_left();
        }
        self.page = page;
    }

    /// Close the window. The page stays, so the next plain open of the window is where it was left.
    pub fn close(&mut self) {
        if self.open {
            self.note_left();
        }
        self.open = false;
        self.focus = false;
    }

    /// Whether the window should be brought to the front now; true once per request.
    pub fn take_focus(&mut self) -> bool {
        std::mem::take(&mut self.focus)
    }

    /// The page that is up is being left, by switching page or closing the window.
    fn note_left(&mut self) {
        match self.page {
            Page::Preferences => self.left_preferences = true,
            Page::Workspace => self.left_workspace = true,
            Page::Home | Page::Tours => {}
        }
    }

    /// Whether the Preferences page was left (by switching page or closing the window) since the
    /// last call. Its unfinished input is dropped then, see [`Shared::settle`].
    pub fn take_left_preferences(&mut self) -> bool {
        std::mem::take(&mut self.left_preferences)
    }

    /// The same for the Workspace page, where a token may have been typed and not saved.
    pub fn take_left_workspace(&mut self) -> bool {
        std::mem::take(&mut self.left_workspace)
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
    pub home: HomeState,
    /// The Tours page. Its jobs hold their own handle on it, so they finish when the window is closed.
    pub tours: ToursState,
    /// The Workspace page: the token field and what the site said about itself.
    pub workspace: WorkspaceState,
    /// A capture a Home tile asked for; the app hides the window, then runs it.
    pub capture: Option<Command>,
    /// A capture or recording is going on: the tiles are greyed out.
    pub busy: bool,
}

impl Shared {
    pub fn new(prefs: settings_ui::Shared) -> Self {
        Self { nav: Nav::default(), prefs, home: HomeState::default(), tours: ToursState::default(), workspace: WorkspaceState::default(), capture: None, busy: false }
    }

    /// Drop a page's unfinished input once the page has been left. Settings themselves are
    /// saved as they are changed, so the only things that can be left half done are a shortcut
    /// waiting for its keys on Preferences (cancelled) and a token typed but not saved on
    /// Workspace (wiped, exactly as when the settings window closed).
    pub fn settle(&mut self, ctx: &egui::Context) {
        if self.nav.take_left_preferences() {
            self.prefs.actions.push(UiAction::StopListening);
        }
        if self.nav.take_left_workspace() {
            self.workspace.leave(ctx);
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

    /// Drop the waiting capture: something else was started in the meantime.
    pub fn cancel(&mut self) {
        self.waiting = None;
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

/// The window the user was in before Mythicshot: the last window seen in front that is neither one
/// of ours nor the taskbar (which is in front while the tray icon is clicked). A capture started
/// from the main window gives focus back to it first, so "Window" captures that window and not
/// whatever Windows happens to pick once the main window is gone.
#[derive(Debug, Default)]
pub struct LastForeground {
    window: Option<WindowId>,
}

impl LastForeground {
    /// The window in front now, and whose it is.
    pub fn note(&mut self, window: WindowId, kind: WindowKind) {
        if kind == WindowKind::Other {
            self.window = Some(window);
        }
    }

    /// The window to give focus back to, if one was ever seen.
    pub fn get(&self) -> Option<WindowId> {
        self.window
    }
}

/// The command a Home tile's capture runs once the main window is gone, given the window in front
/// then. "Window" captures the window in front, so when that is one of Mythicshot's own (an open
/// editor, because the app the user came from has closed), the taskbar, the desktop or nothing,
/// it becomes the region selector, where a window is picked by pointing at it.
pub fn tile_command(cmd: Command, front: Option<WindowKind>) -> Command {
    match cmd {
        Command::Window if front != Some(WindowKind::Other) => Command::Region,
        cmd => cmd,
    }
}

/// The host name for the "Connected to" line: the saved server's, when a token is saved too.
/// Nothing is asked of the server; this only says what is set up.
/// `bound` is the address the token was saved for: with another address set, the token is not
/// used there, so that is not a connection.
pub fn connected_host(server: &str, token: &TokenStatus, bound: Option<&str>) -> Option<String> {
    if *token != TokenStatus::Saved || publish_ui::binding_problem(bound, server).is_some() {
        return None;
    }
    crate::publish::host_of(server)
}

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("main-window")
}

/// Show the window. A request opens it, or, when it is already open, brings it to the front (see
/// [`Request`] for which page it shows). Without a request, an open window is kept drawn and a
/// closed one stays closed.
pub fn show(ctx: &egui::Context, shared: &Arc<Mutex<Shared>>, icon: Arc<egui::IconData>, request: Option<Request>) {
    {
        let mut s = shared.lock().expect("main window state");
        if let Some(request) = request {
            s.nav.apply(request);
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
        if !s.nav.is_open() || s.capture.is_some() || !s.prefs.actions.is_empty() || s.tours.unfinished().is_changed() || publish_ui::has_token_site_change() {
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
    let host = connected_host(&publish_ui::server(), &token, publish_ui::token_site().as_deref());
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
fn page_ui(ui: &mut egui::Ui, s: &mut Shared, out: &mut PageOut) {
    match s.nav.page() {
        Page::Home => home_ui::ui(ui, &mut s.home, &s.prefs.settings, s.busy, out),
        Page::Tours => tours_ui::ui(ui, &mut s.tours, &tours_ui::Conn::current(), out),
        Page::Workspace => {
            // The site address is a setting like any other: the app saves it on its next frame.
            if workspace_ui::ui(ui, &mut s.workspace, &mut s.prefs.settings.publish_server, &workspace_ui::Conn::current()) {
                s.prefs.actions.push(UiAction::SettingsChanged);
            }
        }
        Page::Preferences => {
            let p = &mut s.prefs;
            let view = settings_ui::View { settings: &mut p.settings, errors: &p.errors, snipping_tool: p.snipping_tool, autostart: p.autostart };
            for action in settings_ui::ui(ui, &mut p.state, view) {
                match action {
                    // The Output tab's link: a page switch, which is the window's own business.
                    UiAction::OpenWorkspace => out.go_to = Some(Page::Workspace),
                    action => p.actions.push(action),
                }
            }
        }
    }
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
        assert_eq!(request_for(Command::Preferences), Some(Request::Page(Page::Preferences)));
        // The `--settings` argument.
        assert_eq!(start_request(["mythicshot.exe".to_string(), "--settings".to_string()]), Some(Request::Page(Page::Preferences)));
        assert_eq!(start_request(["mythicshot.exe".to_string()]), None);
        let mut nav = Nav::default();
        nav.request(Page::Tours);
        nav.take_focus();
        nav.apply(Request::Page(Page::Preferences));
        assert_eq!(nav.page(), Page::Preferences);
        assert!(nav.is_open() && nav.take_focus());
        // The tray click and "Open Mythicshot" ask for the window, not for a page.
        assert_eq!(request_for(Command::OpenApp), Some(Request::Open));
        // Nothing else opens the window.
        for cmd in [Command::Region, Command::Window, Command::FullScreen, Command::Record, Command::OpenFolder, Command::Exit, Command::ToggleMagnifier] {
            assert_eq!(request_for(cmd), None, "{cmd:?}");
        }
    }

    #[test]
    fn a_tray_click_opens_a_closed_window_on_home() {
        let mut nav = Nav::default();
        nav.apply(Request::Open);
        assert!(nav.is_open() && nav.take_focus());
        assert_eq!(nav.page(), Page::Home);
        // Closed on another page, it opens on Home again.
        nav.request(Page::Preferences);
        nav.close();
        nav.take_left_preferences();
        nav.apply(Request::Open);
        assert_eq!(nav.page(), Page::Home);
        assert!(nav.is_open() && nav.take_focus());
    }

    #[test]
    fn a_tray_click_on_an_open_window_raises_it_and_keeps_its_page_and_what_is_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut s = shared(tmp.path());
        s.nav.request(Page::Workspace);
        s.nav.take_focus();
        s.workspace.type_token_for_test("dp_0123456789abcdef0123456789abcdef01234567");
        s.nav.apply(Request::Open);
        s.settle(&ctx);
        assert_eq!(s.nav.page(), Page::Workspace, "it does not jump to Home");
        assert!(s.nav.take_focus(), "it comes to the front");
        assert!(s.workspace.has_unsaved_token(), "the typed token is still there");
        assert!(!s.nav.take_left_workspace());
        s.nav.go(Page::Preferences);
        s.settle(&ctx);
        s.nav.apply(Request::Open);
        s.settle(&ctx);
        assert_eq!(s.nav.page(), Page::Preferences);
        assert!(s.nav.take_focus());
        assert!(s.prefs.actions.is_empty(), "a shortcut being recorded is not stopped");
        assert!(!s.nav.take_left_preferences());
        // The same on every other page.
        for page in [Page::Home, Page::Tours] {
            s.nav.go(page);
            s.nav.apply(Request::Open);
            assert_eq!(s.nav.page(), page);
            assert!(s.nav.take_focus());
        }
    }

    #[test]
    fn the_window_to_go_back_to_is_the_last_one_that_was_not_ours_or_the_taskbar() {
        let mut last = LastForeground::default();
        assert_eq!(last.get(), None);
        last.note(11, WindowKind::Other);
        // The click on the tray icon, then our own window in front.
        last.note(22, WindowKind::Shell);
        last.note(33, WindowKind::Ours);
        assert_eq!(last.get(), Some(11));
        // The user went to another app and came back: that app is the one to return to.
        last.note(44, WindowKind::Other);
        last.note(33, WindowKind::Ours);
        assert_eq!(last.get(), Some(44));
    }

    #[test]
    fn the_window_tile_never_captures_one_of_our_own_windows() {
        assert_eq!(tile_command(Command::Window, Some(WindowKind::Other)), Command::Window);
        // The app the user came from is gone and an editor, the taskbar or nothing is in front.
        for front in [Some(WindowKind::Ours), Some(WindowKind::Shell), None] {
            assert_eq!(tile_command(Command::Window, front), Command::Region, "{front:?}");
        }
        // The other tiles do not depend on what is in front.
        for cmd in [Command::Region, Command::FullScreen, Command::Record] {
            for front in [Some(WindowKind::Other), Some(WindowKind::Ours), Some(WindowKind::Shell), None] {
                assert_eq!(tile_command(cmd, front), cmd);
            }
        }
    }

    #[test]
    fn a_waiting_capture_can_be_dropped() {
        let t0 = Instant::now();
        let mut h = HideThenRun::default();
        h.start(Command::Region, t0);
        h.cancel();
        assert!(!h.is_waiting());
        assert_eq!(h.take_due(t0 + HIDE_BEFORE_CAPTURE * 2), None, "it never fires");
        assert_eq!(h.due_in(t0), None);
        // Dropping nothing is fine.
        h.cancel();
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
        // The token saved for the very address that is asked about.
        let connected_host = |server: &str, token: &TokenStatus| super::connected_host(server, token, crate::publish::check_server(server).ok().as_deref());
        let saved = TokenStatus::Saved;
        // A token saved for another address, or for none, is not a connection to this one.
        assert_eq!(super::connected_host("https://tours.example.co", &saved, Some("https://tours.example.com")), None);
        assert_eq!(super::connected_host("https://tours.example.com", &saved, None), None);
        assert_eq!(super::connected_host("https://tours.example.com", &saved, Some("https://tours.example.com")).as_deref(), Some("tours.example.com"));
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
    fn the_menu_names_the_four_pages_in_order() {
        assert_eq!(Page::ALL.map(Page::label), ["Home", "Tours", "Workspace", "Preferences"]);
    }

    #[test]
    fn leaving_workspace_is_noticed_once_however_it_is_left() {
        let mut nav = Nav::default();
        nav.request(Page::Workspace);
        assert!(!nav.take_left_workspace());
        nav.go(Page::Tours);
        assert!(nav.take_left_workspace() && !nav.take_left_workspace());
        nav.request(Page::Workspace);
        nav.close();
        assert!(nav.take_left_workspace());
        nav.request(Page::Workspace);
        nav.request(Page::Preferences);
        assert!(nav.take_left_workspace() && !nav.take_left_preferences());
        // Staying on it is not leaving it, and leaving another page is not leaving this one.
        nav.request(Page::Workspace);
        nav.take_left_preferences();
        nav.go(Page::Workspace);
        nav.request(Page::Workspace);
        assert!(!nav.take_left_workspace());
        nav.go(Page::Home);
        nav.take_left_workspace();
        nav.go(Page::Tours);
        nav.close();
        assert!(!nav.take_left_workspace());
    }

    #[test]
    fn the_output_tabs_link_switches_to_the_workspace_page() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        s.nav.request(Page::Preferences);
        s.prefs.state.tab = settings_ui::Tab::Output;
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIZE[0], 2000.0));
        // Find the link where it was drawn, then click it.
        fn find(shape: &egui::Shape, text: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == text => Some(t.pos + t.galley.size() / 2.0),
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, text)),
                _ => None,
            }
        }
        let frame = |s: &mut Shared, events: Vec<egui::Event>| {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| window_ui(ui, s));
            out.textures_delta.clear();
            out
        };
        frame(&mut s, vec![]);
        let out = frame(&mut s, vec![]);
        let text: Vec<String> = out.shapes.iter().filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        }).collect();
        assert!(text.iter().any(|t| t == settings_ui::TOUR_SITE_MOVED), "{text:?}");
        assert!(!text.iter().any(|t| t == "Test connection" || t == "API token" || t == "Server"), "the Output tab no longer holds the site settings: {text:?}");
        let at = out.shapes.iter().find_map(|c| find(&c.shape, settings_ui::OPEN_WORKSPACE)).expect("the link is drawn");
        let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        for events in [vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![]] {
            frame(&mut s, events);
        }
        assert_eq!(s.nav.page(), Page::Workspace);
        assert!(s.prefs.actions.iter().all(|a| !matches!(a, UiAction::OpenWorkspace)), "the app never sees the link");
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
        // The first frame says the folder is being read; the answer follows.
        let first = drawn(&ctx, &mut s);
        assert!(first.contains(home_ui::READING_RECENT) && !first.contains(home_ui::NOTHING_RECENT), "{first}");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut home = first;
        while home.contains(home_ui::READING_RECENT) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            home = drawn(&ctx, &mut s);
        }
        for word in ["Home", "Tours", "Workspace", "Preferences", "Not connected", "Capture", "Region", "Window", "Full screen", "Record", home_ui::RECORD_A_TOUR, "Recent", home_ui::NOTHING_RECENT] {
            assert!(home.lines().any(|l| l == word), "Home is missing {word:?} in:\n{home}");
        }
        // No token is saved in a test run, so Tours says it is not connected and asks nothing of any site.
        s.nav.go(Page::Tours);
        let tours = drawn(&ctx, &mut s);
        assert!(tours.lines().any(|l| l == tours_ui::RECORD_A_TOUR) && !tours.contains("Recent"), "{tours}");
        assert!(tours.contains(tours_ui::NOT_CONNECTED) || tours.contains(tours_ui::CHECKING), "{tours}");
        // Workspace, with nothing set up: the rows are there and no site is asked.
        s.nav.go(Page::Workspace);
        let workspace = drawn(&ctx, &mut s);
        for word in ["Site address", "Token", "Site version", "Test connection", "Open site"] {
            assert!(workspace.lines().any(|l| l == word), "Workspace is missing {word:?} in:\n{workspace}");
        }
        assert!(!workspace.contains("Recent"));
        s.nav.go(Page::Preferences);
        let prefs = drawn(&ctx, &mut s);
        for word in ["General", "Output", "Hotkeys", "Left-click on the tray icon", "Open Mythicshot", "Capture a region"] {
            assert!(prefs.lines().any(|l| l == word), "Preferences is missing {word:?} in:\n{prefs}");
        }
        assert!(!prefs.contains("settings file"), "nothing is said about a file that loaded");
        // A settings file that could not be read: the page says so, with the file's path.
        let problem = crate::settings::LoadProblem { path: "C:/cfg/settings.toml".into(), backup: None, partly_read: false };
        s.prefs.settings.load_problem = Some(problem.clone());
        let warned = drawn(&ctx, &mut s).replace('\n', " ");
        assert!(warned.contains("changes will not be saved") && warned.contains("settings.toml"), "{warned}");
    }

    #[test]
    fn leaving_workspace_wipes_an_unsaved_token_and_leaving_preferences_stops_a_shortcut_recording() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        for leave in [|n: &mut Nav| n.go(Page::Home), |n: &mut Nav| n.close(), |n: &mut Nav| n.request(Page::Tours)] {
            let mut s = shared(tmp.path());
            s.nav.request(Page::Workspace);
            s.workspace.type_token_for_test("dp_0123456789abcdef0123456789abcdef01234567");
            s.settle(&ctx);
            assert!(s.workspace.has_unsaved_token(), "nothing is dropped while the page is up");
            leave(&mut s.nav);
            s.settle(&ctx);
            assert!(!s.workspace.has_unsaved_token());
            assert!(s.prefs.actions.is_empty(), "no shortcut was being recorded there");

            let mut s = shared(tmp.path());
            s.nav.request(Page::Preferences);
            s.settle(&ctx);
            assert!(s.prefs.actions.is_empty());
            leave(&mut s.nav);
            s.settle(&ctx);
            assert!(matches!(s.prefs.actions.as_slice(), [UiAction::StopListening]));
            // Once is enough.
            s.settle(&ctx);
            assert_eq!(s.prefs.actions.len(), 1);
        }
    }

    #[test]
    fn a_tile_click_reaches_the_app_as_a_capture_to_run_after_hiding() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        s.nav.request(Page::Home);
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIZE[0], SIZE[1]));
        // The first tile sits just right of the menu, under the "Capture" heading.
        let at = egui::pos2(NAV_WIDTH + 60.0, 100.0);
        let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        for events in [vec![], vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![]] {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| window_ui(ui, &mut s));
            out.textures_delta.clear();
        }
        assert_eq!(s.capture, Some(Command::Region));
        // While something is being captured the tiles do nothing.
        s.capture = None;
        s.busy = true;
        for events in [vec![click(true)], vec![click(false)], vec![]] {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| window_ui(ui, &mut s));
            out.textures_delta.clear();
        }
        assert_eq!(s.capture, None);
    }
}
