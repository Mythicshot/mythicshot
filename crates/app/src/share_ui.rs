//! "Publish to": the window that shares one screenshot ("Publish image") or one recording
//! ("Publish recording") with a link, and the background job that does it.
//!
//! The window goes through the stages of [`Stage`]: the form, the upload (after "Preparing the
//! clip…" for a recording), and then the link or the reason there is none. Everything it says
//! comes from [`ShareDialog::view`], and every button goes through [`ShareDialog::press`], so
//! the wording and the steps between stages are tested without a window. The clipboard is
//! reached only through the function the caller hands in.
//!
//! The job runs on a thread of its own and is a [`Run`]: quitting cancels it and waits for its
//! clean-up DELETE like a tour's, and closing the editor cancels it too. The token is loaded by
//! the job itself and never reaches the UI thread.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Stroke, StrokeKind, vec2};

use crate::theme::{self, Strong as _};
use crate::editor_ui::Editor;
use crate::icons::{self, Icon};
use crate::publish::{
    self,
    access::{Access, Granted},
    share::{self, Item, Kind, MediaType, Refusal, ShareError, Shared},
};
use crate::publish_ui::{self, Run};


// The window's measures, from the mockup the owner approved. The tour result screen uses them too.
/// The width of what the window holds.
pub const WINDOW_WIDTH: f32 = 520.0;
/// Around what the window holds.
pub const WINDOW_PADDING: f32 = 28.0;
/// Between the text, the link row and the action row.
pub const BLOCK_GAP: f32 = 20.0;
/// The link row: the link and the Copy link button, one height.
pub const LINK_ROW_HEIGHT: f32 = 46.0;
/// Every button of the window but Copy link.
pub const BUTTON_HEIGHT: f32 = 44.0;
/// Between the buttons of the action row.
pub const BUTTON_GAP: f32 = 12.0;
/// The window's heading: 20 points, as in the owner's mockup (the app's other headings are 18).
pub const HEADING_SIZE: f32 = 20.0;
/// What Copy link says for a moment after a copy that worked.
pub const COPIED_LABEL: &str = "Copied";
pub const OPEN_IN_BROWSER: &str = "Open in browser";

/// The entry in the image editor's "Send to" menu and in the recording editor's Export menu.
pub const MENU_ENTRY: &str = "Publish to…";
/// The heading of that entry's part of the Export menu.
pub const EXPORT_HEADING: &str = "Share";
pub const TITLE_LABEL: &str = "Title (optional)";
pub const UPLOADING: &str = "Uploading…";
pub const PREPARING: &str = "Preparing the clip…";
pub const CANCELLING: &str = "Cancelling…";
pub const COPIED: &str = "The link was copied. Paste it to whoever should see it.";
pub const NO_FFMPEG: &str = "Mythicshot did not find the video tool (ffmpeg) on this computer, so the clip is sent as a GIF. With it, the same clip is far smaller.";
/// The status line after a cancel. True also when the clean-up could not reach the site: an
/// item that was never finished is never online.
pub const CANCELLED: &str = "Publishing was cancelled. Nothing was shared.";
/// When "Copy link" could not set the clipboard. The wording is proposed and waits for the owner's approval.
pub const COPY_FAILED: &str = "Could not copy the link. Select it and copy it yourself.";
/// Prefix of the temporary folder a recording's clip is prepared in.
pub const TEMP_PREFIX: &str = "mythicshot-share-";
/// The PNG file of an image being shared, once the thread that makes it is done.
type Encoded = Arc<Mutex<Option<Result<Vec<u8>, String>>>>;

/// When the image could not be turned into a PNG file. The wording is proposed and waits for the owner's approval.
pub const NOT_ENCODED: &str = "Mythicshot could not make a PNG file of the image.";
/// What a panic in the job becomes.
const BROKE_OFF: &str = "Publishing failed because of an internal error.";

/// The small title above the window's heading.
pub fn caption(kind: Kind) -> &'static str {
    match kind {
        Kind::Image => "Publish image",
        Kind::Clip => "Publish recording",
    }
}

pub fn heading(kind: Kind) -> &'static str {
    match kind {
        Kind::Image => "Share this image with a link",
        Kind::Clip => "Share this recording with a link",
    }
}

pub fn warning(kind: Kind) -> String {
    format!("Anyone who has the link can open the {}. It stays online until you delete it in the dashboard.", kind.noun())
}

pub fn online_heading(kind: Kind) -> String {
    format!("The {} is online", kind.noun())
}

/// When the site gave no link Mythicshot can open. The wording is proposed and waits for the owner's approval.
pub fn no_link(kind: Kind) -> String {
    format!("The site did not give a link Mythicshot can open. Find the {} in the dashboard.", kind.noun())
}

/// "To Cowfee Raid Tools at mythicshot.com": the workspace's name, when the site has told it,
/// and the host of the configured address. Only the host until the name is known; None for an
/// address that is not one.
pub fn to_line(workspace: Option<&str>, server: &str) -> Option<String> {
    let host = publish::host_of(server)?;
    let name = workspace.map(share::plain_text).filter(|n| !n.is_empty());
    Some(match name {
        Some(name) => format!("To {} at {host}", publish_ui::short_title(&name, publish_ui::ROW_TITLE_MAX)),
        None => format!("To {host}"),
    })
}

/// "1920 × 1080 · PNG · 1.4 MB".
pub fn image_details(width: u32, height: u32, media: MediaType, bytes: u64) -> String {
    format!("{width} × {height} · {} · {}", media.label(), publish_ui::size_text(bytes))
}

/// "12 s · 960 × 540 · MP4 · about 18 MB"; without "about …" when there is no estimate.
pub fn clip_details(duration_ms: u64, width: u32, height: u32, media: MediaType, estimate: Option<u64>) -> String {
    let seconds = ((duration_ms + 500) / 1000).max(1);
    let base = format!("{seconds} s · {width} × {height} · {}", media.label());
    match estimate {
        Some(bytes) => format!("{base} · about {}", publish_ui::size_text(bytes)),
        None => base,
    }
}

/// "0.9 of 1.4 MB": what is sent, in the unit and with the decimals the total is written with
/// ([`publish_ui::size_text`]), rounded down so it reads as all of it only once it is.
pub fn sent_text(sent: u64, total: u64) -> String {
    const MB: u64 = 1_048_576;
    const GB: u64 = 1_073_741_824;
    let whole = publish_ui::size_text(total);
    let Some((number, unit)) = whole.split_once(' ') else { return whole };
    if sent >= total {
        return format!("{number} of {number} {unit}");
    }
    let tenths = |unit_bytes: u64| (u128::from(sent) * 10 / u128::from(unit_bytes)) as u64;
    let done = match (unit, number.contains('.')) {
        ("KB", _) => (sent / 1024).to_string(),
        ("MB", true) => format!("{}.{}", tenths(MB) / 10, tenths(MB) % 10),
        ("MB", false) => (sent / MB).to_string(),
        _ => format!("{}.{}", tenths(GB) / 10, tenths(GB) % 10),
    };
    format!("{done} of {whole}")
}

/// Why sharing cannot start now: the same reason "Publish tour…" gives for a missing or
/// unusable workspace connection, or None when it can.
pub fn blocked_reason(server: &str) -> Option<String> {
    publish_ui::publish_blocked(false, true, server, &publish_ui::token_status(), publish_ui::token_site().as_deref()).and_then(|b| b.reason())
}

/// Put `text` on the clipboard; false when it could not be set. Tests never reach the real clipboard.
pub fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("clipboard text {text}")) {
        return false;
    }
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())).is_ok()
}

// ---- The job ----

/// What a running job is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Progress {
    Preparing(f32),
    Uploading(u64, u64),
}

struct JobState {
    progress: Progress,
    result: Option<Result<Shared, ShareError>>,
    finished: bool,
}

/// A share running on a thread of its own.
pub struct ShareJob {
    run: Arc<Run>,
    state: Arc<Mutex<JobState>>,
}

/// What the job's work reports through: its progress, and whether it was cancelled.
pub struct Reporter {
    run: Arc<Run>,
    state: Arc<Mutex<JobState>>,
}

impl Reporter {
    fn set(&self, p: Progress) -> bool {
        if let Ok(mut s) = self.state.lock() {
            s.progress = p;
        }
        !self.run.is_cancelled()
    }

    /// The clip is being prepared, `done` of 1; false once cancelled.
    pub fn preparing(&self, done: f32) -> bool {
        self.set(Progress::Preparing(done.clamp(0.0, 1.0)))
    }

    /// `sent` of `total` bytes are uploaded; false once cancelled.
    pub fn uploading(&self, sent: u64, total: u64) -> bool {
        self.set(Progress::Uploading(sent, total))
    }

    pub fn cancelled(&self) -> bool {
        self.run.is_cancelled()
    }

    /// The temporary folder the job writes into, for quitting to remove should the job not end in time.
    pub fn keep_dir(&self, dir: Option<PathBuf>) {
        self.run.set_dir(dir);
    }
}

/// Tells whether a job has ended, for whoever must wait for it (a closed editor's recording).
#[derive(Clone)]
pub struct JobWaiter(Arc<Mutex<JobState>>);

impl JobWaiter {
    pub fn is_finished(&self) -> bool {
        self.0.lock().map(|s| s.finished).unwrap_or(true)
    }
}

impl ShareJob {
    /// Run `work` on a thread of its own. A panic in it is a failure with a sentence; either
    /// way the run is marked finished.
    pub fn spawn(first: Kind, work: impl FnOnce(&Reporter) -> Result<Shared, ShareError> + Send + 'static) -> ShareJob {
        let run = Run::begin();
        let progress = match first {
            Kind::Image => Progress::Uploading(0, 0),
            Kind::Clip => Progress::Preparing(0.0),
        };
        let state = Arc::new(Mutex::new(JobState { progress, result: None, finished: false }));
        let reporter = Reporter { run: run.clone(), state: state.clone() };
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&reporter))).unwrap_or_else(|_| Err(ShareError::Failed(BROKE_OFF.to_string())));
            if let Ok(mut s) = reporter.state.lock() {
                s.result = Some(result);
                s.finished = true;
            }
            reporter.run.finish();
        });
        ShareJob { run, state }
    }

    fn cancel(&self) {
        self.run.cancel();
    }

    pub fn waiter(&self) -> JobWaiter {
        JobWaiter(self.state.clone())
    }

    /// The progress, and the result once the job has ended (taken once).
    fn take(&self) -> (Progress, Option<Result<Shared, ShareError>>) {
        match self.state.lock() {
            Ok(mut s) => (s.progress, s.result.take()),
            Err(_) => (Progress::Uploading(0, 0), Some(Err(ShareError::Failed(BROKE_OFF.to_string())))),
        }
    }
}

// ---- The window ----

/// Where the window is.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Before anything is sent: the title, the details and Publish.
    Form,
    /// No workspace is connected, or the token cannot be used here: the reason, as for a tour.
    Blocked(String),
    Preparing(f32),
    Uploading { sent: u64, total: u64 },
    /// Cancel was pressed; the job is sending its clean-up.
    Cancelling,
    /// Shared. `copied`: the link was put on the clipboard when the upload finished.
    Online { link: Option<String>, copied: bool },
    Refused(Refusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Cancel,
    Publish,
    Close,
    Open,
    CopyLink,
    Done,
}

impl Button {
    pub fn label(self) -> &'static str {
        match self {
            Button::Cancel => "Cancel",
            Button::Publish => "Publish",
            Button::Close => "Close",
            Button::Open => OPEN_IN_BROWSER,
            Button::CopyLink => "Copy link",
            Button::Done => "Done",
        }
    }
}

/// One line of the window, in the order it shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// Grey text: the destination, the details, the progress, the sentence about the clipboard.
    Muted(String),
    /// Plain text: a refusal's sentence, the reason sharing is blocked.
    Text(String),
    /// The title field, under its label.
    Title,
    /// In a coloured box: what goes public.
    Warning(String),
    /// A small grey line under a refusal, and the help under the address field.
    Note(String),
    /// "Who can view" over its dropdown: the choices offered, and the one chosen.
    Who { offered: Vec<who::Choice>, chosen: who::Choice },
    /// "Email addresses" over the field the addresses are typed in.
    Addresses,
}

/// What the window shows now.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub caption: &'static str,
    pub heading: String,
    pub lines: Vec<Line>,
    pub progress: Option<f32>,
    /// The link, in a read-only field.
    pub link: Option<String>,
    /// From left to right; the last one is the main button. Online, in the order keyboard focus
    /// takes them: Copy link (in the link row), then the action row.
    pub buttons: Vec<Button>,
    /// A button shown but not usable yet.
    pub disabled: Option<Button>,
    /// Copy link says "Copied": the link was put on the clipboard a moment ago.
    pub copied: bool,
    /// Online, with the small lock under the link: who can open what was shared, when it is not
    /// anyone with the link.
    pub lock: Option<String>,
}

impl View {
    /// What `button` says now.
    pub fn label(&self, button: Button) -> &'static str {
        match button {
            Button::CopyLink if self.copied => COPIED_LABEL,
            other => other.label(),
        }
    }

    /// Every sentence of the window, for tests.
    #[cfg(test)]
    pub fn texts(&self) -> Vec<String> {
        let mut out = vec![self.caption.to_string(), self.heading.clone()];
        for line in &self.lines {
            match line {
                Line::Muted(t) | Line::Text(t) | Line::Warning(t) | Line::Note(t) => out.push(t.clone()),
                Line::Title => out.push(TITLE_LABEL.to_string()),
                Line::Who { chosen, .. } => out.extend([who::LABEL.to_string(), chosen.label().to_string()]),
                Line::Addresses => out.push(who::ADDRESSES_LABEL.to_string()),
            }
        }
        out.extend(self.link.clone());
        out.extend(self.lock.clone());
        out.extend(self.buttons.iter().map(|b| self.label(*b).to_string()));
        out
    }
}

/// What the window asks of the editor.
#[derive(Debug, Clone, PartialEq)]
pub enum ShareAction {
    /// Publish was pressed: check the connection again, then [`ShareDialog::begin`].
    Publish,
    /// The window closed, with a status line to show when there is one.
    Closed(Option<String>),
    /// Open the link in the browser; the window stays.
    Open(String),
    /// The job ended: a status line for the editor. The window stays.
    Status(String),
}

/// Makes the job when Publish is pressed: given the server, the cleaned title and who can view.
pub type Starter = Box<dyn FnOnce(String, String, Access) -> ShareJob + Send>;

/// Asked every frame while the file is still being made off the UI thread: None until it is,
/// then the details line with its size, or why it cannot be shared.
pub type Measure = Box<dyn FnMut() -> Option<Result<String, ShareError>> + Send>;

/// The "Publish image" or "Publish recording" window.
pub struct ShareDialog {
    kind: Kind,
    server: String,
    /// The workspace's name once the site has told it.
    workspace: Arc<Mutex<Option<String>>>,
    /// Ask the site for the workspace's name when the form shows (not in tests).
    asks_site: bool,
    asked: bool,
    /// What the site's version says about who can view, once it has answered.
    site: Arc<Mutex<who::Site>>,
    /// Who can view: never remembered, and the addresses are overwritten when the window goes.
    pub who: who::Form,
    /// Who can open the item, as the site said when it was created.
    granted: Option<Granted>,
    pub title: String,
    details: String,
    gif_note: bool,
    stage: Stage,
    starter: Option<Starter>,
    /// Set while the file is being made: Publish waits for it.
    measure: Option<Measure>,
    job: Option<ShareJob>,
    /// When the link last went on the clipboard, by [`ShareDialog::clock`]: Copy link says "Copied" for a moment.
    copied_at: Option<Instant>,
    /// The time "Copied" is measured by; the tests move their own.
    clock: Box<dyn Fn() -> Instant + Send>,
    /// The last "Copy link" could not set the clipboard.
    copy_failed: bool,
    /// The profile the window was opened or last switched to: the window follows a switch until
    /// Publish is pressed, and the job keeps the profile it started with.
    profile: Option<String>,
}

impl Drop for ShareDialog {
    /// A window that goes away while its job runs cancels it: nobody would see the link. The
    /// job then deletes what it sent.
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
    }
}

impl ShareDialog {
    /// The window before anything is asked of the site. `blocked` is why sharing cannot start
    /// (see [`blocked_reason`]); `title` fills the title field, cut to the site's limit.
    pub fn new(kind: Kind, server: String, title: &str, details: String, gif_note: bool, blocked: Option<String>, starter: Option<Starter>) -> ShareDialog {
        let title = share::clean_title(title);
        let stage = match blocked {
            Some(why) => Stage::Blocked(why),
            None => Stage::Form,
        };
        ShareDialog {
            kind,
            server,
            workspace: Arc::new(Mutex::new(None)),
            asks_site: false,
            asked: false,
            site: Arc::new(Mutex::new(who::Site::Unknown)),
            who: who::Form::default(),
            granted: None,
            title,
            details,
            gif_note,
            stage,
            starter,
            measure: None,
            job: None,
            copied_at: None,
            clock: Box::new(Instant::now),
            copy_failed: false,
            profile: publish_ui::pin().profile,
        }
    }

    /// Before anything is sent, the window follows a switch of profile: the new one's address,
    /// workspace and who-can-view. Later stages keep the profile they started with.
    pub fn follow_profile(&mut self, pin: &crate::profiles::Pin) {
        if self.profile == pin.profile && self.server == pin.server {
            return;
        }
        if !self.can_switch() {
            return;
        }
        self.profile = pin.profile.clone();
        self.server = pin.server.clone();
        self.workspace = Arc::new(Mutex::new(None));
        self.site = Arc::new(Mutex::new(who::Site::Unknown));
        self.asked = false;
        self.ask_site();
    }

    /// Whether the profile can be switched from this window: not once Publish was pressed.
    pub fn can_switch(&self) -> bool {
        matches!(self.stage, Stage::Form | Stage::Blocked(_)) && self.job.is_none()
    }

    /// The window as the editors open it: it asks the site for the workspace's name, off the UI thread, once the form shows.
    pub fn open(kind: Kind, server: String, title: &str, details: String, gif_note: bool, blocked: Option<String>, starter: Starter) -> ShareDialog {
        let mut d = ShareDialog::new(kind, server, title, details, gif_note, blocked, Some(starter));
        d.asks_site = true;
        d.ask_site();
        d
    }

    /// The file is still being made: until `measure` says it is ready, the details line shows
    /// what is known and Publish waits.
    pub fn measuring(mut self, measure: Measure) -> ShareDialog {
        self.measure = Some(measure);
        self
    }

    /// The window on another clock than the real one.
    #[cfg(test)]
    pub fn with_clock(mut self, clock: impl Fn() -> Instant + Send + 'static) -> ShareDialog {
        self.clock = Box::new(clock);
        self
    }

    /// How long Copy link still says "Copied"; None when it does not.
    fn copied_left(&self) -> Option<Duration> {
        let since = (self.clock)().saturating_duration_since(self.copied_at?);
        publish_ui::COPIED_FOR.checked_sub(since).filter(|left| !left.is_zero())
    }

    #[cfg(test)]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    fn ask_site(&mut self) {
        if !self.asks_site || self.asked || self.stage != Stage::Form {
            return;
        }
        self.asked = true;
        let (server, name, site) = (self.server.clone(), self.workspace.clone(), self.site.clone());
        // Pinned to the window's profile, on this thread.
        let pin = crate::profiles::Pin { profile: self.profile.clone(), server: server.clone() };
        // One request tells both the workspace's name and the site's version.
        std::thread::spawn(move || {
            match publish_ui::token_for_pin(&pin).and_then(|token| publish::workspace(&server, &token)) {
                Ok(ws) => {
                    if let Ok(mut s) = site.lock() {
                        *s = who::Site::of(ws.version.as_deref());
                    }
                    if let Ok(mut n) = name.lock() {
                        *n = Some(ws.name);
                    }
                }
                // Only "Anyone with the link" is offered then, with the line that says why.
                Err(_) => {
                    if let Ok(mut s) = site.lock() {
                        *s = who::Site::Failed;
                    }
                }
            }
        });
    }

    /// What the site said about itself, or that it could not be asked.
    #[cfg(test)]
    pub fn set_site(&mut self, site: who::Site) {
        if let Ok(mut s) = self.site.lock() {
            *s = site;
        }
        self.who.set_site(site);
    }

    /// The site's version, as `GET /api/v1/me` said it (None: it tells none).
    #[cfg(test)]
    pub fn set_site_version(&mut self, version: Option<&str>) {
        if let Ok(mut s) = self.site.lock() {
            *s = who::Site::of(version);
        }
        self.who.set_site(who::Site::of(version));
    }

    /// The workspace's name, as the site said it.
    #[cfg(test)]
    pub fn set_workspace(&self, name: &str) {
        if let Ok(mut n) = self.workspace.lock() {
            *n = Some(name.to_string());
        }
    }

    /// Before anything is sent, the window follows the connection: a workspace connected
    /// meanwhile shows the form, one gone shows why. Later stages are left alone.
    pub fn update_blocked(&mut self, blocked: Option<String>) {
        match (&self.stage, blocked) {
            (Stage::Form | Stage::Blocked(_), Some(why)) => self.stage = Stage::Blocked(why),
            (Stage::Blocked(_), None) => {
                self.stage = Stage::Form;
                self.ask_site();
            }
            _ => {}
        }
    }

    /// Show why the item cannot be shared (over a limit before anything was sent, say); a cancel closes nothing here.
    pub fn refuse(&mut self, e: &ShareError) {
        if let Some(r) = e.refusal(self.kind) {
            self.stage = Stage::Refused(r);
        }
    }

    /// Publish: make the job with the starter and follow it. Only from the form, and only once.
    pub fn begin(&mut self) {
        if self.stage != Stage::Form || self.measure.is_some() {
            return;
        }
        // Publish waits for addresses that can be sent.
        let Ok(access) = self.who.access() else { return };
        let Some(starter) = self.starter.take() else { return };
        let job = starter(self.server.clone(), share::clean_title(&self.title), access);
        self.start(job);
    }

    /// Follow `job`.
    pub fn start(&mut self, job: ShareJob) {
        self.stage = match job.take().0 {
            Progress::Preparing(f) => Stage::Preparing(f),
            Progress::Uploading(sent, total) => Stage::Uploading { sent, total },
        };
        self.job = Some(job);
    }

    /// The running job's waiter, for a recording that must outlive the editor until the job ends.
    pub fn waiter(&self) -> Option<JobWaiter> {
        self.job.as_ref().map(ShareJob::waiter)
    }

    /// Take in what the job did. When it shared the item, the link goes on the clipboard
    /// through `copy` (false when it could not be set). Some when the editor has something to do.
    pub fn poll(&mut self, copy: &mut dyn FnMut(&str) -> bool) -> Option<ShareAction> {
        if let Ok(site) = self.site.lock()
            && *site != who::Site::Unknown
        {
            self.who.set_site(*site);
        }
        if let Some(measured) = self.measure.as_mut().and_then(|m| m()) {
            self.measure = None;
            match measured {
                Ok(details) => self.details = details,
                // Before anything is sent: shown in the window, nothing for the status bar.
                Err(e) => self.refuse(&e),
            }
        }
        let job = self.job.as_ref()?;
        let (progress, result) = job.take();
        let Some(result) = result else {
            if self.stage != Stage::Cancelling {
                self.stage = match progress {
                    Progress::Preparing(f) => Stage::Preparing(f),
                    Progress::Uploading(sent, total) => Stage::Uploading { sent, total },
                };
            }
            return None;
        };
        self.job = None;
        match result {
            Ok(shared) => {
                // Cancel was pressed, but the commit had already gone: the item is online. The
                // clipboard is left as it was; the link is shown with its buttons.
                let cancelled = self.stage == Stage::Cancelling;
                let copied = !cancelled && shared.link.as_deref().is_some_and(&mut *copy);
                if copied {
                    self.copied_at = Some((self.clock)());
                }
                self.granted = shared.access;
                self.stage = Stage::Online { link: shared.link, copied };
                Some(ShareAction::Status(online_heading(self.kind)))
            }
            Err(ShareError::Cancelled) => Some(ShareAction::Closed(Some(CANCELLED.to_string()))),
            Err(e) => {
                self.refuse(&e);
                e.refusal(self.kind).map(|r| ShareAction::Status(r.heading))
            }
        }
    }

    /// A press on `button`. `copy` puts a link on the clipboard.
    pub fn press(&mut self, button: Button, copy: &mut dyn FnMut(&str) -> bool) -> Option<ShareAction> {
        match (button, &self.stage) {
            (Button::Cancel, Stage::Form | Stage::Blocked(_)) => Some(ShareAction::Closed(None)),
            (Button::Cancel, Stage::Preparing(_) | Stage::Uploading { .. }) => {
                if let Some(job) = &self.job {
                    job.cancel();
                }
                self.stage = Stage::Cancelling;
                None
            }
            (Button::Publish, Stage::Form) if self.measure.is_none() && self.who.problem().is_none() => Some(ShareAction::Publish),
            (Button::Close, Stage::Refused(_) | Stage::Blocked(_)) | (Button::Done, Stage::Online { .. }) => Some(ShareAction::Closed(None)),
            (Button::Open, Stage::Online { link: Some(link), .. }) => Some(ShareAction::Open(link.clone())),
            (Button::CopyLink, Stage::Online { link: Some(link), .. }) => {
                let copied = copy(link);
                self.copied_at = copied.then(|| (self.clock)());
                self.copy_failed = !copied;
                None
            }
            _ => None,
        }
    }

    /// The button Escape (or Enter) stands for now.
    fn key_button(&self, escape: bool) -> Option<Button> {
        match (&self.stage, escape) {
            (Stage::Form, true) | (Stage::Preparing(_) | Stage::Uploading { .. }, true) => Some(Button::Cancel),
            (Stage::Blocked(_) | Stage::Refused(_), _) => Some(Button::Close),
            (Stage::Online { .. }, _) => Some(Button::Done),
            (Stage::Form, false) => Some(Button::Publish),
            _ => None,
        }
    }

    /// What the window shows now.
    pub fn view(&self) -> View {
        let kind = self.kind;
        let mut v = View { caption: caption(kind), heading: heading(kind).to_string(), lines: Vec::new(), progress: None, link: None, buttons: Vec::new(), disabled: None, copied: false, lock: None };
        match &self.stage {
            Stage::Form => {
                let name = self.workspace.lock().ok().and_then(|n| n.clone());
                v.lines.extend(to_line(name.as_deref(), &self.server).map(Line::Muted));
                v.lines.push(Line::Title);
                v.lines.push(Line::Muted(self.details.clone()));
                if self.gif_note {
                    v.lines.push(Line::Muted(NO_FFMPEG.to_string()));
                }
                // Who can view, then what goes public: only for anyone with the link.
                for part in self.who.parts() {
                    v.lines.push(match part {
                        who::Part::Choice { offered, chosen } => Line::Who { offered, chosen },
                        who::Part::Addresses => Line::Addresses,
                        who::Part::Help(t) => Line::Note(t.to_string()),
                        who::Part::Muted(t) => Line::Muted(t),
                    });
                }
                if self.who.chosen() == who::Choice::Anyone {
                    v.lines.push(Line::Warning(warning(kind)));
                }
                v.buttons = vec![Button::Cancel, Button::Publish];
                if self.measure.is_some() || self.who.problem().is_some() {
                    v.disabled = Some(Button::Publish);
                }
            }
            Stage::Blocked(why) => {
                v.lines.push(Line::Text(why.clone()));
                v.buttons = vec![Button::Close];
            }
            Stage::Preparing(done) => {
                v.heading = PREPARING.to_string();
                v.progress = Some(*done);
                v.buttons = vec![Button::Cancel];
            }
            Stage::Uploading { sent, total } => {
                v.heading = UPLOADING.to_string();
                v.progress = Some(if *total == 0 { 0.0 } else { (*sent as f64 / *total as f64) as f32 });
                if *total > 0 {
                    v.lines.push(Line::Muted(sent_text(*sent, *total)));
                }
                v.buttons = vec![Button::Cancel];
            }
            Stage::Cancelling => {
                v.heading = CANCELLING.to_string();
            }
            Stage::Online { link, copied } => {
                v.heading = online_heading(kind);
                v.lock = self.granted.map(who::online_line);
                match link {
                    Some(link) => {
                        v.link = Some(link.clone());
                        if *copied {
                            v.lines.push(Line::Muted(COPIED.to_string()));
                        }
                        if self.copy_failed {
                            v.lines.push(Line::Muted(COPY_FAILED.to_string()));
                        }
                        v.copied = self.copied_left().is_some();
                        v.buttons = vec![Button::CopyLink, Button::Open, Button::Done];
                    }
                    None => {
                        v.lines.push(Line::Text(no_link(kind)));
                        v.buttons = vec![Button::Done];
                    }
                }
            }
            Stage::Refused(r) => {
                v.heading = r.heading.clone();
                v.lines.push(Line::Text(r.text.clone()));
                v.lines.extend(r.note.map(|n| Line::Note(n.to_string())));
                v.buttons = vec![Button::Close];
            }
        }
        v
    }

    /// Draw the window; Some when the editor has something to do. Escape and Enter are taken from the editor.
    pub fn show(&mut self, ctx: &egui::Context, copy: &mut dyn FnMut(&str) -> bool) -> Option<ShareAction> {
        // The frame that sees the job end draws its result at once (the link, a refusal): no
        // frame without the window, and none that waits for the mouse to move. Only a closed
        // window draws nothing.
        let polled = self.poll(copy);
        if let Some(ShareAction::Closed(_)) = polled {
            return polled;
        }
        // An open dropdown takes Escape itself, and Enter in the address field is a new line.
        let escape = !egui::Popup::is_any_open(ctx) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let enter = !self.who.typing(ctx) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let mut pressed = if escape || enter { self.key_button(escape).filter(|b| self.view().disabled != Some(*b)) } else { None };
        let view = self.view();
        let online = matches!(self.stage, Stage::Online { .. });
        let can_switch = self.can_switch();
        modal(egui::Id::unique("share-window")).frame(window_frame(ctx)).show(ctx, |ui| {
            theme::roomy(ui);
            ui.set_width(WINDOW_WIDTH);
            ui.horizontal(|ui| {
                ui.label(RichText::new(view.caption).color(theme::MUTED).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    crate::profiles_ui::dropdown(ui, "share-window", can_switch);
                });
            });
            ui.add_space(4.0);
            heading_label(ui, &view.heading);
            for line in &view.lines {
                ui.add_space(6.0);
                match line {
                    Line::Muted(t) => {
                        ui.add(egui::Label::new(RichText::new(t).color(theme::MUTED)).wrap());
                    }
                    Line::Text(t) => {
                        ui.add(egui::Label::new(t.as_str()).wrap());
                    }
                    Line::Note(t) => {
                        ui.add(egui::Label::new(RichText::new(t).color(theme::MUTED).size(12.0)).wrap());
                    }
                    Line::Warning(t) => {
                        theme::caution_box(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(t).color(theme::CAUTION_TEXT)).wrap());
                        });
                    }
                    Line::Title => {
                        ui.label(RichText::new(TITLE_LABEL).bold());
                        ui.add(egui::TextEdit::singleline(&mut self.title).char_limit(share::TITLE_MAX).desired_width(f32::INFINITY));
                    }
                    Line::Who { offered, chosen } => who::draw_part(ui, &who::Part::Choice { offered: offered.clone(), chosen: *chosen }, &mut self.who, theme::MUTED),
                    Line::Addresses => who::draw_part(ui, &who::Part::Addresses, &mut self.who, theme::MUTED),
                }
            }
            // Without a link the lock line follows the sentence; with one it goes under the link row.
            if let (None, Some(lock)) = (&view.link, &view.lock) {
                ui.add_space(6.0);
                who::lock_line(ui, lock, theme::MUTED);
            }
            if let Some(p) = view.progress {
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(p));
            }
            if online {
                if let Some(link) = &view.link {
                    gap(ui, BLOCK_GAP);
                    if link_row(ui, link, Button::CopyLink.label(), view.copied).clicked() {
                        pressed = Some(Button::CopyLink);
                    }
                    if let Some(lock) = &view.lock {
                        ui.add_space(8.0);
                        who::lock_line(ui, lock, theme::MUTED);
                    }
                }
                gap(ui, BLOCK_GAP);
                // With a link the accent belongs to Copy link; without one Done stands alone and keeps it.
                let actions: Vec<Button> = view.buttons.iter().copied().filter(|b| *b != Button::CopyLink).collect();
                let row: Vec<ActionButton> = actions.iter().map(|b| ActionButton { label: view.label(*b), accent: view.link.is_none() }).collect();
                for (button, response) in actions.iter().zip(action_row(ui, &row)) {
                    if response.clicked() {
                        pressed = Some(*button);
                    }
                }
            } else if !view.buttons.is_empty() {
                gap(ui, BLOCK_GAP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (i, button) in view.buttons.iter().enumerate().rev() {
                        let main = i + 1 == view.buttons.len();
                        let text = RichText::new(view.label(*button)).size(BUTTON_TEXT);
                        let b = if main { egui::Button::new(theme::strong(text.color(theme::ON_PRIMARY))).fill(theme::PRIMARY) } else { egui::Button::new(text) };
                        if ui.add_enabled(view.disabled != Some(*button), b.min_size(vec2(BUTTON_MIN_WIDTH, BUTTON_HEIGHT)).corner_radius(theme::control_radius(BUTTON_HEIGHT))).clicked() {
                            pressed = Some(*button);
                        }
                    }
                });
            }
        });
        // What the job said comes first; a press on a window the user has not seen yet is dropped.
        let action = match polled {
            Some(action) => Some(action),
            None => {
                if pressed.is_some() {
                    // A press changes what the window shows ("Copied", a stage): the next frame now.
                    ctx.request_repaint();
                }
                pressed.and_then(|b| self.press(b, copy))
            }
        };
        if self.job.is_some() || self.measure.is_some() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        // A frame for when "Copied" goes back to "Copy link", and none after that.
        match self.copied_left() {
            Some(left) => ctx.request_repaint_after(left),
            None => self.copied_at = None,
        }
        action
    }
}

// ---- The look of the window, shared with the tour result screen ----

/// The size of the buttons' text.
const BUTTON_TEXT: f32 = 14.0;
/// The narrowest a button of the stages without a link row is.
const BUTTON_MIN_WIDTH: f32 = 96.0;
/// The copy icon, and the space between it and its label.
const ICON_SIZE: f32 = 16.0;
const ICON_GAP: f32 = 8.0;
/// Left and right of what a filled button holds.
const BUTTON_PADDING_X: f32 = 18.0;
/// Left and right of the link in its field.
const FIELD_PADDING_X: f32 = 14.0;
const LINK_TEXT: f32 = 13.0;

/// The window's frame: the app's popup frame, as every window of the app has (surface-raised,
/// a 1px border-strong edge, radius-lg), with the mockup's padding.
pub fn window_frame(ctx: &egui::Context) -> egui::Frame {
    let style = ctx.global_style();
    egui::Frame::popup(&style).inner_margin(WINDOW_PADDING).corner_radius(CornerRadius::same(theme::RADIUS_LG))
}

/// Every dialog of the app: an [`egui::Modal`] whose backdrop still covers the whole window and
/// takes every click outside the dialog, so nothing behind it reacts, but draws nothing: no window
/// of the app is dimmed behind a dialog.
pub fn modal(id: egui::Id) -> egui::Modal {
    egui::Modal::new(id).backdrop_color(Color32::TRANSPARENT)
}

/// The window's heading.
pub fn heading_label(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).heading().size(HEADING_SIZE)).wrap());
}

/// `space` between two blocks, the item spacing egui adds of itself included.
pub fn gap(ui: &mut egui::Ui, space: f32) {
    ui.add_space((space - ui.spacing().item_spacing.y).max(0.0));
}

/// The link and its copy button in one bordered row, [`LINK_ROW_HEIGHT`] high across the
/// whole width. The link is in a monospace face on the darker field text boxes have, on one
/// line, cut short with "…" when it does not fit (the whole link shows on hover). The field
/// takes keyboard focus before the button; a click or double-click on it selects it whole,
/// and Ctrl+C while it has focus copies the whole link, never only the part that shows. The
/// button closes the row on the right: primary with a copy icon and `copy_label`, or while
/// `copied` the success tint and text with a tick and [`COPIED_LABEL`]. It is as wide for both, so
/// nothing moves. The button's response.
pub fn link_row(ui: &mut egui::Ui, link: &str, copy_label: &str, copied: bool) -> egui::Response {
    let visuals = ui.visuals().clone();
    let font = egui::FontId::proportional(BUTTON_TEXT);
    let text_width = |ui: &egui::Ui, text: &str| ui.painter().layout_no_wrap(text.to_string(), font.clone(), theme::TEXT).size().x;
    let button_width = text_width(ui, copy_label).max(text_width(ui, COPIED_LABEL)) + ICON_SIZE + ICON_GAP + 2.0 * BUTTON_PADDING_X;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), LINK_ROW_HEIGHT), Sense::hover());
    let r = theme::control_radius(LINK_ROW_HEIGHT);
    let button = egui::Rect::from_min_max(egui::pos2(row.right() - button_width, row.top()), row.max);
    let field = egui::Rect::from_min_max(row.min, egui::pos2(button.left(), row.bottom()));

    let left_corners = CornerRadius { nw: r, sw: r, ne: 0, se: 0 };
    ui.painter().rect_filled(field, left_corners, visuals.extreme_bg_color);
    // Laid out here rather than as a label: a label's selection would copy only what shows.
    let text_color = visuals.strong_text_color();
    let mut job = egui::text::LayoutJob::single_section(link.to_string(), egui::TextFormat::simple(egui::FontId::monospace(LINK_TEXT), text_color));
    job.wrap = egui::text::TextWrapping::truncate_at_width(field.width() - 2.0 * FIELD_PADDING_X);
    let shown = ui.fonts_mut(|f| f.layout_job(job));
    let mut field_response = ui.interact(field, ui.make_persistent_id("link-row-field"), Sense::click()).on_hover_cursor(egui::CursorIcon::Text);
    if shown.elided {
        field_response = field_response.on_hover_text(link);
    }
    if field_response.clicked() || field_response.double_clicked() {
        field_response.request_focus();
    }
    let selected = field_response.has_focus();
    if selected && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
        ui.ctx().copy_text(link.to_string());
    }
    let text_pos = egui::pos2(field.left() + FIELD_PADDING_X, field.center().y - shown.size().y / 2.0);
    if selected {
        // Selected whole: what Ctrl+C copies.
        ui.painter().rect_filled(egui::Rect::from_min_size(text_pos, shown.size()).expand(2.0), 2.0, visuals.selection.bg_fill);
        ui.painter().rect_stroke(field.shrink(2.0), left_corners, visuals.selection.stroke, StrokeKind::Inside);
    }
    ui.painter().galley(text_pos, shown, text_color);
    field_response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, link));

    // After the field: keyboard focus goes link, then this button, then the action row.
    let response = ui.interact(button, ui.make_persistent_id("link-row-copy"), Sense::click());
    let (base, hover, fg, icon, label) = if copied {
        (theme::SUCCESS_TINT, theme::SUCCESS_TINT.lerp_to_gamma(theme::SUCCESS_BORDER, 0.25), theme::SUCCESS_TEXT, Icon::Check, COPIED_LABEL)
    } else {
        (theme::PRIMARY, theme::PRIMARY_HOVER, theme::ON_PRIMARY, Icon::Copy, copy_label)
    };
    let fill = if response.is_pointer_button_down_on() || response.hovered() { hover } else { base };
    let right = CornerRadius { nw: 0, sw: 0, ne: r, se: r };
    ui.painter().rect_filled(button, right, fill);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, fg);
    let content = ICON_SIZE + ICON_GAP + galley.size().x;
    let left = button.center().x - content / 2.0;
    icons::paint(ui.painter(), egui::Rect::from_center_size(egui::pos2(left + ICON_SIZE / 2.0, button.center().y), egui::Vec2::splat(ICON_SIZE)), icon, fg);
    ui.painter().galley(egui::pos2(left + ICON_SIZE + ICON_GAP, button.center().y - galley.size().y / 2.0), galley, fg);
    ui.painter().rect_stroke(row, r, Stroke::new(1.0, theme::BORDER_STRONG), StrokeKind::Inside);
    if response.has_focus() {
        ui.painter().rect_stroke(button.shrink(2.0), right, visuals.selection.stroke, StrokeKind::Inside);
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    response
}

/// One button of [`action_row`].
pub struct ActionButton<'a> {
    pub label: &'a str,
    /// Filled with the accent. The online window gives it to none of them: it belongs to Copy link.
    pub accent: bool,
}

/// `buttons` side by side across the whole width, all as wide, [`BUTTON_GAP`] apart and
/// [`BUTTON_HEIGHT`] high, left to right in the order keyboard focus takes them. Their responses.
pub fn action_row(ui: &mut egui::Ui, buttons: &[ActionButton]) -> Vec<egui::Response> {
    let width = ui.available_width();
    let n = buttons.len().max(1) as f32;
    let each = (width - BUTTON_GAP * (n - 1.0)) / n;
    let (row, _) = ui.allocate_exact_size(vec2(width, BUTTON_HEIGHT), Sense::hover());
    buttons
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let rect = egui::Rect::from_min_size(egui::pos2(row.left() + i as f32 * (each + BUTTON_GAP), row.top()), vec2(each, BUTTON_HEIGHT));
            let text = RichText::new(b.label).size(BUTTON_TEXT);
            let button = if b.accent { egui::Button::new(theme::strong(text.color(theme::ON_PRIMARY))).fill(theme::PRIMARY) } else { egui::Button::new(text) };
            ui.put(rect, button.min_size(rect.size()).corner_radius(theme::control_radius(BUTTON_HEIGHT)))
        })
        .collect()
}

// ---- The editors ----

impl Editor {
    /// "Publish to…" in the image editor: the edited image as "Save" would write it, as PNG.
    pub(crate) fn open_share_image(&mut self) {
        self.open_share_image_with(share::png_bytes);
    }

    /// [`Editor::open_share_image`] with the encoder given. An image over a limit that needs no
    /// file (its sides) is refused at once and never encoded; any other is encoded on a thread
    /// of its own, and the window shows its size once it is known.
    pub(crate) fn open_share_image_with(&mut self, encode: fn(&image::RgbaImage) -> Result<Vec<u8>, String>) {
        if self.share.is_some() {
            return;
        }
        let img = self.doc.flatten();
        let (width, height) = (img.width(), img.height());
        let server = publish_ui::server();
        let blocked = blocked_reason(&server);
        let known = format!("{width} × {height} · {}", MediaType::Png.label());
        if let Some(t) = share::too_large(Kind::Image, None, true, width, height, None) {
            let mut d = ShareDialog::new(Kind::Image, server, self.title(), known, false, blocked, None);
            d.refuse(&ShareError::TooLarge(t));
            self.share = Some(d);
            return;
        }
        let img = Arc::new(img);
        let encoded: Encoded = Arc::new(Mutex::new(None));
        let (source, slot) = (img.clone(), encoded.clone());
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| encode(&source))).unwrap_or_else(|_| Err(String::new()));
            if let Ok(mut s) = slot.lock() {
                *s = Some(result);
            }
        });
        let ready = encoded.clone();
        let measure: Measure = Box::new(move || {
            let slot = ready.lock().ok()?;
            Some(match slot.as_ref()? {
                Ok(png) => {
                    let bytes = png.len() as u64;
                    match share::too_large(Kind::Image, Some(bytes), true, width, height, None) {
                        Some(t) => Err(ShareError::TooLarge(t)),
                        None => Ok(image_details(width, height, MediaType::Png, bytes)),
                    }
                }
                Err(_) => Err(ShareError::Failed(NOT_ENCODED.to_string())),
            })
        });
        let starter: Starter = Box::new(move |server, title, access| {
            // The job keeps the profile it starts with, whatever is switched to meanwhile.
            let pin = publish_ui::pin_for(&server);
            // Held until the job ends: its token is not ended at the site under it.
            let held = publish_ui::hold(&pin);
            ShareJob::spawn(Kind::Image, move |r| {
                let _held = held;
                let png = encoded.lock().ok().and_then(|mut s| s.take()).and_then(Result::ok).ok_or_else(|| ShareError::Failed(NOT_ENCODED.to_string()))?;
                let token = publish_ui::token_for_pin(&pin).map_err(ShareError::Failed)?;
                let item = Item { media: MediaType::Png, data: png, width, height, duration_ms: None, title, preview: share::preview_of(&img) };
                share::share(&server, &token, &item, &access, |sent, total| r.uploading(sent, total))
            })
        });
        self.share = Some(ShareDialog::open(Kind::Image, server, self.title(), known, false, blocked, starter).measuring(measure));
    }

    /// The share window, while it is open: draw it and do what it asks.
    pub(crate) fn share_window(&mut self, ctx: &egui::Context) {
        let Some(d) = self.share.as_mut() else { return };
        d.follow_profile(&publish_ui::pin());
        d.update_blocked(blocked_reason(d.server()));
        match d.show(ctx, &mut copy_to_clipboard) {
            None => {}
            Some(ShareAction::Publish) => {
                // Checked again: the connection may have changed since the window opened.
                match blocked_reason(d.server()) {
                    Some(why) => d.update_blocked(Some(why)),
                    None => d.begin(),
                }
                let waiter = d.waiter();
                if let (Some(v), Some(w)) = (self.video.as_mut(), waiter) {
                    v.share_waiter = Some(w);
                }
            }
            Some(ShareAction::Closed(status)) => {
                self.share = None;
                if let Some(status) = status {
                    self.notify(status);
                }
            }
            Some(ShareAction::Open(url)) => {
                // The opener checks the link itself, and its error never repeats the link.
                if let Err(e) = mythicshot_destinations::open_url(&url) {
                    eprintln!("Publish to: could not open the browser: {e}");
                    self.notify("Could not open the browser. Copy the link instead.");
                }
            }
            // Never the link itself in the log.
            Some(ShareAction::Status(status)) => {
                eprintln!("Publish to: {status}");
                self.notify(status);
            }
        }
    }
}

/// Remove `dir`, trying again for a moment while a program that wrote into it (ffmpeg, just
/// stopped) still holds a file there.
pub fn remove_soon(dir: &Path) {
    for _ in 0..20 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A folder name `tempfile` gives a clip being prepared: the prefix and six letters or digits.
pub fn is_temp_dir_name(name: &str) -> bool {
    name.strip_prefix(TEMP_PREFIX).is_some_and(|rest| rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Remove clip folders in `temp` older than `age` that a crash or a forced quit left behind:
/// only real folders (not links) with exactly that name, holding nothing but a clip file.
pub fn sweep_stale(temp: &Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for e in entries.flatten() {
        if !e.file_name().to_str().is_some_and(is_temp_dir_name) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
        if !meta.is_dir() || meta.modified().ok().and_then(|m| m.elapsed().ok()).is_none_or(|a| a < age) {
            continue;
        }
        let only_clip = std::fs::read_dir(e.path()).map(|inner| inner.flatten().all(|i| i.file_name() == "clip.mp4" || i.file_name() == "clip.gif")).unwrap_or(false);
        if only_clip {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

pub mod who;

#[cfg(test)]
pub(crate) mod tests;
