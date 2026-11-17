//! "Publish tour": the server and token state the editor reads, the reason the
//! export panel's button is disabled, and the tour picker.
//!
//! Everything that talks to the network or the credential store runs on a background
//! thread. The token is never held here: each background job loads it from the store
//! itself and drops it when done, so it never reaches the UI thread after it is saved.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};

use crate::publish::{self, RemoteTour, Target};

const MUTED: Color32 = Color32::from_rgb(163, 170, 184);
const ERROR: Color32 = Color32::from_rgb(255, 140, 140);
const PRIMARY: Color32 = Color32::from_rgb(47, 111, 235);

/// Whether an API token is saved. The token itself is never kept here.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenStatus {
    /// Not looked up yet, or being looked up.
    Checking,
    Saved,
    Missing,
    /// The credential store cannot be used (locked, unavailable): the store's message.
    Unavailable(String),
}

/// The server address from the settings and whether a token is saved, shared by the
/// settings window and every editor. `generation` makes a slow lookup that finishes
/// after a newer one leave the newer answer in place.
struct Config {
    server: String,
    token: TokenStatus,
    generation: u64,
}

static CONFIG: Mutex<Config> = Mutex::new(Config { server: String::new(), token: TokenStatus::Checking, generation: 0 });

fn config() -> std::sync::MutexGuard<'static, Config> {
    CONFIG.lock().unwrap_or_else(|e| e.into_inner())
}

/// At start-up: take the server from the settings and look up the token.
pub fn init(settings: &mut crate::settings::Settings) {
    // Test hook: the server to publish to, for a check against a local server. Never the token.
    #[cfg(debug_assertions)]
    if let Ok(server) = std::env::var("MYTHICSHOT_TEST_PUBLISH") {
        settings.publish_server = server;
    }
    set_server(&settings.publish_server);
    refresh_token();
}

pub fn set_server(server: &str) {
    config().server = server.to_string();
}

pub fn server() -> String {
    config().server.clone()
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
}

fn count(n: u32, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// The right-hand side of a picker row: "3 chapters · 14 steps · published".
pub fn tour_row_detail(t: &RemoteTour) -> String {
    format!("{} · {} · {}", count(t.chapters, "chapter", "chapters"), count(t.steps, "step", "steps"), t.status)
}

/// A whole picker row as one line: "Title · 3 chapters · 14 steps · draft".
pub fn tour_row_label(t: &RemoteTour) -> String {
    format!("{} · {}", t.title, tour_row_detail(t))
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
}

impl Picker {
    pub fn open(server: String, title: String) -> Picker {
        let picker = Picker { server, title: title.clone(), default_title: title, choice: Choice::New, list: Arc::new(Mutex::new(ListState::Loading)) };
        picker.load();
        picker
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

    /// Draw the dialog; Some once the user cancels or chooses.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<PickerOutcome> {
        let mut outcome = None;
        let mut retry = false;
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
                let edit = egui::TextEdit::singleline(&mut self.title).hint_text("Tour title").desired_width(ui.available_width());
                if ui.add_enabled(self.choice == Choice::New, edit).gained_focus() {
                    self.choice = Choice::New;
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
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
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
                                if ui.radio(chosen, &t.title).on_hover_text(tour_row_label(t)).clicked() {
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
            ui.add_space(16.0);
            let title_ok = !self.title.trim().is_empty();
            let can_upload = match self.choice {
                Choice::New => title_ok,
                Choice::Tour(_) => true,
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let upload = egui::Button::new(RichText::new("Upload draft").strong()).fill(PRIMARY);
                if ui.add_enabled(can_upload, upload).clicked() {
                    outcome = Some(match &self.choice {
                        Choice::New => PickerOutcome::Upload { title: self.title.trim().to_string(), target: Target::New },
                        Choice::Tour(id) => PickerOutcome::Upload { title: self.default_title.clone(), target: Target::Chapter(id.clone()) },
                    });
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(PickerOutcome::Cancel);
                }
            });
        });
        if outcome.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            outcome = Some(PickerOutcome::Cancel);
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
    fn picker_rows_name_chapters_steps_and_status() {
        assert_eq!(tour_row_label(&tour("Title", 3, 14, "draft")), "Title · 3 chapters · 14 steps · draft");
        assert_eq!(tour_row_detail(&tour("Customer onboarding", 3, 14, "published")), "3 chapters · 14 steps · published");
        assert_eq!(tour_row_detail(&tour("One", 1, 1, "draft")), "1 chapter · 1 step · draft");
        assert_eq!(tour_row_detail(&tour("Empty", 0, 0, "draft")), "0 chapters · 0 steps · draft");
    }
}
