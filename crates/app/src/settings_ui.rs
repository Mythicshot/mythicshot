//! The Preferences page of the main window: the General, Output and Hotkeys tabs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Stroke, vec2};

use mythicshot_destinations::{DEFAULT_PATTERN, DestId, ImageFormat, expand_pattern};

use crate::hotkeys::{self, Action, EditorAction};
use crate::publish_ui::{self, TokenStatus};
use crate::settings::{Settings, TrayLeftClick};

const WARN_BG: Color32 = Color32::from_rgb(58, 42, 16);
const WARN_TEXT: Color32 = Color32::from_rgb(255, 196, 107);
const MUTED: Color32 = Color32::from_rgb(163, 170, 184);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    General,
    Output,
    Hotkeys,
}

/// A shortcut slot: a global hotkey or a key inside the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bind {
    Global(Action),
    Editor(EditorAction),
}

impl Bind {
    pub fn label(self) -> &'static str {
        match self {
            Bind::Global(a) => a.label(),
            Bind::Editor(a) => a.label(),
        }
    }

    pub fn default_binding(self) -> &'static str {
        match self {
            Bind::Global(a) => a.default_binding(),
            Bind::Editor(a) => a.default_binding(),
        }
    }
}

#[derive(Default)]
pub struct SettingsState {
    pub tab: Tab,
    pub listening: Option<Bind>,
    pub publishing: PublishingState,
}

/// The "Tour publishing" group's own state. The token field is only ever a masked
/// input. It is emptied, wiped and given a new id (dropping its undo history) on Save,
/// on Forget, when the page changes and when the window closes, and a saved token is
/// never read back.
#[derive(Default)]
pub struct PublishingState {
    token_input: String,
    /// Part of the token field's id; a new value gives the field a fresh, empty state.
    field: u64,
    /// The token field's id as last drawn, so its stored state can be removed.
    field_id: Option<egui::Id>,
    /// The last Save, Forget or Test connection result.
    message: Arc<Mutex<Option<Outcome>>>,
    /// A Save, Forget or Test connection is running on its background thread.
    busy: Arc<Mutex<bool>>,
}

/// A finished Save, Forget or Test connection.
#[derive(Clone)]
struct Outcome {
    text: String,
    ok: bool,
    /// For Test connection: the address it tested. The result shows only while the field still holds it.
    server: Option<String>,
}

impl PublishingState {
    fn is_busy(&self) -> bool {
        self.busy.lock().map(|b| *b).unwrap_or(false)
    }

    /// Empty the token field: wipe its text, remove its stored state with the undo
    /// history, and give it a new id.
    pub fn clear_token_field(&mut self, ctx: &egui::Context) {
        publish_ui::wipe(&mut self.token_input);
        if let Some(id) = self.field_id.take() {
            ctx.data_mut(|d| d.remove::<egui::text_edit::TextEditState>(id));
        }
        self.field += 1;
    }

    /// The typed token for Save; the field is cleared as it is taken.
    fn take_for_save(&mut self, ctx: &egui::Context) -> String {
        let entered = self.token_input.clone();
        self.clear_token_field(ctx);
        entered
    }

    /// Run `work` on a background thread and show what it returns. `server` is the
    /// address a Test connection checks.
    fn run(&self, server: Option<String>, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        if let Ok(mut b) = self.busy.lock() {
            *b = true;
        }
        if let Ok(mut m) = self.message.lock() {
            *m = None;
        }
        let (busy, message) = (self.busy.clone(), self.message.clone());
        std::thread::spawn(move || {
            let (text, ok) = match work() {
                Ok(text) => (text, true),
                Err(text) => (text, false),
            };
            if let Ok(mut m) = message.lock() {
                *m = Some(Outcome { text, ok, server });
            }
            if let Ok(mut b) = busy.lock() {
                *b = false;
            }
        });
    }

    /// The result to show beside Test connection, if it still applies to `server`.
    fn current_message(&self, server: &str) -> Option<(String, bool)> {
        let m = self.message.lock().ok()?.clone()?;
        match &m.server {
            Some(tested) if tested.trim() != server.trim() => None,
            _ => Some((m.text, m.ok)),
        }
    }

    /// Whether a token has been typed and not saved.
    #[cfg(test)]
    pub fn has_unsaved_token(&self) -> bool {
        !self.token_input.is_empty()
    }

    #[cfg(test)]
    pub fn type_token_for_test(&mut self, token: &str) {
        self.token_input = token.to_string();
    }

    fn clear_message(&self) {
        if let Ok(mut m) = self.message.lock() {
            *m = None;
        }
    }
}

/// The masked token field. Egui's undo history for it is dropped every frame, so an
/// earlier text can never be brought back with Ctrl+Z.
fn token_field(ui: &mut egui::Ui, p: &mut PublishingState, hint: &str, width: f32) -> egui::Response {
    let edit = egui::TextEdit::singleline(&mut p.token_input).id_salt(("publish-token", p.field)).password(true).hint_text(hint).desired_width(width);
    let resp = ui.add(edit);
    p.field_id = Some(resp.id);
    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), resp.id) {
        state.clear_undoer();
        state.store(ui.ctx(), resp.id);
    }
    resp
}

pub enum UiAction {
    Listen(Bind),
    StopListening,
    SetBinding(Bind, String),
    ResetAll,
    OpenSettingsFolder,
    SettingsChanged,
    SetAutostart(bool),
    SetTakePrintScreen(bool),
}

/// The Preferences page's part of the state shared between the app and the main window (see
/// `main_window::Shared`). The window draws on its own schedule: it edits `settings`, queues
/// `actions`, and the app applies them on its next frame and refreshes the rest.
pub struct Shared {
    pub settings: Settings,
    pub errors: HashMap<Bind, String>,
    pub snipping_tool: Option<bool>,
    pub autostart: bool,
    pub state: SettingsState,
    pub actions: Vec<UiAction>,
}

pub struct View<'a> {
    pub settings: &'a mut Settings,
    pub errors: &'a HashMap<Bind, String>,
    pub snipping_tool: Option<bool>,
    pub autostart: bool,
}

/// Draw the page: the three tabs in a row, and the selected one under them.
pub fn ui(ui: &mut egui::Ui, state: &mut SettingsState, view: View<'_>) -> Vec<UiAction> {
    let mut out = Vec::new();
    ui.horizontal(|ui| {
        for (tab, name) in [(Tab::General, "General"), (Tab::Output, "Output"), (Tab::Hotkeys, "Hotkeys")] {
            let selected = state.tab == tab;
            if ui.add(egui::Button::selectable(selected, name).min_size(vec2(96.0, 30.0))).clicked() {
                if state.tab != tab {
                    state.publishing.clear_token_field(ui.ctx());
                }
                state.tab = tab;
                out.push(UiAction::StopListening);
            }
        }
    });
    ui.separator();
    ui.add_space(6.0);
    match state.tab {
        Tab::General => general(ui, view, &mut out),
        Tab::Output => output_tab(ui, &mut state.publishing, view, &mut out),
        Tab::Hotkeys => hotkeys_tab(ui, state, view, &mut out),
    }
    out
}

fn general(ui: &mut egui::Ui, view: View<'_>, out: &mut Vec<UiAction>) {
    let s = view.settings;
    ui.heading("General");
    ui.add_space(12.0);
    let mut autostart = view.autostart;
    if ui.checkbox(&mut autostart, "Start Mythicshot when I sign in").changed() {
        out.push(UiAction::SetAutostart(autostart));
    }
    if ui.checkbox(&mut s.magnifier, "Show the magnifier when selecting a region").changed() {
        out.push(UiAction::SettingsChanged);
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label("Left-click on the tray icon");
        for choice in TrayLeftClick::ALL {
            if ui.radio_value(&mut s.tray_left_click, choice, choice.label()).changed() {
                out.push(UiAction::SettingsChanged);
            }
        }
    });
    ui.add_space(16.0);
    if ui.button("Open settings folder").clicked() {
        out.push(UiAction::OpenSettingsFolder);
    }
    // Which build this is.
    ui.add_space(16.0);
    ui.label(RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION"))).size(12.0).color(Color32::from_rgb(140, 148, 164)));
}

fn output_tab(ui: &mut egui::Ui, publishing: &mut PublishingState, view: View<'_>, out: &mut Vec<UiAction>) {
    egui::ScrollArea::vertical().show(ui, |ui| output_page(ui, publishing, view, out));
}

fn output_page(ui: &mut egui::Ui, publishing: &mut PublishingState, view: View<'_>, out: &mut Vec<UiAction>) {
    let s = view.settings;
    let mut changed = false;
    ui.heading("Output");
    ui.add_space(10.0);

    ui.label(RichText::new("AFTER CAPTURE").size(11.0).color(MUTED));
    ui.label(RichText::new("Every ticked destination runs, in this order. \"Ask every time\" shows a menu instead.").color(MUTED).size(12.0));
    ui.add_space(4.0);
    for d in DestId::ALL {
        let mut on = s.has_destination(*d);
        if ui.checkbox(&mut on, d.label()).on_hover_text(d.help()).changed() {
            s.toggle_destination(*d);
            changed = true;
        }
    }

    ui.add_space(14.0);
    ui.label(RichText::new("SAVING").size(11.0).color(MUTED));
    ui.add_space(4.0);
    ui.label("Captures folder");
    ui.horizontal(|ui| {
        let mut dir = s.output_dir.display().to_string();
        if ui.add(egui::TextEdit::singleline(&mut dir).desired_width(ui.available_width() - 90.0)).changed() {
            s.output_dir = dir.into();
            changed = true;
        }
        if ui.button("Browse…").clicked() {
            if let Some(p) = rfd::FileDialog::new().set_directory(&s.output_dir).pick_folder() {
                s.output_dir = p;
                changed = true;
            }
        }
    });
    ui.add_space(8.0);
    ui.label("File name pattern");
    ui.horizontal(|ui| {
        if ui.add(egui::TextEdit::singleline(&mut s.filename_pattern).desired_width(ui.available_width() - 90.0).font(egui::TextStyle::Monospace)).changed() {
            changed = true;
        }
        if ui.button("Default").clicked() {
            s.filename_pattern = DEFAULT_PATTERN.to_string();
            changed = true;
        }
    });
    let example = expand_pattern(&s.filename_pattern, "Window title", chrono::Local::now());
    ui.label(RichText::new(format!("Example: {example}.{}", s.image_format.extension())).color(MUTED).size(12.0));
    ui.label(RichText::new("Tokens: ${YYYY} ${MM} ${DD} ${hh} ${mm} ${ss} ${title} ${user}. Use / for subfolders.").color(MUTED).size(12.0));
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Format");
        changed |= ui.radio_value(&mut s.image_format, ImageFormat::Png, "PNG").changed();
        changed |= ui.radio_value(&mut s.image_format, ImageFormat::Jpeg, "JPEG").changed();
        if s.image_format == ImageFormat::Jpeg {
            changed |= ui.add(egui::Slider::new(&mut s.jpeg_quality, 40..=100).text("quality")).changed();
        }
    });

    ui.add_space(14.0);
    ui.label(RichText::new("RECORDING").size(11.0).color(MUTED));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Frame rate");
        egui::ComboBox::from_id_salt("record-fps").width(90.0).selected_text(format!("{} fps", s.record_fps)).show_ui(ui, |ui| {
            for fps in [10, 15, 24, 30] {
                changed |= ui.selectable_value(&mut s.record_fps, fps, format!("{fps} fps")).changed();
            }
        });
        ui.add_space(12.0);
        ui.label("Countdown");
        egui::ComboBox::from_id_salt("countdown").width(90.0).selected_text(if s.countdown_secs == 0 { "None".to_string() } else { format!("{} s", s.countdown_secs) }).show_ui(ui, |ui| {
            for secs in [0, 3, 5, 10] {
                let label = if secs == 0 { "None".to_string() } else { format!("{secs} s") };
                changed |= ui.selectable_value(&mut s.countdown_secs, secs, label).changed();
            }
        });
    });
    changed |= ui.checkbox(&mut s.record_keys, "Record key presses for the key overlay").on_hover_text("Off by default: the log could include anything you type, passwords too. It stays in memory and is deleted with the recording.").changed();
    ui.horizontal(|ui| {
        use mythicshot_editor::overlays::{ClickStyle, KeyStyle};
        ui.label("Show keys as");
        egui::ComboBox::from_id_salt("overlay-keys").width(130.0).selected_text(key_style_label(s.overlays.keys)).show_ui(ui, |ui| {
            for k in [KeyStyle::Off, KeyStyle::Chips, KeyStyle::Keyboard] {
                changed |= ui.selectable_value(&mut s.overlays.keys, k, key_style_label(k)).changed();
            }
        });
        ui.label("clicks as");
        egui::ComboBox::from_id_salt("overlay-clicks").width(130.0).selected_text(click_style_label(s.overlays.clicks)).show_ui(ui, |ui| {
            for c in [ClickStyle::Off, ClickStyle::Ripple, ClickStyle::Mouse, ClickStyle::Both] {
                changed |= ui.selectable_value(&mut s.overlays.clicks, c, click_style_label(c)).changed();
            }
        });
    });
    ui.horizontal(|ui| {
        changed |= ui.checkbox(&mut s.overlays.pointer, "Show the mouse pointer").changed();
        changed |= ui.checkbox(&mut s.overlays.halo, "Highlight it").changed();
    });
    ui.horizontal(|ui| {
        ui.label("GIF quality");
        changed |= ui.add(egui::Slider::new(&mut s.gif_quality, 50..=100)).on_hover_text("Lower is smaller; 90 looks clean for screen recordings").changed();
    });
    ui.label(RichText::new("GIFs are saved to the captures folder with the file name pattern above.").color(MUTED).size(12.0));

    ui.add_space(14.0);
    ui.label(RichText::new("TOUR PUBLISHING").size(11.0).color(MUTED));
    ui.add_space(4.0);
    changed |= tour_publishing(ui, publishing, &mut s.publish_server);
    ui.add_space(8.0);
    if changed {
        out.push(UiAction::SettingsChanged);
    }
}

/// The "Tour publishing" group: the server, the API token (Save and Forget) and Test
/// connection. Returns whether the server address changed.
fn tour_publishing(ui: &mut egui::Ui, p: &mut PublishingState, server: &mut String) -> bool {
    let busy = p.is_busy();
    if busy {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    let token = publish_ui::token_status();
    if token == TokenStatus::Checking {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    let mut changed = false;
    ui.label("Server");
    let test_server = publish_ui::test_server();
    let effective = match &test_server {
        Some(t) => {
            let mut shown = t.clone();
            ui.add_enabled(false, egui::TextEdit::singleline(&mut shown).desired_width(ui.available_width() - 90.0));
            ui.label(RichText::new("Debug build: set by MYTHICSHOT_TEST_PUBLISH for this run, not saved.").color(MUTED).size(12.0));
            t.clone()
        }
        None => {
            if ui.add(egui::TextEdit::singleline(server).hint_text("https://tours.example.com").desired_width(ui.available_width() - 90.0)).changed() {
                changed = true;
                p.clear_message();
            }
            server.clone()
        }
    };
    if !effective.trim().is_empty()
        && let Err(e) = crate::publish::check_server(&effective)
    {
        ui.label(RichText::new(e).color(WARN_TEXT).size(12.0));
    }
    ui.add_space(8.0);
    ui.label("API token");
    ui.horizontal(|ui| {
        let hint = if token == TokenStatus::Saved { "A token is saved; paste a new one to replace it" } else { "Paste the token from the server's setup page" };
        let width = ui.available_width() - 150.0;
        token_field(ui, p, hint, width);
        if ui.add_enabled(!busy && !p.token_input.trim().is_empty(), egui::Button::new("Save")).clicked() {
            // The field is emptied now; the token goes to the background thread and nowhere else.
            let mut entered = p.take_for_save(ui.ctx());
            p.run(None, move || {
                let saved = crate::publish::store_token(&entered);
                publish_ui::wipe(&mut entered);
                publish_ui::refresh_token_now();
                saved.map(|()| "Token saved.".to_string())
            });
        }
        // Forget stays available when the store holds something it cannot read, so it can be removed.
        if ui.add_enabled(!busy && token != TokenStatus::Missing, egui::Button::new("Forget")).clicked() {
            p.clear_token_field(ui.ctx());
            p.run(None, || {
                let forgotten = crate::publish::forget_token();
                publish_ui::refresh_token_now();
                forgotten.map(|()| "Token removed from Windows Credential Manager.".to_string())
            });
        }
    });
    match &token {
        TokenStatus::Checking => ui.label(RichText::new("Checking for a saved token…").color(MUTED).size(12.0)),
        TokenStatus::Saved => ui.label(RichText::new("A token is saved.").color(MUTED).size(12.0)),
        TokenStatus::Missing => ui.label(RichText::new("No token saved.").color(MUTED).size(12.0)),
        TokenStatus::Unavailable(e) => ui.label(RichText::new(e).color(WARN_TEXT).size(12.0)),
    };
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy, egui::Button::new("Test connection")).clicked() {
            let tested = effective.clone();
            p.run(Some(tested.clone()), move || {
                let server = crate::publish::check_server(&tested)?;
                let token = publish_ui::token_for_job()?;
                let name = crate::publish::workspace_name(&server, &token)?;
                Ok(format!("Connected to workspace \"{name}\""))
            });
        }
        if busy {
            ui.add(egui::Spinner::new());
        } else if let Some((text, ok)) = p.current_message(&effective) {
            let color = if ok { Color32::from_rgb(125, 211, 140) } else { WARN_TEXT };
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
        }
    });
    ui.label(RichText::new("The token is kept in Windows Credential Manager, not in the settings file.").color(MUTED).size(12.0));
    changed
}

pub fn key_style_label(k: mythicshot_editor::overlays::KeyStyle) -> &'static str {
    use mythicshot_editor::overlays::KeyStyle;
    match k {
        KeyStyle::Off => "Off",
        KeyStyle::Chips => "Key chips",
        KeyStyle::Keyboard => "On-screen keyboard",
    }
}

pub fn click_style_label(c: mythicshot_editor::overlays::ClickStyle) -> &'static str {
    use mythicshot_editor::overlays::ClickStyle;
    match c {
        ClickStyle::Off => "Off",
        ClickStyle::Ripple => "Ripple",
        ClickStyle::Mouse => "On-screen mouse",
        ClickStyle::Both => "Ripple and mouse",
    }
}

fn hotkeys_tab(ui: &mut egui::Ui, state: &mut SettingsState, view: View<'_>, out: &mut Vec<UiAction>) {
    ui.horizontal(|ui| {
        ui.heading("Hotkeys");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Reset all").clicked() {
                out.push(UiAction::ResetAll);
            }
            if ui.button("Open settings file").on_hover_text("Keybinds are saved in settings.toml; copy it to move them to another PC").clicked() {
                out.push(UiAction::OpenSettingsFolder);
            }
        });
    });
    ui.add_space(8.0);

    if cfg!(windows) {
        let mut take = view.settings.take_print_screen;
        if ui
            .checkbox(&mut take, "Take the Print Screen key from other apps")
            .on_hover_text("On: Mythicshot gets Print Screen shortcuts first, even when OneDrive, Snipping Tool or another screenshot tool wants them. Off: Windows gives them to whichever app asks first.")
            .changed()
        {
            view.settings.take_print_screen = take;
            out.push(UiAction::SetTakePrintScreen(take));
        }
        ui.add_space(8.0);
    }
    let prtsc_failed = Action::ALL
        .iter()
        .any(|a| view.errors.contains_key(&Bind::Global(*a)) && view.settings.binding(*a).to_ascii_lowercase().contains("printscreen"));
    let snipping = view.snipping_tool == Some(true) && !(cfg!(windows) && view.settings.take_print_screen);
    if snipping || prtsc_failed {
        egui::Frame::new().fill(WARN_BG).corner_radius(CornerRadius::same(8)).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if snipping {
                ui.label(RichText::new("Windows is using the Print Screen key for Snipping Tool").color(WARN_TEXT).strong());
                ui.label(RichText::new("Turn off Settings › Accessibility › Keyboard › \"Use the Print screen key to open screen capture\", then restart Mythicshot, or pick other keys below.").color(WARN_TEXT));
            } else {
                ui.label(RichText::new("Another app is holding the Print Screen shortcuts").color(WARN_TEXT).strong());
                ui.label(RichText::new("Another screenshot tool is probably running. Close it or turn off its shortcuts, then restart Mythicshot, or pick other keys below.").color(WARN_TEXT));
            }
        });
        ui.add_space(8.0);
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        let mut current_group = "";
        for &action in Action::ALL {
            if action.group() != current_group {
                current_group = action.group();
                group_title(ui, current_group);
            }
            row(ui, state, &view, Bind::Global(action), view.settings.binding(action), out);
        }
        group_title(ui, "Editor");
        ui.label(RichText::new("These work inside the editor window only.").color(MUTED).size(12.0));
        ui.add_space(4.0);
        for &action in EditorAction::ALL {
            row(ui, state, &view, Bind::Editor(action), view.settings.editor_binding(action), out);
        }
    });
}

fn group_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title.to_uppercase()).size(11.0).color(MUTED));
    ui.add_space(4.0);
}

fn row(ui: &mut egui::Ui, state: &mut SettingsState, view: &View<'_>, action: Bind, binding: &str, out: &mut Vec<UiAction>) {
    let listening = state.listening == Some(action);
    let error = view.errors.get(&action);
    let fill = if listening {
        Color32::from_rgb(30, 42, 64)
    } else if error.is_some() {
        Color32::from_rgb(36, 30, 20)
    } else {
        Color32::from_rgb(27, 31, 39)
    };
    egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.set_min_height(34.0);
            ui.label(RichText::new(action.label()).size(14.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_sized([56.0, 32.0], egui::Button::new("Reset")).on_hover_text("Reset to default").clicked() {
                    out.push(UiAction::SetBinding(action, action.default_binding().to_string()));
                }
                let text = if listening {
                    "Press new keys…".to_string()
                } else if binding.is_empty() {
                    "Not set".to_string()
                } else {
                    hotkeys::display(binding)
                };
                let stroke = if listening {
                    Stroke::new(1.0, Color32::from_rgb(138, 180, 255))
                } else if error.is_some() {
                    Stroke::new(1.0, Color32::from_rgb(201, 138, 30))
                } else {
                    Stroke::new(1.0, Color32::from_rgb(52, 59, 72))
                };
                let button = egui::Button::new(RichText::new(text).monospace()).stroke(stroke).min_size(vec2(170.0, 32.0));
                if ui.add(button).on_hover_text("Click, then press the new shortcut").clicked() {
                    out.push(if listening { UiAction::StopListening } else { UiAction::Listen(action) });
                }
                if listening {
                    ui.label(RichText::new("Esc cancels · Backspace clears").color(MUTED).size(12.0));
                } else if let Some(e) = error {
                    ui.label(RichText::new(e).color(WARN_TEXT).size(12.0));
                }
            });
        });
    });
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "dp_0123456789abcdef0123456789abcdef01234567";

    /// One frame of just the token field, `t` seconds in, with these input events.
    fn frame(ctx: &egui::Context, p: &mut PublishingState, t: f64, events: Vec<egui::Event>) {
        let input = egui::RawInput { time: Some(t), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            token_field(ui, p, "", 300.0);
        });
        out.textures_delta.clear();
    }

    fn focus(ctx: &egui::Context, p: &PublishingState) {
        let id = p.field_id.expect("drawn");
        ctx.memory_mut(|m| m.request_focus(id));
    }

    fn undo() -> egui::Event {
        egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }
    }

    #[test]
    fn after_save_the_token_cannot_be_brought_back_by_undo() {
        let ctx = egui::Context::default();
        let mut p = PublishingState::default();
        frame(&ctx, &mut p, 0.0, vec![]);
        focus(&ctx, &p);
        frame(&ctx, &mut p, 1.0, vec![]);
        frame(&ctx, &mut p, 2.0, vec![egui::Event::Text(TOKEN.into())]);
        for t in 3..8 {
            frame(&ctx, &mut p, t as f64 * 2.0, vec![]);
        }
        assert_eq!(p.token_input, TOKEN);
        let old = p.field_id.unwrap();
        let taken = p.take_for_save(&ctx);
        assert_eq!(taken, TOKEN);
        assert!(p.token_input.is_empty());
        assert!(egui::text_edit::TextEditState::load(&ctx, old).is_none(), "the old field's state is gone");
        frame(&ctx, &mut p, 20.0, vec![]);
        assert_ne!(p.field_id, Some(old), "the field has a fresh id");
        focus(&ctx, &p);
        frame(&ctx, &mut p, 21.0, vec![]);
        frame(&ctx, &mut p, 22.0, vec![undo()]);
        frame(&ctx, &mut p, 23.0, vec![undo()]);
        assert!(p.token_input.is_empty(), "undo brought back {:?}", p.token_input);
    }

    #[test]
    fn undo_never_restores_a_token_even_in_the_same_field() {
        let ctx = egui::Context::default();
        let mut p = PublishingState::default();
        frame(&ctx, &mut p, 0.0, vec![]);
        focus(&ctx, &p);
        frame(&ctx, &mut p, 1.0, vec![]);
        frame(&ctx, &mut p, 2.0, vec![egui::Event::Text(TOKEN.into())]);
        for t in 3..8 {
            frame(&ctx, &mut p, t as f64 * 2.0, vec![]);
        }
        // Select all and delete, then undo.
        let key = |k, m| egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m };
        frame(&ctx, &mut p, 20.0, vec![key(egui::Key::A, egui::Modifiers::COMMAND), key(egui::Key::Backspace, egui::Modifiers::NONE)]);
        assert!(p.token_input.is_empty());
        frame(&ctx, &mut p, 22.0, vec![]);
        frame(&ctx, &mut p, 24.0, vec![undo()]);
        assert!(p.token_input.is_empty(), "undo brought back {:?}", p.token_input);
    }

    #[test]
    fn closing_or_leaving_the_page_clears_an_unsaved_token() {
        let ctx = egui::Context::default();
        let mut p = PublishingState { token_input: TOKEN.into(), ..Default::default() };
        let before = p.field;
        p.clear_token_field(&ctx);
        assert!(p.token_input.is_empty());
        assert_ne!(p.field, before);
    }

    #[test]
    fn a_test_result_only_shows_for_the_address_it_tested() {
        let p = PublishingState::default();
        *p.message.lock().unwrap() = Some(Outcome { text: "Connected to workspace \"Acme\"".into(), ok: true, server: Some("https://a.example.com".into()) });
        assert!(p.current_message("https://a.example.com").is_some());
        assert!(p.current_message(" https://a.example.com ").is_some());
        assert!(p.current_message("https://b.example.com").is_none());
        // Save and Forget results are not tied to an address.
        *p.message.lock().unwrap() = Some(Outcome { text: "Token saved.".into(), ok: true, server: None });
        assert!(p.current_message("https://b.example.com").is_some());
    }
}
