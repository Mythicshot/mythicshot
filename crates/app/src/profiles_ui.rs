//! The profiles in the windows: the dropdown that shows and switches the active profile (on the
//! Tours page, in the upload and share dialogs, on the Workspace page), the add form, the
//! "Sign in again" notice, the list with Remove, and the question an update from another
//! profile is refused with. Every sentence is the approved one (see `profiles`).
//!
//! What the forms do is driven through [`AddFlow`] and [`Panel`] methods, so it is tested
//! without a window; they keep the profiles through a [`Shelf`], which is the app's own list
//! and the OS credential store outside tests, and a fake in them. Everything that uses the
//! credential store or the site runs on a thread of its own.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use eframe::egui::{self, RichText};

use crate::profiles::holds::Holds;
use crate::profiles::{self, Book, Candidate, Ending, Owner, Profile, State};
use crate::publish::connect::{self, Clock, ConnectError};
use crate::publish::ending::Outcome;
use crate::publish::{self, KeyStore};
use crate::publish_ui::{self, ProfilesRequest};
use crate::sign_in_ui::{self, Code, Opener, Reporter, SignInAction, SignInDialog, SignInJob};
use crate::theme;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---- Where the profiles are kept ----

/// What a change of the profiles came to.
#[derive(Debug)]
pub enum Done {
    /// A new profile was saved, and is the active one.
    Saved,
    /// The profile with this id was signed in again.
    Renewed(String),
    /// The sign-in was for another account than the profile's: nothing was stored.
    Other(Candidate),
    Removed,
    /// The store's sentence.
    Failed(String),
    /// The change is done and came to `then`, and it let go of a token nothing else here holds:
    /// that token is ended at its site afterwards, on the same background thread, once `then` was
    /// handed over. Never handed over itself.
    ThenEnd { then: Box<Done>, ending: Ending, after: AfterEnd },
}

/// What becomes of the site's answer to ending a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterEnd {
    /// A removal: one line where profile messages show, unless the token was ended.
    Notice,
    /// A "Sign in again": silent for the person, one line in the app's log.
    Log,
}

/// A change of the profiles, run on a background thread.
pub type Change = Box<dyn FnOnce(&mut Book, &dyn KeyStore) -> Done + Send>;

/// The profiles and the store they are changed with.
pub trait Shelf: Send + Sync {
    fn book(&self) -> Book;
    /// Run `change` on the calling thread (never the UI thread).
    fn change(&self, change: Change) -> Done;
    /// Make `id` the active profile.
    fn switch(&self, id: &str) -> bool;
    /// Ask the site to end a token a change let go of, on the calling thread (never the UI thread).
    fn end(&self, ending: &Ending) -> Outcome;
    /// Which profiles' tokens running jobs hold.
    fn holds(&self) -> Holds;
    /// Whether anything here holds `ending`'s token now (a profile saved meanwhile with the same
    /// token, say): asked again just before it is ended, briefly, with no request sent.
    fn held(&self, ending: &Ending) -> bool;
}

/// The app's own profiles and the OS credential store.
pub struct AppShelf;

impl Shelf for AppShelf {
    fn book(&self) -> Book {
        publish_ui::book()
    }
    fn change(&self, change: Change) -> Done {
        publish_ui::change_profiles(|book, store| change(book, store))
    }
    fn switch(&self, id: &str) -> bool {
        publish_ui::switch_profile(id)
    }
    fn end(&self, ending: &Ending) -> Outcome {
        ending.send()
    }
    fn holds(&self) -> Holds {
        profiles::holds::global().clone()
    }
    fn held(&self, ending: &Ending) -> bool {
        publish_ui::still_held(ending)
    }
}

/// The tokens being ended at their sites after a change, and the line a removal leaves when its
/// site did not end the token.
#[derive(Clone, Default)]
struct Endings {
    /// How many are not done yet: waiting for a running job, or for their site.
    busy: Arc<AtomicUsize>,
    /// How many of those are being sent now (a wait for a job is not counted): only these keep
    /// the page repainting, for at most the call's timeout.
    sending: Arc<AtomicUsize>,
    /// Every line not yet shown, oldest first.
    notices: Arc<Mutex<Vec<String>>>,
    /// Wakes the page that shows the lines, once per line.
    waker: Arc<Mutex<Option<Waker>>>,
}

/// Asks for one repaint of the page.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

impl Endings {
    #[cfg(test)]
    fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst) > 0
    }

    fn sending(&self) -> bool {
        self.sending.load(Ordering::SeqCst) > 0
    }

    /// A line for the page, which is woken once for it.
    fn say(&self, text: String) {
        lock(&self.notices).push(text);
        let waker = lock(&self.waker).clone();
        if let Some(wake) = waker {
            wake();
        }
    }
}

/// One token on its way to its site: counted while it is, whichever way the thread ends.
struct Busy(Arc<AtomicUsize>);

impl Busy {
    fn new(count: &Arc<AtomicUsize>) -> Busy {
        count.fetch_add(1, Ordering::SeqCst);
        Busy(count.clone())
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Run `change` on a thread of its own; its end lands in `slot`. A token the change let go of
/// is ended at its site after that, on the same thread: the change's end does not wait for the
/// site, and what the site said lands in `endings`. The end waits until no running job holds
/// the profile's token any more ([`Holds::wait_free`]); it is skipped, silently, when something
/// here holds the token by then. Should the app close first, it is simply never sent.
fn spawn_change(shelf: &Arc<dyn Shelf>, slot: &Arc<Mutex<Option<Done>>>, endings: &Endings, change: Change) {
    let (shelf, slot, endings) = (shelf.clone(), slot.clone(), endings.clone());
    std::thread::spawn(move || {
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shelf.change(change))).unwrap_or_else(|_| Done::Failed(profiles::NOT_SAVED.to_string()));
        let (done, then_end) = match done {
            Done::ThenEnd { then, ending, after } => (*then, Some((ending, after))),
            done => (done, None),
        };
        // Counted before the change's end is handed over, so nobody sees it idle in between.
        let busy = then_end.as_ref().map(|_| Busy::new(&endings.busy));
        *lock(&slot) = Some(done);
        let Some((ending, after)) = then_end else { return };
        // A job that started with this token runs to its end first.
        shelf.holds().wait_free(&ending.id);
        let sending = Busy::new(&endings.sending);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| if shelf.held(&ending) { None } else { Some(shelf.end(&ending)) })).unwrap_or_else(|_| Some(Outcome::NotReached("the request failed".into())));
        match (after, outcome) {
            // Held here again meanwhile: nothing was sent, and nothing is said.
            (_, None) => {}
            (AfterEnd::Notice, Some(outcome)) => {
                if let Some(text) = profiles::removal_notice(&ending.name, &outcome) {
                    endings.say(text);
                }
            }
            (AfterEnd::Log, Some(outcome)) => eprintln!("{}", profiles::renewal_log_line(&ending.server, &outcome)),
        }
        drop(sending);
        // The token's copy is wiped here.
        drop(ending);
        drop(busy);
    });
}

// ---- The dropdown ----

/// One row of the dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// "alex's workspace · app.mythicshot.com".
    pub detail: String,
    pub active: bool,
    /// Amber, "Sign in again": it cannot be used until it is signed in again.
    pub waiting: bool,
}

/// The rows for `book`; `refused` says the site refused the active profile's token now.
pub fn entries(book: &Book, refused: bool) -> Vec<Entry> {
    book.list
        .iter()
        .map(|p| {
            let active = book.active.as_deref() == Some(p.id.as_str());
            Entry { id: p.id.clone(), name: p.name.clone(), detail: p.detail(), active, waiting: p.state == State::SignInAgain || (active && refused) }
        })
        .collect()
}

/// What was picked in the dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Switch(String),
    Add,
    Manage,
}

/// The status dot before a name.
fn dot(ui: &mut egui::Ui, waiting: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, if waiting { theme::CAUTION_BORDER } else { theme::SUCCESS_BORDER });
}

/// The dropdown: the active profile with its dot, and the menu of every profile, "+ Add
/// profile…" and "Manage profiles…". `enabled` is false while a job of this window runs: a
/// switch then waits until it has ended. Without any profile it shows nothing.
pub fn dropdown_with(ui: &mut egui::Ui, salt: &str, rows: &[Entry], enabled: bool) -> Option<Pick> {
    if rows.is_empty() {
        return None;
    }
    let mut pick = None;
    let active = rows.iter().find(|e| e.active);
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            dot(ui, active.is_none_or(|a| a.waiting));
            let text = active.map_or_else(|| crate::workspace_ui::NOT_CONNECTED.to_string(), |a| a.name.clone());
            egui::ComboBox::from_id_salt(("profiles", salt)).selected_text(RichText::new(text).strong()).width(240.0).show_ui(ui, |ui| {
                ui.set_min_width(300.0);
                for e in rows {
                    ui.horizontal(|ui| {
                        dot(ui, e.waiting);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                if ui.add(egui::Button::selectable(e.active, RichText::new(&e.name).strong())).clicked() && !e.active {
                                    pick = Some(Pick::Switch(e.id.clone()));
                                }
                                if e.active {
                                    ui.label(RichText::new(profiles::ACTIVE).size(12.0).color(theme::MUTED));
                                }
                                if e.waiting {
                                    theme::caution_line(ui, profiles::SIGN_IN_AGAIN, 12.0, theme::Fit::Truncate);
                                }
                            });
                            ui.label(RichText::new(&e.detail).size(12.0).color(theme::MUTED));
                        });
                    });
                }
                ui.separator();
                if ui.add(egui::Button::new(RichText::new(profiles::ADD_PROFILE).color(theme::GOLD)).frame(false)).clicked() {
                    pick = Some(Pick::Add);
                }
                if ui.add(egui::Button::new(RichText::new(profiles::MANAGE_PROFILES).color(theme::MUTED)).frame(false)).clicked() {
                    pick = Some(Pick::Manage);
                }
            });
        });
    });
    pick
}

/// The dropdown over the app's own profiles, with what is picked done: a switch at once, and
/// "+ Add profile…" or "Manage profiles…" on the Workspace page of the main window, which the
/// app opens. True when the active profile changed.
pub fn dropdown(ui: &mut egui::Ui, salt: &str, enabled: bool) -> bool {
    let rows = entries(&publish_ui::book(), publish_ui::token_refused());
    match dropdown_with(ui, salt, &rows, enabled) {
        Some(Pick::Switch(id)) => publish_ui::switch_profile(&id),
        Some(Pick::Add) => {
            publish_ui::request_profiles(ProfilesRequest::Add);
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            false
        }
        Some(Pick::Manage) => {
            publish_ui::request_profiles(ProfilesRequest::Manage);
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            false
        }
        None => false,
    }
}

/// A note in the mockup's colours: ok, warning or refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Bad,
}

pub fn note(ui: &mut egui::Ui, tone: Tone, text: &str) {
    let (fill, fg) = match tone {
        Tone::Ok => (theme::SUCCESS_TINT, theme::SUCCESS_TEXT),
        Tone::Bad => (theme::DANGER_TINT, theme::DANGER_TEXT),
        // A caution carries its icon: the app's own caution box.
        Tone::Warn => {
            theme::caution_box(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(text).color(theme::CAUTION_TEXT)).wrap());
            });
            return;
        }
    };
    egui::Frame::new().fill(fill).corner_radius(egui::CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).color(fg)).wrap());
    });
}

// ---- Updating a tour from another profile ----

/// The question an update is refused with when the recording's tour is not the active profile's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerPrompt {
    pub owner: Owner,
    /// The recording's title, for "Upload as a new tour".
    pub title: String,
}

/// What was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerChoice {
    /// Switch to the tour's profile, then update.
    Switch(String),
    NewTour,
    Cancel,
}

impl OwnerPrompt {
    /// The sentence, and the buttons from the main one on.
    pub fn view(&self) -> (String, Vec<String>) {
        match &self.owner {
            Owner::Other { name, .. } => (profiles::other_profile(name), vec![profiles::switch_to(name), profiles::UPLOAD_AS_NEW.to_string(), profiles::CANCEL.to_string()]),
            _ => (profiles::OWNER_REMOVED.to_string(), vec![profiles::UPLOAD_AS_NEW.to_string(), profiles::CANCEL.to_string()]),
        }
    }

    /// The choice of the button at `index` in [`OwnerPrompt::view`].
    pub fn choice(&self, index: usize) -> OwnerChoice {
        match (&self.owner, index) {
            (Owner::Other { id, .. }, 0) => OwnerChoice::Switch(id.clone()),
            (Owner::Other { .. }, 1) | (_, 0) => OwnerChoice::NewTour,
            _ => OwnerChoice::Cancel,
        }
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<OwnerChoice> {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            return Some(OwnerChoice::Cancel);
        }
        let (text, buttons) = self.view();
        let mut chosen = None;
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(22.0);
        crate::share_ui::modal(egui::Id::unique("owner-prompt")).frame(frame).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Publish tour");
            ui.add_space(8.0);
            note(ui, Tone::Bad, &text);
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                for (i, label) in buttons.iter().enumerate() {
                    let b = if i == 0 { theme::primary_button(label.as_str()) } else { egui::Button::new(label.as_str()) };
                    if ui.add(b).clicked() {
                        chosen = Some(self.choice(i));
                    }
                }
            });
        });
        chosen
    }
}

// ---- Adding a profile, and signing one in again ----

/// The paste field's hint and its button: the app's existing wording for a pasted token and for
/// checking a site.
const PASTE_HINT: &str = "Paste the token from the site's setup page";
const CHECK_PASTE: &str = "Test connection";

/// What a sign-in is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    New,
    /// "Sign in again" for the profile with this id: only the same account renews it.
    Renew(String),
}

/// Where the form is.
pub enum Step {
    /// The address, and Sign in. "Save profile" cannot be pressed yet.
    Address,
    /// The sign-in passed the check: the name, and "Save profile".
    Checked { candidate: Candidate, name: String },
    /// A profile for this site and account is there already.
    Same { candidate: Candidate, id: String, name: String },
    /// A sign-in for profile `id` passed the check and is that profile's account: what it is, and
    /// "Save profile" to take it. Nothing is stored before.
    Renew { candidate: Candidate, id: String },
    /// Saved, switched, renewed or cancelled: the form is done.
    Finished,
}

/// What Sign in does on its own thread for the add form: ask the site at `server` for a code,
/// show it, wait for it, then check the new token with the site. Only a sign-in that passed the
/// check is handed over, through `slot`, and only as long as nobody cancelled; nothing is ever
/// stored here. A failure says "This profile was not saved." and then the app's existing
/// sentence for it ([`profiles::not_saved`]).
pub fn add_work(server: String, slot: Arc<Mutex<Option<Candidate>>>, clock: Arc<dyn Clock + Send + Sync>) -> impl FnOnce(&Reporter) -> Result<Option<String>, ConnectError> + Send + 'static {
    move |r| {
        let not_saved = |e: ConnectError| match e.text() {
            None => ConnectError::Cancelled,
            Some((_, why)) => ConnectError::Failed(profiles::not_saved(&why)),
        };
        let started = connect::start(&server, &connect::device_name()).map_err(not_saved)?;
        r.code(Code::of(&started));
        let connected = connect::wait(&started, &|| r.cancelled(), &*clock).map_err(not_saved)?;
        if r.cancelled() {
            return Err(ConnectError::Cancelled);
        }
        let account = connected.account.clone();
        let signed_in = profiles::SignedIn { name: connected.account.clone() };
        let candidate = profiles::checked(&started.server, signed_in, connected.token).map_err(ConnectError::Failed)?;
        // A cancel while the site was asked: the token goes with the candidate, unstored.
        if r.cancelled() {
            return Err(ConnectError::Cancelled);
        }
        *lock(&slot) = Some(candidate);
        Ok(account)
    }
}

/// The add form (and "Sign in again"): an address, a browser sign-in, the check, then the name
/// and "Save profile". Nothing is stored before "Save profile" (or a renewal of the same
/// account); a failed, cancelled or unanswered sign-in leaves nothing behind.
pub struct AddFlow {
    pub purpose: Purpose,
    pub address: String,
    step: Step,
    sign_in: Option<SignInDialog>,
    slot: Arc<Mutex<Option<Candidate>>>,
    /// A save or renewal running on its thread, and its end.
    working: bool,
    done: Arc<Mutex<Option<Done>>>,
    /// Why the last save, or the last pasted token, could not be taken.
    pub error: Option<String>,
    /// "Paste a token instead" was clicked: the paste field shows (for a site without Sign in).
    pasting: bool,
    /// The pasted token; only ever a masked field, wiped when it is checked or the form goes.
    paste: String,
    /// The check of a pasted token, running on its thread, and its end.
    pasted: Arc<Mutex<Option<Result<Candidate, String>>>>,
    checking_paste: bool,
    /// The token a renewal replaced, being ended at its site.
    endings: Endings,
    shelf: Arc<dyn Shelf>,
    open: Opener,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Drop for AddFlow {
    fn drop(&mut self) {
        publish_ui::wipe(&mut self.paste);
    }
}

impl AddFlow {
    pub fn new(purpose: Purpose, address: String, shelf: Arc<dyn Shelf>, open: Opener, clock: Arc<dyn Clock + Send + Sync>) -> AddFlow {
        AddFlow { purpose, address, step: Step::Address, sign_in: None, slot: Arc::default(), working: false, done: Arc::default(), error: None, pasting: false, paste: String::new(), pasted: Arc::default(), checking_paste: false, endings: Endings::default(), shelf, open, clock }
    }

    /// The form for the app's own profiles, the real browser and the real clock.
    pub fn for_app(purpose: Purpose, address: String) -> AddFlow {
        AddFlow::new(purpose, address, Arc::new(AppShelf), Arc::new(sign_in_ui::open_in_browser), Arc::new(connect::RealClock::new()))
    }

    #[cfg(test)]
    pub fn step(&self) -> &Step {
        &self.step
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.step, Step::Finished)
    }

    #[cfg(test)]
    pub fn is_signing_in(&self) -> bool {
        self.sign_in.is_some()
    }

    #[cfg(test)]
    pub fn sign_in_dialog(&self) -> Option<&SignInDialog> {
        self.sign_in.as_ref()
    }

    /// Whether Sign in can be pressed: the address passes the address rule and nothing runs.
    pub fn can_sign_in(&self) -> bool {
        matches!(self.step, Step::Address) && !self.working && self.sign_in.is_none() && publish::check_server(&self.address).is_ok()
    }

    /// Whether "Save profile" can be pressed: only once the sign-in passed the check.
    pub fn can_save(&self) -> bool {
        matches!(self.step, Step::Checked { .. } | Step::Renew { .. }) && !self.working
    }

    /// Sign in: the browser window for the address, after the address check.
    pub fn begin_sign_in(&mut self) {
        if !self.can_sign_in() {
            return;
        }
        let Ok(server) = publish::check_server(&self.address) else { return };
        self.error = None;
        *lock(&self.slot) = None;
        let (slot, clock) = (self.slot.clone(), self.clock.clone());
        let starter: sign_in_ui::Starter = Arc::new(move |server| SignInJob::spawn(add_work(server, slot.clone(), clock.clone())));
        self.sign_in = Some(SignInDialog::open(server, starter, self.open.clone()));
    }

    /// What the sign-in window said: it closed, or the sign-in passed the check.
    pub fn on_sign_in(&mut self, action: SignInAction) {
        self.sign_in = None;
        let candidate = lock(&self.slot).take();
        if let (SignInAction::Connected(_), Some(candidate)) = (action, candidate) {
            self.accept(candidate);
        }
    }

    /// Take in the sign-in window's job without drawing it (what drawing it does first).
    #[cfg(test)]
    pub fn poll_sign_in(&mut self) {
        if let Some(action) = self.sign_in.as_mut().and_then(SignInDialog::poll) {
            self.on_sign_in(action);
        }
    }

    fn accept(&mut self, candidate: Candidate) {
        match self.purpose.clone() {
            // The profile's account: confirmed by "Save profile" before anything is stored.
            Purpose::Renew(id) if profiles::would_renew(&self.shelf.book(), &id, &candidate) => self.step = Step::Renew { candidate, id },
            // Another account: offered as a profile of its own; the old one stays as it was.
            Purpose::Renew(_) => {
                self.purpose = Purpose::New;
                self.offer(candidate);
            }
            Purpose::New => self.offer(candidate),
        }
    }

    /// A sign-in that is not a renewal: "Switch to it" when its profile is there, else the name.
    fn offer(&mut self, candidate: Candidate) {
        let book = self.shelf.book();
        self.step = match profiles::find_same(&book, &candidate) {
            Some(p) => Step::Same { candidate, id: p.id.clone(), name: p.name.clone() },
            None => {
                let name = candidate.default_name();
                Step::Checked { candidate, name }
            }
        };
    }

    /// The name field of the checked step.
    #[cfg(test)]
    pub fn name_mut(&mut self) -> Option<&mut String> {
        match &mut self.step {
            Step::Checked { name, .. } => Some(name),
            _ => None,
        }
    }

    /// "Save profile".
    pub fn save(&mut self) {
        if !self.can_save() {
            return;
        }
        let (candidate, name) = match std::mem::replace(&mut self.step, Step::Address) {
            Step::Checked { candidate, name } => (candidate, name),
            Step::Renew { candidate, id } => {
                self.working = true;
                spawn_change(
                    &self.shelf,
                    &self.done,
                    &self.endings,
                    Box::new(move |book, store| match profiles::renew(book, store, &id, candidate) {
                        // The new token is stored and confirmed: only now is the old one ended.
                        Ok(profiles::Renewal::Renewed(Some(ending))) => Done::ThenEnd { then: Box::new(Done::Renewed(id)), ending, after: AfterEnd::Log },
                        Ok(profiles::Renewal::Renewed(None)) => Done::Renewed(id),
                        Ok(profiles::Renewal::OtherAccount(c)) => Done::Other(c),
                        Err(e) => Done::Failed(e),
                    }),
                );
                return;
            }
            other => {
                self.step = other;
                return;
            }
        };
        self.working = true;
        spawn_change(
            &self.shelf,
            &self.done,
            &self.endings,
            Box::new(move |book, store| match profiles::save_new(book, store, candidate, &name) {
                Ok(_) => Done::Saved,
                Err(e) => Done::Failed(e),
            }),
        );
    }

    /// "Switch to it": the profile that is there already becomes the active one. When it was
    /// waiting for a sign-in, this one (the same account) renews it.
    pub fn switch_to_it(&mut self) {
        let Step::Same { candidate, id, .. } = std::mem::replace(&mut self.step, Step::Address) else { return };
        let waiting = self.shelf.book().get(&id).is_some_and(|p| p.state == State::SignInAgain);
        if waiting {
            self.purpose = Purpose::Renew(id);
            self.accept(candidate);
        } else {
            drop(candidate);
            self.shelf.switch(&id);
            self.step = Step::Finished;
        }
    }

    /// "Try a different account": the sign-in that came back as an account with a profile goes,
    /// unstored, as on Cancel, and a fresh browser sign-in starts for the same address.
    pub fn try_different_account(&mut self) {
        let Step::Same { candidate, .. } = std::mem::replace(&mut self.step, Step::Address) else { return };
        drop(candidate);
        *lock(&self.slot) = None;
        self.begin_sign_in();
    }

    /// Whether Cancel can be pressed: not while a save or renewal runs (it would not stop it).
    #[cfg(test)]
    pub fn can_cancel(&self) -> bool {
        !self.working
    }

    /// "Paste a token instead": the field for a token from the site's setup page.
    pub fn begin_paste(&mut self) {
        self.pasting = true;
    }

    #[cfg(test)]
    pub fn type_paste_for_test(&mut self, token: &str) {
        self.paste = token.to_string();
    }

    /// Whether the pasted token can be checked: an address the address rule takes, a token, and
    /// nothing else running.
    pub fn can_check_paste(&self) -> bool {
        matches!(self.step, Step::Address) && self.pasting && !self.working && !self.checking_paste && self.sign_in.is_none() && !self.paste.trim().is_empty() && publish::check_server(&self.address).is_ok()
    }

    /// Check the pasted token with the site, as a sign-in is checked, before anything is saved.
    /// The field is wiped at once; the token goes to the check's thread and nowhere else.
    pub fn check_paste(&mut self) {
        if !self.can_check_paste() {
            return;
        }
        let Ok(server) = publish::check_server(&self.address) else { return };
        self.error = None;
        self.checking_paste = true;
        let token = connect::Secret::new(std::mem::take(&mut self.paste));
        let slot = self.pasted.clone();
        std::thread::spawn(move || {
            let result = if publish::is_token(token.expose()) {
                profiles::checked(&server, profiles::SignedIn::default(), token)
            } else {
                Err(profiles::not_saved(publish::NOT_A_TOKEN))
            };
            *lock(&slot) = Some(result);
        });
    }

    /// Whether a pasted token is being checked.
    #[cfg(test)]
    pub fn is_checking_paste(&self) -> bool {
        self.checking_paste
    }

    /// Cancel: whatever was signed in goes, unstored. Not while a save or renewal runs.
    pub fn cancel(&mut self) {
        if self.working {
            return;
        }
        publish_ui::wipe(&mut self.paste);
        self.sign_in = None;
        *lock(&self.slot) = None;
        self.step = Step::Finished;
    }

    /// Take in a save or renewal that ended.
    pub fn poll(&mut self) {
        self.poll_done();
    }

    fn poll_done(&mut self) {
        let pasted = lock(&self.pasted).take();
        if let Some(result) = pasted {
            self.checking_paste = false;
            match result {
                // Cancelled meanwhile: the token goes, unstored.
                Ok(_) if self.is_finished() => {}
                Ok(candidate) => {
                    self.pasting = false;
                    self.accept(candidate);
                }
                Err(e) => self.error = Some(e),
            }
        }
        let Some(mut done) = lock(&self.done).take() else { return };
        self.working = false;
        // Never handed over (see [`spawn_change`]); what it came to is all that counts here.
        while let Done::ThenEnd { then, .. } = done {
            done = *then;
        }
        match done {
            Done::Saved | Done::Removed | Done::ThenEnd { .. } => self.step = Step::Finished,
            Done::Renewed(id) => {
                self.shelf.switch(&id);
                self.step = Step::Finished;
            }
            // Another account than the profile's: offered as a profile of its own.
            Done::Other(candidate) => {
                self.purpose = Purpose::New;
                self.offer(candidate);
            }
            Done::Failed(e) => {
                self.error = Some(e);
                self.step = Step::Address;
            }
        }
    }

    /// Whether a save or a renewal runs.
    #[cfg(test)]
    pub fn is_working(&self) -> bool {
        self.working
    }

    /// Whether the token a renewal replaced is still being ended at its site.
    #[cfg(test)]
    pub fn is_ending(&self) -> bool {
        self.endings.busy()
    }

    /// Draw the form; the sign-in window over it while it is open.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll_done();
        if let Some(d) = self.sign_in.as_mut()
            && let Some(action) = d.show(ui.ctx())
        {
            self.on_sign_in(action);
        }
        if self.working || self.checking_paste || self.sign_in.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let mut pressed: Option<&'static str> = None;
        match &mut self.step {
            Step::Address | Step::Finished => {
                ui.label(RichText::new(profiles::SITE_ADDRESS).strong().color(theme::MUTED));
                ui.horizontal(|ui| {
                    let renewing = matches!(self.purpose, Purpose::Renew(_));
                    ui.add_enabled(!renewing && self.sign_in.is_none(), egui::TextEdit::singleline(&mut self.address).hint_text("https://tours.example.com").desired_width(300.0));
                    let can = matches!(self.step, Step::Address) && !self.working && self.sign_in.is_none() && publish::check_server(&self.address).is_ok();
                    if ui.add_enabled(can, theme::primary_button(profiles::SIGN_IN)).clicked() {
                        pressed = Some(profiles::SIGN_IN);
                    }
                });
                if !self.address.trim().is_empty()
                    && let Err(e) = publish::check_server(&self.address)
                {
                    theme::caution_line(ui, e, theme::SMALL_SIZE, theme::Fit::Wrap);
                }
                ui.add(egui::Label::new(RichText::new(profiles::ADDRESS_HINT).color(theme::MUTED).size(12.0)).wrap());
                // A site without Sign in: a token from its setup page, checked the same way.
                if self.pasting {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.paste).password(true).hint_text(PASTE_HINT).desired_width(240.0));
                        let can = matches!(self.step, Step::Address) && !self.working && !self.checking_paste && self.sign_in.is_none() && !self.paste.trim().is_empty() && publish::check_server(&self.address).is_ok();
                        if ui.add_enabled(can, egui::Button::new(CHECK_PASTE)).clicked() {
                            pressed = Some(CHECK_PASTE);
                        }
                        if self.checking_paste {
                            ui.add(egui::Spinner::new());
                        }
                    });
                } else if ui.add_enabled(!self.working && self.sign_in.is_none(), egui::Link::new(crate::workspace_ui::PASTE_INSTEAD)).clicked() {
                    self.begin_paste();
                }
                if let Some(e) = &self.error {
                    ui.add_space(6.0);
                    note(ui, Tone::Bad, e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.working, egui::Button::new(profiles::CANCEL)).clicked() {
                        pressed = Some(profiles::CANCEL);
                    }
                    ui.add_enabled(false, theme::primary_button(profiles::SAVE_PROFILE));
                    if self.working {
                        ui.add(egui::Spinner::new());
                    }
                });
            }
            Step::Checked { candidate, name } => {
                note(ui, Tone::Ok, &profiles::connected(candidate.account.as_deref(), &candidate.workspace));
                ui.add_space(6.0);
                ui.label(RichText::new(profiles::PROFILE_NAME).strong().color(theme::MUTED));
                ui.add(egui::TextEdit::singleline(name).char_limit(profiles::NAME_MAX).desired_width(300.0));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.working, egui::Button::new(profiles::CANCEL)).clicked() {
                        pressed = Some(profiles::CANCEL);
                    }
                    if ui.add_enabled(!self.working, theme::primary_button(profiles::SAVE_PROFILE)).clicked() {
                        pressed = Some(profiles::SAVE_PROFILE);
                    }
                });
            }
            Step::Renew { candidate, .. } => {
                note(ui, Tone::Ok, &profiles::connected(candidate.account.as_deref(), &candidate.workspace));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.working, egui::Button::new(profiles::CANCEL)).clicked() {
                        pressed = Some(profiles::CANCEL);
                    }
                    if ui.add_enabled(!self.working, theme::primary_button(profiles::SAVE_PROFILE)).clicked() {
                        pressed = Some(profiles::SAVE_PROFILE);
                    }
                });
            }
            Step::Same { name, .. } => {
                note(ui, Tone::Warn, &format!("{}\n{}", profiles::exists(name), profiles::exists_why()));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.working, theme::primary_button(profiles::SWITCH_TO_IT)).clicked() {
                        pressed = Some(profiles::SWITCH_TO_IT);
                    }
                    if ui.add_enabled(!self.working, egui::Button::new(profiles::TRY_DIFFERENT_ACCOUNT)).clicked() {
                        pressed = Some(profiles::TRY_DIFFERENT_ACCOUNT);
                    }
                    if ui.button(profiles::CANCEL).clicked() {
                        pressed = Some(profiles::CANCEL);
                    }
                });
            }
        }
        match pressed {
            Some(profiles::SIGN_IN) => self.begin_sign_in(),
            Some(profiles::SAVE_PROFILE) => self.save(),
            Some(profiles::SWITCH_TO_IT) => self.switch_to_it(),
            Some(profiles::TRY_DIFFERENT_ACCOUNT) => self.try_different_account(),
            Some(profiles::CANCEL) => self.cancel(),
            Some(CHECK_PASTE) => self.check_paste(),
            _ => {}
        }
    }
}

// ---- The Workspace page's part ----

/// What the Workspace page shows of the profiles.
#[derive(Default)]
pub enum Mode {
    /// The active profile and what the site says about it (the page's rows below).
    #[default]
    Details,
    Add(Box<AddFlow>),
    Manage,
}

/// The profiles' part of the Workspace page: the dropdown, the notices, the add form and the
/// list with Remove.
pub struct Panel {
    pub mode: Mode,
    /// A Remove waiting for its answer: the profile's id.
    removing: Option<String>,
    working: bool,
    done: Arc<Mutex<Option<Done>>>,
    /// Why the last Remove did not go through.
    error: Option<String>,
    /// The tokens of removed profiles, being ended at their sites.
    endings: Endings,
    /// Removed profiles whose sites did not end their tokens: one line each, oldest first.
    notices: Vec<String>,
    shelf: Arc<dyn Shelf>,
    /// Makes the add form; tests hand in one with fakes.
    make: Arc<dyn Fn(Purpose, String) -> AddFlow + Send + Sync>,
}

impl Default for Panel {
    fn default() -> Self {
        Panel { mode: Mode::Details, removing: None, working: false, done: Arc::default(), error: None, endings: Endings::default(), notices: Vec::new(), shelf: Arc::new(AppShelf), make: Arc::new(AddFlow::for_app) }
    }
}

/// What the page shows under the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Below {
    /// The active profile's rows (status, workspace, tours, version, Sign out).
    Details,
    /// Nothing: the add form or the list is shown.
    Nothing,
}

impl Panel {
    #[cfg(test)]
    pub fn with(shelf: Arc<dyn Shelf>, make: Arc<dyn Fn(Purpose, String) -> AddFlow + Send + Sync>) -> Panel {
        Panel { shelf, make, ..Panel::default() }
    }

    /// "+ Add profile…": the empty form.
    pub fn add(&mut self) {
        self.mode = Mode::Add(Box::new((self.make)(Purpose::New, String::new())));
    }

    /// "Sign in again" for profile `id`: its address, and the browser at once.
    pub fn sign_in_again(&mut self, id: &str) {
        let Some(p) = self.shelf.book().get(id).cloned() else { return };
        let mut flow = (self.make)(Purpose::Renew(p.id.clone()), p.server.clone());
        flow.begin_sign_in();
        self.mode = Mode::Add(Box::new(flow));
    }

    /// "Remove profile": the question first.
    pub fn ask_remove(&mut self, id: &str) {
        self.error = None;
        self.notices.clear();
        self.removing = Some(id.to_string());
    }

    /// The question's text, while it is asked.
    pub fn remove_question(&self) -> Option<String> {
        let id = self.removing.as_ref()?;
        self.shelf.book().get(id).map(|p| profiles::remove_question(&p.name))
    }

    /// "Remove": that profile and its token go from this computer, whatever the site says; then
    /// its site is asked to end the token, unless anything else here still holds it.
    pub fn confirm_remove(&mut self) {
        let Some(id) = self.removing.take() else { return };
        self.working = true;
        spawn_change(
            &self.shelf,
            &self.done,
            &self.endings,
            Box::new(move |book, store| match profiles::remove(book, store, &id) {
                Ok(Some(ending)) => Done::ThenEnd { then: Box::new(Done::Removed), ending, after: AfterEnd::Notice },
                Ok(None) => Done::Removed,
                Err(e) => Done::Failed(e),
            }),
        );
    }

    pub fn cancel_remove(&mut self) {
        self.removing = None;
    }

    /// Take in a Remove that ended, and a request from another page or dialog.
    pub fn poll(&mut self, request: Option<ProfilesRequest>) {
        if let Some(done) = lock(&self.done).take() {
            self.working = false;
            if let Done::Failed(e) = done {
                self.error = Some(e);
            }
        }
        self.notices.append(&mut lock(&self.endings.notices));
        match request {
            Some(ProfilesRequest::Add) => self.add(),
            Some(ProfilesRequest::Manage) => self.mode = Mode::Manage,
            None => {}
        }
        if let Mode::Add(flow) = &mut self.mode {
            flow.poll();
            if flow.is_finished() {
                self.mode = Mode::Details;
            }
        }
    }

    #[cfg(test)]
    pub fn is_working(&self) -> bool {
        self.working
    }

    /// Whether a removed profile's token is still being ended at its site.
    #[cfg(test)]
    pub fn is_ending(&self) -> bool {
        self.endings.busy()
    }

    /// The lines removals left because their sites did not end the tokens, oldest first.
    #[cfg(test)]
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// What wakes the page when a removal's line arrives; the page sets its own when drawn.
    pub fn set_waker(&mut self, waker: Waker) {
        *lock(&self.endings.waker) = Some(waker);
    }

    /// Whether the page keeps repainting by itself: only while something it shows is changing.
    pub fn wants_repaint(&self) -> bool {
        self.working || self.endings.sending()
    }

    /// Draw the panel under the page's heading. With no profile at all, the add form shows by itself.
    pub fn ui(&mut self, ui: &mut egui::Ui, request: Option<ProfilesRequest>) -> Below {
        self.poll(request);
        let book = self.shelf.book();
        if book.list.is_empty() && !matches!(self.mode, Mode::Add(_)) {
            self.add();
        }
        // While a removal runs or its site is being asked; a line that arrives later wakes the
        // page by itself, so a wait for a running job repaints nothing.
        let ctx = ui.ctx().clone();
        self.set_waker(Arc::new(move || ctx.request_repaint()));
        if self.wants_repaint() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let mut pick = None;
        ui.horizontal(|ui| {
            ui.heading("Workspace");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                pick = dropdown_with(ui, "workspace", &entries(&book, publish_ui::token_refused()), !self.working);
            });
        });
        ui.add_space(10.0);
        match pick {
            Some(Pick::Switch(id)) => {
                self.shelf.switch(&id);
                self.mode = Mode::Details;
            }
            Some(Pick::Add) => self.add(),
            Some(Pick::Manage) => self.mode = Mode::Manage,
            None => {}
        }
        if let Some(e) = &self.error {
            note(ui, Tone::Bad, e);
            ui.add_space(8.0);
        }
        for text in &self.notices {
            note(ui, Tone::Warn, text);
            ui.add_space(8.0);
        }
        // The update that adds profiles could not run at this start: the store's own sentence.
        if let Some(problem) = publish_ui::migration_problem() {
            theme::caution_line(ui, problem, theme::BODY_SIZE, theme::Fit::Wrap);
            ui.add_space(8.0);
        }
        self.remove_window(ui.ctx());
        let mut below = Below::Nothing;
        match &mut self.mode {
            Mode::Add(flow) => {
                flow.ui(ui);
                if flow.is_finished() {
                    self.mode = Mode::Details;
                }
            }
            Mode::Manage => self.manage_ui(ui, &book),
            Mode::Details => {
                if let Some(text) = publish_ui::migrated_notice() {
                    note(ui, Tone::Ok, &text);
                    ui.add_space(8.0);
                }
                if let Some(p) = book.active() {
                    if needs_sign_in(p, publish_ui::token_refused(), &publish_ui::token_status()) {
                        let id = p.id.clone();
                        note(ui, Tone::Warn, &profiles::needs_sign_in(&p.name));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!self.working, theme::primary_button(profiles::SIGN_IN_AGAIN)).clicked() {
                                self.sign_in_again(&id);
                            }
                            if ui.add_enabled(!self.working, egui::Button::new(profiles::REMOVE_PROFILE)).clicked() {
                                self.ask_remove(&id);
                            }
                        });
                        ui.add_space(10.0);
                    }
                    // The credential store cannot be used: its own sentence.
                    if let publish_ui::TokenStatus::Unavailable(e) = publish_ui::token_status() {
                        theme::caution_line(ui, e, theme::BODY_SIZE, theme::Fit::Wrap);
                        ui.add_space(8.0);
                    }
                    below = Below::Details;
                }
            }
        }
        below
    }

    fn manage_ui(&mut self, ui: &mut egui::Ui, book: &Book) {
        ui.label(RichText::new(profiles::MANAGE_PROFILES.trim_end_matches('…')).strong());
        ui.add_space(6.0);
        let mut again = None;
        let mut remove = None;
        for e in entries(book, publish_ui::token_refused()) {
            ui.horizontal(|ui| {
                dot(ui, e.waiting);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&e.name).strong());
                        if e.active {
                            ui.label(RichText::new(profiles::ACTIVE).size(12.0).color(theme::MUTED));
                        }
                    });
                    ui.label(RichText::new(&e.detail).size(12.0).color(theme::MUTED));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!self.working, egui::Button::new(profiles::REMOVE_PROFILE)).clicked() {
                        remove = Some(e.id.clone());
                    }
                    if e.waiting && ui.add_enabled(!self.working, egui::Button::new(profiles::SIGN_IN_AGAIN)).clicked() {
                        again = Some(e.id.clone());
                    }
                });
            });
            ui.separator();
        }
        if let Some(id) = again {
            self.sign_in_again(&id);
        }
        if let Some(id) = remove {
            self.ask_remove(&id);
        }
    }

    fn remove_window(&mut self, ctx: &egui::Context) {
        let Some(text) = self.remove_question() else {
            self.removing = None;
            return;
        };
        let mut answer = None;
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            answer = Some(false);
        }
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(22.0);
        crate::share_ui::modal(egui::Id::unique("remove-profile")).frame(frame).show(ctx, |ui| {
            ui.set_width(420.0);
            note(ui, Tone::Warn, &text);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.add(theme::danger_button(profiles::REMOVE)).clicked() {
                    answer = Some(true);
                }
                if ui.button(profiles::CANCEL).clicked() {
                    answer = Some(false);
                }
            });
        });
        match answer {
            Some(true) => self.confirm_remove(),
            Some(false) => self.cancel_remove(),
            None => {}
        }
    }
}

/// Whether profile `p` (the active one) shows "needs you to sign in again": the site refused
/// its token, or it has none (signed out).
pub fn needs_sign_in(p: &Profile, refused: bool, token: &publish_ui::TokenStatus) -> bool {
    p.state == State::SignInAgain || refused || *token == publish_ui::TokenStatus::Missing
}

#[cfg(test)]
mod tests;
