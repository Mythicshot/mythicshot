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

use crate::project::{self, Store};
use crate::publish::{self, Finished, Images, Outcome, RemoteTour, Target};

const MUTED: Color32 = Color32::from_rgb(163, 170, 184);
const ERROR: Color32 = Color32::from_rgb(255, 140, 140);
const PRIMARY: Color32 = Color32::from_rgb(47, 111, 235);

/// The server's cap on a tour title, in characters.
pub const TITLE_MAX: usize = 200;
/// From this many characters the title field shows its count.
const TITLE_COUNT_FROM: usize = 180;
/// Longest tour title shown in a picker row before it is cut with an ellipsis.
pub(crate) const ROW_TITLE_MAX: usize = 40;
/// Prefix of the temporary folder a publish writes its bundle into.
pub const TEMP_PREFIX: &str = "mythicshot-publish-";
/// How long "Link copied" shows after Copy.
const COPIED_FOR: Duration = Duration::from_secs(2);
/// A leftover publish folder older than this is removed at start-up.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// What a save of a recording that was cut off left behind is removed at start-up once it is this old.
const STAGING_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// What a conversion that was cut off left behind is removed at start-up once it is this old:
/// no conversion of this run has started yet, and an hour leaves alone one that another
/// Mythicshot on this computer is in the middle of.
const CONVERTING_AFTER: Duration = Duration::from_secs(60 * 60);

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
    /// The site address the saved token belongs to (as the address rule writes it); None when it is tied to none.
    saved_for: Option<String>,
    /// A new value of `saved_for` the settings file does not hold yet ("" for none).
    saved_for_change: Option<String>,
    /// The setting: an upload keeps its recording on this computer, to be edited later.
    keep_recordings: bool,
    /// The setting: how many GB the saved recordings may take before a publish says so; 0 for never.
    warn_gb: u32,
}

static CONFIG: Mutex<Config> = Mutex::new(Config { server: String::new(), test_server: None, token: TokenStatus::Checking, generation: 0, saved_for: None, saved_for_change: None, keep_recordings: true, warn_gb: 0 });

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
    config().saved_for = settings.publish_saved_for.as_deref().and_then(|a| publish::check_server(a).ok());
    set_keep_recordings(settings.keep_published_recordings);
    set_warn_gb(settings.saved_recordings_warn_gb);
    project::set_enabled(settings.store_recordings_as_video);
    std::thread::spawn(refresh_token_now);
    std::thread::spawn(|| sweep_stale(&std::env::temp_dir(), STALE_AFTER));
    // Look for ffmpeg once per run, on a thread of its own and never on the UI thread (it runs
    // the program twice): the MP4 and WebM exports are offered only when this has found one,
    // and neither it nor the tidying below waits for the other.
    std::thread::spawn(|| {
        mythicshot_recording::ffmpeg::find_now();
    });
    // The saved recordings' folder is looked at off the UI thread, whatever the switches say:
    // what an earlier run could not finish is finished either way. Then this thread is the
    // conversion queue's for the rest of the run.
    std::thread::spawn(|| {
        let store = Store::default_root().map(Store::at);
        if let Some(store) = &store {
            tidy_saved_recordings(store);
        }
        // What expansions for an editor left in the temporary folder when an earlier run was cut off.
        crate::tours_ui::sweep_stale_edits(&crate::tours_ui::edit_base(), std::process::id(), crate::tours_ui::EDIT_STALE_AFTER);
        // Answers what the search above found, once it has: the program is not run again.
        project::start(store.as_ref(), mythicshot_recording::ffmpeg::find_now(), project::gate());
    });
}

/// What a start does to the folder of saved recordings, in the order the store asks for: put
/// back a recording a cut-off save moved aside, finish removals a closed editor was holding
/// up, and remove what cut-off saves left behind a day or more ago; then finish what a swap
/// of frames for a video that was cut off left to do, and remove what cut-off conversions left
/// behind an hour or more ago. No saved recording is removed by it. Never call it on the UI
/// thread.
pub fn tidy_saved_recordings(store: &Store) {
    store.recover_replaced();
    store.finish_removals();
    store.sweep_staging(STAGING_AFTER);
    store.finish_conversions();
    store.sweep_converting(CONVERTING_AFTER);
}

/// The setting "keep the recording when I publish a tour": set at start-up and whenever the
/// settings change.
pub fn set_keep_recordings(keep: bool) {
    config().keep_recordings = keep;
}

/// Whether an upload keeps its recording on this computer.
pub fn keeps_recordings() -> bool {
    config().keep_recordings
}

/// The setting "warn me when saved recordings take more than": set at start-up and whenever
/// the settings change. 0 means no warning.
pub fn set_warn_gb(gb: u32) {
    config().warn_gb = gb;
}

/// How many GB the saved recordings may take before a publish that goes past it says so; 0
/// until the settings are read, and when no limit is set.
pub fn warn_gb() -> u32 {
    config().warn_gb
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

/// A number that changes every time the token is looked up again: at start-up and after
/// each Save and Forget. What was read from the site with an earlier number may belong to
/// another token, so the pages read it again.
pub fn token_epoch() -> u64 {
    config().generation
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

// ---- The token belongs to one site ----

/// Where to sort out a token that belongs to another address.
pub const FIX_ON_WORKSPACE: &str = "Choose on the Workspace page whether to use it for this address instead or replace it.";
pub const USE_IT_HERE: &str = "Use it for this address instead";
/// Beside that button: what pressing it does to the address the token belonged to.
pub const USE_IT_HERE_MEANS: &str = "The token will then be used only for this address, and no longer for the one it was saved for. Or replace the token above.";

/// Why the saved token may not be sent to `server`, when it may not: it was saved for
/// another address, or for none. None when it may, and for an address that is not one (the
/// address rule reports that itself). `bound` is the address the token belongs to.
pub fn binding_problem(bound: Option<&str>, server: &str) -> Option<String> {
    let server = publish::check_server(server).ok()?;
    match bound.and_then(|b| publish::check_server(b).ok()) {
        Some(b) if b == server => None,
        Some(b) => Some(match publish::host_of(&b) {
            Some(host) if publish::host_of(&server).as_deref() != Some(host.as_str()) => format!("The saved token belongs to {host}, so it is not sent to this address."),
            _ => format!("The saved token belongs to {b}, so it is not sent to this address."),
        }),
        None => Some("The saved token is not tied to a site address yet, so it is not sent anywhere.".to_string()),
    }
}

/// What a settings file from before the token was tied to an address records, once per
/// install: the address configured at this very start when it is usable, none (an empty
/// string) otherwise. It does not wait for the credential store, which may be locked or
/// slow: whatever token is in it was saved while this address was the configured one.
/// None when nothing is to be done: the adoption already ran, or the file records an address.
pub fn first_binding(settled: bool, recorded: Option<&str>, server: &str) -> Option<String> {
    if settled || recorded.is_some() {
        return None;
    }
    Some(publish::check_server(server).unwrap_or_default())
}

/// Run the one-time adoption on the settings read at start-up, and mark it as run whatever
/// it did, so that it never runs again: from then on a token with no recorded address is
/// tied to none, and only the user ties it. True when the settings changed and must be saved.
pub fn settle_first_binding(settings: &mut crate::settings::Settings) -> bool {
    if settings.publish_binding_settled {
        return false;
    }
    if let Some(site) = first_binding(false, settings.publish_saved_for.as_deref(), &settings.publish_server) {
        settings.publish_saved_for = Some(site);
    }
    settings.publish_binding_settled = true;
    true
}

/// The address the saved token belongs to. In a debug build run with the test hook, that is
/// the hook's address: it was named on purpose for this run.
pub fn token_site() -> Option<String> {
    let c = config();
    match &c.test_server {
        Some(test) => publish::check_server(test).ok(),
        None => c.saved_for.clone(),
    }
}

/// Tie the saved token to `site` (None: to no address). Called when a token is saved or
/// forgotten, and for "Use it for this address instead". The app writes it to the settings.
pub fn bind_token(site: Option<String>) {
    let mut c = config();
    c.saved_for = site.clone();
    c.saved_for_change = Some(site.unwrap_or_default());
}

/// A new binding the settings file does not hold yet, once.
pub fn take_token_site_change() -> Option<String> {
    config().saved_for_change.take()
}

pub fn has_token_site_change() -> bool {
    config().saved_for_change.is_some()
}

/// The saved token for a job that is about to send it to `server`, and to nowhere else: a
/// token saved for another address is refused here, whatever the caller checked.
pub fn token_for(server: &str) -> Result<String, String> {
    if let Some(problem) = binding_problem(token_site().as_deref(), server) {
        return Err(format!("{problem} {FIX_ON_WORKSPACE}"));
    }
    token_for_job()
}

/// The saved token, for a background job that is about to use it. A missing token and
/// an unusable store are both errors, each with its own message.
fn token_for_job() -> Result<String, String> {
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
    /// The saved token belongs to another address than the configured one.
    OtherSite(String),
}

pub const ADD_SERVER_AND_TOKEN: &str = "Add a site address and a token on the Workspace page of the main window to publish.";

/// What stops a publish, in the order the user would fix it; None when it can start.
/// `bound` is the address the saved token belongs to ([`token_site`]).
pub fn publish_blocked(exporting: bool, has_steps: bool, server: &str, token: &TokenStatus, bound: Option<&str>) -> Option<Blocked> {
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
        TokenStatus::Saved => binding_problem(bound, server).map(|p| Blocked::OtherSite(format!("{p} {FIX_ON_WORKSPACE}"))),
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
            Blocked::BadServer(e) | Blocked::Store(e) | Blocked::OtherSite(e) => Some(e.clone()),
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

/// The size of what is saved on this computer, as the Tours page and Preferences both write
/// it: KB under a megabyte, MB with one decimal, whole megabytes from a hundred up, and GB
/// with one decimal from a gigabyte.
pub fn size_text(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if bytes < 1_048_576 {
        format!("{} KB", (bytes + 512) / 1024)
    } else if mb < 99.95 {
        format!("{mb:.1} MB")
    } else if bytes < 1_073_741_824 {
        format!("{mb:.0} MB")
    } else {
        format!("{:.1} GB", mb / 1024.0)
    }
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

// ---- Keeping the recording ----

/// What became of keeping the recording of an upload on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// Saved. `left_out`: what could not be saved with it, in words for a sentence ("the cover
    /// logo", "a pasted picture"); empty when everything was. `over`: this save took the saved
    /// recordings past the limit, with what they take now in bytes and the limit in GB; `None`
    /// when it did not, also when they were past it before.
    Saved { left_out: Vec<String>, over: Option<(u64, u32)> },
    /// The setting is off: nothing was saved, and there is nothing to say.
    Off,
    /// Not saved; the reason, as the store words it (lower-case, to follow [`NOT_KEPT`]), or
    /// [`project::NOT_PUT_BACK`], a sentence that stands alone.
    Failed(String),
}

/// An upload that worked: what became of it on the site, and of its recording on this computer.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishDone {
    pub finished: Finished,
    pub kept: Kept,
}

/// Before the reason a recording could not be kept; true for a new tour and for a chapter
/// added to one. The wording is proposed and waits for the owner's approval.
pub const NOT_KEPT: &str = "The recording could not be saved on this computer, so it can't be edited later: ";
/// Before the list of what a kept recording was saved without. The wording is proposed and waits for the owner's approval.
pub const KEPT_WITHOUT: &str = "The recording was saved for later editing, but without ";
/// The reason when this computer has no folder for saved recordings: keeping is simply off.
/// The wording is proposed and waits for the owner's approval.
pub const NO_FOLDER_FOR_RECORDINGS: &str = "the folder for saved recordings could not be found on this computer.";
/// The reason when saving broke off for no reason the store could name.
/// The wording is proposed and waits for the owner's approval.
pub const KEEPING_BROKE_OFF: &str = "saving failed because of an internal error.";

/// What the project format calls a thing that could not be saved or restored, and what
/// several of them are called after their number. The wording is proposed and waits for the
/// owner's approval.
const SEVERAL: [(&str, &str); 5] = [
    ("a pasted picture", "pasted pictures"),
    ("an annotation on a frame the recording does not have", "annotations on frames the recording does not have"),
    ("an annotation that could not be read", "annotations that could not be read"),
    ("an annotation of a kind this version does not know", "annotations of a kind this version does not know"),
    ("an annotation far outside the picture", "annotations far outside the picture"),
];

/// The end of a sentence that lists `things`: "the cover logo and 2 pasted pictures.". Each
/// kind is named once, in the order it came up, with its number when there are several.
pub fn listed(things: &[String]) -> String {
    let mut kinds: Vec<(&str, usize)> = Vec::new();
    for thing in things.iter().map(|t| t.as_str()) {
        match kinds.iter_mut().find(|(kind, _)| *kind == thing) {
            Some((_, n)) => *n += 1,
            None => kinds.push((thing, 1)),
        }
    }
    let named: Vec<String> = kinds
        .iter()
        .map(|(kind, n)| match (n, SEVERAL.iter().find(|(one, _)| one == kind)) {
            (1, _) => kind.to_string(),
            (n, Some((_, several))) => format!("{n} {several}"),
            (n, None) => format!("{kind} ({n} times)"),
        })
        .collect();
    match named.split_last() {
        None => ".".to_string(),
        Some((last, [])) => format!("{last}."),
        Some((last, rest)) => format!("{} and {last}.", rest.join(", ")),
    }
}

/// Before the reason the saved recording could not be rewritten after an update: the site has
/// the changes and the recording on this computer does not. The wording is proposed and waits
/// for the owner's approval.
pub const UPDATE_NOT_SAVED: &str = "The changes were uploaded, but the recording saved on this computer could not be updated, so it no longer matches the tour: ";

/// The result screen's sentence about the recording, when there is something to say: it could
/// not be kept, or it was kept without something. After an `update` the recording was saved
/// before, so a save that failed is said as what it is: the saved one no longer matches.
pub fn kept_note(kept: &Kept, update: bool) -> Option<String> {
    match kept {
        Kept::Off => None,
        Kept::Saved { left_out, .. } if left_out.is_empty() => None,
        Kept::Saved { left_out, .. } => Some(format!("{KEPT_WITHOUT}{}", listed(left_out))),
        Kept::Failed(reason) if reason == project::NOT_PUT_BACK => Some(reason.clone()),
        Kept::Failed(reason) if update => Some(format!("{UPDATE_NOT_SAVED}{reason}")),
        Kept::Failed(reason) => Some(format!("{NOT_KEPT}{reason}")),
    }
}

/// What a save that worked says about the limit: `Some` with the new total and the limit only
/// when this save crossed it. `after` is what the saved recordings take once it is saved and
/// `added` what this save added, so what they took before is the difference; a total that was
/// past the limit already, one still within it, and no limit (0) are all `None`.
pub fn went_over(after: u64, added: u64, limit_gb: u32) -> Option<(u64, u32)> {
    project::pushed_over(after.saturating_sub(added), after, limit_gb).then_some((after, limit_gb))
}

/// The result screen's sentence about the limit, when this publish took the saved recordings
/// past it. Nothing is removed or held back because of it.
pub fn over_note(kept: &Kept) -> Option<String> {
    match kept {
        Kept::Saved { over: Some((bytes, limit_gb)), .. } => Some(project::over_limit_sentence(*bytes, *limit_gb)),
        Kept::Saved { over: None, .. } | Kept::Off | Kept::Failed(_) => None,
    }
}

// ---- The result screen ----

/// What the tour was before this upload, as far as the app knows: the result screen needs
/// it to say the truth about a tour that was already public, and to count the tour's steps.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Before {
    /// The tour was public already. An upload that is not published leaves that version live.
    pub public: bool,
    /// Its public link, when the tour list gave one.
    pub link: Option<String>,
    /// The steps it had: 0 for a new tour, None when not known.
    pub steps: Option<u32>,
}

impl Before {
    /// A tour that this upload creates.
    pub fn new_tour() -> Before {
        Before { public: false, link: None, steps: Some(0) }
    }

    /// A tour from the picker's list.
    pub fn of(tour: &RemoteTour) -> Before {
        Before { public: tour.status == "published", link: tour.link.clone(), steps: Some(tour.steps) }
    }
}

/// After an update that was uploaded and not published, whatever the reason.
const UPDATE_NOT_PUBLIC: &str = "The update is uploaded, but the public page still shows the previous version.";
/// After an update the site would not publish, because that would publish more than the update.
/// The wording is proposed and waits for the owner's approval.
pub const UPDATE_HELD: &str = "This tour has other changes that were never published, so the update was uploaded without publishing. Publish the changes from the Tours page when all of it is ready.";

/// The result screen's heading. `update` is whether a chapter was replaced over the same link
/// ("Update tour"), as opposed to a new tour or a new chapter.
pub fn result_heading(outcome: &Outcome, update: bool) -> &'static str {
    match (outcome, update) {
        (Outcome::Public { .. }, false) => "Tour published",
        (Outcome::Draft | Outcome::NotPublic { .. }, false) => "Draft uploaded",
        // Only an update is ever held back. Should one come up otherwise, it is an upload that is not public.
        (Outcome::Held, false) => "Draft uploaded",
        (Outcome::Public { .. }, true) => "Tour updated",
        (Outcome::Draft, true) => "Draft updated",
        (Outcome::NotPublic { .. }, true) => "Update uploaded",
        (Outcome::Held, true) => "Update uploaded",
    }
}

/// The step count beside the tour's title, always the tour's own when it can be known:
/// after a publish the server's count; otherwise what the tour had plus what was
/// uploaded. When what the tour had is not known, the upload's count, named as such.
/// An update replaces a chapter, so what the tour had is never added to it: without a
/// publish the count is the replaced chapter's new one, named as the chapter's.
pub fn result_steps(done: &Finished, before: &Before, update: bool) -> String {
    let n = |steps: usize| count(u32::try_from(steps).unwrap_or(u32::MAX), "step", "steps");
    match (&done.outcome, update, before.steps) {
        (Outcome::Public { .. }, _, _) => n(done.steps),
        (Outcome::Draft | Outcome::NotPublic { .. } | Outcome::Held, true, _) => format!("this chapter: {}", n(done.steps)),
        (_, false, Some(had)) => n(done.steps.saturating_add(had as usize)),
        (_, false, None) => format!("this chapter: {}", n(done.steps)),
    }
}

/// Whether the tour is public after this upload, in the line's last words. A tour that
/// was public before stays public whatever became of this upload: the server switches
/// versions only when a publish finishes. After an update the site itself has said what the
/// tour is: a draft, or public with the update waiting.
fn result_visibility(outcome: &Outcome, before: &Before, update: bool) -> &'static str {
    match (outcome, update) {
        (Outcome::Public { .. }, _) => "public",
        (Outcome::Draft, true) => "not public",
        (Outcome::NotPublic { .. } | Outcome::Held, true) => "the public version is unchanged",
        (_, false) if before.public => "the public version is unchanged",
        (_, false) => "not public",
    }
}

/// "Customer onboarding · 14 steps · public".
pub fn result_line(title: &str, done: &Finished, before: &Before, update: bool) -> String {
    format!("{} · {} · {}", short_title(title, ROW_TITLE_MAX), result_steps(done, before, update), result_visibility(&done.outcome, before, update))
}

/// Where the upload stands when it was not published, as a sentence.
fn uploaded_sentence(before: &Before, update: bool) -> &'static str {
    if update {
        UPDATE_NOT_PUBLIC
    } else if before.public {
        "The draft is uploaded; the public version is unchanged."
    } else {
        "The draft is uploaded, but it is not public."
    }
}

/// What else the result screen has to say, and whether it is an error.
pub fn result_note(outcome: &Outcome, before: &Before, update: bool) -> Option<(String, bool)> {
    match outcome {
        Outcome::Draft | Outcome::Public { url: Some(_) } => None,
        Outcome::Public { url: None } if before.link.is_some() => None,
        Outcome::Public { url: None } => Some(("The tour is public, but the server gave no link that can be shown. Look it up in the server's tour list.".to_string(), false)),
        // Not an error: the site did what is safe, and the user finishes it on the Tours page.
        Outcome::Held if update => Some((UPDATE_HELD.to_string(), false)),
        Outcome::Held => None,
        Outcome::NotPublic { cancelled: true, .. } => Some((format!("Publishing was cancelled. {}", uploaded_sentence(before, update)), false)),
        Outcome::NotPublic { cancelled: false, error } => Some((format!("{error} {}", uploaded_sentence(before, update)), true)),
    }
}

/// The link the result screen offers: the new one after a publish; for a tour that was
/// public before and still is, the link the tour list gave.
pub fn result_link<'a>(outcome: &'a Outcome, before: &'a Before) -> Option<&'a str> {
    match outcome {
        Outcome::Public { url: Some(url) } => Some(url),
        Outcome::Public { url: None } => before.link.as_deref(),
        Outcome::Draft | Outcome::NotPublic { .. } => before.link.as_deref().filter(|_| before.public),
        // Held back: the tour is public, and its link still shows the version before the update.
        Outcome::Held => before.link.as_deref().filter(|_| before.public),
    }
}

/// The one-line message for the status bar and the log. It never holds the link.
pub fn result_status(done: &Finished, before: &Before, update: bool) -> String {
    if update {
        // The count is the tour's after a publish, and the replaced chapter's new one for a draft.
        let steps = count(u32::try_from(done.steps).unwrap_or(u32::MAX), "step", "steps");
        return match &done.outcome {
            Outcome::Public { .. } => format!("Tour updated: {steps}"),
            Outcome::Draft => format!("Draft updated: {steps}"),
            Outcome::NotPublic { .. } => "Update uploaded: the public page still shows the previous version".to_string(),
            Outcome::Held => "Update uploaded: the public page still shows the previous version".to_string(),
        };
    }
    let steps = result_steps(done, before, false);
    let rest = if before.public { "the public version is unchanged" } else { "the draft is uploaded, but not public" };
    match &done.outcome {
        Outcome::Public { .. } => format!("Tour published: {steps}"),
        Outcome::Draft if before.public => format!("Tour uploaded as a draft: {steps}; the public version is unchanged"),
        Outcome::Draft => format!("Tour uploaded as a draft: {steps}"),
        // Only an update is ever held back; said as the draft upload it then is.
        Outcome::Held if before.public => format!("Tour uploaded as a draft: {steps}; the public version is unchanged"),
        Outcome::Held => format!("Tour uploaded as a draft: {steps}"),
        Outcome::NotPublic { cancelled: true, .. } => format!("Publishing cancelled: {rest}"),
        Outcome::NotPublic { cancelled: false, .. } => format!("Publishing failed: {rest}"),
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
    /// Publish the uploaded tour again, without uploading; see [`take_retry`].
    Retry,
    /// Open the public link in the browser; the screen stays.
    Open(String),
}

/// What "Try again" needs: the tour to publish, and what the next result screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    pub title: String,
    pub before: Before,
    pub tour_id: String,
    /// The chapter the upload made, as its commit answered.
    pub chapter_id: String,
    /// The steps the upload had.
    pub steps: usize,
    /// What became of the upload's recording. A retry publishes only: it saves nothing again,
    /// and its result screen says what the first one said.
    pub kept: Kept,
    /// The upload was an update of a chapter: the next result screen words it as one.
    pub update: bool,
}

/// "Try again": takes the result screen away and says what to publish, but only when
/// the publish can start. While another job runs the screen stays as it is, so the
/// result and its "Try again" are never lost.
pub fn take_retry(screen: &mut Option<ResultDialog>, job_running: bool) -> Option<Retry> {
    if job_running || !screen.as_ref().is_some_and(ResultDialog::can_retry) {
        return None;
    }
    screen.take().map(|d| Retry { title: d.title, before: d.before, tour_id: d.done.tour_id, chapter_id: d.done.chapter_id, steps: d.done.steps, kept: d.kept, update: d.update })
}

/// The screen after a publish or a draft upload: what happened, and for a public tour its link.
pub struct ResultDialog {
    title: String,
    before: Before,
    done: Finished,
    kept: Kept,
    /// A chapter was replaced over the same link, as opposed to a new tour or a new chapter.
    update: bool,
    copied: Option<Instant>,
}

impl ResultDialog {
    pub fn new(title: String, before: Before, done: PublishDone) -> ResultDialog {
        ResultDialog { title, before, done: done.finished, kept: done.kept, update: false, copied: None }
    }

    /// The screen after "Update tour", and after "Try again" for an update.
    pub fn after_update(title: String, before: Before, done: PublishDone) -> ResultDialog {
        ResultDialog { update: true, ..ResultDialog::new(title, before, done) }
    }

    /// The sentence about the recording on this computer, when the screen has one.
    fn kept_line(&self) -> Option<String> {
        kept_note(&self.kept, self.update)
    }

    /// The sentence about the limit, when this publish went past it. An update saves over a
    /// recording that was there already, so it never has one.
    fn over_line(&self) -> Option<String> {
        if self.update { None } else { over_note(&self.kept) }
    }

    fn url(&self) -> Option<&str> {
        result_link(&self.done.outcome, &self.before)
    }

    /// The link is the version published before, not something this upload made.
    fn link_is_older(&self) -> bool {
        !matches!(self.done.outcome, Outcome::Public { .. })
    }

    /// What a press on "Open in browser" asks for.
    fn open_action(&self) -> Option<ResultAction> {
        self.url().map(|u| ResultAction::Open(u.to_string()))
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
            ui.heading(result_heading(&self.done.outcome, self.update));
            ui.label(RichText::new(result_line(&self.title, &self.done, &self.before, self.update)).color(MUTED)).on_hover_text(&self.title);
            if let Some((note, is_error)) = result_note(&self.done.outcome, &self.before, self.update) {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(note).color(if is_error { ERROR } else { MUTED })).wrap());
            }
            // Not an error of the upload: the tour is on the site either way.
            if let Some(note) = self.kept_line() {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(note).color(MUTED)).wrap());
            }
            // A line of its own under it: the recording was kept, and nothing is removed.
            if let Some(note) = self.over_line() {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(note).color(MUTED)).wrap());
            }
            if let Some(url) = self.url().map(str::to_string) {
                ui.add_space(12.0);
                if self.link_is_older() {
                    ui.label(RichText::new("The public version's link:").color(MUTED).size(12.0));
                }
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
                        action = self.open_action();
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
                    action = Some(ResultAction::Retry);
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

/// The tour and chapter an editor's recording was saved for: what "Update tour" replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTarget {
    pub tour_id: String,
    pub chapter_id: String,
    pub tour_title: String,
}

/// Whether the dialog can offer "Update", from what the site said so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOffer {
    /// The site is still being asked.
    Checking,
    /// The tour is on the site, and the site can replace a chapter. `public`: the tour is public now.
    Offered { public: bool },
    /// The site is older than 0.1.4, or tells no version.
    SiteTooOld,
    /// The tour is not in the workspace's list.
    TourGone,
}

/// What the dialog offers for the tour `tour_id`. `version_known` is whether the site has
/// answered about itself (`version` is what it said, None for a site that tells none), and
/// `tours` the workspace's list once it is read. The version decides first: a site that cannot
/// replace a chapter is not asked for its list.
pub fn update_offer(version_known: bool, version: Option<&str>, tours: Option<&[RemoteTour]>, tour_id: &str) -> UpdateOffer {
    if !version_known {
        return UpdateOffer::Checking;
    }
    if !publish::supports_update(version) {
        return UpdateOffer::SiteTooOld;
    }
    match tours {
        None => UpdateOffer::Checking,
        Some(tours) => match tours.iter().find(|t| t.id == tour_id) {
            Some(t) => UpdateOffer::Offered { public: t.is_public() },
            None => UpdateOffer::TourGone,
        },
    }
}

/// `Update "Customer onboarding"`: the first choice of a dialog opened through Edit.
pub fn update_label(tour_title: &str) -> String {
    format!("Update \"{}\"", short_title(tour_title, ROW_TITLE_MAX))
}

/// The line under "Update". The second wording is proposed and waits for the owner's approval.
pub fn update_hint(public: bool) -> &'static str {
    if public { "Same link. The public page changes when the upload is complete." } else { "Same link. The tour stays a draft." }
}

pub const NEW_TOUR_LABEL: &str = "Publish as a new tour";
pub const NEW_TOUR_HINT: &str = "A new link. The existing tour is left as it is.";
pub const UPDATE_BUTTON: &str = "Update tour";
/// Why Update is not offered on an older site. The wording is proposed and waits for the owner's approval.
pub const UPDATE_NEEDS_NEWER_SITE: &str = "Updating a tour over the same link needs a site running version 0.1.4 or newer.";
/// Why Update is not offered for a tour the site does not list. The wording is proposed and waits for the owner's approval.
pub const TOUR_NOT_ON_SITE: &str = "This tour is no longer on the site, so it can only be published as a new tour.";
/// Why Update is not offered when the site address in Workspace is not the one the recording
/// was saved for: its tour may well exist, on the other site. The wording is proposed and waits
/// for the owner's approval.
pub const SAVED_FOR_ANOTHER_ADDRESS: &str = "This recording was saved for another site address, so here it can only be published as a new tour.";
/// Under the Update choice, when the cover logo could not be opened with the recording and
/// has not been added again: an update of the tour's first chapter carries the tour's
/// settings, so the tour on the site would lose its logo. The wording is proposed and waits
/// for the owner's approval.
pub const UPDATE_REMOVES_LOGO: &str = "The cover logo could not be opened with this recording, so updating removes it from the tour. Add the logo again in Tour settings to keep it.";

/// The line under the Update choice about a cover logo that is missing: only while Update is
/// offered, and only for a recording that was opened without its logo and still has none.
fn logo_warning(shown: &Shown, logo_missing: bool) -> Option<&'static str> {
    (logo_missing && matches!(shown, Shown::Update { .. })).then_some(UPDATE_REMOVES_LOGO)
}

/// Whether pressing Update again can work after an update failed with `error`: not when the
/// tour or the chapter is gone, and not on a site that cannot replace a chapter.
pub fn update_can_be_tried_again(error: &str) -> bool {
    !(publish::is_gone(error) || publish::is_chapter_gone(error) || publish::needs_newer_site(error))
}

/// Why an upload did not start, as the dialog shows it: a heading and a small line only for a
/// message that has them, around the sentence.
#[derive(Debug, PartialEq)]
struct Refusal<'a> {
    heading: Option<&'static str>,
    body: &'a str,
    note: Option<&'static str>,
}

fn refusal_parts(why: &str) -> Refusal<'_> {
    match publish::full_workspace(why) {
        Some(body) => Refusal { heading: Some(publish::FULL_HEADING), body, note: Some(publish::FULL_NOTE) },
        None => Refusal { heading: None, body: why, note: None },
    }
}

/// Draw why the last upload did not start, under the dialog's choices.
fn show_refusal(ui: &mut egui::Ui, why: &str) {
    let r = refusal_parts(why);
    ui.add_space(8.0);
    if let Some(heading) = r.heading {
        ui.add(egui::Label::new(RichText::new(heading).color(ERROR).strong()).wrap());
        ui.add_space(2.0);
    }
    ui.add(egui::Label::new(RichText::new(r.body).color(ERROR)).wrap());
    if let Some(note) = r.note {
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(note).color(MUTED).size(12.0)).wrap());
    }
}

/// The dialog to show again after a publish or an update ended with `error` and nothing
/// uploaded; None when none comes back. `left` is the dialog as the user left it when the
/// upload started. A workspace without room brings that very dialog back, with its title, its
/// choice and its mode, and the message under them: nothing is chosen for the user. Any other
/// failed update gets the dialog `again` makes for it; a cancel brings nothing back.
pub fn dialog_after_refusal(left: Option<Picker>, error: String, updating: bool, again: impl FnOnce(String) -> Option<Picker>) -> Option<Picker> {
    if error == publish::CANCELLED {
        return None;
    }
    match left.filter(|_| publish::full_workspace(&error).is_some()) {
        Some(mut picker) => {
            picker.refuse(error);
            Some(picker)
        }
        None if updating => again(error),
        None => None,
    }
}

/// The two things a dialog opened through Edit can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditChoice {
    Update,
    NewTour,
}

/// What a dialog opened through Edit shows, from what is known.
#[derive(Debug, Clone, PartialEq)]
enum Shown {
    /// The site is being asked: nothing can be chosen.
    Asking,
    /// The site could not be asked; the reason, beside "Retry". Only a new tour can be chosen.
    Failed(String),
    /// Update is offered, first and by default.
    Update { public: bool },
    /// Only a new tour is offered, with the line that says why when there is one to say.
    OnlyNew(Option<&'static str>),
}

/// What is chosen: Update when it is offered and nothing else was picked. A new tour only
/// once the user picked it, also when it is the only choice, so that Enter never makes a new
/// public tour of what was meant as an update.
fn edit_choice(shown: &Shown, picked_new: bool) -> Option<EditChoice> {
    match shown {
        Shown::Asking => None,
        Shown::Update { .. } if !picked_new => Some(EditChoice::Update),
        Shown::Update { .. } | Shown::Failed(_) | Shown::OnlyNew(_) => picked_new.then_some(EditChoice::NewTour),
    }
}

/// The main button of a dialog opened through Edit.
fn edit_button(shown: &Shown, choice: Option<EditChoice>, mode: Mode) -> &'static str {
    match (choice, shown) {
        (Some(EditChoice::Update), _) | (None, Shown::Asking) => UPDATE_BUTTON,
        (Some(EditChoice::NewTour), _) | (None, _) => mode.button(),
    }
}

/// Why a dialog opened through Edit does not offer Update, whatever the site says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    No,
    /// The sentence under the choices already says why (an update that just failed).
    Said,
    /// With this line.
    Because(&'static str),
}

/// The state of a dialog opened through Edit.
struct EditMode {
    target: EditTarget,
    /// The user picked "Publish as a new tour".
    picked_new: bool,
    block: Block,
    /// The recording was opened without its cover logo, and has none now.
    logo_missing: bool,
    /// What the site says its version is: None until it answered, Some(None) for a site that tells none.
    version: Arc<Mutex<Option<Option<String>>>>,
}

/// What the user chose in the picker.
pub enum PickerOutcome {
    Cancel,
    /// Replace the chapter `target` names with this recording, under the chapter title `title`;
    /// `before` is what the tour was, as the site's list said, and `target.tour_title` the
    /// tour's title as that list has it.
    Update { title: String, target: EditTarget, before: Before },
    /// Upload with this title (the new tour's, or the chapter's) into this target, then
    /// publish it or leave it a draft. `tour_title` names the tour on the result screen,
    /// and `before` is what that tour was.
    Upload { title: String, target: Target, mode: Mode, tour_title: String, before: Before },
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
    /// Set when the editor was opened through Edit: the dialog offers Update or a new tour.
    edit: Option<EditMode>,
}

impl Picker {
    fn new(server: String, title: &str) -> Picker {
        let title = short_title(title, TITLE_MAX);
        Picker { server, title: title.clone(), default_title: title, choice: Choice::New, mode: Mode::default(), list: Arc::new(Mutex::new(ListState::Loading)), refused: None, edit: None }
    }

    /// The dialog of an editor opened through Edit, before anything is asked of the site.
    fn new_for_edit(server: String, title: &str, target: EditTarget, block: Block) -> Picker {
        let edit = EditMode { target, picked_new: false, block, logo_missing: false, version: Arc::new(Mutex::new(None)) };
        Picker { edit: Some(edit), ..Picker::new(server, title) }
    }

    /// The dialog of an editor opened through Edit: Update first and by default, or a new
    /// tour. The site is asked for its version and its tours on a background thread.
    pub fn open_for_edit(server: String, title: String, target: EditTarget) -> Picker {
        let picker = Picker::new_for_edit(server, &title, target, Block::No);
        picker.load_for_edit();
        picker
    }

    /// The same dialog for a recording whose tour belongs to another site address than the
    /// one configured now: only a new tour is offered, with the line that says why, and the
    /// site is not asked.
    pub fn open_for_edit_elsewhere(server: String, title: String, target: EditTarget) -> Picker {
        Picker::new_for_edit(server, &title, target, Block::Because(SAVED_FOR_ANOTHER_ADDRESS))
    }

    /// Tell a dialog opened through Edit that the recording was opened without its cover logo
    /// and still has none: the Update choice then says what updating does to the tour's logo.
    pub fn with_logo_missing(mut self, missing: bool) -> Picker {
        if let Some(edit) = self.edit.as_mut() {
            edit.logo_missing = missing;
        }
        self
    }

    /// The tour the recording was saved for, as the site's list has it now; None until the
    /// list is read, and for a tour it does not hold.
    fn site_tour(&self) -> Option<RemoteTour> {
        let edit = self.edit.as_ref()?;
        match &*self.list.lock().ok()? {
            ListState::Loaded(tours) => tours.iter().find(|t| t.id == edit.target.tour_id).cloned(),
            _ => None,
        }
    }

    /// The title of the tour the recording was saved for: the one the site has now (the tour
    /// may have been renamed since the recording was saved), and until the site has said, the
    /// one saved with the recording.
    fn tour_title(&self) -> String {
        let saved = self.edit.as_ref().map(|e| e.target.tour_title.clone()).unwrap_or_default();
        self.site_tour().map(|t| t.title).filter(|t| !t.trim().is_empty()).unwrap_or(saved)
    }

    /// The line under the Update choice about a missing cover logo, when it shows.
    fn logo_line(&self, shown: &Shown) -> Option<&'static str> {
        logo_warning(shown, self.edit.as_ref().is_some_and(|e| e.logo_missing))
    }

    /// The same dialog after an update failed with `error`: it says why, and offers Update
    /// again only when pressing it again can work.
    pub fn open_after_failed_update(server: String, title: String, target: EditTarget, error: String) -> Picker {
        let mut picker = if update_can_be_tried_again(&error) { Picker::open_for_edit(server, title, target) } else { Picker::new_for_edit(server, &title, target, Block::Said) };
        picker.refuse(error);
        picker
    }

    /// Ask the site what it is and what it has, for a dialog opened through Edit. A site that
    /// cannot replace a chapter is not asked for its tours: only a new tour is offered then.
    fn load_for_edit(&self) {
        let Some(edit) = &self.edit else { return };
        if let Ok(mut l) = self.list.lock() {
            *l = ListState::Loading;
        }
        if let Ok(mut v) = edit.version.lock() {
            *v = None;
        }
        let (list, version, server) = (self.list.clone(), edit.version.clone(), self.server.clone());
        std::thread::spawn(move || {
            let result = token_for(&server).and_then(|token| {
                let told = publish::workspace(&server, &token)?.version;
                let can = publish::supports_update(told.as_deref());
                if let Ok(mut v) = version.lock() {
                    *v = Some(told);
                }
                if can { publish::list_tours(&server, &token) } else { Ok(Vec::new()) }
            });
            let state = match result {
                Ok(tours) => ListState::Loaded(tours),
                Err(e) => ListState::Failed(e),
            };
            if let Ok(mut l) = list.lock() {
                *l = state;
            }
        });
    }

    /// What a dialog opened through Edit shows now.
    fn shown(&self) -> Shown {
        let Some(edit) = &self.edit else { return Shown::OnlyNew(None) };
        match edit.block {
            Block::Said => return Shown::OnlyNew(None),
            Block::Because(why) => return Shown::OnlyNew(Some(why)),
            Block::No => {}
        }
        let version = edit.version.lock().ok().and_then(|v| v.clone());
        let Ok(list) = self.list.lock() else { return Shown::Asking };
        let tours = match &*list {
            ListState::Loaded(tours) => Some(tours.as_slice()),
            ListState::Loading | ListState::Failed(_) => None,
        };
        match (update_offer(version.is_some(), version.as_ref().and_then(|v| v.as_deref()), tours, &edit.target.tour_id), &*list) {
            (UpdateOffer::Checking, ListState::Failed(e)) => Shown::Failed(e.clone()),
            (UpdateOffer::Checking, _) => Shown::Asking,
            (UpdateOffer::Offered { public }, _) => Shown::Update { public },
            (UpdateOffer::SiteTooOld, _) => Shown::OnlyNew(Some(UPDATE_NEEDS_NEWER_SITE)),
            (UpdateOffer::TourGone, _) => Shown::OnlyNew(Some(TOUR_NOT_ON_SITE)),
        }
    }

    /// What the main button of a dialog opened through Edit would do now; None when it cannot be pressed.
    fn edit_outcome(&self, shown: &Shown) -> Option<PickerOutcome> {
        let edit = self.edit.as_ref()?;
        match edit_choice(shown, edit.picked_new)? {
            EditChoice::NewTour => (!self.title.trim().is_empty()).then(|| self.upload()),
            EditChoice::Update => {
                // What the tour is, as the list this offer came from said: its title too, which
                // the result screen shows and the saved recording takes over.
                let tour = self.site_tour()?;
                let target = EditTarget { tour_title: self.tour_title(), ..edit.target.clone() };
                Some(PickerOutcome::Update { title: self.default_title.clone(), target, before: Before::of(&tour) })
            }
        }
    }

    /// Draw the dialog of an editor opened through Edit; see [`Picker::show`].
    fn show_edit(&mut self, ctx: &egui::Context) -> Option<PickerOutcome> {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            return Some(PickerOutcome::Cancel);
        }
        let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let shown = self.shown();
        let tour_title = self.tour_title();
        let logo_line = self.logo_line(&shown);
        let choice = edit_choice(&shown, self.edit.as_ref().is_some_and(|e| e.picked_new));
        let mut outcome = None;
        let (mut retry, mut pick) = (false, None);
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(22.0);
        egui::Modal::new(egui::Id::unique("publish-tour-picker")).frame(frame).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Publish tour");
            ui.add_space(4.0);
            let hint = |ui: &mut egui::Ui, text: &str| {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.add(egui::Label::new(RichText::new(text).color(MUTED).size(12.0)).wrap());
                });
            };
            match &shown {
                Shown::Asking => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(RichText::new("Loading tours…").color(MUTED));
                    });
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                Shown::Failed(e) => {
                    ui.add(egui::Label::new(RichText::new(e).color(ERROR)).wrap());
                    if ui.button("Retry").clicked() {
                        retry = true;
                    }
                }
                Shown::OnlyNew(Some(why)) => {
                    ui.add(egui::Label::new(RichText::new(*why).color(MUTED)).wrap());
                }
                Shown::OnlyNew(None) | Shown::Update { .. } => {}
            }
            // Update, first: shown while the site is asked (it cannot be chosen yet) and when it is offered.
            if matches!(shown, Shown::Asking | Shown::Update { .. }) {
                ui.add_space(4.0);
                let offered = matches!(shown, Shown::Update { .. });
                let radio = egui::RadioButton::new(choice == Some(EditChoice::Update), update_label(&tour_title));
                if ui.add_enabled(offered, radio).on_hover_text(&tour_title).clicked() {
                    pick = Some(EditChoice::Update);
                }
                if let Shown::Update { public } = shown {
                    hint(ui, update_hint(public));
                }
                if let Some(line) = logo_line {
                    hint(ui, line);
                }
            }
            ui.add_space(8.0);
            let new_chosen = choice == Some(EditChoice::NewTour);
            if ui.add_enabled(shown != Shown::Asking, egui::RadioButton::new(new_chosen, NEW_TOUR_LABEL)).clicked() {
                pick = Some(EditChoice::NewTour);
            }
            hint(ui, NEW_TOUR_HINT);
            if new_chosen {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    let count = title_count(&self.title);
                    let width = ui.available_width() - if count.is_some() { 70.0 } else { 0.0 };
                    ui.add(egui::TextEdit::singleline(&mut self.title).char_limit(TITLE_MAX).hint_text("Tour title").desired_width(width));
                    if let Some(count) = count {
                        ui.label(RichText::new(count).color(MUTED).size(12.0));
                    }
                });
                for mode in Mode::ALL {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        if ui.radio(self.mode == mode, mode.label()).clicked() {
                            self.mode = mode;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(48.0);
                        ui.add(egui::Label::new(RichText::new(mode.hint(Goes::NewTour)).color(MUTED).size(12.0)).wrap());
                    });
                }
            }
            if let Some(why) = &self.refused {
                show_refusal(ui, why);
            }
            ui.add_space(16.0);
            let can = self.edit_outcome(&shown).is_some();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let main = egui::Button::new(RichText::new(edit_button(&shown, choice, self.mode)).strong()).fill(PRIMARY);
                if ui.add_enabled(can, main).clicked() {
                    outcome = self.edit_outcome(&shown);
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(PickerOutcome::Cancel);
                }
            });
        });
        if let (Some(pick), Some(edit)) = (pick, self.edit.as_mut()) {
            edit.picked_new = pick == EditChoice::NewTour;
        }
        if retry {
            self.load_for_edit();
        }
        if outcome.is_none() && enter && pick.is_none() && !retry {
            outcome = self.edit_outcome(&shown);
        }
        outcome
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
            let result = token_for(&server).and_then(|token| publish::list_tours(&server, &token));
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
                PickerOutcome::Upload { title: title.clone(), target: Target::New, mode: self.mode, tour_title: title, before: Before::new_tour() }
            }
            Choice::Tour(id) => {
                // A tour the list no longer holds: its title and what it was are not known.
                let tour = self.chosen_tour();
                let before = tour.as_ref().map(Before::of).unwrap_or_default();
                let tour_title = tour.map_or_else(|| self.default_title.clone(), |t| t.title);
                PickerOutcome::Upload { title: self.default_title.clone(), target: Target::Chapter(id.clone()), mode: self.mode, tour_title, before }
            }
        }
    }

    /// Draw the dialog; Some once the user cancels or chooses. Escape cancels and Enter
    /// is the main button when it can be used; both keys are taken from the editor.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<PickerOutcome> {
        if self.edit.is_some() {
            return self.show_edit(ctx);
        }
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
                show_refusal(ui, why);
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
pub(crate) mod tests {
    use super::*;

    /// `publish_blocked` with the token saved for the very address that is asked about.
    fn blocked(exporting: bool, has_steps: bool, server: &str, token: &TokenStatus) -> Option<Blocked> {
        publish_blocked(exporting, has_steps, server, token, publish::check_server(server).ok().as_deref())
    }

    #[test]
    fn the_token_is_only_for_the_address_it_was_saved_for() {
        let home = Some("https://tours.example.com");
        assert_eq!(binding_problem(home, "https://tours.example.com"), None);
        assert_eq!(binding_problem(home, " https://Tours.Example.com/ "), None, "the same address written another way");
        // Another host: a typo in the top-level domain, a look-alike, a new server.
        for other in ["https://tours.example.co", "https://tours.exarnple.com", "https://example.com", "http://localhost:8787"] {
            assert_eq!(binding_problem(home, other).as_deref(), Some("The saved token belongs to tours.example.com, so it is not sent to this address."), "{other}");
        }
        // The same host at another port or path is another address too, and is named in full.
        assert_eq!(binding_problem(home, "https://tours.example.com:8443").as_deref(), Some("The saved token belongs to https://tours.example.com, so it is not sent to this address."));
        assert!(binding_problem(home, "https://tours.example.com/demo").is_some());
        // Tied to nothing, or to something that is not an address.
        for unbound in [None, Some(""), Some("tours.example.com")] {
            assert_eq!(binding_problem(unbound, "https://tours.example.com").as_deref(), Some("The saved token is not tied to a site address yet, so it is not sent anywhere."), "{unbound:?}");
        }
        // An address that is not one is the address rule's to report.
        assert_eq!(binding_problem(home, "tours.example"), None);
        assert_eq!(binding_problem(home, ""), None);
        for s in [FIX_ON_WORKSPACE, &binding_problem(None, "https://a.example.com").unwrap()] {
            assert!(s.ends_with('.') && !s.contains('!'), "{s}");
        }
    }

    #[test]
    fn publishing_is_blocked_while_the_token_belongs_to_another_address() {
        let saved = TokenStatus::Saved;
        let b = publish_blocked(false, true, "https://tours.example.co", &saved, Some("https://tours.example.com")).expect("blocked");
        assert_eq!(b, Blocked::OtherSite(format!("The saved token belongs to tours.example.com, so it is not sent to this address. {FIX_ON_WORKSPACE}")));
        assert_eq!(b.reason(), Some(b.refusal()));
        assert!(b.refusal().contains("Workspace page"));
        assert!(matches!(publish_blocked(false, true, "https://tours.example.com", &saved, None), Some(Blocked::OtherSite(_))));
        assert_eq!(publish_blocked(false, true, "https://tours.example.com", &saved, Some("https://tours.example.com")), None);
        // What comes first is still said first.
        assert_eq!(publish_blocked(true, true, "https://tours.example.co", &saved, Some("https://tours.example.com")), Some(Blocked::Exporting));
        assert_eq!(publish_blocked(false, true, "https://tours.example.co", &TokenStatus::Missing, Some("https://tours.example.com")), Some(Blocked::NoToken));
    }

    /// The settings as the next start reads them from the file.
    fn restarted(settings: &crate::settings::Settings) -> crate::settings::Settings {
        toml::from_str(&toml::to_string_pretty(settings).unwrap()).unwrap()
    }

    /// What the pages say about the saved token and `server`, with these settings.
    fn problem(settings: &crate::settings::Settings) -> Option<String> {
        let bound = settings.publish_saved_for.as_deref().and_then(|a| publish::check_server(a).ok());
        binding_problem(bound.as_deref(), &settings.publish_server)
    }

    #[test]
    fn a_normal_upgrade_ties_the_token_to_the_configured_address() {
        // A file from before the tie: an address, no record.
        let mut s: crate::settings::Settings = toml::from_str("publish_server = \" https://Tours.Example.com/ \"\n").unwrap();
        assert!(!s.publish_binding_settled && s.publish_saved_for.is_none());
        assert!(settle_first_binding(&mut s));
        assert_eq!((s.publish_saved_for.as_deref(), s.publish_binding_settled), (Some("https://tours.example.com"), true));
        assert_eq!(problem(&s), None);
        // The next start does nothing, and the file says why.
        let mut s = restarted(&s);
        assert!(!settle_first_binding(&mut s));
        assert_eq!(s.publish_saved_for.as_deref(), Some("https://tours.example.com"));
        // A file of the version that recorded the address but had no flag yet keeps its record.
        let mut s: crate::settings::Settings = toml::from_str("publish_server = \"https://new.example.com\"\npublish_saved_for = \"https://old.example.com\"\n").unwrap();
        assert!(settle_first_binding(&mut s));
        assert_eq!((s.publish_saved_for.as_deref(), s.publish_binding_settled), (Some("https://old.example.com"), true));
        let mut s: crate::settings::Settings = toml::from_str("publish_server = \"https://new.example.com\"\npublish_saved_for = \"\"\n").unwrap();
        assert!(settle_first_binding(&mut s));
        assert_eq!(s.publish_saved_for.as_deref(), Some(""));
    }

    #[test]
    fn the_adoption_does_not_wait_for_the_token_so_a_later_address_is_never_adopted() {
        // First start: the credential store is locked or still being read. That is not asked at all.
        let mut s: crate::settings::Settings = toml::from_str("publish_server = \"https://tours.example.com\"\n").unwrap();
        assert!(settle_first_binding(&mut s));
        assert_eq!(s.publish_saved_for.as_deref(), Some("https://tours.example.com"));
        // The address is changed afterwards, and the app is started again with the store usable.
        s.publish_server = "https://tours.example.co".into();
        let mut s = restarted(&s);
        assert!(!settle_first_binding(&mut s));
        assert_eq!(s.publish_saved_for.as_deref(), Some("https://tours.example.com"), "still the address of the first start");
        assert_eq!(problem(&s).as_deref(), Some("The saved token belongs to tours.example.com, so it is not sent to this address."));
        assert!(matches!(publish_blocked(false, true, &s.publish_server, &TokenStatus::Saved, s.publish_saved_for.as_deref()), Some(Blocked::OtherSite(_))));
    }

    #[test]
    fn with_no_address_at_the_first_start_the_token_is_tied_to_none_for_good() {
        for first in ["", "http://tours.example.com", "not an address"] {
            let mut s = crate::settings::Settings::default();
            s.publish_server = first.into();
            assert!(settle_first_binding(&mut s), "{first:?}");
            assert_eq!((s.publish_saved_for.as_deref(), s.publish_binding_settled), (Some(""), true), "{first:?}");
            // An address typed afterwards is not adopted, at this start or any later one.
            s.publish_server = "https://tours.example.com".into();
            let mut s = restarted(&s);
            assert!(!settle_first_binding(&mut s));
            assert_eq!(problem(&s).as_deref(), Some("The saved token is not tied to a site address yet, so it is not sent anywhere."), "{first:?}");
        }
        // The flag alone decides: with it set and no record at all, nothing is adopted either.
        let mut s = crate::settings::Settings::default();
        (s.publish_server, s.publish_binding_settled) = ("https://tours.example.com".into(), true);
        assert!(!settle_first_binding(&mut s));
        assert_eq!(s.publish_saved_for, None);
        assert_eq!(problem(&s).as_deref(), Some("The saved token is not tied to a site address yet, so it is not sent anywhere."));
        assert_eq!(first_binding(true, None, "https://tours.example.com"), None);
        assert_eq!(first_binding(false, Some(""), "https://tours.example.com"), None);
        assert_eq!(first_binding(false, None, "https://tours.example.com").as_deref(), Some("https://tours.example.com"));
    }

    #[test]
    fn the_tie_survives_the_settings_file_and_holds_no_token() {
        // The record survives the settings file, and holds no token.
        let mut settings = crate::settings::Settings::default();
        settings.publish_binding_settled = true;
        settings.publish_saved_for = Some("https://tours.example.com".into());
        let text = toml::to_string_pretty(&settings).unwrap();
        assert!(!text.to_ascii_lowercase().contains("token"), "{text}");
        let read: crate::settings::Settings = toml::from_str(&text).unwrap();
        assert_eq!(read.publish_saved_for.as_deref(), Some("https://tours.example.com"));
        settings.publish_saved_for = Some(String::new());
        let read: crate::settings::Settings = toml::from_str(&toml::to_string_pretty(&settings).unwrap()).unwrap();
        assert_eq!(read.publish_saved_for.as_deref(), Some(""), "tied to none is a record too");
    }

    fn tour(title: &str, chapters: u32, steps: u32, status: &str) -> RemoteTour {
        RemoteTour { id: "t1".into(), title: title.into(), status: status.into(), chapters, steps, link: None, updated_at: None, published_at: None, unpublished_changes: false }
    }

    #[test]
    fn publish_is_ready_with_steps_a_server_and_a_saved_token() {
        assert_eq!(blocked(false, true, "https://tours.example.com", &TokenStatus::Saved), None);
        assert_eq!(blocked(false, true, "http://localhost:8787/", &TokenStatus::Saved), None);
    }

    #[test]
    fn each_missing_piece_has_its_own_disabled_reason() {
        let ok = "https://tours.example.com";
        // A running export or no steps: disabled, with no extra line (the status bar and the tour line say why).
        assert_eq!(blocked(true, true, ok, &TokenStatus::Saved), Some(Blocked::Exporting));
        assert_eq!(Blocked::Exporting.reason(), None);
        assert_eq!(blocked(false, false, ok, &TokenStatus::Saved), Some(Blocked::NoSteps));
        assert_eq!(Blocked::NoSteps.reason(), None);
        // No server, or no token: the approved line.
        assert_eq!(blocked(false, true, "  ", &TokenStatus::Saved), Some(Blocked::NoServer));
        assert_eq!(blocked(false, true, "", &TokenStatus::Missing), Some(Blocked::NoServer));
        assert_eq!(Blocked::NoServer.reason().as_deref(), Some(ADD_SERVER_AND_TOKEN));
        assert_eq!(blocked(false, true, ok, &TokenStatus::Missing), Some(Blocked::NoToken));
        assert_eq!(Blocked::NoToken.reason().as_deref(), Some(ADD_SERVER_AND_TOKEN));
        // A server address that cannot be used says what is wrong with it.
        let bad = blocked(false, true, "http://tours.example.com", &TokenStatus::Saved);
        assert!(matches!(&bad, Some(Blocked::BadServer(e)) if e.contains("https://")), "{bad:?}");
        assert!(bad.unwrap().reason().unwrap().contains("https://"));
        // A token still being looked up, and a locked store, which is not "no token".
        assert_eq!(blocked(false, true, ok, &TokenStatus::Checking), Some(Blocked::Checking));
        assert_eq!(Blocked::Checking.reason().as_deref(), Some("Checking for a saved token…"));
        let locked = "Could not use the credential store: the credential store is locked or not reachable.";
        let b = blocked(false, true, ok, &TokenStatus::Unavailable(locked.into()));
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
        assert!(matches!(p.upload(), PickerOutcome::Upload { mode: Mode::PublishNow, target: Target::New, ref tour_title, ref before, .. } if tour_title == "Demo" && *before == Before::new_tour()));
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
            PickerOutcome::Upload { title, target, mode, tour_title, before } => {
                assert_eq!((title.as_str(), tour_title.as_str(), mode), ("Recording", "Customer onboarding", Mode::Draft));
                assert_eq!(target, Target::Chapter("tour_bbbbbbbbbbbbbbbbbbbbbbbb".into()));
                // The result screen is told the tour was public, and how many steps it had.
                assert_eq!(before, Before { public: true, link: None, steps: Some(14) });
            }
            PickerOutcome::Cancel | PickerOutcome::Update { .. } => panic!("not an upload"),
        }
        p.choice = Choice::Tour("t1".into());
        assert!(matches!(p.upload(), PickerOutcome::Upload { before: Before { public: false, steps: Some(2), .. }, .. }));
        // A tour that is not in the list: nothing is known about it, and nothing is claimed.
        p.choice = Choice::Tour("gone".into());
        assert!(matches!(p.upload(), PickerOutcome::Upload { before: Before { public: false, link: None, steps: None }, .. }));
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

    const TOUR_ID: &str = "tour_aaaaaaaaaaaaaaaaaaaaaaaa";
    const LINK: &str = "https://tours.example.com/t/abc";

    const CHAPTER_ID: &str = "ch_0123456789abcdef01234567";

    fn finished(steps: usize, outcome: Outcome) -> Finished {
        Finished { tour_id: TOUR_ID.into(), chapter_id: CHAPTER_ID.into(), steps, outcome }
    }

    /// An upload whose recording was kept whole.
    fn done(steps: usize, outcome: Outcome) -> PublishDone {
        PublishDone { finished: finished(steps, outcome), kept: Kept::Saved { left_out: vec![], over: None } }
    }

    fn failed() -> Outcome {
        Outcome::NotPublic { cancelled: false, error: "Could not publish the tour: the server is busy (HTTP 429). Try again later.".into() }
    }

    fn cancelled() -> Outcome {
        Outcome::NotPublic { cancelled: true, error: String::new() }
    }

    #[test]
    fn the_result_screen_says_what_happened_to_a_new_tour() {
        let new = Before::new_tour();
        let public = Outcome::Public { url: Some(LINK.into()) };
        assert_eq!(result_heading(&public, false), "Tour published");
        assert_eq!(result_line("Customer onboarding", &finished(14, public.clone()), &new, false), "Customer onboarding · 14 steps · public");
        assert_eq!(result_line("One", &finished(1, public.clone()), &new, false), "One · 1 step · public");
        assert_eq!(result_note(&public, &new, false), None);
        assert_eq!(result_link(&public, &new), Some(LINK));
        assert_eq!(result_status(&finished(14, public.clone()), &new, false), "Tour published: 14 steps");

        assert_eq!(result_heading(&Outcome::Draft, false), "Draft uploaded");
        assert_eq!(result_line("Customer onboarding", &finished(14, Outcome::Draft), &new, false), "Customer onboarding · 14 steps · not public");
        assert_eq!(result_line("One", &finished(1, Outcome::Draft), &new, false), "One · 1 step · not public");
        assert_eq!(result_note(&Outcome::Draft, &new, false), None);
        assert_eq!(result_link(&Outcome::Draft, &new), None);
        assert_eq!(result_status(&finished(1, Outcome::Draft), &new, false), "Tour uploaded as a draft: 1 step");

        // A publish that failed or was cancelled after the upload: the truth is "uploaded, not public".
        assert_eq!(result_heading(&failed(), false), "Draft uploaded");
        assert_eq!(result_line("One", &finished(2, failed()), &new, false), "One · 2 steps · not public");
        assert_eq!(result_note(&failed(), &new, false), Some(("Could not publish the tour: the server is busy (HTTP 429). Try again later. The draft is uploaded, but it is not public.".to_string(), true)));
        assert_eq!(result_note(&cancelled(), &new, false), Some(("Publishing was cancelled. The draft is uploaded, but it is not public.".to_string(), false)));
        assert_eq!(result_status(&finished(2, cancelled()), &new, false), "Publishing cancelled: the draft is uploaded, but not public");
        assert_eq!(result_status(&finished(2, failed()), &new, false), "Publishing failed: the draft is uploaded, but not public");
        assert_eq!(result_link(&failed(), &new), None);

        // Public without a usable link: say so, show none.
        let no_link = Outcome::Public { url: None };
        assert_eq!(result_heading(&no_link, false), "Tour published");
        assert!(result_note(&no_link, &new, false).is_some_and(|(n, is_error)| n.contains("no link") && !is_error));
        assert_eq!(result_link(&no_link, &new), None);
        // A long title is cut.
        assert!(result_line(&"x".repeat(100), &finished(2, Outcome::Draft), &new, false).chars().count() < 70);
    }

    #[test]
    fn a_tour_that_was_public_is_never_called_not_public() {
        // A chapter of 2 steps went into a public tour of 14; the tour list gave its link.
        let was = Before { public: true, link: Some(LINK.into()), steps: Some(14) };
        for outcome in [Outcome::Draft, failed(), cancelled()] {
            let done = finished(2, outcome.clone());
            assert_eq!(result_heading(&outcome, false), "Draft uploaded");
            let line = result_line("Customer onboarding", &done, &was, false);
            assert_eq!(line, "Customer onboarding · 16 steps · the public version is unchanged", "{outcome:?}");
            assert!(!line.contains("not public"));
            // The tour is public, so its link is still offered.
            assert_eq!(result_link(&outcome, &was), Some(LINK), "{outcome:?}");
            let status = result_status(&done, &was, false);
            assert!(status.contains("the public version is unchanged") && !status.contains("not public"), "{status}");
            assert!(!status.contains("https://"), "the link stays out of the status bar and the log");
        }
        assert_eq!(result_note(&Outcome::Draft, &was, false), None);
        assert_eq!(result_note(&failed(), &was, false), Some(("Could not publish the tour: the server is busy (HTTP 429). Try again later. The draft is uploaded; the public version is unchanged.".to_string(), true)));
        assert_eq!(result_note(&cancelled(), &was, false), Some(("Publishing was cancelled. The draft is uploaded; the public version is unchanged.".to_string(), false)));
        assert_eq!(result_status(&finished(2, Outcome::Draft), &was, false), "Tour uploaded as a draft: 16 steps; the public version is unchanged");

        // The list gave no link (today's server gives none): the words are the same, no link is shown.
        let no_link = Before { link: None, ..was.clone() };
        assert_eq!(result_line("T", &finished(2, Outcome::Draft), &no_link, false), "T · 16 steps · the public version is unchanged");
        assert_eq!(result_link(&Outcome::Draft, &no_link), None);
        // A link of a tour that is not public is never offered, whatever the list said.
        let draft_tour = Before { public: false, link: Some(LINK.into()), steps: Some(14) };
        assert_eq!(result_link(&Outcome::Draft, &draft_tour), None);
        assert_eq!(result_line("T", &finished(2, failed()), &draft_tour, false), "T · 16 steps · not public");

        // Republished: the new version is public, with the server's link, or the list's when it gave none.
        let republished = Outcome::Public { url: Some("https://tours.example.com/t/new".into()) };
        assert_eq!(result_line("T", &finished(16, republished.clone()), &was, false), "T · 16 steps · public");
        assert_eq!(result_link(&republished, &was), Some("https://tours.example.com/t/new"));
        assert_eq!(result_link(&Outcome::Public { url: None }, &was), Some(LINK));
        assert_eq!(result_note(&Outcome::Public { url: None }, &was, false), None);
    }

    #[test]
    fn the_step_count_is_the_tours_or_says_it_is_the_chapters() {
        // A new tour: what was uploaded is the tour.
        assert_eq!(result_steps(&finished(3, Outcome::Draft), &Before::new_tour(), false), "3 steps");
        // A chapter of 2 into a tour of 14: the tour has 16, published or not.
        let had = Before { public: false, link: None, steps: Some(14) };
        assert_eq!(result_steps(&finished(2, Outcome::Draft), &had, false), "16 steps");
        assert_eq!(result_steps(&finished(2, failed()), &had, false), "16 steps");
        // After a publish the server's count is the tour's; nothing is added to it.
        assert_eq!(result_steps(&finished(16, Outcome::Public { url: None }), &had, false), "16 steps");
        assert_eq!(result_steps(&finished(1, Outcome::Public { url: None }), &Before::new_tour(), false), "1 step");
        // What the tour had is not known: the count is named as the chapter's.
        let unknown = Before::default();
        assert_eq!(result_steps(&finished(2, Outcome::Draft), &unknown, false), "this chapter: 2 steps");
        assert_eq!(result_steps(&finished(1, cancelled()), &unknown, false), "this chapter: 1 step");
        assert_eq!(result_line("T", &finished(1, Outcome::Draft), &unknown, false), "T · this chapter: 1 step · not public");
        assert_eq!(result_steps(&finished(5, Outcome::Public { url: None }), &unknown, false), "5 steps");
    }

    #[test]
    fn try_again_keeps_the_result_screen_while_another_job_runs() {
        let was = Before { public: true, link: Some(LINK.into()), steps: Some(14) };
        let mut screen = Some(ResultDialog::new("Demo".into(), was.clone(), done(2, failed())));
        // A job is running: nothing starts, and the screen with its "Try again" stays.
        assert_eq!(take_retry(&mut screen, true), None);
        assert!(screen.as_ref().is_some_and(ResultDialog::can_retry));
        // Free: the screen goes and the retry names the uploaded tour, with what the next screen needs.
        assert_eq!(take_retry(&mut screen, false), Some(Retry { title: "Demo".into(), before: was.clone(), tour_id: TOUR_ID.into(), chapter_id: CHAPTER_ID.into(), steps: 2, kept: Kept::Saved { left_out: vec![], over: None }, update: false }));
        assert!(screen.is_none());
        assert_eq!(take_retry(&mut screen, false), None);
        // What the first screen said about the recording goes along, so the next one says it too.
        let not_kept = Kept::Failed("the disk is full.".into());
        let mut screen = Some(ResultDialog::new("Demo".into(), was.clone(), PublishDone { finished: finished(2, failed()), kept: not_kept.clone() }));
        assert_eq!(take_retry(&mut screen, false).map(|r| (r.chapter_id, r.kept)), Some((CHAPTER_ID.to_string(), not_kept)));
        // A screen that offers no "Try again" is never taken away by one.
        for outcome in [Outcome::Draft, cancelled(), Outcome::Public { url: Some(LINK.into()) }] {
            let mut screen = Some(ResultDialog::new("Demo".into(), was.clone(), done(2, outcome)));
            assert_eq!(take_retry(&mut screen, false), None);
            assert!(screen.is_some());
        }
    }

    #[test]
    fn the_result_screen_says_when_the_recording_could_not_be_kept() {
        let screen = |kept: Kept| ResultDialog::new("Demo".into(), Before::new_tour(), PublishDone { finished: finished(2, Outcome::Public { url: Some(LINK.into()) }), kept });
        // Kept whole, or the switch is off: nothing to say.
        assert_eq!(screen(Kept::Saved { left_out: vec![], over: None }).kept_line(), None);
        assert_eq!(screen(Kept::Off).kept_line(), None);
        // Not kept: one sentence with the store's reason after the colon.
        let failed_to_keep = screen(Kept::Failed(project::DISK_FULL.into()));
        assert_eq!(failed_to_keep.kept_line().as_deref(), Some("The recording could not be saved on this computer, so it can't be edited later: the disk is full."));
        // The upload itself is reported as it went: a kept recording is no part of that.
        assert_eq!(result_heading(&failed_to_keep.done.outcome, false), "Tour published");
        assert!(!failed_to_keep.can_retry());
        // No folder on this computer: the same sentence, with that reason.
        assert_eq!(screen(Kept::Failed(NO_FOLDER_FOR_RECORDINGS.into())).kept_line().as_deref(), Some("The recording could not be saved on this computer, so it can't be edited later: the folder for saved recordings could not be found on this computer."));
        assert_eq!(screen(Kept::Failed(project::SAVING_CANCELLED.into())).kept_line().as_deref(), Some("The recording could not be saved on this computer, so it can't be edited later: saving was cancelled."));
        assert_eq!(screen(Kept::Failed(KEEPING_BROKE_OFF.into())).kept_line().as_deref(), Some("The recording could not be saved on this computer, so it can't be edited later: saving failed because of an internal error."));
        // The one reason that is a sentence of its own is shown on its own.
        assert_eq!(screen(Kept::Failed(project::NOT_PUT_BACK.into())).kept_line().as_deref(), Some(project::NOT_PUT_BACK));
        // Kept, but without something: a full sentence that says how many of a kind.
        let without = screen(Kept::Saved { left_out: vec!["the cover logo".into(), "a pasted picture".into(), "a pasted picture".into()], over: None });
        assert_eq!(without.kept_line().as_deref(), Some("The recording was saved for later editing, but without the cover logo and 2 pasted pictures."));
        assert_eq!(screen(Kept::Saved { left_out: vec!["a pasted picture".into()], over: None }).kept_line().as_deref(), Some("The recording was saved for later editing, but without a pasted picture."));
        // Every case draws, with and without a link, and after a publish that failed.
        let ctx = egui::Context::default();
        for kept in [Kept::Off, Kept::Saved { left_out: vec!["the cover logo".into()], over: None }, Kept::Failed("the disk is full.".into())] {
            for outcome in [Outcome::Public { url: Some(LINK.into()) }, Outcome::Draft, failed()] {
                let mut d = ResultDialog::new("Demo".into(), Before::new_tour(), PublishDone { finished: finished(2, outcome), kept: kept.clone() });
                let mut out = None;
                ctx.run_ui(egui::RawInput::default(), |ui| out = d.show(ui.ctx())).textures_delta.clear();
                assert_eq!(out, None);
            }
        }
    }

    #[test]
    fn a_list_names_each_kind_once_and_says_how_many() {
        let list = |things: &[&str]| listed(&things.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        assert_eq!(list(&["the cover logo"]), "the cover logo.");
        assert_eq!(list(&["the cover logo", "a pasted picture"]), "the cover logo and a pasted picture.");
        assert_eq!(list(&["a pasted picture", "the cover logo", "a pasted picture", "an annotation that could not be read"]), "2 pasted pictures, the cover logo and an annotation that could not be read.");
        assert_eq!(list(&["an annotation on a frame the recording does not have"; 3]), "3 annotations on frames the recording does not have.");
        assert_eq!(list(&["an annotation of a kind this version does not know"; 2]), "2 annotations of a kind this version does not know.");
        assert_eq!(list(&["an annotation far outside the picture"; 2]), "2 annotations far outside the picture.");
        assert_eq!(list(&["an annotation that could not be read"; 12]), "12 annotations that could not be read.");
        // A kind this list has no word for is still counted.
        assert_eq!(list(&["a thing", "a thing"]), "a thing (2 times).");
        // Every kind the project format can name is known here by exactly its words.
        let format = include_str!("project/state.rs");
        for (one, _) in SEVERAL {
            assert!(format.contains(&format!("\"{one}\"")), "the project format no longer says: {one}");
        }
        let mut said: Vec<&str> = format.split("dropped.push(\"").skip(1).filter_map(|rest| rest.split('"').next()).collect();
        said.sort();
        said.dedup();
        let mut known: Vec<&str> = SEVERAL.iter().map(|(one, _)| *one).chain(["the cover logo"]).collect();
        known.sort();
        assert_eq!(said, known, "a new kind needs its word for several (the cover logo is only ever one)");
    }

    /// Tests that change the shared switch take this first, so they do not cross.
    pub(crate) static SWITCH: Mutex<()> = Mutex::new(());

    #[test]
    fn keeping_recordings_is_on_until_the_setting_says_otherwise() {
        // It leaves the shared switch as it found it.
        let _alone = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
        let was = keeps_recordings();
        set_keep_recordings(false);
        assert!(!keeps_recordings());
        set_keep_recordings(true);
        assert!(keeps_recordings());
        set_keep_recordings(was);
    }

    // ---- The notice after a publish that went past the limit ----

    const GB: u64 = 1 << 30;
    const OVER_5: &str = "Saved recordings take 5.4 GB, more than the 5 GB you set. Nothing is removed automatically: remove recordings you no longer need in Preferences.";

    /// A recording kept whole by a publish that took the saved recordings to 5.4 GB with a limit of 5.
    fn kept_past_the_limit() -> Kept {
        Kept::Saved { left_out: vec![], over: Some((5_798_205_850, 5)) }
    }

    #[test]
    fn the_limit_is_the_settings_until_they_change() {
        // It leaves the shared setting as it found it.
        let _alone = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
        let was = warn_gb();
        for gb in [5, 0, 1, 9999] {
            set_warn_gb(gb);
            assert_eq!(warn_gb(), gb);
        }
        set_warn_gb(was);
    }

    #[test]
    fn the_result_screen_says_when_this_publish_went_past_the_limit() {
        let screen = |kept: Kept, outcome: Outcome| ResultDialog::new("Demo".into(), Before::new_tour(), PublishDone { finished: finished(2, outcome), kept });
        let public = || Outcome::Public { url: Some(LINK.into()) };
        // Its own line, and no line about the recording itself: that was kept whole.
        let over = screen(kept_past_the_limit(), public());
        assert_eq!((over.over_line().as_deref(), over.kept_line()), (Some(OVER_5), None));
        // Kept without something and past the limit: two lines, neither changed by the other.
        let both = screen(Kept::Saved { left_out: vec!["the cover logo".into()], over: Some((5_798_205_850, 5)) }, public());
        assert_eq!(both.kept_line().as_deref(), Some("The recording was saved for later editing, but without the cover logo."));
        assert_eq!(both.over_line().as_deref(), Some(OVER_5));
        // Whatever became of the publish step, the recording was saved all the same; the upload
        // is reported as it went, and the screen draws with the line on it.
        let ctx = egui::Context::default();
        for outcome in [public(), Outcome::Public { url: None }, Outcome::Draft, failed(), cancelled()] {
            let mut d = screen(kept_past_the_limit(), outcome.clone());
            assert_eq!(d.over_line().as_deref(), Some(OVER_5), "{outcome:?}");
            assert_eq!(result_heading(&d.done.outcome, false), result_heading(&outcome, false));
            assert_eq!(d.can_retry(), matches!(outcome, Outcome::NotPublic { cancelled: false, .. }));
            let mut out = None;
            ctx.run_ui(egui::RawInput::default(), |ui| out = d.show(ui.ctx())).textures_delta.clear();
            assert_eq!(out, None, "{outcome:?}");
        }
        // "Try again" publishes only; its result screen says what the first one said.
        let mut first = Some(screen(kept_past_the_limit(), failed()));
        let retry = take_retry(&mut first, false).unwrap();
        assert_eq!(retry.kept, kept_past_the_limit());
        let again = ResultDialog::new(retry.title, retry.before, PublishDone { finished: finished(2, public()), kept: retry.kept });
        assert_eq!(again.over_line().as_deref(), Some(OVER_5));
        // An update saves over a recording that was there already: its screen never has the line.
        let update = ResultDialog::after_update("Demo".into(), Before::new_tour(), PublishDone { finished: finished(2, public()), kept: kept_past_the_limit() });
        assert_eq!(update.over_line(), None);
    }

    #[test]
    fn it_says_nothing_when_the_total_was_already_over_or_is_still_under_or_no_limit_is_set() {
        const MB: u64 = 1 << 20;
        // Under or exactly at the limit before, past it now: this publish did it.
        assert_eq!(went_over(5 * GB + 100 * MB, 172 * MB, 5), Some((5 * GB + 100 * MB, 5)));
        assert_eq!(went_over(5 * GB + 1, 1, 5), Some((5 * GB + 1, 5)), "exactly at the limit was not past it");
        assert_eq!(went_over(6 * GB, 6 * GB, 5), Some((6 * GB, 5)), "the first recording alone");
        assert_eq!(went_over(GB + 1, 2, 1), Some((GB + 1, 1)));
        // Already past it before this publish: said in Preferences, not here again.
        assert_eq!(went_over(6 * GB, 172 * MB, 5), None);
        assert_eq!(went_over(5 * GB + 2, 1, 5), None);
        // Still within it, also when exactly reaching it.
        assert_eq!(went_over(2 * GB, 172 * MB, 5), None);
        assert_eq!(went_over(5 * GB, 172 * MB, 5), None);
        assert_eq!(went_over(0, 0, 5), None);
        // No limit: never, whatever the recordings take.
        for (after, added) in [(0, 0), (6 * GB, 172 * MB), (u64::MAX, 1), (u64::MAX, u64::MAX)] {
            assert_eq!(went_over(after, added, 0), None);
        }
        // A save said to have added more than there is: before it there was nothing, never less.
        assert_eq!(went_over(6 * GB, u64::MAX, 5), Some((6 * GB, 5)));
        assert_eq!(went_over(GB, u64::MAX, 5), None);
        // The largest numbers do not overflow.
        assert_eq!(went_over(u64::MAX, 1, u32::MAX), None);
        assert_eq!(went_over(u64::MAX, u64::MAX, u32::MAX), Some((u64::MAX, u32::MAX)));
        // And a recording that was not saved, or one saved without crossing, has no sentence.
        for kept in [Kept::Off, Kept::Failed("the disk is full.".into()), Kept::Failed(project::NOT_PUT_BACK.into()), Kept::Saved { left_out: vec![], over: None }, Kept::Saved { left_out: vec!["the cover logo".into()], over: None }] {
            assert_eq!(over_note(&kept), None, "{kept:?}");
            let screen = ResultDialog::new("Demo".into(), Before::new_tour(), PublishDone { finished: finished(2, Outcome::Draft), kept });
            assert_eq!(screen.over_line(), None);
        }
    }

    #[test]
    fn the_sentence_names_the_total_and_the_limit_and_that_nothing_is_removed() {
        assert_eq!(over_note(&kept_past_the_limit()).as_deref(), Some(OVER_5));
        // The total as sizes are written everywhere else, and the limit as it was set.
        assert_eq!(
            over_note(&Kept::Saved { left_out: vec![], over: Some((1_288_490_189, 1)) }).as_deref(),
            Some("Saved recordings take 1.2 GB, more than the 1 GB you set. Nothing is removed automatically: remove recordings you no longer need in Preferences.")
        );
        // It is the sentence Preferences shows past the limit, word for word.
        for (bytes, gb) in [(5_798_205_850, 5), (GB + 1, 1), (10_000 * GB, 9999)] {
            let note = over_note(&Kept::Saved { left_out: vec![], over: Some((bytes, gb)) }).unwrap();
            assert_eq!(note, project::over_limit_sentence(bytes, gb));
            assert!(note.contains(&size_text(bytes)) && note.contains(&format!("the {gb} GB you set")) && note.contains("Nothing is removed automatically"), "{note}");
        }
        // What the recording was saved without is the other line's business.
        assert_eq!(over_note(&Kept::Saved { left_out: vec!["a pasted picture".into()], over: Some((5_798_205_850, 5)) }).as_deref(), Some(OVER_5));
    }

    #[test]
    fn a_start_finishes_what_a_cut_off_save_left_and_removes_no_recording() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("projects"));
        // No folder yet: nothing happens, and none is made.
        tidy_saved_recordings(&store);
        assert!(!store.root().exists());
        // A staging folder from two days ago goes; one from now, and anything else, stays.
        std::fs::create_dir_all(store.root()).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        let old = store.root().join(format!(".staging-{}-0123456789abcdef", now - 2 * 24 * 60 * 60 * 1000));
        let fresh = store.root().join(format!(".staging-{now}-0123456789abcdef"));
        let other = store.root().join("mine");
        for dir in [&old, &fresh, &other] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("frames.zst"), b"x").unwrap();
        }
        tidy_saved_recordings(&store);
        assert!(!old.exists() && fresh.exists() && other.join("frames.zst").exists());
    }

    #[test]
    fn open_in_browser_asks_for_the_shown_link_and_nothing_else() {
        let was = Before { public: true, link: Some(LINK.into()), steps: Some(14) };
        let d = ResultDialog::new("Demo".into(), Before::new_tour(), done(2, Outcome::Public { url: Some("https://x.dev/t/a".into()) }));
        assert_eq!(d.open_action(), Some(ResultAction::Open("https://x.dev/t/a".into())));
        assert!(!d.link_is_older());
        // A draft in a tour that is public: the older version's link, marked as such.
        let d = ResultDialog::new("Demo".into(), was, done(2, Outcome::Draft));
        assert_eq!(d.open_action(), Some(ResultAction::Open(LINK.into())));
        assert!(d.link_is_older());
        // No link, nothing to open.
        let d = ResultDialog::new("Demo".into(), Before::new_tour(), done(2, Outcome::Draft));
        assert_eq!(d.open_action(), None);
        // Whatever is opened passes the opener's own rule.
        assert!(mythicshot_destinations::is_web_link(LINK));
    }

    #[test]
    fn only_a_failed_publish_can_be_tried_again() {
        let screen = |outcome| ResultDialog::new("Demo".into(), Before::new_tour(), done(2, outcome));
        assert!(screen(failed()).can_retry());
        assert!(!screen(cancelled()).can_retry());
        assert!(!screen(Outcome::Draft).can_retry());
        assert!(!screen(Outcome::Public { url: Some(LINK.into()) }).can_retry());
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
        let was = Before { public: true, link: Some(LINK.into()), steps: Some(14) };
        let cases = [Outcome::Public { url: Some("https://x.dev/t/a".into()) }, Outcome::Public { url: None }, Outcome::Draft, failed(), cancelled()];
        for (outcome, before) in cases.iter().cloned().map(|o| (o, Before::new_tour())).chain(cases.iter().cloned().map(|o| (o, was.clone()))) {
            let mut d = ResultDialog::new("Demo".into(), before, done(2, outcome));
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

    // ---- The dialog of an editor opened through Edit ----

    fn edit_target() -> EditTarget {
        EditTarget { tour_id: TOUR_ID.into(), chapter_id: CHAPTER_ID.into(), tour_title: "Customer onboarding".into() }
    }

    /// The tour the editor's recording was saved for, as the site lists it.
    fn listed_tour(status: &str) -> RemoteTour {
        RemoteTour { id: TOUR_ID.into(), link: (status == "published").then(|| LINK.to_string()), ..tour("Customer onboarding", 2, 14, status) }
    }

    /// A dialog opened through Edit, with what the site said put in place by hand: nothing is asked of any site.
    fn edit_picker(version: Option<Option<&str>>, tours: Option<Vec<RemoteTour>>) -> Picker {
        let p = Picker::new_for_edit(String::new(), "Chapter one", edit_target(), Block::No);
        *p.edit.as_ref().unwrap().version.lock().unwrap() = version.map(|v| v.map(str::to_string));
        if let Some(tours) = tours {
            *p.list.lock().unwrap() = ListState::Loaded(tours);
        }
        p
    }

    fn pick_new(p: &mut Picker) {
        p.edit.as_mut().unwrap().picked_new = true;
    }

    fn key_press(k: egui::Key) -> egui::RawInput {
        egui::RawInput { events: vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], ..Default::default() }
    }

    fn draw(p: &mut Picker, input: egui::RawInput) -> Option<PickerOutcome> {
        let ctx = egui::Context::default();
        let mut out = None;
        ctx.run_ui(input, |ui| out = p.show(ui.ctx())).textures_delta.clear();
        out
    }

    #[test]
    fn update_is_the_first_and_default_choice_and_the_button_says_update_tour() {
        let mut p = edit_picker(Some(Some("0.1.4")), Some(vec![tour("Another", 1, 3, "draft"), listed_tour("published")]));
        let shown = p.shown();
        assert_eq!(shown, Shown::Update { public: true });
        // Nothing was picked: Update is chosen, and the button says so.
        let choice = edit_choice(&shown, false);
        assert_eq!(choice, Some(EditChoice::Update));
        assert_eq!(edit_button(&shown, choice, p.mode), "Update tour");
        assert_eq!(update_label("Customer onboarding"), "Update \"Customer onboarding\"");
        assert_eq!(update_label(&"x".repeat(100)).chars().count(), "Update \"\"".chars().count() + ROW_TITLE_MAX);
        // The button replaces exactly the tour and chapter the recording was saved for, and
        // the result screen is told what the tour was.
        match p.edit_outcome(&shown) {
            Some(PickerOutcome::Update { title, target, before }) => {
                assert_eq!((title.as_str(), &target), ("Chapter one", &edit_target()));
                assert_eq!(before, Before { public: true, link: Some(LINK.into()), steps: Some(14) });
            }
            _ => panic!("the button does not update"),
        }
        assert!(draw(&mut p, egui::RawInput::default()).is_none(), "it stays until the user chooses");
        // The second choice, once picked: the existing title field and Publish or Upload draft.
        pick_new(&mut p);
        let choice = edit_choice(&shown, true);
        assert_eq!(choice, Some(EditChoice::NewTour));
        assert_eq!(edit_button(&shown, choice, Mode::PublishNow), "Publish");
        assert_eq!(edit_button(&shown, choice, Mode::Draft), "Upload draft");
        assert!(matches!(p.edit_outcome(&shown), Some(PickerOutcome::Upload { target: Target::New, mode: Mode::PublishNow, ref title, ref tour_title, ref before }) if title == "Chapter one" && tour_title == "Chapter one" && *before == Before::new_tour()));
        p.mode = Mode::Draft;
        assert!(matches!(p.edit_outcome(&shown), Some(PickerOutcome::Upload { target: Target::New, mode: Mode::Draft, .. })));
        // A new tour needs a title; an update does not ask for one.
        p.title = "  ".into();
        assert!(p.edit_outcome(&shown).is_none());
        p.edit.as_mut().unwrap().picked_new = false;
        assert!(matches!(p.edit_outcome(&shown), Some(PickerOutcome::Update { .. })));
        // Never a new chapter of some tour: that list is not part of this dialog.
        assert_eq!(p.choice, Choice::New);
        assert!(draw(&mut p, egui::RawInput::default()).is_none());
    }

    #[test]
    fn the_hints_say_same_link_and_new_link() {
        assert_eq!(update_hint(true), "Same link. The public page changes when the upload is complete.");
        assert_eq!(NEW_TOUR_LABEL, "Publish as a new tour");
        assert_eq!(NEW_TOUR_HINT, "A new link. The existing tour is left as it is.");
        assert_eq!(UPDATE_BUTTON, "Update tour");
        for s in [update_hint(true), update_hint(false), NEW_TOUR_HINT, UPDATE_NEEDS_NEWER_SITE, TOUR_NOT_ON_SITE, SAVED_FOR_ANOTHER_ADDRESS, UPDATE_REMOVES_LOGO, UPDATE_HELD] {
            assert!(s.ends_with('.') && !s.contains('!'), "{s}");
        }
    }

    #[test]
    fn a_site_older_than_0_1_4_or_one_that_tells_no_version_offers_only_a_new_tour_and_says_why() {
        let tours = vec![listed_tour("published")];
        for version in [Some("0.1.3"), Some("0.1.0"), None, Some("abc"), Some("")] {
            assert_eq!(update_offer(true, version, Some(&tours), TOUR_ID), UpdateOffer::SiteTooOld, "{version:?}");
            // The version decides before the list is read.
            assert_eq!(update_offer(true, version, None, TOUR_ID), UpdateOffer::SiteTooOld, "{version:?}");
            let mut p = edit_picker(Some(version), Some(vec![]));
            let shown = p.shown();
            assert_eq!(shown, Shown::OnlyNew(Some("Updating a tour over the same link needs a site running version 0.1.4 or newer.")));
            // Update cannot be chosen or pressed; the one choice left is the user's to pick.
            assert_eq!(edit_choice(&shown, false), None);
            assert!(p.edit_outcome(&shown).is_none());
            assert_eq!(edit_button(&shown, None, p.mode), "Publish");
            assert!(draw(&mut p, key_press(egui::Key::Enter)).is_none(), "Enter never makes a new tour of what was opened to update");
            pick_new(&mut p);
            assert!(matches!(p.edit_outcome(&shown), Some(PickerOutcome::Upload { target: Target::New, .. })));
            assert!(matches!(draw(&mut p, key_press(egui::Key::Enter)), Some(PickerOutcome::Upload { target: Target::New, .. })));
        }
        for version in ["0.1.4", "0.1.5", "0.2.0", "1.0.0"] {
            assert_eq!(update_offer(true, Some(version), Some(&tours), TOUR_ID), UpdateOffer::Offered { public: true }, "{version}");
        }
    }

    #[test]
    fn a_tour_that_is_no_longer_on_the_site_offers_only_a_new_tour_and_says_why() {
        let others = vec![tour("Another", 1, 3, "published")];
        assert_eq!(update_offer(true, Some("0.1.4"), Some(&others), TOUR_ID), UpdateOffer::TourGone);
        assert_eq!(update_offer(true, Some("0.1.4"), Some(&[]), TOUR_ID), UpdateOffer::TourGone);
        let mut p = edit_picker(Some(Some("0.1.4")), Some(others));
        let shown = p.shown();
        assert_eq!(shown, Shown::OnlyNew(Some("This tour is no longer on the site, so it can only be published as a new tour.")));
        assert!(p.edit_outcome(&shown).is_none());
        assert!(draw(&mut p, key_press(egui::Key::Enter)).is_none());
        pick_new(&mut p);
        assert!(matches!(draw(&mut p, key_press(egui::Key::Enter)), Some(PickerOutcome::Upload { target: Target::New, .. })));
    }

    #[test]
    fn a_recording_saved_for_another_site_address_says_so_and_offers_only_a_new_tour() {
        // The address in Workspace is not the one the recording was saved for: nothing is known
        // about its tour, so the dialog does not claim that it is gone. No site is asked at all.
        let mut p = Picker::open_for_edit_elsewhere(String::new(), "Chapter one".into(), edit_target());
        let shown = p.shown();
        assert_eq!(shown, Shown::OnlyNew(Some("This recording was saved for another site address, so here it can only be published as a new tour.")));
        assert_ne!(SAVED_FOR_ANOTHER_ADDRESS, TOUR_NOT_ON_SITE);
        assert!(matches!(*p.list.lock().unwrap(), ListState::Loading), "nothing was loaded, and nothing is being");
        assert!(p.edit_outcome(&shown).is_none());
        assert!(draw(&mut p, key_press(egui::Key::Enter)).is_none());
        // Even a list that names the tour does not bring Update back.
        *p.edit.as_ref().unwrap().version.lock().unwrap() = Some(Some("0.1.4".into()));
        *p.list.lock().unwrap() = ListState::Loaded(vec![listed_tour("published")]);
        assert_eq!(p.shown(), Shown::OnlyNew(Some(SAVED_FOR_ANOTHER_ADDRESS)));
        // A new tour is still the user's to pick.
        pick_new(&mut p);
        assert!(matches!(draw(&mut p, key_press(egui::Key::Enter)), Some(PickerOutcome::Upload { target: Target::New, .. })));
    }

    #[test]
    fn a_tour_renamed_on_the_site_is_named_as_the_site_names_it() {
        // The recording was saved when the tour was "Customer onboarding"; the site calls it otherwise now.
        let renamed = RemoteTour { title: "Onboarding, spring edition".into(), ..listed_tour("published") };
        let mut p = edit_picker(Some(Some("0.1.4")), Some(vec![tour("Another", 1, 3, "draft"), renamed]));
        let shown = p.shown();
        assert_eq!(p.tour_title(), "Onboarding, spring edition");
        assert_eq!(update_label(&p.tour_title()), "Update \"Onboarding, spring edition\"");
        // Update carries the site's title on: to the result screen, and into the saved recording.
        let Some(PickerOutcome::Update { title, target, before }) = p.edit_outcome(&shown) else { panic!("the button does not update") };
        assert_eq!(title, "Chapter one", "the chapter's title is the recording's own");
        assert_eq!(target, EditTarget { tour_title: "Onboarding, spring edition".into(), ..edit_target() }, "the same tour and chapter, under the site's title");
        let done = PublishDone { finished: Finished { tour_id: TOUR_ID.into(), chapter_id: CHAPTER_ID.into(), steps: 14, outcome: Outcome::Public { url: Some(LINK.into()) } }, kept: Kept::Saved { left_out: vec![], over: None } };
        let screen = ResultDialog::after_update(target.tour_title.clone(), before.clone(), done.clone());
        assert_eq!(result_line(&screen.title, &screen.done, &screen.before, true), "Onboarding, spring edition · 14 steps · public");
        assert!(draw(&mut p, egui::RawInput::default()).is_none());

        // Until the site has said, and when it does not list the tour or gives it no title: the saved one.
        assert_eq!(edit_picker(None, None).tour_title(), "Customer onboarding");
        assert_eq!(edit_picker(Some(Some("0.1.4")), Some(vec![tour("Another", 1, 3, "draft")])).tour_title(), "Customer onboarding");
        let blank = RemoteTour { title: "  ".into(), ..listed_tour("draft") };
        assert_eq!(edit_picker(Some(Some("0.1.4")), Some(vec![blank])).tour_title(), "Customer onboarding");
        // A tour that was not renamed is named as before.
        let same = edit_picker(Some(Some("0.1.4")), Some(vec![listed_tour("published")]));
        assert!(matches!(same.edit_outcome(&same.shown()), Some(PickerOutcome::Update { ref target, .. }) if *target == edit_target()));
    }

    #[test]
    fn the_update_choice_says_when_updating_removes_the_cover_logo_and_only_then() {
        const LINE: &str = "The cover logo could not be opened with this recording, so updating removes it from the tour. Add the logo again in Tour settings to keep it.";
        let offered = || edit_picker(Some(Some("0.1.4")), Some(vec![listed_tour("published")]));
        // The recording lost its logo when it was opened, and Update is offered: the line shows, for a draft too.
        let mut p = offered().with_logo_missing(true);
        assert_eq!(p.logo_line(&p.shown()), Some(LINE));
        assert!(draw(&mut p, egui::RawInput::default()).is_none());
        let draft = edit_picker(Some(Some("0.1.4")), Some(vec![listed_tour("draft")])).with_logo_missing(true);
        assert_eq!(draft.logo_line(&draft.shown()), Some(LINE));
        // Update can still be pressed: the line warns, it does not refuse.
        assert!(matches!(p.edit_outcome(&p.shown()), Some(PickerOutcome::Update { .. })));
        // A recording that opened whole, or got its logo back: no line.
        let whole = offered();
        assert_eq!(whole.logo_line(&whole.shown()), None);
        assert_eq!(offered().with_logo_missing(false).logo_line(&Shown::Update { public: true }), None);
        // Not while Update is not offered: still asking, the site could not be asked, an older
        // site, a tour that is gone, another address, after an update that cannot be tried again.
        for shown in [Shown::Asking, Shown::Failed("Could not read the workspace.".into()), Shown::OnlyNew(Some(UPDATE_NEEDS_NEWER_SITE)), Shown::OnlyNew(Some(TOUR_NOT_ON_SITE)), Shown::OnlyNew(None)] {
            assert_eq!(logo_warning(&shown, true), None, "{shown:?}");
            assert_eq!(logo_warning(&shown, false), None, "{shown:?}");
        }
        for public in [true, false] {
            assert_eq!(logo_warning(&Shown::Update { public }, true), Some(LINE));
            assert_eq!(logo_warning(&Shown::Update { public }, false), None);
        }
        let elsewhere = Picker::open_for_edit_elsewhere(String::new(), "Chapter one".into(), edit_target()).with_logo_missing(true);
        assert_eq!(elsewhere.logo_line(&elsewhere.shown()), None);
        // The dialog of a new recording has no such choice, and takes no such line.
        let fresh = Picker::new(String::new(), "Chapter one").with_logo_missing(true);
        assert_eq!(fresh.logo_line(&fresh.shown()), None);
        assert!(LINE.ends_with('.') && !LINE.contains('!'));
    }

    #[test]
    fn nothing_can_be_chosen_while_the_site_is_still_being_asked() {
        assert_eq!(update_offer(false, None, None, TOUR_ID), UpdateOffer::Checking);
        // Not even with a list in hand: the version is not known yet.
        assert_eq!(update_offer(false, None, Some(&[listed_tour("published")]), TOUR_ID), UpdateOffer::Checking);
        assert_eq!(update_offer(true, Some("0.1.4"), None, TOUR_ID), UpdateOffer::Checking);
        for mut p in [edit_picker(None, None), edit_picker(Some(Some("0.1.4")), None)] {
            let shown = p.shown();
            assert_eq!(shown, Shown::Asking);
            for picked_new in [false, true] {
                assert_eq!(edit_choice(&shown, picked_new), None);
            }
            assert_eq!(edit_button(&shown, None, p.mode), "Update tour");
            pick_new(&mut p);
            assert!(p.edit_outcome(&shown).is_none());
            assert!(draw(&mut p, key_press(egui::Key::Enter)).is_none());
            assert!(matches!(draw(&mut p, key_press(egui::Key::Escape)), Some(PickerOutcome::Cancel)), "it can still be closed");
        }
        // The site could not be asked: the reason shows beside Retry, Update is not offered,
        // and a new tour is only what the user picks.
        let mut p = edit_picker(None, None);
        *p.list.lock().unwrap() = ListState::Failed("Could not read the workspace: the server could not be reached.".into());
        let shown = p.shown();
        assert_eq!(shown, Shown::Failed("Could not read the workspace: the server could not be reached.".into()));
        assert_eq!(edit_choice(&shown, false), None);
        assert!(draw(&mut p, key_press(egui::Key::Enter)).is_none());
        assert_eq!(edit_choice(&shown, true), Some(EditChoice::NewTour));
    }

    #[test]
    fn a_draft_tour_is_updated_as_a_draft_and_the_hint_does_not_promise_a_public_page() {
        assert_eq!(update_offer(true, Some("0.1.4"), Some(&[listed_tour("draft")]), TOUR_ID), UpdateOffer::Offered { public: false });
        let mut p = edit_picker(Some(Some("0.1.4")), Some(vec![listed_tour("draft")]));
        let shown = p.shown();
        assert_eq!(shown, Shown::Update { public: false });
        assert_eq!(update_hint(false), "Same link. The tour stays a draft.");
        assert!(!update_hint(false).contains("public"));
        // The button is the same, and the result screen is told the tour was not public.
        assert_eq!(edit_button(&shown, edit_choice(&shown, false), p.mode), "Update tour");
        assert!(matches!(p.edit_outcome(&shown), Some(PickerOutcome::Update { before: Before { public: false, link: None, steps: Some(14) }, .. })));
        assert!(draw(&mut p, egui::RawInput::default()).is_none());
    }

    #[test]
    fn enter_updates_and_escape_cancels() {
        let mut p = edit_picker(Some(Some("0.1.4")), Some(vec![listed_tour("published")]));
        assert!(matches!(draw(&mut p, key_press(egui::Key::Enter)), Some(PickerOutcome::Update { ref target, .. }) if *target == edit_target()));
        assert!(matches!(draw(&mut p, key_press(egui::Key::Escape)), Some(PickerOutcome::Cancel)));
        // With "Publish as a new tour" picked, Enter is that choice's button.
        pick_new(&mut p);
        assert!(matches!(draw(&mut p, key_press(egui::Key::Enter)), Some(PickerOutcome::Upload { target: Target::New, mode: Mode::PublishNow, .. })));
        // An upload that could not start keeps the dialog, with the reason, and both choices.
        p.refuse("Wait for the running export to finish, then upload.".into());
        assert_eq!(p.shown(), Shown::Update { public: true });
        assert!(draw(&mut p, egui::RawInput::default()).is_none());
    }

    #[test]
    fn a_failed_update_brings_the_dialog_back_with_the_reason_and_a_new_tour_still_offered() {
        // Pressing Update again cannot work: only a new tour is offered, and the site is not asked again.
        let again_is_pointless = [
            "Could not start the upload: that tour no longer exists.",
            "Could not start the upload: that chapter is no longer part of the tour.",
            "Could not start the upload: the site needs an update to version 0.1.4 or newer.",
        ];
        for error in again_is_pointless {
            assert!(!update_can_be_tried_again(error), "{error}");
            let mut p = Picker::open_after_failed_update(String::new(), "Chapter one".into(), edit_target(), error.into());
            assert_eq!(p.refused.as_deref(), Some(error));
            let shown = p.shown();
            assert_eq!(shown, Shown::OnlyNew(None), "the sentence already says why");
            assert!(p.edit_outcome(&shown).is_none());
            assert!(draw(&mut p, key_press(egui::Key::Enter)).is_none());
            pick_new(&mut p);
            assert!(matches!(draw(&mut p, key_press(egui::Key::Enter)), Some(PickerOutcome::Upload { target: Target::New, .. })), "{error}");
        }
        // Anything else may work the next time: Update stays, once the site has answered.
        for error in ["Could not finish the upload: the chapter was changed by another upload meanwhile. Update again.", "Could not send step 3: the server could not be reached."] {
            assert!(update_can_be_tried_again(error), "{error}");
        }
        // (What that dialog shows is `edit_picker`'s: opening it for real would ask a site.)
        let mut p = edit_picker(Some(Some("0.1.4")), Some(vec![listed_tour("published")]));
        p.refuse("Could not finish the upload: the chapter was changed by another upload meanwhile. Update again.".into());
        assert!(matches!(draw(&mut p, key_press(egui::Key::Enter)), Some(PickerOutcome::Update { .. })));
        pick_new(&mut p);
        assert!(matches!(p.edit_outcome(&p.shown()), Some(PickerOutcome::Upload { target: Target::New, .. })));
    }

    const FULL: &str = "The tour was not uploaded. This tour needs 310 MB, and your workspace has 120 MB left of its 5.0 GB. Delete tours you no longer need, or ask for more room, and publish again. Tours that are already online are not affected.";

    #[test]
    fn a_full_workspace_is_shown_as_a_heading_the_sentence_and_a_small_line() {
        assert_eq!(
            refusal_parts(FULL),
            Refusal {
                heading: Some("The tour was not uploaded"),
                body: "This tour needs 310 MB, and your workspace has 120 MB left of its 5.0 GB. Delete tours you no longer need, or ask for more room, and publish again.",
                note: Some("Tours that are already online are not affected."),
            }
        );
        // Every other reason is one sentence, as it is.
        for why in ["Wait for the running export to finish, then upload.", "Could not start the upload: the server is busy (HTTP 429). Try again later.", ""] {
            assert_eq!(refusal_parts(why), Refusal { heading: None, body: why, note: None });
        }
    }

    #[test]
    fn after_a_full_workspace_the_dialog_is_back_with_the_message_and_update_still_there() {
        // Making room is the user's to do; after that the same update can work, so it stays offered.
        assert!(update_can_be_tried_again(FULL));
        let mut p = edit_picker(Some(Some("0.1.8")), Some(vec![listed_tour("published")]));
        p.refuse(FULL.into());
        assert_eq!(p.shown(), Shown::Update { public: true });
        assert!(draw(&mut p, egui::RawInput::default()).is_none());
        assert!(matches!(draw(&mut p, key_press(egui::Key::Enter)), Some(PickerOutcome::Update { .. })));
        // The dialog for a new tour draws it too.
        let mut p = Picker::new(String::new(), "Demo");
        p.refuse(FULL.into());
        assert!(draw(&mut p, egui::RawInput::default()).is_none());
    }

    /// What a main-button press gave, in short.
    fn pressed(p: &mut Picker) -> String {
        match draw(p, key_press(egui::Key::Enter)) {
            Some(PickerOutcome::Upload { title, target, mode, .. }) => format!("upload {title:?} {target:?} {mode:?}"),
            Some(PickerOutcome::Update { target, .. }) => format!("update {}", target.chapter_id),
            Some(PickerOutcome::Cancel) => "cancel".into(),
            None => "nothing".into(),
        }
    }

    fn never(_: String) -> Option<Picker> {
        panic!("no other dialog is made")
    }

    #[test]
    fn after_a_full_workspace_the_dialog_comes_back_as_the_user_left_it() {
        // From an editor opened through Edit, "Publish as a new tour" with a typed title, as a draft.
        let mut p = edit_picker(Some(Some("0.1.8")), Some(vec![listed_tour("published")]));
        pick_new(&mut p);
        p.title = "A tour of its own".into();
        p.mode = Mode::Draft;
        let asked = pressed(&mut p);
        assert_eq!(asked, r#"upload "A tour of its own" New Draft"#);
        let mut back = dialog_after_refusal(Some(p), FULL.into(), false, never).expect("the dialog comes back");
        assert_eq!(back.refused.as_deref(), Some(FULL));
        assert_eq!(pressed(&mut back), asked, "never an update of the tour the recording was saved for");

        // A new chapter of a listed tour, published now.
        let mut p = Picker::new(String::new(), "Chapter two");
        *p.list.lock().unwrap() = ListState::Loaded(vec![listed_tour("draft")]);
        p.choice = Choice::Tour(TOUR_ID.into());
        let asked = pressed(&mut p);
        assert_eq!(asked, format!(r#"upload "Chapter two" Chapter("{TOUR_ID}") PublishNow"#));
        let mut back = dialog_after_refusal(Some(p), FULL.into(), false, never).unwrap();
        assert_eq!(back.refused.as_deref(), Some(FULL));
        assert_eq!(pressed(&mut back), asked);

        // A new tour with a typed title.
        let mut p = Picker::new(String::new(), "Demo");
        p.title = "Typed".into();
        let mut back = dialog_after_refusal(Some(p), FULL.into(), false, never).unwrap();
        assert_eq!(pressed(&mut back), r#"upload "Typed" New PublishNow"#);

        // An update: Update is still the choice, in the dialog that was left.
        let p = edit_picker(Some(Some("0.1.8")), Some(vec![listed_tour("published")]));
        let mut back = dialog_after_refusal(Some(p), FULL.into(), true, never).unwrap();
        assert_eq!(back.shown(), Shown::Update { public: true });
        assert_eq!(back.refused.as_deref(), Some(FULL));
        assert_eq!(pressed(&mut back), format!("update {CHAPTER_ID}"));
    }

    #[test]
    fn any_other_ending_brings_back_what_it_did_before() {
        let left = || Some(edit_picker(Some(Some("0.1.8")), Some(vec![listed_tour("published")])));
        // A failed update gets the dialog made for it, with the reason.
        let gone = "Could not start the upload: that tour no longer exists.";
        let made = dialog_after_refusal(left(), gone.into(), true, |e| {
            assert_eq!(e, gone);
            let mut p = Picker::new_for_edit(String::new(), "Chapter one", edit_target(), Block::Said);
            p.refuse(e);
            Some(p)
        })
        .unwrap();
        assert_eq!(made.shown(), Shown::OnlyNew(None));
        // A failed publish of a new tour or chapter says so in the status bar only.
        assert!(dialog_after_refusal(left(), "Could not send step 3: the server could not be reached.".into(), false, never).is_none());
        // A cancel is the user's own doing.
        assert!(dialog_after_refusal(left(), publish::CANCELLED.into(), true, never).is_none());
        assert!(dialog_after_refusal(left(), publish::CANCELLED.into(), false, never).is_none());
        // No dialog was left (the publish was not started from one): a full workspace on an
        // update still gets the update's dialog, and otherwise there is none to bring back.
        assert!(dialog_after_refusal(None, FULL.into(), true, |_| left()).is_some());
        assert!(dialog_after_refusal(None, FULL.into(), false, never).is_none());
    }

    // ---- The result screen after an update ----

    /// What the tour was when Update was pressed: public, 14 steps in two chapters, with its link.
    fn was_public() -> Before {
        Before { public: true, link: Some(LINK.into()), steps: Some(14) }
    }

    fn updated(steps: usize, outcome: Outcome) -> ResultDialog {
        ResultDialog::after_update("Customer onboarding".into(), was_public(), done(steps, outcome))
    }

    #[test]
    fn the_result_screen_says_tour_updated_with_the_same_link() {
        // Published: the site's own count of the tour, and the tour's link, which is the same one.
        let public = Outcome::Public { url: Some(LINK.into()) };
        assert_eq!(result_heading(&public, true), "Tour updated");
        assert_eq!(result_line("Customer onboarding", &finished(14, public.clone()), &was_public(), true), "Customer onboarding · 14 steps · public");
        assert_eq!(result_note(&public, &was_public(), true), None);
        assert_eq!(result_status(&finished(14, public.clone()), &was_public(), true), "Tour updated: 14 steps");
        assert_eq!(result_status(&finished(1, public.clone()), &was_public(), true), "Tour updated: 1 step");
        let screen = updated(14, public.clone());
        assert_eq!(screen.url(), Some(LINK));
        assert!(!screen.link_is_older() && !screen.can_retry());
        // The site gave no link this time: the one the tour had is still its link.
        assert_eq!(updated(14, Outcome::Public { url: None }).url(), Some(LINK));
        assert_eq!(result_note(&Outcome::Public { url: None }, &was_public(), true), None);

        // A tour that is a draft stays one.
        let draft = Before { public: false, link: None, steps: Some(14) };
        assert_eq!(result_heading(&Outcome::Draft, true), "Draft updated");
        assert_eq!(result_line("Customer onboarding", &finished(14, Outcome::Draft), &draft, true), "Customer onboarding · this chapter: 14 steps · not public");
        assert_eq!(result_note(&Outcome::Draft, &draft, true), None);
        assert_eq!(result_link(&Outcome::Draft, &draft), None);
        assert_eq!(result_status(&finished(14, Outcome::Draft), &draft, true), "Draft updated: 14 steps");
        // The site says draft although the list said public a moment ago: the site is believed.
        assert_eq!(result_line("T", &finished(3, Outcome::Draft), &was_public(), true), "T · this chapter: 3 steps · not public");

        // Every case draws, and the words of a new upload are untouched.
        let ctx = egui::Context::default();
        for outcome in [public.clone(), Outcome::Public { url: None }, Outcome::Draft, failed(), cancelled(), Outcome::Held] {
            for before in [was_public(), draft.clone()] {
                let mut d = ResultDialog::after_update("Demo".into(), before, done(2, outcome.clone()));
                let mut out = None;
                ctx.run_ui(egui::RawInput::default(), |ui| out = d.show(ui.ctx())).textures_delta.clear();
                assert_eq!(out, None);
            }
        }
        assert_eq!(result_heading(&public, false), "Tour published");
        assert_eq!(result_status(&finished(14, public), &was_public(), false), "Tour published: 14 steps");
    }

    #[test]
    fn an_update_whose_publish_failed_says_the_public_page_shows_the_previous_version_and_offers_try_again() {
        assert_eq!(result_heading(&failed(), true), "Update uploaded");
        assert_eq!(result_heading(&cancelled(), true), "Update uploaded");
        assert_eq!(
            result_note(&failed(), &was_public(), true),
            Some(("Could not publish the tour: the server is busy (HTTP 429). Try again later. The update is uploaded, but the public page still shows the previous version.".to_string(), true))
        );
        assert_eq!(result_note(&cancelled(), &was_public(), true), Some(("Publishing was cancelled. The update is uploaded, but the public page still shows the previous version.".to_string(), false)));
        for outcome in [failed(), cancelled()] {
            assert_eq!(result_line("Customer onboarding", &finished(3, outcome.clone()), &was_public(), true), "Customer onboarding · this chapter: 3 steps · the public version is unchanged");
            let status = result_status(&finished(3, outcome.clone()), &was_public(), true);
            assert_eq!(status, "Update uploaded: the public page still shows the previous version");
            assert!(!status.contains("https://"));
            // The tour is public all along: its link is offered, marked as the version before.
            let screen = updated(3, outcome);
            assert_eq!(screen.url(), Some(LINK));
            assert!(screen.link_is_older());
        }
        // "Try again" is offered after a failure only, and it names the chapter: it publishes,
        // and neither uploads nor saves again.
        assert!(updated(3, failed()).can_retry());
        assert!(!updated(3, cancelled()).can_retry());
        let mut screen = Some(updated(3, failed()));
        assert_eq!(take_retry(&mut screen, true), None, "not while another job runs");
        let retry = take_retry(&mut screen, false).unwrap();
        assert_eq!(retry, Retry { title: "Customer onboarding".into(), before: was_public(), tour_id: TOUR_ID.into(), chapter_id: CHAPTER_ID.into(), steps: 3, kept: Kept::Saved { left_out: vec![], over: None }, update: true });
        // A retry of a new upload is not one of an update.
        let mut screen = Some(ResultDialog::new("Demo".into(), was_public(), done(2, failed())));
        assert!(!take_retry(&mut screen, false).unwrap().update);
    }

    #[test]
    fn an_update_held_back_for_other_unpublished_changes_says_so() {
        let held = Outcome::Held;
        assert_eq!(result_heading(&held, true), "Update uploaded");
        assert_eq!(
            result_note(&held, &was_public(), true),
            Some(("This tour has other changes that were never published, so the update was uploaded without publishing. Publish the changes from the Tours page when all of it is ready.".to_string(), false))
        );
        assert_eq!(result_line("Customer onboarding", &finished(3, held.clone()), &was_public(), true), "Customer onboarding · this chapter: 3 steps · the public version is unchanged");
        assert_eq!(result_status(&finished(3, held.clone()), &was_public(), true), "Update uploaded: the public page still shows the previous version");
        // Publishing from here would publish the other changes too: no "Try again".
        let screen = updated(3, held.clone());
        assert!(!screen.can_retry());
        let mut screen = Some(screen);
        assert_eq!(take_retry(&mut screen, false), None);
        // The public version is still there, under the link it had.
        let screen = screen.unwrap();
        assert_eq!(screen.url(), Some(LINK));
        assert!(screen.link_is_older());
        assert_eq!(result_link(&held, &Before { public: false, link: Some(LINK.into()), steps: None }), None);
        // Only an update is ever held back; should it come up for another upload, nothing false is said.
        assert_eq!(result_heading(&held, false), "Draft uploaded");
        assert_eq!(result_note(&held, &was_public(), false), None);
        assert_eq!(result_status(&finished(2, held), &was_public(), false), "Tour uploaded as a draft: 16 steps; the public version is unchanged");
    }

    #[test]
    fn the_step_count_after_an_update_is_never_the_old_count_plus_the_new() {
        // The tour had 14 steps; the replaced chapter now has 3.
        for outcome in [Outcome::Draft, failed(), cancelled(), Outcome::Held] {
            assert_eq!(result_steps(&finished(3, outcome.clone()), &was_public(), true), "this chapter: 3 steps", "{outcome:?}");
            assert_eq!(result_steps(&finished(1, outcome.clone()), &Before::default(), true), "this chapter: 1 step", "{outcome:?}");
            let status = result_status(&finished(3, outcome.clone()), &was_public(), true);
            assert!(!status.contains("17") && !result_line("T", &finished(3, outcome), &was_public(), true).contains("17"), "{status}");
        }
        // After a publish the site counts the tour itself.
        assert_eq!(result_steps(&finished(12, Outcome::Public { url: None }), &was_public(), true), "12 steps");
        // A new chapter still adds up, as before.
        assert_eq!(result_steps(&finished(3, Outcome::Draft), &was_public(), false), "17 steps");
    }

    #[test]
    fn after_an_update_a_recording_that_could_not_be_saved_is_said_to_no_longer_match() {
        let failed_save = Kept::Failed("the project file could not be written.".into());
        assert_eq!(
            kept_note(&failed_save, true).as_deref(),
            Some("The changes were uploaded, but the recording saved on this computer could not be updated, so it no longer matches the tour: the project file could not be written.")
        );
        // The same reason after a first upload keeps its own sentence.
        assert_eq!(kept_note(&failed_save, false).as_deref(), Some("The recording could not be saved on this computer, so it can't be edited later: the project file could not be written."));
        assert_eq!(kept_note(&Kept::Saved { left_out: vec![], over: None }, true), None);
        assert_eq!(kept_note(&Kept::Saved { left_out: vec!["a pasted picture".into()], over: None }, true).as_deref(), Some("The recording was saved for later editing, but without a pasted picture."));
        // The screen shows it, whatever became of the publish, and the upload is reported as it went.
        for outcome in [Outcome::Public { url: Some(LINK.into()) }, Outcome::Draft, Outcome::Held, failed()] {
            let screen = ResultDialog::after_update("Demo".into(), was_public(), PublishDone { finished: finished(3, outcome.clone()), kept: failed_save.clone() });
            assert!(screen.kept_line().is_some_and(|l| l.starts_with(UPDATE_NOT_SAVED)), "{outcome:?}");
            assert_eq!(result_heading(&screen.done.outcome, true), result_heading(&outcome, true));
        }
        // "Try again" says it again: nothing is saved by a retry.
        let mut screen = Some(ResultDialog::after_update("Demo".into(), was_public(), PublishDone { finished: finished(3, failed()), kept: failed_save.clone() }));
        assert_eq!(take_retry(&mut screen, false).map(|r| (r.kept, r.update)), Some((failed_save, true)));
    }
}
