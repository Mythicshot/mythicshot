//! "Publish tour": the server and token state the editor reads, the reason the
//! export panel's button is disabled, the running publishes, the tour picker with its
//! choice between publishing now and a private draft, and the result screen.
//!
//! Everything that talks to the network or the credential store runs on a background
//! thread. The token is never held here: each background job loads it from the store
//! itself and drops it when done, so it never reaches the UI thread after it is saved.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::publish::{self, Finished, Images, Outcome, RemoteTour, Target};

const MUTED: Color32 = Color32::from_rgb(163, 170, 184);
const ERROR: Color32 = Color32::from_rgb(255, 140, 140);
const PRIMARY: Color32 = Color32::from_rgb(47, 111, 235);

/// The server's cap on a tour title, in characters.
pub const TITLE_MAX: usize = 200;
/// From this many characters the title field shows its count.
const TITLE_COUNT_FROM: usize = 180;
/// Longest tour title shown in a picker row before it is cut with an ellipsis.
const ROW_TITLE_MAX: usize = 40;
/// Prefix of the temporary folder a publish writes its bundle into.
pub const TEMP_PREFIX: &str = "mythicshot-publish-";
/// How long "Link copied" shows after Copy.
const COPIED_FOR: Duration = Duration::from_secs(2);
/// A leftover publish folder older than this is removed at start-up.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether an API token is saved. The token itself is never kept here.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenStatus {
    /// Not looked up yet, or being looked up.
    Checking,
    Saved,
    Missing,
    /// The credential store cannot be used (locked, unavailable), or what it holds is
    /// not a token: the store's message.
    Unavailable(String),
}

/// The server address from the settings and whether a token is saved, shared by the
/// settings window and every editor. `generation` makes a slow lookup that finishes
/// after a newer one leave the newer answer in place.
struct Config {
    server: String,
    /// Debug builds: the server from `MYTHICSHOT_TEST_PUBLISH`, used instead of the saved one
    /// and never written to the settings file.
    test_server: Option<String>,
    token: TokenStatus,
    generation: u64,
}

static CONFIG: Mutex<Config> = Mutex::new(Config { server: String::new(), test_server: None, token: TokenStatus::Checking, generation: 0 });

fn config() -> std::sync::MutexGuard<'static, Config> {
    CONFIG.lock().unwrap_or_else(|e| e.into_inner())
}

/// At start-up: take the server from the settings, look up the token and remove
/// publish folders a crash or a forced quit left behind.
pub fn init(settings: &crate::settings::Settings) {
    // Test hook: the server to publish to, for a check against a local server. In memory
    // only, never saved, and never the token.
    #[cfg(debug_assertions)]
    if let Ok(server) = std::env::var("MYTHICSHOT_TEST_PUBLISH") {
        config().test_server = Some(server);
    }
    set_server(&settings.publish_server);
    refresh_token();
    std::thread::spawn(|| sweep_stale(&std::env::temp_dir(), STALE_AFTER));
}

pub fn set_server(server: &str) {
    config().server = server.to_string();
}

/// The server publishes go to: the test hook's when set, else the saved one.
pub fn server() -> String {
    let c = config();
    c.test_server.clone().unwrap_or_else(|| c.server.clone())
}

/// The test hook's server, when it overrides the saved one.
pub fn test_server() -> Option<String> {
    config().test_server.clone()
}

pub fn token_status() -> TokenStatus {
    config().token.clone()
}

/// Look the token up again on a background thread.
pub fn refresh_token() {
    std::thread::spawn(refresh_token_now);
}

/// Look the token up on this thread; never call it on the UI thread.
pub fn refresh_token_now() {
    let generation = {
        let mut c = config();
        c.generation += 1;
        c.token = TokenStatus::Checking;
        c.generation
    };
    let status = match publish::load_token() {
        Ok(Some(_)) => TokenStatus::Saved,
        Ok(None) => TokenStatus::Missing,
        Err(e) => TokenStatus::Unavailable(e),
    };
    let mut c = config();
    if c.generation == generation {
        c.token = status;
    }
}

/// The saved token, for a background job that is about to use it. A missing token and
/// an unusable store are both errors, each with its own message.
pub fn token_for_job() -> Result<String, String> {
    match publish::load_token() {
        Ok(Some(t)) => Ok(t),
        Ok(None) => Err(ADD_SERVER_AND_TOKEN.to_string()),
        Err(e) => Err(e),
    }
}

/// Overwrite a string's bytes, then empty it, so a token typed into a field does not
/// linger in freed memory.
pub fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference into `bytes`.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    drop(bytes);
}

// ---- Running publishes ----

/// One publish in progress: its cancel flag, whether it has ended, and its temporary folder.
#[derive(Default)]
pub struct Run {
    cancel: AtomicBool,
    done: AtomicBool,
    dir: Mutex<Option<PathBuf>>,
    /// Set once the upload is in and the publish step runs: the images copied so far, when known.
    publishing: Mutex<Option<Option<Images>>>,
}

static RUNS: Mutex<Vec<Arc<Run>>> = Mutex::new(Vec::new());

fn runs() -> std::sync::MutexGuard<'static, Vec<Arc<Run>>> {
    RUNS.lock().unwrap_or_else(|e| e.into_inner())
}

impl Run {
    /// A new publish, known to [`shutdown`] until it finishes.
    pub fn begin() -> Arc<Run> {
        let run = Arc::new(Run::default());
        runs().push(run.clone());
        run
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The publish step has started, or moved on: `images` is the server's last count.
    pub fn set_publishing(&self, images: Option<Images>) {
        if let Ok(mut p) = self.publishing.lock() {
            *p = Some(images);
        }
    }

    /// None while writing and uploading; Some in the publish step, with the count when the server gave one.
    pub fn publishing(&self) -> Option<Option<Images>> {
        self.publishing.lock().ok().and_then(|p| *p)
    }

    pub fn set_dir(&self, dir: Option<PathBuf>) {
        if let Ok(mut d) = self.dir.lock() {
            *d = dir;
        }
    }

    /// The publish has ended and removed its folder.
    pub fn finish(self: &Arc<Self>) {
        self.set_dir(None);
        self.done.store(true, Ordering::Relaxed);
        runs().retain(|r| !Arc::ptr_eq(r, self));
    }
}

/// On quit: cancel every publish, give them up to `wait` to send their abort, then
/// remove the temporary folder of any that is still running.
pub fn shutdown(wait: Duration) {
    let list: Vec<Arc<Run>> = runs().clone();
    if list.is_empty() {
        return;
    }
    for r in &list {
        r.cancel();
    }
    let until = Instant::now() + wait;
    while Instant::now() < until && list.iter().any(|r| !r.done.load(Ordering::Relaxed)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    for r in list.iter().filter(|r| !r.done.load(Ordering::Relaxed)) {
        let dir = r.dir.lock().ok().and_then(|d| d.clone());
        if let Some(dir) = dir {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// A folder name `tempfile` gives a publish: the prefix and six letters or digits.
pub fn is_publish_dir_name(name: &str) -> bool {
    name.strip_prefix(TEMP_PREFIX).is_some_and(|rest| rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Remove publish folders in `temp` older than `age`: only real folders (not links) with
/// exactly the publish name, holding nothing but the bundle folder `tour`.
pub fn sweep_stale(temp: &Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        if !name.to_str().is_some_and(is_publish_dir_name) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
        if !meta.is_dir() || meta.modified().ok().and_then(|m| m.elapsed().ok()).is_none_or(|a| a < age) {
            continue;
        }
        let only_bundle = std::fs::read_dir(e.path()).map(|inner| inner.flatten().all(|i| i.file_name() == "tour")).unwrap_or(false);
        if only_bundle {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

// ---- Why the button is disabled ----

/// Why "Publish tour…" cannot be used right now.
#[derive(Debug, Clone, PartialEq)]
pub enum Blocked {
    Exporting,
    NoSteps,
    NoServer,
    BadServer(String),
    NoToken,
    Checking,
    Store(String),
}

pub const ADD_SERVER_AND_TOKEN: &str = "Add a server and token in Settings › Output to publish.";

/// What stops a publish, in the order the user would fix it; None when it can start.
pub fn publish_blocked(exporting: bool, has_steps: bool, server: &str, token: &TokenStatus) -> Option<Blocked> {
    if exporting {
        return Some(Blocked::Exporting);
    }
    if !has_steps {
        return Some(Blocked::NoSteps);
    }
    if server.trim().is_empty() {
        return Some(Blocked::NoServer);
    }
    if let Err(e) = publish::check_server(server) {
        return Some(Blocked::BadServer(e));
    }
    match token {
        TokenStatus::Saved => None,
        TokenStatus::Missing => Some(Blocked::NoToken),
        TokenStatus::Checking => Some(Blocked::Checking),
        TokenStatus::Unavailable(e) => Some(Blocked::Store(e.clone())),
    }
}

impl Blocked {
    /// The line under the button. A running export and a tour without steps get none:
    /// the status bar and the tour line already say why.
    pub fn reason(&self) -> Option<String> {
        match self {
            Blocked::Exporting | Blocked::NoSteps => None,
            Blocked::NoServer | Blocked::NoToken => Some(ADD_SERVER_AND_TOKEN.to_string()),
            Blocked::BadServer(e) | Blocked::Store(e) => Some(e.clone()),
            Blocked::Checking => Some("Checking for a saved token…".to_string()),
        }
    }

    /// What the picker says when "Upload draft" cannot start: always a sentence.
    pub fn refusal(&self) -> String {
        match self {
            Blocked::Exporting => "Wait for the running export to finish, then upload.".to_string(),
            Blocked::NoSteps => "This recording has no tour steps to upload.".to_string(),
            other => other.reason().unwrap_or_default(),
        }
    }
}

fn count(n: u32, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// The right-hand side of a picker row: "3 chapters · 14 steps · published".
pub fn tour_row_detail(t: &RemoteTour) -> String {
    format!("{} · {} · {}", count(t.chapters, "chapter", "chapters"), count(t.steps, "step", "steps"), t.status)
}

/// A whole picker row as one line: "Title · 3 chapters · 14 steps · draft".
#[cfg_attr(not(test), allow(dead_code))]
pub fn tour_row_label(t: &RemoteTour) -> String {
    format!("{} · {}", t.title, tour_row_detail(t))
}

/// A title cut to `max` characters, the last one an ellipsis, when it is longer.
pub fn short_title(title: &str, max: usize) -> String {
    if title.chars().count() <= max { title.to_string() } else { format!("{}…", title.chars().take(max.saturating_sub(1)).collect::<String>().trim_end()) }
}

/// The count shown beside the title field near the limit, "185 / 200".
pub fn title_count(title: &str) -> Option<String> {
    let n = title.chars().count();
    (n >= TITLE_COUNT_FROM).then(|| format!("{n} / {TITLE_MAX}"))
}

// ---- Publish now, or a private draft ----

/// What "Publish tour" does with the upload. Publishing now is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    PublishNow,
    Draft,
}

/// What the upload goes into, as far as the hints care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goes {
    NewTour,
    /// A new chapter of a tour that is not public.
    DraftTour,
    /// A new chapter of a tour that is public now.
    PublicTour,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::PublishNow, Mode::Draft];

    pub fn label(self) -> &'static str {
        match self {
            Mode::PublishNow => "Publish now",
            Mode::Draft => "Upload as a private draft",
        }
    }

    /// The dialog's main button.
    pub fn button(self) -> &'static str {
        match self {
            Mode::PublishNow => "Publish",
            Mode::Draft => "Upload draft",
        }
    }

    /// The line under the option. The server publishes a tour whole, and a draft upload
    /// leaves what is public untouched, so a chapter of an existing tour says so.
    pub fn hint(self, into: Goes) -> &'static str {
        match (self, into) {
            (Mode::PublishNow, Goes::NewTour) => "Anyone with the link can view it.",
            (Mode::PublishNow, Goes::DraftTour) => "Publishes the whole tour with this chapter. Anyone with the link can view it.",
            (Mode::PublishNow, Goes::PublicTour) => "Republishes the whole tour with this chapter. Anyone with the link can view it.",
            (Mode::Draft, Goes::NewTour | Goes::DraftTour) => "Nothing goes public.",
            (Mode::Draft, Goes::PublicTour) => "Nothing new goes public. The public version stays as it is.",
        }
    }
}

/// The status-bar message during the publish step: "Publishing… 12 of 22 images".
pub fn publishing_message(images: Option<Images>) -> String {
    match images {
        None => "Publishing…".to_string(),
        Some((done, 1)) => format!("Publishing… {done} of 1 image"),
        Some((done, total)) => format!("Publishing… {done} of {total} images"),
    }
}

// ---- The result screen ----

pub fn result_heading(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Public { .. } => "Tour published",
        Outcome::Draft | Outcome::NotPublic { .. } => "Draft uploaded",
    }
}

/// "Customer onboarding · 14 steps · public".
pub fn result_line(title: &str, steps: usize, outcome: &Outcome) -> String {
    let steps = count(u32::try_from(steps).unwrap_or(u32::MAX), "step", "steps");
    let public = if matches!(outcome, Outcome::Public { .. }) { "public" } else { "not public" };
    format!("{} · {steps} · {public}", short_title(title, ROW_TITLE_MAX))
}

/// What else the result screen has to say, and whether it is an error.
pub fn result_note(outcome: &Outcome) -> Option<(String, bool)> {
    match outcome {
        Outcome::Draft | Outcome::Public { url: Some(_) } => None,
        Outcome::Public { url: None } => Some(("The tour is public, but the server gave no link that can be shown. Look it up in the server's tour list.".to_string(), false)),
        Outcome::NotPublic { cancelled: true, .. } => Some(("Publishing was cancelled. The draft is uploaded, but it is not public.".to_string(), false)),
        Outcome::NotPublic { cancelled: false, error } => Some((format!("{error} The draft is uploaded, but it is not public."), true)),
    }
}

/// The one-line message for the status bar and the log.
pub fn result_status(steps: usize, outcome: &Outcome) -> String {
    let steps = count(u32::try_from(steps).unwrap_or(u32::MAX), "step", "steps");
    match outcome {
        Outcome::Public { .. } => format!("Tour published: {steps}"),
        Outcome::Draft => format!("Tour uploaded as a draft: {steps}"),
        Outcome::NotPublic { cancelled: true, .. } => "Publishing cancelled: the draft is uploaded, but not public".to_string(),
        Outcome::NotPublic { cancelled: false, .. } => "Publishing failed: the draft is uploaded, but not public".to_string(),
    }
}

/// "Link copied", for a moment after Copy.
pub fn copied_note(since_copy: Option<Duration>) -> Option<&'static str> {
    since_copy.filter(|d| *d < COPIED_FOR).map(|_| "Link copied")
}

/// What the user chose on the result screen.
#[derive(Debug, Clone, PartialEq)]
pub enum ResultAction {
    Done,
    /// Publish the uploaded tour again, without uploading: the tour's id and its step count.
    Retry { tour_id: String, steps: usize },
    /// Open the public link in the browser; the screen stays.
    Open(String),
}

/// The screen after a publish or a draft upload: what happened, and for a public tour its link.
pub struct ResultDialog {
    title: String,
    done: Finished,
    copied: Option<Instant>,
}

impl ResultDialog {
    pub fn new(title: String, done: Finished) -> ResultDialog {
        ResultDialog { title, done, copied: None }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    fn url(&self) -> Option<&str> {
        match &self.done.outcome {
            Outcome::Public { url } => url.as_deref(),
            _ => None,
        }
    }

    fn can_retry(&self) -> bool {
        matches!(self.done.outcome, Outcome::NotPublic { cancelled: false, .. })
    }

    /// Draw the screen; Some once the user picks something. Escape and Enter are "Done",
    /// and both keys are taken from the editor.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<ResultAction> {
        let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        if escape || enter {
            return Some(ResultAction::Done);
        }
        let mut action = None;
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(22.0);
        egui::Modal::new(egui::Id::unique("publish-tour-result")).frame(frame).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading(result_heading(&self.done.outcome));
            ui.label(RichText::new(result_line(&self.title, self.done.steps, &self.done.outcome)).color(MUTED)).on_hover_text(&self.title);
            if let Some((note, is_error)) = result_note(&self.done.outcome) {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(note).color(if is_error { ERROR } else { MUTED })).wrap());
            }
            if let Some(url) = self.url().map(str::to_string) {
                ui.add_space(12.0);
                // A `&str` buffer: the text can be selected and copied, never changed.
                let mut shown = url.as_str();
                ui.add(egui::TextEdit::singleline(&mut shown).desired_width(f32::INFINITY));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(url.clone());
                        self.copied = Some(Instant::now());
                    }
                    if ui.button("Open in browser").clicked() {
                        action = Some(ResultAction::Open(url.clone()));
                    }
                    if let Some(note) = copied_note(self.copied.map(|t| t.elapsed())) {
                        ui.label(RichText::new(note).color(MUTED));
                        ctx.request_repaint_after(Duration::from_millis(200));
                    }
                });
            }
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("Done").strong()).fill(PRIMARY)).clicked() {
                    action = Some(ResultAction::Done);
                }
                if self.can_retry() && ui.button("Try again").on_hover_text("Publish the uploaded draft, without uploading it again").clicked() {
                    action = Some(ResultAction::Retry { tour_id: self.done.tour_id.clone(), steps: self.done.steps });
                }
            });
        });
        action
    }
}

// ---- The picker ----

enum ListState {
    Loading,
    Loaded(Vec<RemoteTour>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Choice {
    New,
    Tour(String),
}

/// What the user chose in the picker.
pub enum PickerOutcome {
    Cancel,
    /// Upload with this title (the new tour's, or the chapter's) into this target, then
    /// publish it or leave it a draft. `tour_title` names the tour on the result screen.
    Upload { title: String, target: Target, mode: Mode, tour_title: String },
}

/// The "Publish tour" dialog: a new tour with an editable title, or a new chapter of
/// one of the workspace's tours, loaded on a background thread.
pub struct Picker {
    server: String,
    title: String,
    /// The recording's title, used as the chapter's title when adding to a tour.
    default_title: String,
    choice: Choice,
    mode: Mode,
    list: Arc<Mutex<ListState>>,
    /// Why the last "Publish" or "Upload draft" could not start.
    refused: Option<String>,
}

impl Picker {
    fn new(server: String, title: &str) -> Picker {
        let title = short_title(title, TITLE_MAX);
        Picker { server, title: title.clone(), default_title: title, choice: Choice::New, mode: Mode::default(), list: Arc::new(Mutex::new(ListState::Loading)), refused: None }
    }

    pub fn open(server: String, title: String) -> Picker {
        let picker = Picker::new(server, &title);
        picker.load();
        picker
    }

    /// Keep the dialog open and say why the upload did not start.
    pub fn refuse(&mut self, why: String) {
        self.refused = Some(why);
    }

    fn load(&self) {
        if let Ok(mut l) = self.list.lock() {
            *l = ListState::Loading;
        }
        let (list, server) = (self.list.clone(), self.server.clone());
        std::thread::spawn(move || {
            let result = token_for_job().and_then(|token| publish::list_tours(&server, &token));
            let state = match result {
                Ok(tours) => ListState::Loaded(tours),
                Err(e) => ListState::Failed(e),
            };
            if let Ok(mut l) = list.lock() {
                *l = state;
            }
        });
    }

    fn can_upload(&self) -> bool {
        match self.choice {
            Choice::New => !self.title.trim().is_empty(),
            Choice::Tour(_) => true,
        }
    }

    /// The chosen tour's row, when a tour is chosen and the list holds it.
    fn chosen_tour(&self) -> Option<RemoteTour> {
        let Choice::Tour(id) = &self.choice else { return None };
        match &*self.list.lock().ok()? {
            ListState::Loaded(tours) => tours.iter().find(|t| &t.id == id).cloned(),
            _ => None,
        }
    }

    /// What the upload goes into, for the hints.
    fn goes(&self) -> Goes {
        match (&self.choice, self.chosen_tour()) {
            (Choice::New, _) => Goes::NewTour,
            (Choice::Tour(_), Some(t)) if t.status == "published" => Goes::PublicTour,
            (Choice::Tour(_), _) => Goes::DraftTour,
        }
    }

    fn upload(&self) -> PickerOutcome {
        match &self.choice {
            Choice::New => {
                let title = self.title.trim().to_string();
                PickerOutcome::Upload { title: title.clone(), target: Target::New, mode: self.mode, tour_title: title }
            }
            Choice::Tour(id) => {
                let tour_title = self.chosen_tour().map_or_else(|| self.default_title.clone(), |t| t.title);
                PickerOutcome::Upload { title: self.default_title.clone(), target: Target::Chapter(id.clone()), mode: self.mode, tour_title }
            }
        }
    }

    /// Draw the dialog; Some once the user cancels or chooses. Escape cancels and Enter
    /// is the main button when it can be used; both keys are taken from the editor.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<PickerOutcome> {
        let mut outcome = None;
        let mut retry = false;
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            return Some(PickerOutcome::Cancel);
        }
        let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(22.0);
        egui::Modal::new(egui::Id::unique("publish-tour-picker")).frame(frame).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Publish tour");
            ui.add_space(4.0);
            let into = self.goes();
            for mode in Mode::ALL {
                if ui.radio(self.mode == mode, mode.label()).clicked() {
                    self.mode = mode;
                }
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.add(egui::Label::new(RichText::new(mode.hint(into)).color(MUTED).size(12.0)).wrap());
                });
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            if ui.radio(self.choice == Choice::New, "New tour").clicked() {
                self.choice = Choice::New;
            }
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                let count = title_count(&self.title);
                let width = ui.available_width() - if count.is_some() { 70.0 } else { 0.0 };
                let edit = egui::TextEdit::singleline(&mut self.title).char_limit(TITLE_MAX).hint_text("Tour title").desired_width(width);
                if ui.add_enabled(self.choice == Choice::New, edit).gained_focus() {
                    self.choice = Choice::New;
                }
                if let Some(count) = count {
                    ui.label(RichText::new(count).color(MUTED).size(12.0));
                }
            });
            ui.add_space(12.0);
            ui.label(RichText::new("Or add as a new chapter to").color(MUTED));
            ui.add_space(4.0);
            let guard = self.list.lock();
            let Ok(list) = guard else { return };
            match &*list {
                ListState::Loading => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(RichText::new("Loading tours…").color(MUTED));
                    });
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                ListState::Failed(e) => {
                    ui.add(egui::Label::new(RichText::new(e).color(ERROR)).wrap());
                    if ui.button("Retry").clicked() {
                        retry = true;
                    }
                }
                ListState::Loaded(tours) if tours.is_empty() => {
                    ui.label(RichText::new("This workspace has no tours yet.").color(MUTED));
                }
                ListState::Loaded(tours) => {
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for t in tours {
                            let chosen = self.choice == Choice::Tour(t.id.clone());
                            ui.horizontal(|ui| {
                                if ui.radio(chosen, short_title(&t.title, ROW_TITLE_MAX)).on_hover_text(&t.title).clicked() {
                                    self.choice = Choice::Tour(t.id.clone());
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(RichText::new(tour_row_detail(t)).color(MUTED).size(12.0));
                                });
                            });
                        }
                    });
                }
            }
            drop(list);
            if let Some(why) = &self.refused {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(why).color(ERROR)).wrap());
            }
            ui.add_space(16.0);
            let can_upload = self.can_upload();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let upload = egui::Button::new(RichText::new(self.mode.button()).strong()).fill(PRIMARY);
                if ui.add_enabled(can_upload, upload).clicked() {
                    outcome = Some(self.upload());
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(PickerOutcome::Cancel);
                }
            });
        });
        if outcome.is_none() && enter && self.can_upload() {
            outcome = Some(self.upload());
        }
        if retry {
            self.choice = Choice::New;
            self.load();
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tour(title: &str, chapters: u32, steps: u32, status: &str) -> RemoteTour {
        RemoteTour { id: "t1".into(), title: title.into(), status: status.into(), chapters, steps }
    }

    #[test]
    fn publish_is_ready_with_steps_a_server_and_a_saved_token() {
        assert_eq!(publish_blocked(false, true, "https://tours.example.com", &TokenStatus::Saved), None);
        assert_eq!(publish_blocked(false, true, "http://localhost:8787/", &TokenStatus::Saved), None);
    }

    #[test]
    fn each_missing_piece_has_its_own_disabled_reason() {
        let ok = "https://tours.example.com";
        // A running export or no steps: disabled, with no extra line (the status bar and the tour line say why).
        assert_eq!(publish_blocked(true, true, ok, &TokenStatus::Saved), Some(Blocked::Exporting));
        assert_eq!(Blocked::Exporting.reason(), None);
        assert_eq!(publish_blocked(false, false, ok, &TokenStatus::Saved), Some(Blocked::NoSteps));
        assert_eq!(Blocked::NoSteps.reason(), None);
        // No server, or no token: the approved line.
        assert_eq!(publish_blocked(false, true, "  ", &TokenStatus::Saved), Some(Blocked::NoServer));
        assert_eq!(publish_blocked(false, true, "", &TokenStatus::Missing), Some(Blocked::NoServer));
        assert_eq!(Blocked::NoServer.reason().as_deref(), Some(ADD_SERVER_AND_TOKEN));
        assert_eq!(publish_blocked(false, true, ok, &TokenStatus::Missing), Some(Blocked::NoToken));
        assert_eq!(Blocked::NoToken.reason().as_deref(), Some(ADD_SERVER_AND_TOKEN));
        // A server address that cannot be used says what is wrong with it.
        let bad = publish_blocked(false, true, "http://tours.example.com", &TokenStatus::Saved);
        assert!(matches!(&bad, Some(Blocked::BadServer(e)) if e.contains("https://")), "{bad:?}");
        assert!(bad.unwrap().reason().unwrap().contains("https://"));
        // A token still being looked up, and a locked store, which is not "no token".
        assert_eq!(publish_blocked(false, true, ok, &TokenStatus::Checking), Some(Blocked::Checking));
        assert_eq!(Blocked::Checking.reason().as_deref(), Some("Checking for a saved token…"));
        let locked = "Could not use the credential store: the credential store is locked or not reachable.";
        let b = publish_blocked(false, true, ok, &TokenStatus::Unavailable(locked.into()));
        assert_eq!(b, Some(Blocked::Store(locked.into())));
        assert_eq!(b.unwrap().reason().as_deref(), Some(locked));
    }

    #[test]
    fn a_refused_upload_always_says_why() {
        let all = [Blocked::Exporting, Blocked::NoSteps, Blocked::NoServer, Blocked::BadServer("Bad address.".into()), Blocked::NoToken, Blocked::Checking, Blocked::Store("Locked.".into())];
        for b in all {
            assert!(!b.refusal().is_empty(), "{b:?}");
        }
        assert!(Blocked::Exporting.refusal().contains("export"));
        assert_eq!(Blocked::NoToken.refusal(), ADD_SERVER_AND_TOKEN);
    }

    #[test]
    fn picker_rows_name_chapters_steps_and_status() {
        assert_eq!(tour_row_label(&tour("Title", 3, 14, "draft")), "Title · 3 chapters · 14 steps · draft");
        assert_eq!(tour_row_detail(&tour("Customer onboarding", 3, 14, "published")), "3 chapters · 14 steps · published");
        assert_eq!(tour_row_detail(&tour("One", 1, 1, "draft")), "1 chapter · 1 step · draft");
        assert_eq!(tour_row_detail(&tour("Empty", 0, 0, "draft")), "0 chapters · 0 steps · draft");
    }

    #[test]
    fn long_titles_are_cut_with_an_ellipsis_and_the_count_shows_near_the_limit() {
        assert_eq!(short_title("Customer onboarding", 40), "Customer onboarding");
        assert_eq!(short_title("abcdefghij", 5), "abcd…");
        assert_eq!(short_title("ab de fghij", 4), "ab…");
        assert_eq!(short_title(&"é".repeat(41), 40).chars().count(), 40);
        assert_eq!(title_count(&"a".repeat(179)), None);
        assert_eq!(title_count(&"a".repeat(185)).as_deref(), Some("185 / 200"));
        assert_eq!(Picker::new(String::new(), &"x".repeat(250)).title.chars().count(), TITLE_MAX);
    }

    #[test]
    fn only_old_publish_folders_with_the_exact_name_are_swept() {
        assert!(is_publish_dir_name("mythicshot-publish-a1B2c3"));
        for name in ["mythicshot-publish-", "mythicshot-publish-abc", "mythicshot-publish-abcdefg", "mythicshot-publish-ab.def", "other-abcdef", "Mythicshot-publish-abcdef"] {
            assert!(!is_publish_dir_name(name), "{name}");
        }
        let tmp = tempfile::tempdir().unwrap();
        let mk = |name: &str, inner: &[&str]| {
            let d = tmp.path().join(name);
            std::fs::create_dir(&d).unwrap();
            for i in inner {
                std::fs::create_dir(d.join(i)).unwrap();
            }
            d
        };
        let ours = mk("mythicshot-publish-abc123", &["tour"]);
        let empty = mk("mythicshot-publish-def456", &[]);
        let foreign = mk("mythicshot-publish-ghi789", &["tour", "other"]);
        let unrelated = mk("something-else", &[]);
        let file = tmp.path().join("mythicshot-publish-jkl012");
        std::fs::write(&file, b"x").unwrap();
        // Nothing is old enough yet.
        sweep_stale(tmp.path(), Duration::from_secs(3600));
        assert!(ours.exists() && empty.exists());
        // With no age limit, only our own folders go.
        sweep_stale(tmp.path(), Duration::ZERO);
        assert!(!ours.exists() && !empty.exists());
        assert!(foreign.exists() && unrelated.exists() && file.exists());
    }

    #[test]
    fn shutdown_cancels_and_removes_the_folder_of_a_publish_still_running() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mythicshot-publish-zzz999");
        std::fs::create_dir_all(dir.join("tour")).unwrap();
        let run = Run::begin();
        run.set_dir(Some(dir.clone()));
        shutdown(Duration::from_millis(100));
        assert!(run.is_cancelled());
        assert!(!dir.exists());
        run.finish();
        assert!(!runs().iter().any(|r| Arc::ptr_eq(r, &run)));
    }

    #[test]
    fn wipe_empties_the_string() {
        let mut s = String::from("dp_secret");
        wipe(&mut s);
        assert!(s.is_empty());
    }

    #[test]
    fn publish_now_is_the_default_and_the_button_follows_the_choice() {
        let mut p = Picker::new(String::new(), "Demo");
        assert_eq!(p.mode, Mode::PublishNow);
        assert_eq!(Mode::default(), Mode::PublishNow);
        assert_eq!(p.mode.button(), "Publish");
        assert!(matches!(p.upload(), PickerOutcome::Upload { mode: Mode::PublishNow, target: Target::New, ref tour_title, .. } if tour_title == "Demo"));
        p.mode = Mode::Draft;
        assert_eq!(p.mode.button(), "Upload draft");
        assert!(matches!(p.upload(), PickerOutcome::Upload { mode: Mode::Draft, .. }));
        assert_eq!(Mode::ALL.map(Mode::label), ["Publish now", "Upload as a private draft"]);
    }

    #[test]
    fn the_hints_say_what_goes_public() {
        assert_eq!(Mode::PublishNow.hint(Goes::NewTour), "Anyone with the link can view it.");
        assert_eq!(Mode::Draft.hint(Goes::NewTour), "Nothing goes public.");
        assert_eq!(Mode::Draft.hint(Goes::DraftTour), "Nothing goes public.");
        // The server publishes a tour whole, and a draft upload leaves the public version alone.
        assert!(Mode::PublishNow.hint(Goes::DraftTour).starts_with("Publishes the whole tour"));
        assert!(Mode::PublishNow.hint(Goes::PublicTour).starts_with("Republishes the whole tour"));
        assert_eq!(Mode::Draft.hint(Goes::PublicTour), "Nothing new goes public. The public version stays as it is.");
        for mode in Mode::ALL {
            for into in [Goes::DraftTour, Goes::PublicTour] {
                assert!(mode.hint(into).ends_with('.'));
            }
        }
    }

    #[test]
    fn a_chapter_goes_into_the_chosen_tour_and_the_result_names_that_tour() {
        let mut p = Picker::new(String::new(), "Recording");
        let mut public = tour("Customer onboarding", 3, 14, "published");
        public.id = "tour_bbbbbbbbbbbbbbbbbbbbbbbb".into();
        *p.list.lock().unwrap() = ListState::Loaded(vec![tour("Draft one", 1, 2, "draft"), public]);
        assert_eq!(p.goes(), Goes::NewTour);
        p.choice = Choice::Tour("t1".into());
        assert_eq!(p.goes(), Goes::DraftTour);
        p.choice = Choice::Tour("tour_bbbbbbbbbbbbbbbbbbbbbbbb".into());
        assert_eq!(p.goes(), Goes::PublicTour);
        p.mode = Mode::Draft;
        match p.upload() {
            PickerOutcome::Upload { title, target, mode, tour_title } => {
                assert_eq!((title.as_str(), tour_title.as_str(), mode), ("Recording", "Customer onboarding", Mode::Draft));
                assert_eq!(target, Target::Chapter("tour_bbbbbbbbbbbbbbbbbbbbbbbb".into()));
            }
            PickerOutcome::Cancel => panic!("cancelled"),
        }
    }

    #[test]
    fn the_publish_step_shows_the_servers_count() {
        assert_eq!(publishing_message(None), "Publishing…");
        assert_eq!(publishing_message(Some((12, 22))), "Publishing… 12 of 22 images");
        assert_eq!(publishing_message(Some((0, 1))), "Publishing… 0 of 1 image");
        let run = Run::default();
        assert_eq!(run.publishing(), None);
        run.set_publishing(None);
        assert_eq!(run.publishing(), Some(None));
        run.set_publishing(Some((12, 22)));
        assert_eq!(run.publishing(), Some(Some((12, 22))));
    }

    #[test]
    fn the_result_screen_says_what_happened() {
        let public = Outcome::Public { url: Some("https://tours.example.com/t/abc".into()) };
        assert_eq!(result_heading(&public), "Tour published");
        assert_eq!(result_line("Customer onboarding", 14, &public), "Customer onboarding · 14 steps · public");
        assert_eq!(result_line("One", 1, &public), "One · 1 step · public");
        assert_eq!(result_note(&public), None);
        assert_eq!(result_status(14, &public), "Tour published: 14 steps");

        assert_eq!(result_heading(&Outcome::Draft), "Draft uploaded");
        assert_eq!(result_line("Customer onboarding", 14, &Outcome::Draft), "Customer onboarding · 14 steps · not public");
        assert_eq!(result_line("One", 1, &Outcome::Draft), "One · 1 step · not public");
        assert_eq!(result_note(&Outcome::Draft), None);
        assert_eq!(result_status(1, &Outcome::Draft), "Tour uploaded as a draft: 1 step");

        // A publish that failed or was cancelled after the upload: the truth is "uploaded, not public".
        let failed = Outcome::NotPublic { cancelled: false, error: "Could not publish the tour: the server is busy (HTTP 429). Try again later.".into() };
        assert_eq!(result_heading(&failed), "Draft uploaded");
        assert_eq!(result_line("One", 2, &failed), "One · 2 steps · not public");
        assert_eq!(result_note(&failed), Some(("Could not publish the tour: the server is busy (HTTP 429). Try again later. The draft is uploaded, but it is not public.".to_string(), true)));
        let cancelled = Outcome::NotPublic { cancelled: true, error: String::new() };
        assert_eq!(result_note(&cancelled), Some(("Publishing was cancelled. The draft is uploaded, but it is not public.".to_string(), false)));
        assert!(result_status(2, &cancelled).contains("not public") && result_status(2, &failed).contains("not public"));

        // Public without a usable link: say so, show none.
        let no_link = Outcome::Public { url: None };
        assert_eq!(result_heading(&no_link), "Tour published");
        assert!(result_note(&no_link).is_some_and(|(n, is_error)| n.contains("no link") && !is_error));
        // A long title is cut.
        assert!(result_line(&"x".repeat(100), 2, &Outcome::Draft).chars().count() < 70);
    }

    #[test]
    fn only_a_public_tour_has_a_link_and_only_a_failed_publish_can_be_tried_again() {
        let done = |outcome| ResultDialog::new("Demo".into(), Finished { tour_id: "tour_aaaaaaaaaaaaaaaaaaaaaaaa".into(), steps: 2, outcome });
        let public = done(Outcome::Public { url: Some("https://x.dev/t/a".into()) });
        assert_eq!(public.url(), Some("https://x.dev/t/a"));
        assert!(!public.can_retry());
        let draft = done(Outcome::Draft);
        assert_eq!(draft.url(), None);
        assert!(!draft.can_retry());
        assert!(done(Outcome::NotPublic { cancelled: false, error: "x".into() }).can_retry());
        assert!(!done(Outcome::NotPublic { cancelled: true, error: String::new() }).can_retry());
    }

    #[test]
    fn link_copied_shows_for_a_moment() {
        assert_eq!(copied_note(None), None);
        assert_eq!(copied_note(Some(Duration::from_millis(100))), Some("Link copied"));
        assert_eq!(copied_note(Some(COPIED_FOR)), None);
    }

    #[test]
    fn the_result_screen_draws_and_enter_or_escape_is_done() {
        let ctx = egui::Context::default();
        let key = |k: egui::Key| egui::RawInput { events: vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], ..Default::default() };
        let finished = |outcome| Finished { tour_id: "tour_aaaaaaaaaaaaaaaaaaaaaaaa".into(), steps: 2, outcome };
        for outcome in [Outcome::Public { url: Some("https://x.dev/t/a".into()) }, Outcome::Public { url: None }, Outcome::Draft, Outcome::NotPublic { cancelled: false, error: "Could not publish the tour: x.".into() }] {
            let mut d = ResultDialog::new("Demo".into(), finished(outcome));
            let mut out = None;
            ctx.run_ui(egui::RawInput::default(), |ui| out = d.show(ui.ctx())).textures_delta.clear();
            assert_eq!(out, None, "it stays until the user chooses");
            ctx.run_ui(key(egui::Key::Enter), |ui| out = d.show(ui.ctx())).textures_delta.clear();
            assert_eq!(out, Some(ResultAction::Done));
            ctx.run_ui(key(egui::Key::Escape), |ui| out = d.show(ui.ctx())).textures_delta.clear();
            assert_eq!(out, Some(ResultAction::Done));
        }
    }

    #[test]
    fn enter_uploads_and_escape_cancels_the_picker() {
        let ctx = egui::Context::default();
        let key = |k: egui::Key| egui::RawInput { events: vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], ..Default::default() };
        let mut p = Picker::new(String::new(), "Demo");
        let mut out = None;
        ctx.run_ui(key(egui::Key::Enter), |ui| out = p.show(ui.ctx())).textures_delta.clear();
        assert!(matches!(out, Some(PickerOutcome::Upload { ref title, target: Target::New, mode: Mode::PublishNow, .. }) if title == "Demo"));
        let mut out = None;
        ctx.run_ui(key(egui::Key::Escape), |ui| out = p.show(ui.ctx())).textures_delta.clear();
        assert!(matches!(out, Some(PickerOutcome::Cancel)));
        // With no title, Enter does nothing.
        let mut p = Picker::new(String::new(), "  ");
        let mut out = None;
        ctx.run_ui(key(egui::Key::Enter), |ui| out = p.show(ui.ctx())).textures_delta.clear();
        assert!(out.is_none());
    }
}
