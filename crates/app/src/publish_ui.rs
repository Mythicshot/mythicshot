//! "Publish tour": the server and token state the editor reads, the reason the
//! export panel's button is disabled, the running publishes, and the tour picker.
//!
//! Everything that talks to the network or the credential store runs on a background
//! thread. The token is never held here: each background job loads it from the store
//! itself and drops it when done, so it never reaches the UI thread after it is saved.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::publish::{self, RemoteTour, Target};

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
    /// Upload with this title (the new tour's, or the chapter's) into this target.
    Upload { title: String, target: Target },
}

/// The "Publish tour" dialog: a new tour with an editable title, or a new chapter of
/// one of the workspace's tours, loaded on a background thread.
pub struct Picker {
    server: String,
    title: String,
    /// The recording's title, used as the chapter's title when adding to a tour.
    default_title: String,
    choice: Choice,
    list: Arc<Mutex<ListState>>,
    /// Why the last "Upload draft" could not start.
    refused: Option<String>,
}

impl Picker {
    fn new(server: String, title: &str) -> Picker {
        let title = short_title(title, TITLE_MAX);
        Picker { server, title: title.clone(), default_title: title, choice: Choice::New, list: Arc::new(Mutex::new(ListState::Loading)), refused: None }
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

    fn upload(&self) -> PickerOutcome {
        match &self.choice {
            Choice::New => PickerOutcome::Upload { title: self.title.trim().to_string(), target: Target::New },
            Choice::Tour(id) => PickerOutcome::Upload { title: self.default_title.clone(), target: Target::Chapter(id.clone()) },
        }
    }

    /// Draw the dialog; Some once the user cancels or chooses. Escape cancels and Enter
    /// is "Upload draft" when it can be used; both keys are taken from the editor.
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
            ui.label(RichText::new("Uploads as a private draft. Nothing goes public from here.").color(MUTED));
            ui.add_space(12.0);
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
                let upload = egui::Button::new(RichText::new("Upload draft").strong()).fill(PRIMARY);
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
    fn enter_uploads_and_escape_cancels_the_picker() {
        let ctx = egui::Context::default();
        let key = |k: egui::Key| egui::RawInput { events: vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], ..Default::default() };
        let mut p = Picker::new(String::new(), "Demo");
        let mut out = None;
        ctx.run_ui(key(egui::Key::Enter), |ui| out = p.show(ui.ctx())).textures_delta.clear();
        assert!(matches!(out, Some(PickerOutcome::Upload { ref title, target: Target::New }) if title == "Demo"));
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
