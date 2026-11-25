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

use eframe::egui::{self, RichText};

use crate::theme;
use crate::home_ui::{self, HomeState};
use crate::menu_bar::{self, AppRequest, Entry};
use crate::open_image::{DropState, OpenImage};
use crate::platform::{WindowId, WindowKind};
use crate::project::ProjectKey;
use crate::publish_ui::{self, TokenStatus};
use crate::settings_ui::{self, UiAction};
use crate::tours_ui::{self, ToursState};
use crate::workspace_ui::{self, WorkspaceState};
use crate::tray::Command;


mod opening;

pub const TITLE: &str = "Mythicshot";
/// The size the window opens at when the user has not given it one, in points: every page shows
/// whole beside the menu except Preferences' Output and Hotkeys tabs, which scroll, and the
/// Workspace page of a connected profile has about 150 points free under its last row. Cut down
/// to fit a smaller work area, see [`opening`].
pub const SIZE: [f32; 2] = [920.0, 740.0];
pub(crate) const MIN_SIZE: [f32; 2] = [640.0, 420.0];
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

    /// The page's icon in the left menu.
    pub fn icon(self) -> crate::icons::Icon {
        match self {
            Page::Home => crate::icons::Icon::Home,
            Page::Tours => crate::icons::Icon::Tours,
            Page::Workspace => crate::icons::Icon::Workspace,
            Page::Preferences => crate::icons::Icon::Preferences,
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

/// What to open at start-up when the popup after an in-app update is to be shown, given what
/// [`start_request`] asked for. The relaunch after an install starts the app to the tray, where a
/// popup would be seen by nobody, so the window opens on Home to hold it; and whether it was
/// opened only for the popup, so that it closes with it.
pub fn notice_request(start: Option<Request>) -> (Request, bool) {
    match start {
        Some(request) => (request, false),
        None => (Request::Open, true),
    }
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
    /// Open this saved recording in an editor: Edit on the Tours page.
    pub edit_project: Option<ProjectKey>,
    /// Open a picture file in an editor: the Open image tile.
    pub open_image: Option<OpenImage>,
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
    /// The Workspace page's profiles: the dropdown, the add form and the list.
    pub profiles: crate::profiles_ui::Panel,
    /// A capture a Home tile asked for; the app hides the window, then runs it.
    pub capture: Option<Command>,
    /// A saved recording the Tours page asked to edit; the app opens it in an editor.
    pub edit_project: Option<ProjectKey>,
    /// A picture file to open, asked for by the Open image tile; the app opens it in an editor
    /// and the window stays.
    pub open_image: Option<OpenImage>,
    /// A drag of files over the window: what it holds, for the overlay.
    pub drop: DropState,
    /// A capture or recording is going on: the tiles are greyed out.
    pub busy: bool,
    /// What the menu bar asked of the app, in order; the app takes them on its next frame.
    pub app_requests: Vec<AppRequest>,
    /// Updates: the line under the menu bar and the Preferences section. The app hands in the
    /// real one at start and drives it each frame.
    pub update: crate::update_ui::Updater,
    /// The open window's title bar was told to be dark; a closed window is made anew, so it is told again.
    title_dark: bool,
    /// Where and how big the open window was opened, worked out once when it opened: the
    /// viewport builder must stay the same while it is open, or egui would move and size the
    /// window to it again.
    opening: Option<opening::Opening>,
    /// Watches the open window's size for a resize by hand.
    size_memory: opening::SizeMemory,
    /// A size the user gave the window by hand, for the app to save (as `main_window_size`).
    pub size_to_save: Option<[f32; 2]>,
    /// The work area of the monitor the window opens on, in points; tests give their own.
    work_area: fn() -> Option<egui::Rect>,
}

/// The primary monitor's work area in points, as Windows reports it now.
fn primary_work_area() -> Option<egui::Rect> {
    let (r, dpi) = crate::platform::primary_work_area()?;
    opening::work_area_points(r.x, r.y, r.w, r.h, dpi)
}

impl Shared {
    pub fn new(prefs: settings_ui::Shared) -> Self {
        Self { nav: Nav::default(), prefs, home: HomeState::default(), tours: ToursState::default(), workspace: WorkspaceState::default(), profiles: crate::profiles_ui::Panel::default(), capture: None, edit_project: None, open_image: None, drop: DropState::default(), busy: false, app_requests: Vec::new(), update: crate::update_ui::Updater::default(), title_dark: false, opening: None, size_memory: opening::SizeMemory::default(), size_to_save: None, work_area: primary_work_area }
    }

    /// Keep what a page asked of the window or the app in this frame. A frame that asks for
    /// nothing leaves an earlier request waiting for the app.
    fn take(&mut self, out: PageOut) {
        if let Some(page) = out.go_to {
            self.nav.go(page);
        }
        if out.capture.is_some() {
            self.capture = out.capture;
        }
        if out.edit_project.is_some() {
            self.edit_project = out.edit_project;
        }
        if out.open_image.is_some() {
            self.open_image = out.open_image;
        }
    }

    /// Show Preferences at `tab`, as the Preferences menu does: the page's own tab row would.
    /// A shortcut waiting for its keys on another tab stops waiting, as a click on the tab stops it.
    pub fn open_preferences(&mut self, tab: settings_ui::Tab) {
        if self.nav.page() == Page::Preferences && self.prefs.state.tab != tab {
            self.prefs.actions.push(UiAction::StopListening);
        }
        self.prefs.state.tab = tab;
        self.nav.go(Page::Preferences);
    }

    /// What the menu bar shows in this window: no editor, so nothing to save, undo or copy, and
    /// no Properties panel. Capture and record are greyed out like the Home tiles while busy.
    fn menu_state(&self) -> menu_bar::State {
        let settings = &self.prefs.settings;
        let keys = menu_bar::Keys::from(|a| settings.binding(a).to_string(), |a| settings.editor_binding(a).to_string());
        menu_bar::State { busy: self.busy, keys, ..Default::default() }
    }

    /// Run a menu entry clicked in this window.
    fn menu_entry(&mut self, entry: Entry, out: &mut PageOut) {
        match entry {
            // As the Home tiles do: the window goes, then the capture starts.
            Entry::CaptureRegion | Entry::RecordRegion if self.busy => {}
            Entry::CaptureRegion => out.capture = Some(Command::Region),
            Entry::RecordRegion => out.capture = Some(Command::Record),
            Entry::OpenTours => out.go_to = Some(Page::Tours),
            Entry::Workspace => out.go_to = Some(Page::Workspace),
            Entry::Preferences(tab) => self.open_preferences(tab),
            Entry::About => self.open_preferences(settings_ui::Tab::General),
            Entry::CloseWindow => self.close_window(),
            entry => {
                if let Some(request) = entry.app_request() {
                    self.app_requests.push(request);
                }
            }
        }
    }

    /// A request to show the window. One that comes while the popup after an update is up means
    /// the person wants the window: it stays when the popup closes.
    pub fn apply_request(&mut self, request: Request) {
        if self.update.notice().is_some() {
            self.update.keep_window();
        }
        self.nav.apply(request);
    }

    /// At start: take the note an in-app update left at `note` (read and removed, so its popup
    /// shows once) and, when it means a popup, open the window for it now, on the page `start`
    /// asked for or on Home ([`notice_request`]). The request left for the first frame (none once
    /// the window is open for the popup), and what the note meant, for the log.
    pub fn start_with_note(&mut self, start: Option<Request>, note: &std::path::Path, now: std::time::SystemTime, running: crate::update::Version) -> (Option<Request>, Option<crate::update::pending::AfterUpdate>) {
        let Some(after) = crate::update::pending::take(note, now, running) else { return (start, None) };
        let (request, opened) = notice_request(start);
        // Opened here, not as a request on the first frame: a request while the popup is up
        // would keep the window after it.
        self.nav.apply(request);
        self.update.set_notice(after, opened);
        (None, Some(after))
    }

    /// Close the window, as its close button does: the popup after an update goes with it.
    pub fn close_window(&mut self) {
        self.nav.close();
        self.update.dismiss_notice();
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
        // A closed window: a size made by hand that had not settled yet is saved now, and the
        // next opening is worked out afresh.
        if !self.nav.is_open() && self.size_memory.is_open() {
            if let Some(size) = self.size_memory.close() {
                self.size_to_save = Some([size.x, size.y]);
            }
            self.opening = None;
        }
    }

    /// The opening of the window that is opening now: the saved size, or the default, fitted to
    /// the work area. Worked out once per opening.
    fn opening(&mut self) -> opening::Opening {
        if let Some(o) = self.opening.filter(|_| self.size_memory.is_open()) {
            return o;
        }
        let o = opening::opening(self.prefs.settings.main_window_size, (self.work_area)());
        self.opening = Some(o);
        self.size_memory.open(o.size);
        o
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

/// The host name for the "Signed in to" line: the saved server's, when a token is saved too.
/// Nothing is asked of the server; this only says what is set up.
/// `bound` is the address the token was saved for: with another address set, the token is not
/// used there, so that is not a connection. Nor is a token the site refused the last time it
/// was sent (`refused`, [`publish_ui::token_refused`]): it was revoked on the site.
pub fn connected_host(server: &str, token: &TokenStatus, bound: Option<&str>, refused: bool) -> Option<String> {
    if *token != TokenStatus::Saved || refused || publish_ui::binding_problem(bound, server).is_some() {
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
    let (dark_title, opening) = {
        let mut s = shared.lock().expect("main window state");
        if let Some(request) = request {
            s.apply_request(request);
        }
        s.settle(ctx);
        if !s.nav.is_open() {
            s.title_dark = false;
            return;
        }
        (!std::mem::replace(&mut s.title_dark, true), s.opening())
    };
    let builder = builder(icon, opening);
    let shared = shared.clone();
    ctx.show_viewport_deferred(viewport_id(), builder, move |ui, _class| {
        let mut guard = shared.lock().expect("main window state");
        let s = &mut *guard;
        viewport_ui(ui, s);
        // The app applies what was queued on its own frame: wake it instead of waiting for its slow tick.
        if !s.nav.is_open() || s.capture.is_some() || s.edit_project.is_some() || s.open_image.is_some() || !s.app_requests.is_empty() || !s.update.asks.is_empty() || !s.prefs.actions.is_empty() || s.tours.unfinished().is_changed() || publish_ui::has_token_site_change() || publish_ui::has_book_change() || s.size_to_save.is_some() {
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    });
    if dark_title {
        theme::dark_title_bar(ctx, viewport_id());
    }
    ctx.request_repaint_of(viewport_id());
}

/// One frame of the window's viewport: brought to the front when asked, closed by its close
/// button, and its contents drawn.
fn viewport_ui(ui: &mut egui::Ui, s: &mut Shared) {
    if s.nav.take_focus() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
    }
    if ui.ctx().input(|i| i.viewport().close_requested()) {
        // Only the window goes; the app stays in the tray.
        s.close_window();
    }
    window_ui(ui, s);
    watch_size(ui.ctx(), s);
    s.settle(ui.ctx());
}

/// A size the user gave the window by hand is queued for the app to save once it has settled.
fn watch_size(ctx: &egui::Context, s: &mut Shared) {
    if !s.nav.is_open() {
        return;
    }
    let (seen, now) = ctx.input(|i| (opening::Seen::from_info(i.viewport()), i.time));
    if let Some(size) = s.size_memory.observe(seen, now) {
        s.size_to_save = Some([size.x, size.y]);
    }
    if s.size_memory.is_waiting() {
        // Drawn again when it may have settled, even with nothing else going on.
        ctx.request_repaint_after(Duration::from_secs_f64(opening::SETTLE / 2.0));
    }
}

/// The window's viewport. It takes files dropped on it (a picture opens in the editor). Windows
/// lets every window of the app take drops already, as the platform default; it is said here so
/// this window does not depend on that, and no other window is changed.
/// It opens at `opening`'s size and place, never maximized.
fn builder(icon: Arc<egui::IconData>, opening: opening::Opening) -> egui::ViewportBuilder {
    let b = egui::ViewportBuilder::default().with_title(TITLE).with_inner_size(opening.size).with_min_inner_size(MIN_SIZE).with_icon(icon).with_drag_and_drop(true);
    match opening.pos {
        Some(pos) => b.with_position(pos),
        None => b,
    }
}

/// The window's contents: the menu on the left, the selected page beside it.
pub fn window_ui(ui: &mut egui::Ui, s: &mut Shared) {
    window_ui_with(ui, s, &tours_ui::Conn::current());
}

/// [`window_ui`] with the Tours page's site connection given: tests draw the window this way.
fn window_ui_with(ui: &mut egui::Ui, s: &mut Shared, tours_conn: &tours_ui::Conn) {
    window_ui_given(ui, s, tours_conn, None);
}

/// The active profile and its connection, as a test gives them to the Workspace page.
type WorkspaceGiven = (Option<crate::profiles::Profile>, workspace_ui::Conn);

/// [`window_ui_with`] with the Workspace page's active profile and connection given too.
fn window_ui_given(ui: &mut egui::Ui, s: &mut Shared, tours_conn: &tours_ui::Conn, workspace: Option<&WorkspaceGiven>) {
    theme::window_background(ui);
    let mut out = PageOut::default();
    let menu = s.menu_state();
    let bar = menu_bar::bar(ui, "main-menu-bar", &menu);
    if let Some(entry) = bar.picked {
        s.menu_entry(entry, &mut out);
    }
    // A newer version, its download or its install: one line under the menu bar, on every page.
    if s.update.banner().is_some() {
        egui::Panel::top("update-line").resizable(false).frame(egui::Frame::new()).show(ui, |ui| crate::update_ui::banner_ui(ui, &mut s.update));
    }
    egui::Panel::left("main-nav").resizable(false).exact_size(NAV_WIDTH).frame(egui::Frame::side_top_panel(ui.style()).fill(theme::SURFACE)).show(ui, |ui| nav_ui(ui, s.nav.page(), workspace, &mut out));
    egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(0)).show(ui, |ui| theme::page(ui, |ui| page_ui(ui, s, tours_conn, workspace, &mut out)));
    // A picture file dragged over the window, on any page: the overlay over the window below
    // its menu bar while it is there, and the file opened when it is dropped.
    let mut below_bar = ui.ctx().content_rect();
    below_bar.min.y = bar.rect.bottom();
    if let Some(path) = s.drop.frame_from_input(ui.ctx(), below_bar) {
        out.open_image = Some(OpenImage::Dropped(path));
    }
    s.take(out);
    // The popup at the start after an in-app update, over everything else in the window.
    if crate::update_ui::notice_ui(ui.ctx(), &mut s.update) {
        s.nav.close();
    }
}

/// The lines at the foot of the left menu: "Signed in to" and the host when this computer holds
/// a saved connection for the active address (no site is asked), else "Not connected".
pub fn nav_footer(host: Option<&str>) -> Vec<String> {
    match host {
        Some(host) => vec![SIGNED_IN_TO.to_string(), host.to_string()],
        None => vec![workspace_ui::NOT_CONNECTED.to_string()],
    }
}

/// What the foot of the left menu says above the host of a saved connection.
pub const SIGNED_IN_TO: &str = "Signed in to";

fn nav_ui(ui: &mut egui::Ui, current: Page, given: Option<&WorkspaceGiven>, out: &mut PageOut) {
    theme::roomy(ui);
    ui.add_space(theme::ITEM_GAP);
    ui.spacing_mut().item_spacing.y = theme::GRID;
    for page in Page::ALL {
        if theme::nav_item(ui, page.label(), Some(page.icon()), current == page).clicked() {
            out.go_to = Some(page);
        }
    }
    // Whether a site is set up, at the bottom of the menu.
    let host = match given {
        // A test's own profile and connection.
        Some((active, conn)) => active.as_ref().and_then(|a| connected_host(&a.server, &conn.token, conn.bound.as_deref(), conn.refused)),
        None => {
            let token = publish_ui::token_status();
            if token == TokenStatus::Checking {
                ui.ctx().request_repaint_after(Duration::from_millis(200));
            }
            connected_host(&publish_ui::server(), &token, publish_ui::token_site().as_deref(), publish_ui::token_refused())
        }
    };
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.add_space(theme::ITEM_GAP);
        // Laid out from the bottom: the last line first.
        let lines = nav_footer(host.as_deref());
        for (i, line) in lines.iter().enumerate().rev() {
            let text = RichText::new(line).size(theme::SMALL_SIZE).color(theme::MUTED);
            if i + 1 == lines.len() && lines.len() > 1 {
                // The host: cut short with "…" when it does not fit.
                ui.add(egui::Label::new(text).truncate());
            } else {
                ui.label(text);
            }
        }
    });
}

/// Draw the selected page. Each page gets the part of [`Shared`] it owns and a [`PageOut`].
fn page_ui(ui: &mut egui::Ui, s: &mut Shared, tours_conn: &tours_ui::Conn, workspace: Option<&WorkspaceGiven>, out: &mut PageOut) {
    match s.nav.page() {
        Page::Home => home_ui::ui(ui, &mut s.home, &s.prefs.settings, s.busy, out),
        Page::Tours => tours_ui::ui(ui, &mut s.tours, tours_conn, out),
        Page::Workspace => {
            // The profiles first: the title, the active profile's card, the notices, the add
            // form or the list. The cards below are the active profile's, at its own address.
            let request = publish_ui::take_profiles_request();
            let (active, conn) = workspace.cloned().unwrap_or_else(|| (publish_ui::book().active().cloned(), workspace_ui::Conn::current()));
            workspace_page(ui, &mut s.profiles, &mut s.workspace, request, active, &conn, out);
        }
        Page::Preferences => {
            let p = &mut s.prefs;
            let view = settings_ui::View { settings: &mut p.settings, errors: &p.errors, snipping_tool: p.snipping_tool, autostart: p.autostart, updates: Some(&mut s.update) };
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

/// The Workspace page: the profiles' part (the title, the active profile's card, the notices,
/// the add form or the list), then the cards of the active profile, `active`, at its own address.
fn workspace_page(ui: &mut egui::Ui, profiles: &mut crate::profiles_ui::Panel, workspace: &mut WorkspaceState, request: Option<publish_ui::ProfilesRequest>, active: Option<crate::profiles::Profile>, conn: &workspace_ui::Conn, out: &mut PageOut) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let server = active.as_ref().map(|a| a.server.clone()).unwrap_or_default();
        let below = profiles.ui(ui, request, &mut || workspace_ui::status_pill(workspace, &server, conn));
        if below == crate::profiles_ui::Below::Details
            && let Some(active) = active
        {
            workspace.profile_mode = true;
            workspace.profile_account = active.account.clone();
            workspace.profile_workspace = active.workspace.clone();
            let mut address = active.server;
            if workspace_ui::ui(ui, workspace, &mut address, conn).open_tours {
                out.go_to = Some(Page::Tours);
            }
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::settings::Settings;

    mod remember;
    mod roomy;

    #[test]
    fn an_edit_request_reaches_the_app_once() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        s.nav.request(Page::Tours);
        let key = ProjectKey { server: "https://tours.example.com".into(), tour_id: "tour_aaaaaaaaaaaaaaaaaaaaaaaa".into(), chapter_id: "ch_aaaaaaaaaaaaaaaaaaaaaaaa".into() };
        assert_eq!(s.edit_project, None);
        // The Tours page asks in one frame.
        s.take(PageOut { edit_project: Some(key.clone()), ..Default::default() });
        assert_eq!(s.edit_project, Some(key.clone()));
        assert_eq!((s.nav.page(), s.capture), (Page::Tours, None), "nothing else is asked for, and the window stays on its page");
        // Frames that ask for nothing do not take it away before the app has seen it.
        s.take(PageOut::default());
        let ctx = crate::theme::test_context();
        drawn(&ctx, &mut s);
        assert_eq!(s.edit_project, Some(key.clone()));
        // The app takes it, as it takes a capture: once.
        assert_eq!(s.edit_project.take(), Some(key));
        s.take(PageOut::default());
        drawn(&ctx, &mut s);
        assert_eq!(s.edit_project.take(), None);
    }

    #[test]
    fn an_edit_request_still_reaches_the_app_once() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        s.nav.request(Page::Tours);
        let key = |tour: &str| ProjectKey { server: "https://tours.example.com".into(), tour_id: tour.into(), chapter_id: "ch_aaaaaaaaaaaaaaaaaaaaaaaa".into() };
        let (first, second) = (key("tour_aaaaaaaaaaaaaaaaaaaaaaaa"), key("tour_bbbbbbbbbbbbbbbbbbbbbbbb"));
        let ctx = crate::theme::test_context();
        // The app took a request and is opening that recording on its own thread: the window
        // goes on being drawn, and the request is not asked a second time.
        s.take(PageOut { edit_project: Some(first.clone()), ..Default::default() });
        assert_eq!(s.edit_project.take(), Some(first.clone()));
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(s.tours.begin_open(&first, cancel.clone()));
        s.tours.open_rows().progress(&first, 1, 3);
        drawn(&ctx, &mut s);
        assert_eq!(s.edit_project, None);
        // Edit on another tour meanwhile: that request reaches the app, once, like any other.
        s.take(PageOut { edit_project: Some(second.clone()), ..Default::default() });
        s.take(PageOut::default());
        drawn(&ctx, &mut s);
        assert_eq!(s.edit_project.take(), Some(second.clone()));
        drawn(&ctx, &mut s);
        assert_eq!(s.edit_project.take(), None);
        // The page, not the window, says that only one recording opens at a time.
        assert!(!s.tours.begin_open(&second, Arc::default()));
        assert!(!cancel.load(Ordering::SeqCst), "drawing and asking stopped nothing");
        s.tours.open_ended(&first);
        assert!(s.tours.begin_open(&second, Arc::default()));
        assert_eq!((s.nav.page(), s.capture), (Page::Tours, None));
    }

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
        let ctx = crate::theme::test_context();
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
        let connected_host = |server: &str, token: &TokenStatus| super::connected_host(server, token, crate::publish::check_server(server).ok().as_deref(), false);
        let saved = TokenStatus::Saved;
        // A token saved for another address, or for none, is not a connection to this one.
        assert_eq!(super::connected_host("https://tours.example.co", &saved, Some("https://tours.example.com"), false), None);
        assert_eq!(super::connected_host("https://tours.example.com", &saved, None, false), None);
        assert_eq!(super::connected_host("https://tours.example.com", &saved, Some("https://tours.example.com"), false).as_deref(), Some("tours.example.com"));
        // A token the site refused (revoked there) is not a connection either.
        assert_eq!(super::connected_host("https://tours.example.com", &saved, Some("https://tours.example.com"), true), None);
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

    /// One frame of the window at `size` with `events`, its textures dropped.
    fn window_frame(ctx: &egui::Context, s: &mut Shared, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), time: Some(ctx.input(|i| i.time) + 0.5), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| window_ui(ui, s));
        out.textures_delta.clear();
        out
    }

    #[test]
    fn the_menu_bar_runs_along_the_top_with_file_edit_preferences_and_help() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        s.nav.request(Page::Home);
        let ctx = crate::theme::test_context();
        let size = egui::vec2(SIZE[0], SIZE[1]);
        window_frame(&ctx, &mut s, size, vec![]);
        let out = window_frame(&ctx, &mut s, size, vec![]);
        let at: Vec<egui::Rect> = menu_bar::TITLES.iter().map(|t| crate::share_ui::tests::text_rect(&out, t).unwrap_or_else(|| panic!("{t}"))).collect();
        assert!(at.windows(2).all(|w| w[0].right() < w[1].left() && (w[0].center().y - w[1].center().y).abs() < 0.5), "left to right on one line: {at:?}");
        assert!(at[0].left() < NAV_WIDTH / 2.0, "from the left edge, above the left menu: {at:?}");
        // Above everything else: the left menu's first page is below it.
        let home = crate::share_ui::tests::text_rect(&out, "Home").unwrap();
        assert!(at.iter().all(|r| r.bottom() < home.top()), "{at:?} above {home:?}");
    }

    #[test]
    fn the_main_windows_menus_offer_no_editing_and_no_properties() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        let menus = menu_bar::menus(&s.menu_state());
        assert_eq!(crate::menu_bar::tests::labels(&menus[1]), vec![vec!["Undo", "Redo"], vec!["Copy", "Delete"]], "no Properties panel here");
        for e in [Entry::Save, Entry::Export, Entry::PublishTour, Entry::Undo, Entry::Redo, Entry::Copy, Entry::Delete] {
            assert!(!crate::menu_bar::tests::find(&menus, e).enabled, "{e:?} is disabled in the main window");
        }
        for e in [Entry::CaptureRegion, Entry::RecordRegion, Entry::OpenTours, Entry::CloseWindow, Entry::Quit] {
            assert!(crate::menu_bar::tests::find(&menus, e).enabled, "{e:?}");
        }
        // The shortcuts are the ones Preferences > Hotkeys shows.
        assert_eq!(crate::menu_bar::tests::find(&menus, Entry::CaptureRegion).shortcut, "PrtSc");
        assert_eq!(crate::menu_bar::tests::find(&menus, Entry::RecordRegion).shortcut, "Shift+PrtSc");
        s.prefs.settings.hotkeys.insert(crate::hotkeys::Action::CaptureRegion, "control+shift+KeyA".into());
        assert_eq!(crate::menu_bar::tests::find(&menu_bar::menus(&s.menu_state()), Entry::CaptureRegion).shortcut, "Ctrl+Shift+A");
        // While something is being captured, capture and record are greyed out like the Home tiles.
        s.busy = true;
        let busy = menu_bar::menus(&s.menu_state());
        assert!(!crate::menu_bar::tests::find(&busy, Entry::CaptureRegion).enabled && !crate::menu_bar::tests::find(&busy, Entry::RecordRegion).enabled);
    }

    #[test]
    fn each_menu_entry_does_what_the_windows_own_controls_do() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = egui::vec2(SIZE[0], SIZE[1]);
        let fresh = |page: Page| {
            let mut s = shared(tmp.path());
            s.nav.request(page);
            s.nav.take_focus();
            s
        };
        let choose = |s: &mut Shared, menu: &str, entry: &str| crate::menu_bar::tests::choose(&mut |events| window_frame(&ctx, s, size, events), menu, entry);

        // Preferences: the page, at the section chosen; a section is a tab of the page.
        for tab in settings_ui::Tab::ALL {
            let mut s = fresh(Page::Home);
            choose(&mut s, "Preferences", tab.label());
            assert_eq!((s.nav.page(), s.prefs.state.tab), (Page::Preferences, tab));
            assert!(s.nav.is_open() && !s.nav.take_focus(), "the same window, no focus change");
        }
        // From one tab to another stops a shortcut being recorded, as a click on the tab does.
        let mut s = fresh(Page::Preferences);
        s.prefs.state.tab = settings_ui::Tab::Hotkeys;
        choose(&mut s, "Preferences", "General");
        assert!(matches!(s.prefs.actions.as_slice(), [UiAction::StopListening]), "a shortcut being recorded stops");
        // The Workspace page, Tours, and About on Preferences > General, where the version is.
        let mut s = fresh(Page::Home);
        choose(&mut s, "Preferences", "Workspace");
        assert_eq!(s.nav.page(), Page::Workspace);
        choose(&mut s, "File", "Open tours");
        assert_eq!(s.nav.page(), Page::Tours);
        s.prefs.state.tab = settings_ui::Tab::Output;
        choose(&mut s, "Help", "About Mythicshot");
        assert_eq!((s.nav.page(), s.prefs.state.tab), (Page::Preferences, settings_ui::Tab::General));
        let version = window_frame(&ctx, &mut s, size, vec![]);
        // The Updates section's status line starts with it.
        let status = s.update.status_line(chrono::Local::now().naive_local());
        assert!(status.starts_with(concat!("Version ", env!("CARGO_PKG_VERSION"), ".")), "{status}");
        assert!(crate::share_ui::tests::text_rect(&version, &status).is_some(), "the version is on the page");

        // Capture region and Record region: the window goes, then the capture, as from the Home tiles.
        for (entry, cmd) in [("Capture region", Command::Region), ("Record region", Command::Record)] {
            let mut s = fresh(Page::Tours);
            choose(&mut s, "File", entry);
            assert_eq!(s.capture, Some(cmd), "{entry}");
            assert!(s.app_requests.is_empty());
            // Not while something is being captured.
            let mut s = fresh(Page::Home);
            s.busy = true;
            choose(&mut s, "File", entry);
            assert_eq!(s.capture, None, "{entry} while busy");
        }
        // Close window closes only the window; Quit and the help go to the app, in order.
        let mut s = fresh(Page::Home);
        for (menu, entry) in [("File", "Quit Mythicshot"), ("Help", "Troubleshooting"), ("Help", "Open the log folder"), ("Help", "Download page")] {
            choose(&mut s, menu, entry);
        }
        assert_eq!(s.app_requests, vec![AppRequest::Run(Command::Exit), AppRequest::Troubleshooting, AppRequest::LogFolder, AppRequest::DownloadPage]);
        assert!(s.nav.is_open());
        choose(&mut s, "File", "Close window");
        assert!(!s.nav.is_open());
        // The editing entries do nothing here.
        let mut s = fresh(Page::Home);
        for entry in ["Undo", "Redo", "Copy", "Delete"] {
            choose(&mut s, "Edit", entry);
        }
        assert!(s.app_requests.is_empty() && s.capture.is_none() && s.nav.is_open() && s.nav.page() == Page::Home);
    }

    /// The menu bar and every open menu fit the window at its smallest size, at 100%, 125% and
    /// 200%: no label sticks out of its button or row or leaves the window, and nothing of the
    /// window lets the desktop through.
    #[test]
    fn the_menus_fit_the_smallest_window_at_every_scale() {
        let tmp = tempfile::tempdir().unwrap();
        let size = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]);
        for ppp in [1.0, 1.25, 2.0] {
            for menu in menu_bar::TITLES {
                let ctx = crate::theme::test_context();
                ctx.set_pixels_per_point(ppp);
                let mut s = shared(tmp.path());
                s.nav.request(Page::Home);
                let out = crate::menu_bar::tests::open_menu(&mut |events| window_frame(&ctx, &mut s, size, events), menu);
                let what = format!("{menu} at {ppp}");
                let shown = menu_bar::menus(&s.menu_state()).into_iter().find(|m| m.title == menu).unwrap();
                crate::menu_bar::tests::assert_menu_fits(&out, &shown, egui::Rect::from_min_size(egui::Pos2::ZERO, size), &what);
            }
            // With the menus closed, every label of the window fits, the bar's titles too.
            let ctx = crate::theme::test_context();
            ctx.set_pixels_per_point(ppp);
            let mut s = shared(tmp.path());
            s.nav.request(Page::Home);
            window_frame(&ctx, &mut s, size, vec![]);
            let out = window_frame(&ctx, &mut s, size, vec![]);
            assert!(crate::theme::checks::assert_texts_fit(&out, &format!("the window at {ppp}")) >= 4);
            crate::menu_bar::tests::assert_inside(&out, egui::Rect::from_min_size(egui::Pos2::ZERO, size), &format!("the window at {ppp}"));
            assert_eq!(crate::snapshot::uncovered(&crate::theme::test_context(), size, ppp, 3, |ui| window_ui(ui, &mut s)), 0, "at {ppp}");
        }
    }

    #[test]
    fn a_popup_after_an_update_opens_the_window_when_the_app_starts_to_the_tray() {
        // The relaunch after an install starts the app with no argument: to the tray.
        assert_eq!(start_request(["mythicshot.exe".to_string()]), None);
        assert_eq!(notice_request(None), (Request::Open, true), "opened for the popup, on Home");
        // Asked for Preferences anyway: that page, and the window stays after the popup.
        assert_eq!(notice_request(Some(Request::Page(Page::Preferences))), (Request::Page(Page::Preferences), false));
        let mut nav = Nav::default();
        nav.apply(notice_request(None).0);
        assert!(nav.is_open() && nav.take_focus(), "it comes to the front");
        assert_eq!(nav.page(), Page::Home);
    }

    #[test]
    fn the_popup_stands_over_the_window_and_ok_closes_a_window_opened_for_it() {
        use crate::update::pending::AfterUpdate;
        let v = |s: &str| crate::update::Version::parse(s).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = egui::vec2(SIZE[0], SIZE[1]);
        let enter = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        for opened_for_it in [true, false] {
            let mut s = shared(tmp.path());
            let (request, _) = notice_request(None);
            s.nav.apply(request);
            s.update.set_notice(AfterUpdate::Updated { from: v("0.2.38"), to: v("0.2.39") }, opened_for_it);
            window_frame(&ctx, &mut s, size, vec![]);
            let out = window_frame(&ctx, &mut s, size, vec![]);
            assert!(crate::share_ui::tests::text_rect(&out, crate::update_ui::UPDATED_TITLE).is_some(), "the popup is in the window");
            window_frame(&ctx, &mut s, size, vec![enter.clone()]);
            assert_eq!(s.update.notice(), None);
            assert_eq!(s.nav.is_open(), !opened_for_it, "opened for it: {opened_for_it}");
        }
        // Closed by hand with the popup up, through the window's own close button: the popup
        // goes too, and does not come back on the next open.
        let mut s = shared(tmp.path());
        s.nav.apply(Request::Open);
        s.update.set_notice(AfterUpdate::NotFinished { from: v("0.2.38") }, true);
        let mut input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() };
        input.viewports.insert(egui::ViewportId::ROOT, egui::ViewportInfo { events: vec![egui::ViewportEvent::Close], ..Default::default() });
        let mut out = ctx.run_ui(input, |ui| viewport_ui(ui, &mut s));
        out.textures_delta.clear();
        assert!(!s.nav.is_open());
        assert_eq!(s.update.notice(), None);
        s.apply_request(Request::Open);
        let out = window_frame(&ctx, &mut s, size, vec![]);
        assert!(crate::share_ui::tests::text_rect(&out, crate::update_ui::NOT_FINISHED_TITLE).is_none());
    }

    #[test]
    fn a_window_asked_for_while_the_popup_is_up_stays_after_ok() {
        use crate::update::pending::AfterUpdate;
        let v = |s: &str| crate::update::Version::parse(s).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = egui::vec2(SIZE[0], SIZE[1]);
        let enter = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        // The tray's "Open Mythicshot", a hotkey's Preferences, or the tray's update entry.
        for request in [Request::Open, Request::Page(Page::Preferences)] {
            let mut s = shared(tmp.path());
            s.nav.apply(notice_request(None).0);
            s.update.set_notice(AfterUpdate::Updated { from: v("0.2.38"), to: v("0.2.39") }, true);
            s.apply_request(request);
            window_frame(&ctx, &mut s, size, vec![]);
            window_frame(&ctx, &mut s, size, vec![enter.clone()]);
            assert_eq!(s.update.notice(), None, "{request:?}");
            assert!(s.nav.is_open(), "{request:?}: the window the person asked for stays");
        }
        // Without a popup a request is only a request.
        let mut s = shared(tmp.path());
        s.apply_request(Request::Page(Page::Tours));
        assert!(s.nav.is_open() && s.nav.page() == Page::Tours);
    }

    #[test]
    fn close_window_in_the_menu_takes_the_popup_with_it() {
        use crate::update::pending::AfterUpdate;
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        s.nav.apply(Request::Open);
        s.update.set_notice(AfterUpdate::NotFinished { from: crate::update::Version::parse("0.2.38").unwrap() }, false);
        let mut out = PageOut::default();
        s.menu_entry(Entry::CloseWindow, &mut out);
        assert!(!s.nav.is_open());
        assert_eq!(s.update.notice(), None, "never left behind for the next open");
    }

    #[test]
    fn the_start_takes_the_note_and_opens_the_window_for_the_popup() {
        use crate::update::pending::{self, AfterUpdate, Pending};
        let v = |s: &str| crate::update::Version::parse(s).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let note = tmp.path().join(pending::FILE_NAME);
        let now = std::time::SystemTime::now();
        // Started to the tray after an update that worked.
        pending::write(&note, Pending { from: v("0.2.38"), to: v("0.2.39") }).unwrap();
        let mut s = shared(tmp.path());
        let (request, after) = s.start_with_note(None, &note, now, v("0.2.39"));
        assert_eq!(after, Some(AfterUpdate::Updated { from: v("0.2.38"), to: v("0.2.39") }));
        assert_eq!(request, None, "nothing left for the first frame to ask");
        assert!(s.nav.is_open() && s.nav.page() == Page::Home && s.nav.take_focus(), "open on Home, in front");
        assert_eq!(s.update.notice(), after);
        assert!(!note.exists(), "the note is gone");
        // OK closes the window that was opened for it.
        assert!(s.update.press_notice(crate::update_ui::NoticeButton::Ok));
        // Started with --settings after an update that did not finish: that page, kept after OK.
        pending::write(&note, Pending { from: v("0.2.38"), to: v("0.2.39") }).unwrap();
        let mut s = shared(tmp.path());
        let (request, after) = s.start_with_note(start_request(["mythicshot.exe".to_string(), "--settings".to_string()]), &note, now, v("0.2.38"));
        assert_eq!(after, Some(AfterUpdate::NotFinished { from: v("0.2.38") }));
        assert_eq!(request, None);
        assert!(s.nav.is_open() && s.nav.page() == Page::Preferences);
        assert!(!s.update.press_notice(crate::update_ui::NoticeButton::Ok));
        // No note: the start is as before.
        let mut s = shared(tmp.path());
        assert_eq!(s.start_with_note(None, &note, now, v("0.2.39")), (None, None));
        assert!(!s.nav.is_open() && s.update.notice().is_none());
        let args = start_request(["mythicshot.exe".to_string(), "--settings".to_string()]);
        assert_eq!(s.start_with_note(args, &note, now, v("0.2.39")), (args, None));
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
        let ctx = crate::theme::test_context();
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

    /// Every page, and every tab of Preferences, paints the whole window opaque in the theme's
    /// colours: the menu bar and the menu on surface, the page on ground.
    #[test]
    fn every_page_paints_the_whole_window_opaque() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shared(tmp.path());
        let ctx = crate::theme::test_context();
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIZE[0], SIZE[1]));
        let input = || egui::RawInput { screen_rect: Some(window), ..Default::default() };
        let mut pages: Vec<(Page, Option<settings_ui::Tab>)> = Page::ALL.iter().map(|p| (*p, None)).collect();
        pages.extend([settings_ui::Tab::General, settings_ui::Tab::Output, settings_ui::Tab::Hotkeys].map(|t| (Page::Preferences, Some(t))));
        for (page, tab) in pages {
            s.nav.request(page);
            if let Some(tab) = tab {
                s.prefs.state.tab = tab;
            }
            for _ in 0..2 {
                ctx.run_ui(input(), |ui| window_ui(ui, &mut s)).textures_delta.clear();
            }
            let mut out = ctx.run_ui(input(), |ui| window_ui(ui, &mut s));
            out.textures_delta.clear();
            crate::theme::checks::assert_window_opaque(&out, window, &format!("{page:?} {tab:?}"));
            crate::theme::checks::assert_texts_fit(&out, &format!("{page:?} {tab:?}"));
            let fills = crate::theme::checks::fills(&out);
            // The menu bar across the top on surface; under it, from there to the bottom, the
            // left menu on surface and the page on ground.
            let bar = fills.iter().find(|(r, c, _)| *c == theme::SURFACE && r.top() <= window.top() + 0.5 && r.width() >= window.width() - 1.0 && r.height() < 40.0).unwrap_or_else(|| panic!("{page:?}: the menu bar on surface"));
            let below = window.height() - bar.0.height();
            assert!(fills.iter().any(|(r, c, _)| *c == theme::SURFACE && r.height() >= below - 1.0 && r.bottom() >= window.bottom() - 0.5 && r.width() < window.width() / 2.0), "{page:?}: the menu on surface");
            assert!(fills.iter().any(|(r, c, _)| *c == theme::GROUND && r.height() >= below - 1.0 && r.bottom() >= window.bottom() - 0.5), "{page:?}: the page on ground");
        }
        // Filled in pixel by pixel at 125% scaling: not one pixel lets the desktop through, not
        // even at the seam where the menu meets the page.
        for page in Page::ALL {
            s.nav.request(page);
            let uncovered = crate::snapshot::uncovered(&crate::theme::test_context(), window.size(), 1.25, 3, |ui| window_ui(ui, &mut s));
            assert_eq!(uncovered, 0, "{page:?}: pixels the desktop shows through");
        }
    }

    /// How many times one frame told `id`'s window to draw its title bar dark.
    pub(crate) fn dark_title_commands(out: &egui::FullOutput, id: egui::ViewportId) -> usize {
        out.viewport_output.get(&id).map_or(0, |v| v.commands.iter().filter(|c| **c == egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark)).count())
    }

    /// On light-mode Windows the title bar followed Windows and sat light over the dark app. The
    /// window is told once, when it opens, to draw it dark; reopened, it is a new window and is
    /// told again.
    #[test]
    fn the_window_is_told_once_to_draw_its_title_bar_dark() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(shared(tmp.path())));
        let ctx = crate::theme::test_context();
        // As in the app: each viewport is a window of its own.
        ctx.set_embed_viewports(false);
        let icon = Arc::new(egui::IconData::default());
        let frame = |request: Option<Request>| {
            let mut out = ctx.run_ui(egui::RawInput { system_theme: Some(egui::Theme::Light), ..Default::default() }, |ui| show(ui.ctx(), &shared, icon.clone(), request));
            out.textures_delta.clear();
            dark_title_commands(&out, viewport_id())
        };
        assert_eq!(frame(None), 0, "a closed window is told nothing");
        assert_eq!(frame(Some(Request::Open)), 1, "the window opens");
        assert_eq!(frame(None), 0);
        assert_eq!(frame(Some(Request::Page(Page::Tours))), 0, "an open window is not told again");
        shared.lock().unwrap().nav.close();
        assert_eq!(frame(None), 0);
        assert_eq!(frame(Some(Request::Open)), 1, "reopened, it is a new window");
    }

    /// Pictures of the window's pages for the owner, drawn without a window (see `snapshot`).
    /// The Tours page shows three tours from a model filled by hand; nothing is asked of any site.
    #[test]
    #[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
    fn snapshot_the_main_window_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let size = egui::vec2(SIZE[0], SIZE[1]);
        let mut shots: Vec<(&str, Page, Option<settings_ui::Tab>)> = vec![("main-home", Page::Home, None), ("main-workspace", Page::Workspace, None)];
        shots.extend([("settings-general", settings_ui::Tab::General), ("settings-output", settings_ui::Tab::Output), ("settings-hotkeys", settings_ui::Tab::Hotkeys)].map(|(n, t)| (n, Page::Preferences, Some(t))));
        for (name, page, tab) in shots {
            let mut s = shared(tmp.path());
            s.nav.request(page);
            if let Some(tab) = tab {
                s.prefs.state.tab = tab;
            }
            let shot = crate::snapshot::shot(&crate::theme::test_context(), size, 1.5, 4, |ui| window_ui(ui, &mut s));
            assert_eq!(crate::snapshot::save(&shot, name), 0, "{name}: nothing behind the window shows");
        }
        // The Tours page with its rows, drawn the way the window draws it, with a site connection given.
        let mut s = shared(tmp.path());
        s.tours = crate::tours_ui::tests::page_with(Vec::new());
        s.nav.request(Page::Tours);
        let conn = crate::tours_ui::tests::conn(TokenStatus::Saved);
        let shot = crate::snapshot::shot(&crate::theme::test_context(), size, 1.5, 4, |ui| window_ui_with(ui, &mut s, &conn));
        assert_eq!(crate::snapshot::save(&shot, "main-tours"), 0, "the Tours page: nothing behind the window shows");
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
        let ctx = crate::theme::test_context();
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
        for word in ["Home", "Tours", "Workspace", "Preferences", "Not connected", "Capture", "Region", "Window", "Full screen", "Record", "Open image", home_ui::OR_DROP_A_FILE, home_ui::RECORD_A_TOUR, "Recent", home_ui::NOTHING_RECENT] {
            assert!(home.lines().any(|l| l == word), "Home is missing {word:?} in:\n{home}");
        }
        // No token is saved in a test run, so Tours says it is not connected and asks nothing of any site.
        s.nav.go(Page::Tours);
        let tours = drawn(&ctx, &mut s);
        assert!(tours.lines().any(|l| l == tours_ui::RECORD_A_TOUR) && !tours.contains("Recent"), "{tours}");
        assert!(tours.contains(tours_ui::NOT_CONNECTED) || tours.contains(tours_ui::CHECKING), "{tours}");
        // Workspace, with no profile: the add form, with "Save profile" not usable yet, and no site is asked.
        s.nav.go(Page::Workspace);
        let workspace = drawn(&ctx, &mut s);
        for word in ["Workspace", crate::profiles::SITE_ADDRESS, crate::profiles::SIGN_IN, crate::profiles::ADDRESS_HINT, crate::profiles::CANCEL, crate::profiles::SAVE_PROFILE] {
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
        let ctx = crate::theme::test_context();
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
        let ctx = crate::theme::test_context();
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

    /// The Open image tile asks the app for the tray's "Open image from file…", at the usual and
    /// the smallest window (where it wraps to the second row), and also while a capture is going
    /// on, as the tray's item does. The window stays open and no capture is asked for.
    #[test]
    fn the_open_image_tile_asks_for_the_file_dialog_and_the_window_stays() {
        let tmp = tempfile::tempdir().unwrap();
        for (size, busy) in [(SIZE, false), (MIN_SIZE, false), (SIZE, true)] {
            let mut s = shared(tmp.path());
            s.nav.request(Page::Home);
            s.busy = busy;
            let ctx = crate::theme::test_context();
            let size = egui::vec2(size[0], size[1]);
            window_frame(&ctx, &mut s, size, vec![]);
            let out = window_frame(&ctx, &mut s, size, vec![]);
            let at = crate::share_ui::tests::text_rect(&out, home_ui::TILES[4].name).expect("the tile is drawn").center();
            assert!(crate::share_ui::tests::text_rect(&out, home_ui::OR_DROP_A_FILE).is_some_and(|r| r.top() > at.y), "its caption under its name");
            let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            for events in [vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![]] {
                window_frame(&ctx, &mut s, size, events);
            }
            assert_eq!(s.open_image, Some(OpenImage::Pick), "{size:?} busy {busy}");
            assert_eq!(s.capture, None);
            assert!(s.nav.is_open() && s.app_requests.is_empty());
        }
    }

    /// One frame of the window with files dragged over it and dropped on it.
    fn drag_frame(ctx: &egui::Context, s: &mut Shared, size: egui::Vec2, hovered: &[std::path::PathBuf], dropped: &[std::path::PathBuf], events: Vec<egui::Event>) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(ctx.input(|i| i.time) + 0.5),
            hovered_files: hovered.iter().map(|p| egui::HoveredFile { path: Some(p.clone()), mime: String::new() }).collect(),
            dropped_files: dropped.iter().map(|p| Arc::new(crate::open_image::tests::Dropped(p.clone())) as egui::DroppedFileHandle).collect(),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| window_ui(ui, s));
        out.textures_delta.clear();
        out
    }

    /// On every page, a picture file dragged over the window shows the overlay that takes it;
    /// anything else, or several files, the one that refuses; no drag, none. A dropped picture
    /// reaches the app as a file to open, the same open as the tile's; anything else does nothing.
    #[test]
    fn a_picture_dragged_over_the_window_shows_the_overlay_and_a_drop_opens_it() {
        use crate::open_image::{DROP_TO_OPEN, ONLY_PICTURES};
        let tmp = tempfile::tempdir().unwrap();
        let pic = tmp.path().join("Shot 1.JPEG");
        std::fs::write(&pic, "x").unwrap();
        let notes = tmp.path().join("notes.txt");
        std::fs::write(&notes, "x").unwrap();
        let size = egui::vec2(SIZE[0], SIZE[1]);
        let has = |out: &egui::FullOutput, text: &str| crate::share_ui::tests::text_rect(out, text).is_some();
        for page in Page::ALL {
            let ctx = crate::theme::test_context();
            let mut s = shared(tmp.path());
            s.nav.request(page);
            let out = drag_frame(&ctx, &mut s, size, &[], &[], vec![]);
            assert!(!has(&out, DROP_TO_OPEN) && !has(&out, ONLY_PICTURES), "{page:?}: no drag, no overlay");
            let out = drag_frame(&ctx, &mut s, size, std::slice::from_ref(&pic), &[], vec![]);
            assert!(has(&out, DROP_TO_OPEN) && !has(&out, ONLY_PICTURES), "{page:?}");
            for refused in [vec![notes.clone()], vec![pic.clone(), pic.clone()], vec![tmp.path().to_path_buf()]] {
                let out = drag_frame(&ctx, &mut s, size, &refused, &[], vec![]);
                assert!(has(&out, ONLY_PICTURES) && !has(&out, DROP_TO_OPEN), "{page:?} {refused:?}");
            }
            // The drag leaves: the overlay goes.
            let out = drag_frame(&ctx, &mut s, size, &[], &[], vec![]);
            assert!(!has(&out, DROP_TO_OPEN) && !has(&out, ONLY_PICTURES), "{page:?}");
            drag_frame(&ctx, &mut s, size, std::slice::from_ref(&notes), &[], vec![]);
            drag_frame(&ctx, &mut s, size, &[], std::slice::from_ref(&notes), vec![]);
            assert_eq!(s.open_image, None, "{page:?}: a drop of something else does nothing");
            drag_frame(&ctx, &mut s, size, &[], &[pic.clone(), pic.clone()], vec![]);
            assert_eq!(s.open_image, None, "{page:?}: nor does a drop of several");
            drag_frame(&ctx, &mut s, size, std::slice::from_ref(&pic), &[], vec![]);
            let out = drag_frame(&ctx, &mut s, size, &[], std::slice::from_ref(&pic), vec![]);
            assert!(!has(&out, DROP_TO_OPEN));
            assert_eq!(s.open_image, Some(OpenImage::Dropped(pic.clone())), "{page:?}");
            assert!(s.capture.is_none() && s.nav.is_open() && s.nav.page() == page);
        }
    }

    /// The overlay is only painted: with a drag over the window a click still reaches the page.
    #[test]
    fn the_overlay_never_takes_a_click() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = tmp.path().join("shot.png");
        std::fs::write(&pic, "x").unwrap();
        let mut s = shared(tmp.path());
        s.nav.request(Page::Home);
        let ctx = crate::theme::test_context();
        let size = egui::vec2(SIZE[0], SIZE[1]);
        let at = egui::pos2(NAV_WIDTH + 60.0, 100.0);
        let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        for events in [vec![], vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![]] {
            drag_frame(&ctx, &mut s, size, std::slice::from_ref(&pic), &[], events);
        }
        assert_eq!(s.capture, Some(Command::Region), "the first tile took the click");
    }

    /// The overlay fits the window at its smallest and usual sizes at 100% and 200%: its words
    /// inside the frame, on one or more lines.
    #[test]
    fn the_overlay_fits_the_window_at_every_size_and_scale() {
        use crate::open_image::{DROP_TO_OPEN, ONLY_PICTURES};
        let tmp = tempfile::tempdir().unwrap();
        let pic = tmp.path().join("shot.png");
        std::fs::write(&pic, "x").unwrap();
        let notes = tmp.path().join("notes.txt");
        std::fs::write(&notes, "x").unwrap();
        for size in [SIZE, MIN_SIZE] {
            for ppp in [1.0, 2.0] {
                for (file, text) in [(&pic, DROP_TO_OPEN), (&notes, ONLY_PICTURES)] {
                    let ctx = crate::theme::test_context();
                    ctx.set_pixels_per_point(ppp);
                    let mut s = shared(tmp.path());
                    s.nav.request(Page::Home);
                    let size = egui::vec2(size[0], size[1]);
                    drag_frame(&ctx, &mut s, size, std::slice::from_ref(file), &[], vec![]);
                    let out = drag_frame(&ctx, &mut s, size, std::slice::from_ref(file), &[], vec![]);
                    let words = crate::share_ui::tests::text_rect(&out, text).unwrap();
                    let inner = egui::Rect::from_min_size(egui::Pos2::ZERO, size).shrink(theme::PAGE_PAD);
                    assert!(inner.contains_rect(words), "{size:?} at {ppp}: {words:?} outside {inner:?}");
                    // The see-through tint covers the window below the menu bar, never the bar.
                    let (tint, ..) = crate::open_image::overlay_colours(text == DROP_TO_OPEN);
                    let tinted = crate::theme::checks::fills(&out).into_iter().find(|(_, c, _)| *c == tint).map(|(r, _, _)| r).expect("the tint is painted");
                    let file = crate::share_ui::tests::text_rect(&out, "File").unwrap();
                    assert!(tinted.top() >= file.bottom() && tinted.bottom() >= size.y - 0.5 && tinted.width() >= size.x - 0.5, "{size:?} at {ppp}: {tinted:?}");
                }
            }
        }
    }

    /// The main window's viewport takes dropped files, said outright rather than left to the platform.
    #[test]
    fn the_main_windows_viewport_takes_dropped_files() {
        assert_eq!(builder(Arc::new(egui::IconData::default()), opening::opening(None, None)).drag_and_drop, Some(true));
    }
}
