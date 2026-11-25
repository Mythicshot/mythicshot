//! Updates as the person sees them: the line under the main window's menu bar, the Updates
//! section of Preferences, the tray entry, and the popup at the start after an in-app update.
//! The network, the files and the installer are `update`'s; this holds what is going on
//! ([`Updater`]) and draws it.
//!
//! The work runs on threads of its own and lands in slots that [`Updater::poll`] takes in on the
//! next frame, as `profiles_ui` does; nothing here ever waits for the network.
//!
//! Wording: every sentence here is approved by the owner, character for character; add none.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::NaiveDateTime;
use eframe::egui::{self, RichText};

use crate::theme;
use crate::update::pending::AfterUpdate;
use crate::update::{self, Checked, Downloaded, Release, Version};

// ---- Approved wording ----

/// Approved: the section's title in Preferences.
pub const SECTION: &str = "Updates";
/// Approved: the setting's checkbox.
pub const CHECKBOX: &str = "Check for a newer version when Mythicshot starts";
/// Approved: the helper text under it.
pub const HELPER: &str = "Asks mythicshot.com for the current version number. Nothing about you or this computer is sent.";
/// Approved: the button beside the status line.
pub const CHECK_NOW: &str = "Check now";
/// Approved: the link on the line that offers a newer version.
pub const WHATS_NEW: &str = "What's new";
/// Approved: while the install waits for work that is running.
pub const WAITING: &str = "A recording or upload is still running. The update starts when it finishes.";
/// Approved: the download was not the file the release file lists.
pub const MISMATCH: &str = "The download did not match what mythicshot.com lists, so it was not installed. Nothing was changed.";

/// Approved: the line that offers a newer version.
pub fn available_text(new: Version, current: Version) -> String {
    format!("Mythicshot {new} is available. You have {current}.")
}

/// Approved: the tray menu's first entry while an update is available.
pub fn tray_text(new: Version) -> String {
    format!("Update to {new}…")
}

/// Approved: the download's progress, in MB with one decimal.
pub fn downloading_text(new: Version, done: u64, total: u64) -> String {
    format!("Downloading Mythicshot {new}… {} of {} MB", mb(done), mb(total))
}

/// Approved: the download matched; the app is about to close for the installer.
pub fn closing_text(new: Version) -> String {
    format!("Checked. Mythicshot will close, install {new} and open again.")
}

fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

/// The buttons of the line, each with its approved label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Drawn as a link.
    WhatsNew,
    Later,
    UpdateNow,
    Cancel,
    CancelUpdate,
    TryAgain,
    OpenDownloadPage,
}

impl Button {
    /// Approved labels.
    pub fn label(self) -> &'static str {
        match self {
            Button::WhatsNew => WHATS_NEW,
            Button::Later => "Later",
            Button::UpdateNow => "Update now",
            Button::Cancel => "Cancel",
            Button::CancelUpdate => "Cancel update",
            Button::TryAgain => "Try again",
            Button::OpenDownloadPage => "Open download page",
        }
    }
}

/// Approved: the line in the main window when the download broke off or stalled, with "Try
/// again" and "Open download page".
pub const DOWNLOAD_FAILED: &str = "The download from mythicshot.com did not finish, so nothing was installed.";

/// Approved: the popup's title at the start after an in-app update that worked.
pub const UPDATED_TITLE: &str = "Mythicshot is up to date";
/// Approved: the popup's title when the version updated from is still the one running.
pub const NOT_FINISHED_TITLE: &str = "The update did not finish";
/// Approved: the popup's button that closes it.
pub const OK: &str = "OK";

/// Approved: the popup's line after an update that worked.
pub fn updated_text(from: Version, to: Version) -> String {
    format!("Updated from {from} to {to}.")
}

/// Approved: the popup's lines when the update did not finish, naming where Check now is. The
/// second sentence stands on a line of its own, so "Preferences › Updates" is never split.
pub fn not_finished_text(from: Version) -> String {
    format!("Mythicshot is still {from}.\nYou can try again from Preferences › Updates.")
}

/// The popup's buttons, each with its approved label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeButton {
    /// Opens the changes page, then closes the popup.
    WhatsNew,
    /// The primary: closes the popup.
    Ok,
}

impl NoticeButton {
    pub fn label(self) -> &'static str {
        match self {
            NoticeButton::WhatsNew => WHATS_NEW,
            NoticeButton::Ok => OK,
        }
    }
}

/// The popup for `after`: its title, its line, and its buttons left to right (the primary last).
pub fn notice_view(after: AfterUpdate) -> (&'static str, String, &'static [NoticeButton]) {
    match after {
        AfterUpdate::Updated { from, to } => (UPDATED_TITLE, updated_text(from, to), &[NoticeButton::WhatsNew, NoticeButton::Ok]),
        AfterUpdate::NotFinished { from } => (NOT_FINISHED_TITLE, not_finished_text(from), &[NoticeButton::Ok]),
    }
}

// ---- The status line ----

/// What the last check this run found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    Newer(Version),
    /// Only a Check now that failed is noted; a failed check at start is shown nowhere.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastCheck {
    pub at: NaiveDateTime,
    pub outcome: Outcome,
}

/// Approved: when a check was, as the status line says it: "today, 6:13 PM", "yesterday,
/// 6:13 PM" or "Nov 24, 6:13 PM".
fn when(at: NaiveDateTime, now: NaiveDateTime) -> String {
    let time = at.format("%-I:%M %p").to_string();
    match (now.date() - at.date()).num_days() {
        0 => format!("today, {time}"),
        1 => format!("yesterday, {time}"),
        _ => format!("{}, {time}", at.format("%b %-d")),
    }
}

/// The status line beside Check now. Approved pattern: "Version 0.2.34. Last checked today,
/// 6:13 PM: up to date.", and the approved line of every other state.
pub fn status_text(running: Version, checking: bool, last: Option<&LastCheck>, now: NaiveDateTime) -> String {
    if checking {
        return format!("Version {running}. Checking now…");
    }
    match last {
        None => format!("Version {running}. Not checked yet."),
        Some(last) => {
            let when = when(last.at, now);
            match last.outcome {
                Outcome::UpToDate => format!("Version {running}. Last checked {when}: up to date."),
                Outcome::Newer(new) => format!("Version {running}. Last checked {when}: {new} is available."),
                Outcome::Failed => format!("Version {running}. Last checked {when}: mythicshot.com could not be reached or did not answer as expected."),
            }
        }
    }
}

// ---- The work ----

/// What the update screens need of the world: the network, the updates folder and the log. The
/// app hands in [`Official`]; a test its own, which sends nothing anywhere.
pub trait Net: Send + Sync {
    fn check(&self, running: Version) -> Result<Checked, String>;
    fn download(&self, release: &Release, attempt: u64, done: &AtomicU64, cancel: &AtomicBool) -> Downloaded;
    /// Remove partial downloads and the installers of other versions than `running`; how many.
    fn clean(&self, running: Version) -> usize;
    /// Delete a downloaded installer that will not be used.
    fn discard(&self, path: &Path);
    /// One `capture.log` line.
    fn log(&self, step: &str);
}

/// mythicshot.com, the app's own updates folder and `capture.log`.
pub struct Official;

impl Net for Official {
    fn check(&self, running: Version) -> Result<Checked, String> {
        update::check(running)
    }
    fn download(&self, release: &Release, attempt: u64, done: &AtomicU64, cancel: &AtomicBool) -> Downloaded {
        match update::updates_dir() {
            Some(dir) => update::download(release, &dir, attempt, done, cancel),
            None => Downloaded::Failed("there is no folder for updates".into()),
        }
    }
    fn clean(&self, running: Version) -> usize {
        update::updates_dir().map_or(0, |dir| update::clean(&dir, running))
    }
    fn discard(&self, path: &Path) {
        // Only a file in the updates folder, under a name the app gave it.
        if update::updates_dir().is_some_and(|dir| path.parent() == Some(dir.as_path())) {
            let _ = std::fs::remove_file(path);
        }
    }
    fn log(&self, step: &str) {
        update::log(step);
    }
}

/// A network that is never asked anything: for a window made before the app hands in its own.
struct Nowhere;

impl Net for Nowhere {
    fn check(&self, _running: Version) -> Result<Checked, String> {
        Err("not connected".into())
    }
    fn download(&self, _release: &Release, _attempt: u64, _done: &AtomicU64, _cancel: &AtomicBool) -> Downloaded {
        Downloaded::Failed("not connected".into())
    }
    fn clean(&self, _running: Version) -> usize {
        0
    }
    fn discard(&self, _path: &Path) {}
    fn log(&self, _step: &str) {}
}

type Slot<T> = Arc<Mutex<Option<T>>>;

fn take<T>(slot: &Slot<T>) -> Option<T> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Run `work` on a thread of its own; what it ends with (or `broke` if it panics) lands in the slot.
fn spawn<T: Send + 'static>(wake: &Arc<dyn Fn() + Send + Sync>, work: impl FnOnce() -> T + Send + 'static, broke: T) -> Slot<T> {
    let slot: Slot<T> = Arc::new(Mutex::new(None));
    let (into, wake) = (slot.clone(), wake.clone());
    std::thread::spawn(move || {
        let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or(broke);
        *into.lock().unwrap_or_else(|e| e.into_inner()) = Some(end);
        wake();
    });
    slot
}

/// Where the download stands.
enum Flow {
    Idle,
    Downloading { release: Release, done: Arc<AtomicU64>, cancel: Arc<AtomicBool>, slot: Slot<Downloaded> },
    /// Verified: waits until nothing is running, then the app closes and starts the installer.
    Ready { release: Release, path: PathBuf },
    Mismatch,
    Failed,
}

/// A web address the app is asked to open: the page and its place on it (empty for none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    Open(&'static str, String),
}

/// What the line in the main window shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub text: String,
    /// The download's progress, 0 to 1.
    pub progress: Option<f32>,
    pub buttons: Vec<Button>,
}

/// Everything about updates in this run.
pub struct Updater {
    net: Arc<dyn Net>,
    running: Version,
    wake: Arc<dyn Fn() + Send + Sync>,
    started: bool,
    /// The work at start: cleaning, and the check when the setting is on (None when it is off).
    start_slot: Option<Slot<Option<Result<Checked, String>>>>,
    /// The work at start includes a check (the setting was on). A clean alone is no check: Check
    /// now runs beside it.
    start_checks: bool,
    /// A Check now that runs.
    asked: Option<Slot<Result<Checked, String>>>,
    last: Option<LastCheck>,
    available: Option<Release>,
    /// Later was pressed: the line is hidden until the next start.
    later: bool,
    flow: Flow,
    attempts: u64,
    /// Set by the app every frame while the install waits: something is still running.
    waiting: bool,
    /// What the app is asked to open, oldest first; it takes them.
    pub asks: Vec<Ask>,
    /// The popup at the start after an in-app update, until it is closed.
    notice: Option<Notice>,
}

/// The popup at the start after an in-app update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Notice {
    after: AfterUpdate,
    /// The main window was opened only to show it: it closes with it.
    close_window: bool,
}

impl Default for Updater {
    fn default() -> Self {
        Updater::new(Arc::new(Nowhere), Version::running())
    }
}

impl Updater {
    pub fn new(net: Arc<dyn Net>, running: Version) -> Updater {
        Updater { net, running, wake: Arc::new(|| {}), started: false, start_slot: None, start_checks: false, asked: None, last: None, available: None, later: false, flow: Flow::Idle, attempts: 0, waiting: true, asks: Vec::new(), notice: None }
    }

    /// Wake the app when background work ends.
    pub fn with_wake(mut self, wake: impl Fn() + Send + Sync + 'static) -> Updater {
        self.wake = Arc::new(wake);
        self
    }

    /// Once per run, after the UI is up: remove old installers, and with the setting on (`check`)
    /// ask the site once. Off: no request at all. Never waits.
    pub fn start(&mut self, check: bool) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        self.start_checks = check;
        let (net, running) = (self.net.clone(), self.running);
        self.start_slot = Some(spawn(
            &self.wake,
            move || {
                let removed = net.clean(running);
                net.log(&format!("clean: {removed} old installers removed"));
                check.then(|| net.check(running))
            },
            Some(Err("the check broke off".into())),
        ));
    }

    /// Check now: works whatever the setting says. One at a time: none starts while one runs,
    /// the check at start included (the button is greyed out then, and Help › Check for updates
    /// asks through here).
    pub fn check_now(&mut self) {
        if self.checking() {
            return;
        }
        let (net, running) = (self.net.clone(), self.running);
        self.asked = Some(spawn(&self.wake, move || net.check(running), Err("the check broke off".into())));
    }

    /// Whether background work this holds a slot for is still running.
    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.start_slot.is_some() || self.asked.is_some() || matches!(self.flow, Flow::Downloading { .. })
    }

    /// A check is running: a Check now, or the check at start (not a clean alone).
    fn checking(&self) -> bool {
        self.asked.is_some() || (self.start_checks && self.start_slot.is_some())
    }

    /// Take in what background work ended with. `now` is the time a check that ended is noted at.
    pub fn poll(&mut self, now: NaiveDateTime) {
        if let Some(slot) = &self.start_slot
            && let Some(end) = take(slot)
        {
            self.start_slot = None;
            match end {
                None => self.net.log("check: off"),
                Some(Ok(found)) => self.checked(found, now),
                // At start a failure is shown nowhere; it is only logged.
                Some(Err(e)) => self.net.log(&format!("check: failed ({e})")),
            }
        }
        if let Some(slot) = &self.asked
            && let Some(end) = take(slot)
        {
            self.asked = None;
            match end {
                Ok(found) => self.checked(found, now),
                Err(e) => {
                    self.net.log(&format!("check now: failed ({e})"));
                    self.last = Some(LastCheck { at: now, outcome: Outcome::Failed });
                }
            }
        }
        let ended = match &self.flow {
            Flow::Downloading { slot, .. } => take(slot),
            _ => None,
        };
        if let Some(end) = ended {
            let flow = std::mem::replace(&mut self.flow, Flow::Idle);
            self.flow = match (end, flow) {
                (Downloaded::Ready(path), Flow::Downloading { release, .. }) => {
                    self.net.log("download: size and SHA-256 match");
                    Flow::Ready { release, path }
                }
                (Downloaded::Mismatch, _) => {
                    self.net.log("download: did not match the release file, deleted");
                    Flow::Mismatch
                }
                (Downloaded::Failed(e), _) => {
                    self.net.log(&format!("download: failed ({e})"));
                    Flow::Failed
                }
                (_, _) => {
                    self.net.log("download: cancelled");
                    Flow::Idle
                }
            };
        }
    }

    fn checked(&mut self, found: Checked, now: NaiveDateTime) {
        match found {
            Checked::UpToDate => {
                self.net.log(&format!("check: up to date at {}", self.running));
                self.last = Some(LastCheck { at: now, outcome: Outcome::UpToDate });
                if matches!(self.flow, Flow::Idle) {
                    self.available = None;
                }
            }
            Checked::Newer(release) => {
                self.net.log(&format!("check: {} offered", release.version()));
                self.last = Some(LastCheck { at: now, outcome: Outcome::Newer(release.version()) });
                if matches!(self.flow, Flow::Idle | Flow::Mismatch | Flow::Failed) {
                    self.available = Some(release);
                }
            }
        }
    }

    /// The status line beside Check now.
    pub fn status_line(&self, now: NaiveDateTime) -> String {
        status_text(self.running, self.checking(), self.last.as_ref(), now)
    }

    /// The tray menu's first entry, while an update is available.
    pub fn tray_label(&self) -> Option<String> {
        self.available.as_ref().map(|r| tray_text(r.version()))
    }

    /// The tray entry: what Update now (or Try again) does. True when a download started.
    pub fn tray_update(&mut self) -> bool {
        self.start_download()
    }

    fn start_download(&mut self) -> bool {
        let Some(release) = self.available.clone() else { return false };
        if !matches!(self.flow, Flow::Idle | Flow::Mismatch | Flow::Failed) {
            return false;
        }
        self.attempts += 1;
        let (done, cancel) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
        let (net, r, d, c, attempt) = (self.net.clone(), release.clone(), done.clone(), cancel.clone(), self.attempts);
        self.net.log(&format!("download: {} started", release.version()));
        let slot = spawn(&self.wake, move || net.download(&r, attempt, &d, &c), Downloaded::Failed("the download broke off".into()));
        self.flow = Flow::Downloading { release, done, cancel, slot };
        true
    }

    /// A button of the line was pressed.
    pub fn press(&mut self, button: Button) {
        match button {
            Button::WhatsNew => {
                if let Some(r) = &self.available {
                    self.asks.push(Ask::Open(update::CHANGES_PAGE, update::changes_anchor(r.version())));
                }
            }
            Button::OpenDownloadPage => self.asks.push(Ask::Open(update::DOWNLOAD_PAGE, String::new())),
            Button::Later => self.later = true,
            Button::UpdateNow | Button::TryAgain => {
                self.start_download();
            }
            Button::Cancel => {
                if let Flow::Downloading { cancel, .. } = &self.flow {
                    // The download deletes what it wrote once it sees this.
                    cancel.store(true, Ordering::SeqCst);
                    self.net.log("download: cancel pressed");
                    self.flow = Flow::Idle;
                }
            }
            Button::CancelUpdate => self.drop_ready(Flow::Idle, "install: cancelled, installer deleted"),
        }
    }

    fn drop_ready(&mut self, then: Flow, step: &str) {
        if let Flow::Ready { path, .. } = &self.flow {
            self.net.discard(path);
            self.net.log(step);
            self.flow = then;
        }
    }

    /// The verified installer, while it waits to be started.
    pub fn ready(&self) -> Option<(Release, PathBuf)> {
        match &self.flow {
            Flow::Ready { release, path } => Some((release.clone(), path.clone())),
            _ => None,
        }
    }

    /// Whether something is running that the install waits for; the app says so every frame.
    pub fn set_waiting(&mut self, waiting: bool) {
        if waiting && !self.waiting && matches!(self.flow, Flow::Ready { .. }) {
            self.net.log("install: waits for a recording, upload or editor");
        }
        self.waiting = waiting;
    }

    /// How soon the app should look again by itself: about once a second while a verified
    /// installer waits for running work, so the install starts when the work ends even with the
    /// main window closed to the tray; otherwise never.
    pub fn wake_after(&self) -> Option<std::time::Duration> {
        (matches!(self.flow, Flow::Ready { .. }) && self.waiting).then_some(std::time::Duration::from_secs(1))
    }

    /// The installer did not match any more just before it was to start: deleted, nothing installed.
    pub fn install_refused(&mut self) {
        self.drop_ready(Flow::Mismatch, "install: the installer did not match any more, deleted");
    }

    /// The app's quit was cancelled in an editor's question: the update is called off as with
    /// Cancel update.
    pub fn quit_cancelled(&mut self) {
        self.drop_ready(Flow::Idle, "install: cancelled in an editor, installer deleted");
    }

    /// Show the popup for `after` in the main window; `opened_window` says the window was opened
    /// only for it, so it closes with the popup.
    pub fn set_notice(&mut self, after: AfterUpdate, opened_window: bool) {
        self.notice = Some(Notice { after, close_window: opened_window });
    }

    /// The popup that is up, if one is.
    pub fn notice(&self) -> Option<AfterUpdate> {
        self.notice.map(|n| n.after)
    }

    /// A button of the popup was pressed: it closes, and What's new asks for the changes page.
    /// True when the main window was opened only for the popup and should close with it.
    pub fn press_notice(&mut self, button: NoticeButton) -> bool {
        let Some(notice) = self.notice.take() else { return false };
        if let (NoticeButton::WhatsNew, AfterUpdate::Updated { to, .. }) = (button, notice.after) {
            // At the new version's entry, as the update line's What's new opens it.
            self.asks.push(Ask::Open(update::CHANGES_PAGE, update::changes_anchor(to)));
        }
        notice.close_window
    }

    /// The person asked for the main window while the popup is up: it stays when the popup closes.
    pub fn keep_window(&mut self) {
        if let Some(notice) = &mut self.notice {
            notice.close_window = false;
        }
    }

    /// The main window was closed with the popup up: it was seen, and does not come back.
    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// The line in the main window, or None for no line.
    pub fn banner(&self) -> Option<Banner> {
        let line = |text: String, buttons: Vec<Button>| Some(Banner { text, progress: None, buttons });
        match &self.flow {
            Flow::Downloading { release, done, .. } => {
                let done = done.load(Ordering::SeqCst).min(release.size());
                Some(Banner { text: downloading_text(release.version(), done, release.size()), progress: Some(done as f32 / release.size().max(1) as f32), buttons: vec![Button::Cancel] })
            }
            Flow::Ready { .. } if self.waiting => line(WAITING.to_string(), vec![Button::CancelUpdate]),
            Flow::Ready { release, .. } => line(closing_text(release.version()), vec![]),
            Flow::Mismatch => line(MISMATCH.to_string(), vec![Button::TryAgain, Button::OpenDownloadPage]),
            Flow::Failed => line(DOWNLOAD_FAILED.to_string(), vec![Button::TryAgain, Button::OpenDownloadPage]),
            Flow::Idle => match &self.available {
                Some(r) if !self.later => line(available_text(r.version(), self.running), vec![Button::WhatsNew, Button::Later, Button::UpdateNow]),
                _ => None,
            },
        }
    }
}

// ---- Drawing ----

/// The line under the main window's menu bar, in the theme's gold text, when there is one.
pub fn banner_ui(ui: &mut egui::Ui, u: &mut Updater) {
    let Some(banner) = u.banner() else { return };
    if banner.progress.is_some() {
        // The progress moves on its own.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    let mut pressed = None;
    egui::Frame::new().fill(theme::SURFACE).inner_margin(egui::Margin::symmetric(theme::CARD_PAD_X as i8, theme::ITEM_GAP as i8)).show(ui, |ui| {
        // Outside the page, but on its sizes: the buttons match the pages' buttons.
        theme::roomy(ui);
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.set_min_height(theme::CONTROL_HEIGHT);
            ui.label(RichText::new(&banner.text).color(theme::ACCENT));
            for b in &banner.buttons {
                let clicked = match b {
                    Button::WhatsNew => ui.link(b.label()).clicked(),
                    Button::UpdateNow | Button::TryAgain => ui.add(theme::primary_button(b.label())).clicked(),
                    _ => ui.button(b.label()).clicked(),
                };
                if clicked {
                    pressed = Some(*b);
                }
            }
        });
        if let Some(p) = banner.progress {
            ui.add(egui::ProgressBar::new(p).desired_height(theme::SPACE_2).fill(theme::METER_USED));
        }
    });
    if let Some(b) = pressed {
        u.press(b);
    }
}

/// The popup at the start after an in-app update, over the main window, as every dialog of the
/// app is drawn ([`crate::share_ui::modal`]): the title, the line, and the buttons on the right,
/// the primary last. Enter and Escape are OK. True when the window should close with it.
pub fn notice_ui(ctx: &egui::Context, u: &mut Updater) -> bool {
    let Some(after) = u.notice() else { return false };
    let (title, text, buttons) = notice_view(after);
    let mut pressed = None;
    // Enter is OK unless a button has keyboard focus: then it presses that button.
    let free = ctx.memory(|m| m.focused().is_none());
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) || (free && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))) {
        pressed = Some(NoticeButton::Ok);
    }
    let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(theme::DIALOG_PAD);
    crate::share_ui::modal(egui::Id::unique("update-done-notice")).frame(frame).show(ctx, |ui| {
        theme::roomy(ui);
        ui.set_width(theme::DIALOG_WIDTH);
        ui.heading(title);
        ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).wrap());
        theme::block_gap(ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Right to left: the primary first, at the right edge.
            for b in buttons.iter().rev() {
                let button = if *b == NoticeButton::Ok { theme::primary_button(b.label()) } else { egui::Button::new(b.label()) };
                if ui.add(button).clicked() {
                    pressed = Some(*b);
                }
            }
        });
    });
    pressed.is_some_and(|b| u.press_notice(b))
}

/// The Updates section of Preferences: the setting, its helper text, the status line and Check
/// now. True when the setting changed.
pub fn prefs_ui(ui: &mut egui::Ui, u: &mut Updater, settings: &mut crate::settings::Settings, now: NaiveDateTime) -> bool {
    let mut changed = false;
    theme::section_heading(ui, SECTION);
    theme::card(ui, |rows| {
        rows.full(|ui| {
            // The helper text stands under the checkbox's label, a line's gap below it.
            ui.spacing_mut().item_spacing.y = theme::LINE_GAP;
            changed = theme::choices(ui, |ui| ui.checkbox(&mut settings.check_for_updates, CHECKBOX).changed());
            // Its top at the top of the line: centred, one line of it stood lower in the line's height.
            ui.horizontal_top(|ui| {
                ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
                theme::caption(ui, HELPER);
            });
        });
        let checking = u.checking();
        let status = u.status_line(now);
        let mut check = false;
        rows.row_keyed(
            "",
            "updates-status",
            None,
            |ui| {
                ui.add(egui::Label::new(RichText::new(status).size(theme::SMALL_SIZE).color(theme::MUTED)).wrap());
            },
            |_| {},
            |ui| check = ui.add_enabled(!checking, egui::Button::new(CHECK_NOW)).clicked(),
        );
        if check {
            u.check_now();
        }
        if checking {
            rows.repaint_after(std::time::Duration::from_millis(200));
        }
    });
    changed
}

#[cfg(test)]
pub(crate) mod tests;
