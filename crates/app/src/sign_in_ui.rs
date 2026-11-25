//! The "Sign in" window of the Workspace page: the short code to type in the browser, while
//! Mythicshot waits for it to be confirmed, and what happened when it was not.
//!
//! Everything the window says comes from [`SignInDialog::view`], and every button goes through
//! [`SignInDialog::press`], so the wording and the steps are tested without a window. The browser
//! is reached only through the opener the page hands in.
//!
//! The work runs on a thread of its own and is a [`Run`]: quitting cancels it like a publish,
//! and so does closing the window or the Workspace page (the dialog is dropped). The device code
//! and the token stay on that thread; the window only ever sees the short code and the page.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, RichText};

use crate::theme;
use crate::publish::connect::{self, ConnectError, Started};
use crate::publish_ui::Run;


pub const CAPTION: &str = "Sign in";
pub const HEADING: &str = "Type this code in your browser";
pub const OPENED: &str = "Your browser opened the Connections page of your dashboard. Type the code there and press Connect.";
/// When the browser could not be opened at the page.
pub const NOT_OPENED: &str = "Mythicshot could not open your browser. Open the Connections page of your dashboard yourself, type the code there and press Connect.";
/// When the site named no page Mythicshot can open. The wording is proposed and waits for the owner's approval.
pub const NO_PAGE: &str = "Mythicshot cannot open the page for this. Open the Connections page of your dashboard yourself, type the code there and press Connect.";
/// While the site is asked for a code. The wording is proposed and waits for the owner's approval.
pub const STARTING: &str = "Asking the site for a code…";
pub const OPEN_THE_PAGE: &str = "Open the page";
const BROKE_OFF: &str = "Signing in failed because of an internal error.";

/// "Waiting for you to confirm… The code works for 10 minutes."
pub fn waiting(minutes: Option<&str>) -> String {
    match minutes {
        Some(m) => format!("Waiting for you to confirm… The code works for {m}."),
        // Proposed: a code that works for less than a minute.
        None => "Waiting for you to confirm… The code works for less than a minute.".to_string(),
    }
}

/// The help line's first words; "Open the page" follows as a link, then [`go_to`].
pub const HELP_BEFORE: &str = "Browser did not open?";

/// The end of the help line: "or go to app.example.com/connections."
pub fn go_to(address: &str) -> String {
    format!("or go to {address}.")
}

/// The whole help line: "Browser did not open? Open the page or go to app.example.com/connections."
#[cfg(test)]
pub fn browser_help(address: &str) -> String {
    format!("{HELP_BEFORE} {OPEN_THE_PAGE} {}", go_to(address))
}

/// What the window shows of a code: never the device code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub user_code: String,
    pub page: Option<String>,
    pub minutes: Option<String>,
}

impl Code {
    pub fn of(started: &Started) -> Code {
        Code { user_code: started.user_code.clone(), page: started.page.clone(), minutes: started.minutes() }
    }
}

// ---- The job ----

#[derive(Default)]
struct JobState {
    code: Option<Code>,
    result: Option<Result<Option<String>, ConnectError>>,
}

/// A sign-in running on a thread of its own.
pub struct SignInJob {
    run: Arc<Run>,
    state: Arc<Mutex<JobState>>,
}

/// What the work reports through: the code once the site gave one, and whether it was cancelled.
pub struct Reporter {
    run: Arc<Run>,
    state: Arc<Mutex<JobState>>,
}

impl Reporter {
    pub fn code(&self, code: Code) {
        if let Ok(mut s) = self.state.lock() {
            s.code = Some(code);
        }
    }

    pub fn cancelled(&self) -> bool {
        self.run.is_cancelled()
    }
}

impl SignInJob {
    /// Run `work` on a thread of its own. A panic in it is a failure with a sentence; either way
    /// the run is marked finished, for quitting to stop waiting for it.
    pub fn spawn(work: impl FnOnce(&Reporter) -> Result<Option<String>, ConnectError> + Send + 'static) -> SignInJob {
        let run = Run::begin();
        let state: Arc<Mutex<JobState>> = Arc::default();
        let reporter = Reporter { run: run.clone(), state: state.clone() };
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&reporter))).unwrap_or_else(|_| Err(ConnectError::Failed(BROKE_OFF.to_string())));
            if let Ok(mut s) = reporter.state.lock() {
                s.result = Some(result);
            }
            reporter.run.finish();
        });
        SignInJob { run, state }
    }

    fn cancel(&self) {
        self.run.cancel();
    }

    /// The code once there is one, and the result once the job has ended (taken once).
    fn take(&self) -> (Option<Code>, Option<Result<Option<String>, ConnectError>>) {
        match self.state.lock() {
            Ok(mut s) => (s.code.clone(), s.result.take()),
            Err(_) => (None, Some(Err(ConnectError::Failed(BROKE_OFF.to_string())))),
        }
    }
}

// ---- The window ----

/// Where the window is.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// The site is being asked for a code.
    Starting,
    /// The code is shown; Mythicshot asks the site whether it was confirmed.
    Waiting(Code),
    /// Not connected: the sentence, and whether "Try again" can help.
    Failed { text: String, retry: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Cancel,
    Close,
    TryAgain,
}

impl Button {
    pub fn label(self) -> &'static str {
        match self {
            Button::Cancel => "Cancel",
            Button::Close => "Close",
            Button::TryAgain => "Try again",
        }
    }
}

/// One line of the window, in the order it shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Text(String),
    Muted(String),
    /// In a coloured box: why this computer was not connected.
    Warning(String),
    /// "Browser did not open? Open the page or go to …", with "Open the page" a link.
    Help { address: String },
}

/// What the window shows now.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub heading: String,
    pub lines: Vec<Line>,
    /// The short code, large and selectable, after the first line.
    pub code: Option<String>,
    /// From left to right; the last one is the main button.
    pub buttons: Vec<Button>,
}

impl View {
    /// Every sentence of the window, for tests.
    #[cfg(test)]
    pub fn texts(&self) -> Vec<String> {
        let mut out = vec![CAPTION.to_string(), self.heading.clone()];
        for line in &self.lines {
            match line {
                Line::Text(t) | Line::Muted(t) | Line::Warning(t) => out.push(t.clone()),
                Line::Help { address } => out.push(browser_help(address)),
            }
        }
        out.extend(self.code.clone());
        out.extend(self.buttons.iter().map(|b| b.label().to_string()));
        out
    }
}

/// What the window asks of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInAction {
    /// The window closed without connecting.
    Closed,
    /// This computer is connected: the token is saved for the address the window was opened for.
    /// The account that confirmed the code, when the site named it.
    Connected(Option<String>),
}

/// Starts the work for an address: called when the window opens and for "Try again".
pub type Starter = Arc<dyn Fn(String) -> SignInJob + Send + Sync>;
/// Opens a web address in the browser; the error never repeats the address.
pub type Opener = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// The "Sign in" window.
pub struct SignInDialog {
    /// The site address the sign-in was started for, as the address rule writes it.
    server: String,
    stage: Stage,
    job: Option<SignInJob>,
    starter: Starter,
    open: Opener,
    /// The browser was asked to open the page of the code shown.
    opened: bool,
    /// The last time the browser was asked to open the page, it could not.
    open_failed: bool,
}

impl Drop for SignInDialog {
    /// A window that goes away while its job runs cancels it: nothing is stored then.
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
    }
}

impl SignInDialog {
    /// Open the window for `server` and start asking the site for a code.
    pub fn open(server: String, starter: Starter, open: Opener) -> SignInDialog {
        let mut d = SignInDialog { server, stage: Stage::Starting, job: None, starter, open, opened: false, open_failed: false };
        d.begin();
        d
    }

    fn begin(&mut self) {
        self.stage = Stage::Starting;
        self.opened = false;
        self.open_failed = false;
        self.job = Some((self.starter)(self.server.clone()));
    }

    #[cfg(test)]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// The site address the sign-in is for.
    #[cfg(test)]
    pub fn server(&self) -> &str {
        &self.server
    }

    #[cfg(test)]
    pub fn job(&self) -> Option<&SignInJob> {
        self.job.as_ref()
    }

    /// Take in what the job did. The browser is opened once, when the code first shows.
    pub fn poll(&mut self) -> Option<SignInAction> {
        let job = self.job.as_ref()?;
        let (code, result) = job.take();
        if let Some(code) = code
            && self.stage == Stage::Starting
        {
            if let Some(page) = &code.page
                && !self.opened
            {
                self.opened = true;
                let page = page.clone();
                self.open_at(&page);
            }
            self.stage = Stage::Waiting(code);
        }
        let result = result?;
        self.job = None;
        match result {
            Ok(account) => Some(SignInAction::Connected(account)),
            Err(ConnectError::Cancelled) => Some(SignInAction::Closed),
            Err(e) => {
                // Never a secret: the sentences are Mythicshot's own.
                let (_, text) = e.text().unwrap_or_default();
                eprintln!("Sign in: {text}");
                self.stage = Stage::Failed { text, retry: e.can_try_again() };
                None
            }
        }
    }

    /// Open `page` in the browser, and keep whether that worked: the window says so.
    fn open_at(&mut self, page: &str) {
        self.open_failed = match (self.open)(page) {
            Ok(()) => false,
            Err(e) => {
                eprintln!("Sign in: could not open the browser: {e}");
                true
            }
        };
    }

    /// "Open the page" in the help line: the page again.
    pub fn open_page(&mut self) {
        if let Stage::Waiting(Code { page: Some(page), .. }) = &self.stage {
            let page = page.clone();
            self.open_at(&page);
        }
    }

    /// A press on `button`.
    pub fn press(&mut self, button: Button) -> Option<SignInAction> {
        match (button, &self.stage) {
            (Button::Cancel, Stage::Starting | Stage::Waiting(_)) => {
                // Stop asking; the job stores nothing once cancelled.
                if let Some(job) = self.job.take() {
                    job.cancel();
                }
                Some(SignInAction::Closed)
            }
            (Button::Close, Stage::Failed { .. }) => Some(SignInAction::Closed),
            (Button::TryAgain, Stage::Failed { retry: true, .. }) => {
                self.begin();
                None
            }
            _ => None,
        }
    }

    /// The button Escape stands for now.
    fn escape_button(&self) -> Button {
        match self.stage {
            Stage::Failed { .. } => Button::Close,
            _ => Button::Cancel,
        }
    }

    /// What the window shows now.
    pub fn view(&self) -> View {
        match &self.stage {
            Stage::Starting => View { heading: STARTING.to_string(), lines: Vec::new(), code: None, buttons: vec![Button::Cancel] },
            Stage::Waiting(code) => {
                let first = match (&code.page, self.open_failed) {
                    (None, _) => NO_PAGE,
                    (Some(_), true) => NOT_OPENED,
                    (Some(_), false) => OPENED,
                };
                let mut lines = vec![Line::Text(first.to_string()), Line::Muted(waiting(code.minutes.as_deref()))];
                if let Some(page) = &code.page {
                    lines.push(Line::Help { address: connect::shown_address(page) });
                }
                View { heading: HEADING.to_string(), lines, code: Some(code.user_code.clone()), buttons: vec![Button::Cancel] }
            }
            Stage::Failed { text, retry } => View {
                heading: connect::NOT_CONNECTED_HEADING.to_string(),
                lines: vec![Line::Warning(text.clone())],
                code: None,
                buttons: if *retry { vec![Button::Close, Button::TryAgain] } else { vec![Button::Close] },
            },
        }
    }

    /// Draw the window over the page; Some when the page has something to do.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<SignInAction> {
        if let Some(action) = self.poll() {
            return Some(action);
        }
        let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let mut pressed = escape.then(|| self.escape_button());
        let mut open_page = false;
        let view = self.view();
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(theme::DIALOG_PAD);
        crate::share_ui::modal(egui::Id::unique("sign-in-window")).frame(frame).show(ctx, |ui| {
            theme::roomy(ui);
            ui.set_width(theme::DIALOG_WIDTH);
            ui.label(RichText::new(CAPTION).color(theme::MUTED).size(12.0));
            ui.add_space(4.0);
            ui.heading(&view.heading);
            for (i, line) in view.lines.iter().enumerate() {
                ui.add_space(6.0);
                match line {
                    Line::Text(t) => {
                        ui.add(egui::Label::new(t.as_str()).wrap());
                    }
                    Line::Muted(t) => {
                        ui.add(egui::Label::new(RichText::new(t).color(theme::MUTED)).wrap());
                    }
                    Line::Warning(t) => {
                        theme::caution_box(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(t).color(theme::CAUTION_TEXT)).wrap());
                        });
                    }
                    Line::Help { address } => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(HELP_BEFORE).color(theme::MUTED));
                            if ui.link(OPEN_THE_PAGE).clicked() {
                                open_page = true;
                            }
                            // The address in full, never shortened, and selectable so it can be copied.
                            ui.add(egui::Label::new(RichText::new(go_to(address)).color(theme::MUTED)).selectable(true).wrap());
                        });
                    }
                }
                // The code comes right after the first sentence, large and selectable, never editable.
                if i == 0
                    && let Some(code) = &view.code
                {
                    ui.add_space(10.0);
                    let mut shown = code.as_str();
                    ui.add(egui::TextEdit::singleline(&mut shown).font(egui::FontId::monospace(30.0)).desired_width(f32::INFINITY));
                    ui.add_space(4.0);
                }
            }
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for (i, button) in view.buttons.iter().enumerate().rev() {
                    let main = i + 1 == view.buttons.len() && view.buttons.len() > 1;
                    let b = if main { theme::primary_button(button.label()) } else { egui::Button::new(button.label()) };
                    if ui.add(b).clicked() {
                        pressed = Some(*button);
                    }
                }
            });
        });
        if open_page {
            self.open_page();
        }
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        pressed.and_then(|b| self.press(b))
    }
}

/// The opener the page uses: the safe browser opener, which refuses anything but a plain web
/// address. Tests never reach the real browser.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    if cfg!(test) {
        return Err("no browser in tests".to_string());
    }
    mythicshot_destinations::open_url(url)
}

#[cfg(test)]
mod tests;
