//! The settings window (screen 5 of the mockup): General and Hotkeys tabs.

use std::collections::HashMap;

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Stroke, vec2};

use crate::hotkeys::{self, Action, EditorAction};
use crate::settings::Settings;

const WARN_BG: Color32 = Color32::from_rgb(58, 42, 16);
const WARN_TEXT: Color32 = Color32::from_rgb(255, 196, 107);
const MUTED: Color32 = Color32::from_rgb(163, 170, 184);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Hotkeys,
    General,
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
}

pub enum UiAction {
    Listen(Bind),
    StopListening,
    SetBinding(Bind, String),
    ResetAll,
    OpenSettingsFolder,
    SettingsChanged,
}

/// State shared between the app and the settings window. The window is a
/// deferred viewport (eframe's OpenGL backend can't open an immediate one from
/// a tray click), so it draws on its own schedule: it edits `settings`, queues
/// `actions`, and the app applies them on its next frame and refreshes the rest.
pub struct Shared {
    pub settings: Settings,
    pub errors: HashMap<Bind, String>,
    pub snipping_tool: Option<bool>,
    pub state: SettingsState,
    pub actions: Vec<UiAction>,
    pub close_requested: bool,
    pub focus: bool,
}

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("settings")
}

pub fn show(ctx: &egui::Context, shared: &std::sync::Arc<std::sync::Mutex<Shared>>, builder: egui::ViewportBuilder) {
    let shared = shared.clone();
    ctx.show_viewport_deferred(viewport_id(), builder, move |ui, _class| {
        let mut guard = shared.lock().expect("settings state");
        let s = &mut *guard;
        if std::mem::take(&mut s.focus) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            s.close_requested = true;
        }
        let view = View { settings: &mut s.settings, errors: &s.errors, snipping_tool: s.snipping_tool };
        let out = self::ui(ui, &mut s.state, view);
        s.actions.extend(out);
    });
}

pub struct View<'a> {
    pub settings: &'a mut Settings,
    pub errors: &'a HashMap<Bind, String>,
    pub snipping_tool: Option<bool>,
}

pub fn ui(ui: &mut egui::Ui, state: &mut SettingsState, view: View<'_>) -> Vec<UiAction> {
    let mut out = Vec::new();
    egui::Panel::left("settings-nav").resizable(false).exact_size(180.0).show(ui, |ui| {
        ui.add_space(8.0);
        ui.label(RichText::new("Mythicshot settings").strong().size(15.0));
        ui.add_space(10.0);
        for (tab, name) in [(Tab::General, "General"), (Tab::Hotkeys, "Hotkeys")] {
            let selected = state.tab == tab;
            if ui.add_sized([ui.available_width(), 34.0], egui::Button::selectable(selected, name)).clicked() {
                state.tab = tab;
                out.push(UiAction::StopListening);
            }
        }
    });
    egui::CentralPanel::default().show(ui, |ui| match state.tab {
        Tab::General => general(ui, view, &mut out),
        Tab::Hotkeys => hotkeys_tab(ui, state, view, &mut out),
    });
    out
}

fn general(ui: &mut egui::Ui, view: View<'_>, out: &mut Vec<UiAction>) {
    let s = view.settings;
    ui.heading("General");
    ui.add_space(12.0);
    ui.label("Save captures to");
    let mut dir = s.output_dir.display().to_string();
    if ui.add(egui::TextEdit::singleline(&mut dir).desired_width(f32::INFINITY)).changed() {
        s.output_dir = dir.into();
        out.push(UiAction::SettingsChanged);
    }
    ui.add_space(8.0);
    let mut changed = false;
    changed |= ui.checkbox(&mut s.open_editor, "Open each capture in the editor").changed();
    ui.label(RichText::new("When off, captures are saved and copied straight away using the two options below.").color(MUTED).size(12.0));
    changed |= ui.checkbox(&mut s.save_to_file, "Save every capture as a PNG file").changed();
    changed |= ui.checkbox(&mut s.copy_to_clipboard, "Copy every capture to the clipboard").changed();
    changed |= ui.checkbox(&mut s.magnifier, "Show the magnifier when selecting").changed();
    if changed {
        out.push(UiAction::SettingsChanged);
    }
    ui.add_space(16.0);
    if ui.button("Open settings folder").clicked() {
        out.push(UiAction::OpenSettingsFolder);
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

    let prtsc_failed = Action::ALL
        .iter()
        .any(|a| view.errors.contains_key(&Bind::Global(*a)) && view.settings.binding(*a).to_ascii_lowercase().contains("printscreen"));
    if view.snipping_tool == Some(true) || prtsc_failed {
        egui::Frame::new().fill(WARN_BG).corner_radius(CornerRadius::same(8)).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if view.snipping_tool == Some(true) {
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
