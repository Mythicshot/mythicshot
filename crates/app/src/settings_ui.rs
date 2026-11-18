//! The Preferences page of the main window: the General, Output and Hotkeys tabs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Stroke, vec2};

use mythicshot_destinations::{DEFAULT_PATTERN, DestId, ImageFormat, expand_pattern};

use crate::hotkeys::{self, Action, EditorAction};
use crate::project::{Store, Totals};
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
    /// What the Output tab knows about the saved recordings.
    pub saved: SavedRecordings,
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
    /// The Output tab's link to the Workspace page. The main window takes it; it never reaches the app.
    OpenWorkspace,
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
    ui_with(ui, state, view, &mut |folder| crate::platform::open_folder(folder))
}

/// [`ui`], with `open_folder` asked to show a folder in the file manager.
fn ui_with(ui: &mut egui::Ui, state: &mut SettingsState, view: View<'_>, open_folder: &mut dyn FnMut(&Path)) -> Vec<UiAction> {
    let mut out = Vec::new();
    ui.horizontal(|ui| {
        for (tab, name) in [(Tab::General, "General"), (Tab::Output, "Output"), (Tab::Hotkeys, "Hotkeys")] {
            let selected = state.tab == tab;
            if ui.add(egui::Button::selectable(selected, name).min_size(vec2(96.0, 30.0))).clicked() {
                state.tab = tab;
                out.push(UiAction::StopListening);
            }
        }
    });
    ui.separator();
    ui.add_space(6.0);
    // The settings file could not be used as it was at start-up.
    if let Some(problem) = &view.settings.load_problem {
        egui::Frame::new().fill(WARN_BG).corner_radius(CornerRadius::same(8)).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(problem.sentence()).color(WARN_TEXT)).wrap());
        });
        ui.add_space(8.0);
    }
    match state.tab {
        Tab::General => general(ui, view, &mut out),
        Tab::Output => output_tab(ui, view, &mut state.saved, open_folder, &mut out),
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

fn output_tab(ui: &mut egui::Ui, view: View<'_>, saved: &mut SavedRecordings, open_folder: &mut dyn FnMut(&Path), out: &mut Vec<UiAction>) {
    // The tab is opened when it was not drawn in the pass before this one.
    let pass = ui.ctx().cumulative_pass_nr();
    let opened = saved.drawn_in.is_none_or(|last| pass > last + 1);
    saved.drawn_in = Some(pass);
    let ctx = ui.ctx().clone();
    saved.refresh(crate::project::epoch(), opened, &mut |root| count_in_background(root, ctx.clone()));
    egui::ScrollArea::vertical().show(ui, |ui| output_page(ui, view, saved, open_folder, out));
}

fn output_page(ui: &mut egui::Ui, view: View<'_>, saved: &SavedRecordings, open_folder: &mut dyn FnMut(&Path), out: &mut Vec<UiAction>) {
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
    ui.horizontal(|ui| {
        ui.label(TOUR_SITE_MOVED);
        if ui.link(OPEN_WORKSPACE).clicked() {
            out.push(UiAction::OpenWorkspace);
        }
    });
    ui.add_space(8.0);
    // Turning the switch off deletes nothing: the line and the button stay as they are.
    changed |= ui.checkbox(&mut s.keep_published_recordings, KEEP_RECORDING).changed();
    // Their own row, under the switch's label.
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
        if let Some(line) = saved.line() {
            ui.add(egui::Label::new(RichText::new(line).color(MUTED)).extend()).on_hover_text(RECORDINGS_STAY);
        }
        if let Some(root) = saved.root() {
            if ui.button(OPEN_FOLDER).on_hover_text(RECORDINGS_STAY).clicked() {
                open_folder(root);
            }
        }
    });
    ui.add_space(8.0);
    if changed {
        out.push(UiAction::SettingsChanged);
    }
}

/// What the Output tab says where the tour site settings used to be.
pub const TOUR_SITE_MOVED: &str = "Tour site settings are on the Workspace page.";
pub const OPEN_WORKSPACE: &str = "Open Workspace";

/// The switch of the Output tab: a tour's recording is kept on this computer when it is published.
pub const KEEP_RECORDING: &str = "Keep the recording when I publish a tour, so I can edit it later";
pub const OPEN_FOLDER: &str = "Open folder";
/// The hover text of the count and of "Open folder". The wording is proposed and waits for the
/// owner's approval.
pub const RECORDINGS_STAY: &str = "Saved recordings stay on this computer until you delete their tour or remove them from this folder.";

/// How many recordings are saved on this computer and the room they take together. The form
/// is proposed and waits for the owner's approval.
pub fn saved_recordings_line(t: Totals) -> String {
    match t.count {
        0 => "No saved recordings".to_string(),
        1 => format!("1 saved recording · {}", size_text(t.bytes)),
        n => format!("{n} saved recordings · {}", size_text(t.bytes)),
    }
}

/// A size as the editor's status bar writes one (KB under a megabyte, else MB with one
/// decimal), in whole megabytes from a hundred up, and in GB with one decimal from a gigabyte.
fn size_text(bytes: u64) -> String {
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

/// The saved recordings as the Output tab shows them: counted on a background thread, and kept
/// until a project is saved or removed or the tab is opened again. Drawing never reads the disk.
#[derive(Default)]
pub struct SavedRecordings {
    /// The projects folder, looked up when first needed; inside, None when this user has none.
    root: Option<Option<PathBuf>>,
    /// The totals last counted. None until the first count is back.
    shown: Option<Totals>,
    /// [`crate::project::epoch`] when the newest count was started.
    counted_at: Option<u64>,
    /// The count that is running.
    counting: Option<Receiver<Totals>>,
    /// The pass the tab was last drawn in.
    drawn_in: Option<u64>,
}

impl SavedRecordings {
    /// For the projects folder `root`; None stands for a user whose folder cannot be found.
    #[cfg(test)]
    fn at(root: Option<PathBuf>) -> SavedRecordings {
        SavedRecordings { root: Some(root), ..Default::default() }
    }

    /// The folder "Open folder" shows: the store's own, never a path read from a project.
    fn root(&self) -> Option<&Path> {
        self.root.as_ref()?.as_deref()
    }

    /// The line beside the switch. None while the first count is running; a user without a
    /// projects folder has no saved recordings.
    fn line(&self) -> Option<String> {
        match self.root() {
            Some(_) => self.shown.map(saved_recordings_line),
            None => Some(saved_recordings_line(Totals::default())),
        }
    }

    /// Take a finished count, and start one with `count` when the tab was just `opened` or a
    /// project was saved or removed since the last one (`epoch` moved). One count runs at a
    /// time, and what was counted before stays shown meanwhile.
    fn refresh(&mut self, epoch: u64, opened: bool, count: &mut dyn FnMut(&Path) -> Receiver<Totals>) {
        let root = self.root.get_or_insert_with(default_root).clone();
        if let Some(running) = &self.counting {
            match running.try_recv() {
                Ok(totals) => self.shown = Some(totals),
                Err(TryRecvError::Empty) => return,
                // The count died: what is shown stays until the next reason to count.
                Err(TryRecvError::Disconnected) => {}
            }
            self.counting = None;
        }
        let Some(root) = root else { return };
        if opened || self.counted_at != Some(epoch) {
            self.counted_at = Some(epoch);
            self.counting = Some(count(&root));
        }
    }
}

/// This user's projects folder. A test never looks into the real one.
fn default_root() -> Option<PathBuf> {
    if cfg!(test) { None } else { Store::default_root() }
}

/// Count the projects in `root` on a thread of its own, and draw again when it is done.
/// Counting creates nothing.
fn count_in_background(root: &Path, ctx: egui::Context) -> Receiver<Totals> {
    let (tx, rx) = std::sync::mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(Store::at(root).totals());
        ctx.request_repaint();
    });
    rx
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
    use std::sync::mpsc::channel;

    const SCREEN: egui::Vec2 = vec2(600.0, 2000.0);
    /// The folder of a site, as the store names one.
    const SITE: &str = "0123456789abcdef";

    /// A folder the store lists as a project taking `bytes` on the disk. The store counts a
    /// project it cannot open like any other, so the file need not be a real one.
    fn project(root: &Path, site: &str, tour: &str, chapter: &str, bytes: usize) {
        let dir = root.join(site).join(tour).join(chapter);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("project.json"), vec![b' '; bytes]).unwrap();
    }

    /// A count that is back at once, and how many were started.
    fn counted(root: &Path, started: &mut usize) -> Receiver<Totals> {
        *started += 1;
        let (tx, rx) = channel();
        tx.send(Store::at(root.to_path_buf()).totals()).unwrap();
        rx
    }

    fn texts_of(out: &egui::FullOutput) -> Vec<String> {
        fn collect(shape: &egui::Shape, into: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => into.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        texts
    }

    /// Where `text` was drawn.
    fn rect_of(out: &egui::FullOutput, text: &str) -> Option<egui::Rect> {
        fn find(shape: &egui::Shape, text: &str) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == text => Some(egui::Rect::from_min_size(t.pos, t.galley.size())),
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, text)),
                _ => None,
            }
        }
        out.shapes.iter().find_map(|c| find(&c.shape, text))
    }

    fn middle_of(out: &egui::FullOutput, text: &str) -> Option<egui::Pos2> {
        rect_of(out, text).map(|r| r.center())
    }

    /// The Output tab drawn without a window: its settings, its state, what it asked the app
    /// for and every folder it asked to have opened.
    struct Page {
        ctx: egui::Context,
        settings: Settings,
        state: SettingsState,
        actions: Vec<UiAction>,
        opened: Vec<PathBuf>,
        screen: egui::Vec2,
    }

    impl Page {
        fn new(root: Option<PathBuf>) -> Page {
            let state = SettingsState { tab: Tab::Output, listening: None, saved: SavedRecordings::at(root) };
            Page { ctx: egui::Context::default(), settings: Settings::default(), state, actions: Vec::new(), opened: Vec::new(), screen: SCREEN }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen)), events, ..Default::default() };
            let errors = HashMap::new();
            let (settings, state, actions, opened) = (&mut self.settings, &mut self.state, &mut self.actions, &mut self.opened);
            let mut out = self.ctx.run_ui(input, |ui| {
                let view = View { settings: &mut *settings, errors: &errors, snipping_tool: None, autostart: false };
                actions.extend(ui_with(ui, &mut *state, view, &mut |folder| opened.push(folder.to_path_buf())));
            });
            out.textures_delta.clear();
            out
        }

        /// Draw until the count is back and its line is on the page.
        fn settled(&mut self) -> egui::FullOutput {
            for _ in 0..500 {
                self.frame(vec![]);
                if self.state.saved.counting.is_none() {
                    return self.frame(vec![]);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("the count never came back");
        }

        fn click(&mut self, text: &str) {
            let out = self.settled();
            let at = middle_of(&out, text).unwrap_or_else(|| panic!("{text} is drawn: {:?}", texts_of(&out)));
            let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            for events in [vec![egui::Event::PointerMoved(at)], vec![press(true)], vec![press(false)], vec![]] {
                self.frame(events);
            }
        }
    }

    #[test]
    fn the_switch_is_on_by_default_and_turning_it_off_is_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let mut page = Page::new(Some(tmp.path().join("projects")));
        assert!(page.settings.keep_published_recordings);
        page.settled();
        assert!(page.actions.is_empty(), "drawing changes nothing");
        // Off: the app is told, and it saves the settings and applies them to the next publish.
        page.click(KEEP_RECORDING);
        assert!(!page.settings.keep_published_recordings);
        assert!(matches!(page.actions[..], [UiAction::SettingsChanged]));
        // On again, the same way.
        page.click(KEEP_RECORDING);
        assert!(page.settings.keep_published_recordings);
        assert!(matches!(page.actions[..], [UiAction::SettingsChanged, UiAction::SettingsChanged]));
        assert!(page.opened.is_empty() && !tmp.path().join("projects").exists(), "the switch opens and creates nothing");
    }

    #[test]
    fn the_line_counts_recordings_and_their_size() {
        let line = |count, bytes| saved_recordings_line(Totals { count, bytes });
        assert_eq!(line(0, 0), "No saved recordings");
        assert_eq!(line(1, 13_002_342), "1 saved recording · 12.4 MB");
        assert_eq!(line(3, 432_013_312), "3 saved recordings · 412 MB");
        assert_eq!(line(2, 389_021_696), "2 saved recordings · 371 MB");
        // Under a megabyte in KB; from a megabyte with one decimal; from a hundred without.
        assert_eq!(line(1, 860_160), "1 saved recording · 840 KB");
        assert_eq!(line(1, 0), "1 saved recording · 0 KB");
        assert_eq!(line(1, 1_048_575), "1 saved recording · 1024 KB");
        assert_eq!(line(1, 1_048_576), "1 saved recording · 1.0 MB");
        assert_eq!(line(2, 32_505_856), "2 saved recordings · 31.0 MB");
        assert_eq!(line(2, 104_752_742), "2 saved recordings · 99.9 MB");
        assert_eq!(line(2, 104_857_599), "2 saved recordings · 100 MB");
        // From a gigabyte in GB with one decimal.
        assert_eq!(line(9, 1_073_741_823), "9 saved recordings · 1024 MB");
        assert_eq!(line(9, 1_073_741_824), "9 saved recordings · 1.0 GB");
        assert_eq!(line(40, 5_368_709_120), "40 saved recordings · 5.0 GB");
        assert_eq!(line(90, 13_314_398_618), "90 saved recordings · 12.4 GB");
        assert_eq!(line(900, 161_061_273_600), "900 saved recordings · 150.0 GB");
    }

    #[test]
    fn the_totals_are_not_read_again_until_a_project_is_saved_or_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("projects");
        project(&root, SITE, "tour_a", "ch_a", 2_097_152);
        let mut saved = SavedRecordings::at(Some(root.clone()));
        let mut started = 0;
        // Nothing is shown before the first count is back.
        assert_eq!(saved.line(), None);
        saved.refresh(7, true, &mut |root| counted(root, &mut started));
        assert_eq!((started, saved.line()), (1, None));
        // Drawn again and again: the count is taken once and the disk is left alone, also when
        // the folder changed behind the app's back.
        project(&root, SITE, "tour_a", "ch_b", 1_048_576);
        for _ in 0..50 {
            saved.refresh(7, false, &mut |root| counted(root, &mut started));
        }
        assert_eq!((started, saved.line().as_deref()), (1, Some("1 saved recording · 2.0 MB")));
        // A project was saved or removed: counted again, the earlier line staying until it is back.
        saved.refresh(8, false, &mut |root| counted(root, &mut started));
        assert_eq!((started, saved.line().as_deref()), (2, Some("1 saved recording · 2.0 MB")));
        saved.refresh(8, false, &mut |root| counted(root, &mut started));
        assert_eq!((started, saved.line().as_deref()), (2, Some("2 saved recordings · 3.0 MB")));
        // The tab is opened again: counted again, whatever the counter says.
        std::fs::remove_dir_all(root.join(SITE).join("tour_a").join("ch_a")).unwrap();
        saved.refresh(8, true, &mut |root| counted(root, &mut started));
        saved.refresh(8, false, &mut |root| counted(root, &mut started));
        assert_eq!((started, saved.line().as_deref()), (3, Some("1 saved recording · 1.0 MB")));
    }

    #[test]
    fn one_count_runs_at_a_time_and_a_change_during_it_is_counted_afterwards() {
        let tmp = tempfile::tempdir().unwrap();
        let mut saved = SavedRecordings::at(Some(tmp.path().to_path_buf()));
        let mut started = 0;
        let (tx, rx) = channel();
        let mut slow = Some(rx);
        saved.refresh(1, true, &mut |_| {
            started += 1;
            slow.take().unwrap()
        });
        // Still running: nothing else is started, by a change or by opening the tab again.
        for (epoch, opened) in [(1, false), (2, false), (2, true)] {
            saved.refresh(epoch, opened, &mut |root| counted(root, &mut started));
        }
        assert_eq!((started, saved.line()), (1, None));
        // It comes back: it is shown, and the change it may have missed is counted.
        tx.send(Totals { count: 4, bytes: 4_194_304 }).unwrap();
        saved.refresh(2, false, &mut |root| counted(root, &mut started));
        assert_eq!((started, saved.line().as_deref()), (2, Some("4 saved recordings · 4.0 MB")));
        saved.refresh(2, false, &mut |root| counted(root, &mut started));
        assert_eq!((started, saved.line().as_deref()), (2, Some("No saved recordings")));
        // A count that died is not started over and over, and what is shown stays.
        let (dead, rx) = channel::<Totals>();
        drop(dead);
        let mut gone = Some(rx);
        saved.refresh(3, false, &mut |_| {
            started += 1;
            gone.take().unwrap()
        });
        for _ in 0..5 {
            saved.refresh(3, false, &mut |root| counted(root, &mut started));
        }
        assert_eq!((started, saved.line().as_deref()), (3, Some("No saved recordings")));
    }

    #[test]
    fn without_a_projects_folder_nothing_is_counted_and_no_folder_is_offered() {
        let mut saved = SavedRecordings::at(None);
        let mut started = 0;
        assert_eq!(saved.line().as_deref(), Some("No saved recordings"));
        saved.refresh(1, true, &mut |root| counted(root, &mut started));
        saved.refresh(2, false, &mut |root| counted(root, &mut started));
        assert_eq!((started, saved.line().as_deref(), saved.root()), (0, Some("No saved recordings"), None));
        // A state the tests did not give a folder looks for none, so no test reads the real one.
        let mut fresh = SavedRecordings::default();
        fresh.refresh(1, true, &mut |root| counted(root, &mut started));
        assert_eq!((started, fresh.root()), (0, None));
        // On the page: the switch and the line, and no button.
        let mut page = Page::new(None);
        let texts = texts_of(&page.settled());
        assert!(texts.iter().any(|t| t == KEEP_RECORDING) && texts.iter().any(|t| t == "No saved recordings"), "{texts:?}");
        assert!(!texts.iter().any(|t| t == OPEN_FOLDER), "{texts:?}");
    }

    #[test]
    fn open_folder_asks_for_the_projects_folder_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("projects");
        let mut page = Page::new(Some(root.clone()));
        page.settled();
        assert!(page.opened.is_empty(), "nothing is opened by drawing");
        page.click(OPEN_FOLDER);
        assert_eq!(page.opened, [root.clone()]);
        assert!(page.actions.is_empty() && page.settings.keep_published_recordings, "the button changes no setting");
        // Counting and drawing made no folder; making it is left to the helper that opens it.
        assert!(!root.exists());
        // With recordings saved, and with the switch off: the same folder, not a project's.
        project(&root, SITE, "tour_a", "ch_a", 1_048_576);
        page.settings.keep_published_recordings = false;
        page.click(OPEN_FOLDER);
        assert_eq!(page.opened, [root.clone(), root]);
    }

    #[test]
    fn the_page_draws_the_switch_the_line_and_the_button() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("projects");
        project(&root, SITE, "tour_a", "ch_a", 1_048_576);
        project(&root, "fedcba9876543210", "tour_b", "ch_a", 524_288);
        let mut page = Page::new(Some(root.clone()));
        // The first frame reads nothing itself: the line comes when the count is back.
        let first = texts_of(&page.frame(vec![]));
        assert!(first.iter().any(|t| t == KEEP_RECORDING) && first.iter().any(|t| t == OPEN_FOLDER), "{first:?}");
        assert!(!first.iter().any(|t| t.contains("saved recording")), "{first:?}");
        let texts = texts_of(&page.settled());
        let at = |text: &str| texts.iter().position(|t| t == text).unwrap_or_else(|| panic!("{text} is drawn: {texts:?}"));
        // Under the tour publishing heading, in the order of the approved row.
        assert!(at("TOUR PUBLISHING") < at(KEEP_RECORDING));
        assert!(at(KEEP_RECORDING) < at("2 saved recordings · 1.5 MB"));
        assert!(at("2 saved recordings · 1.5 MB") < at(OPEN_FOLDER));
        // Off: nothing is deleted, and the row still says what is saved.
        page.click(KEEP_RECORDING);
        assert!(!page.settings.keep_published_recordings);
        let texts = texts_of(&page.settled());
        assert!(texts.iter().any(|t| t == "2 saved recordings · 1.5 MB") && texts.iter().any(|t| t == OPEN_FOLDER), "{texts:?}");
        assert_eq!(Store::at(root).totals(), Totals { count: 2, bytes: 1_572_864 });
    }

    #[test]
    fn the_count_and_the_button_share_a_row_under_the_switch_at_the_default_window_size() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("projects");
        for i in 0..3 {
            project(&root, SITE, "tour_a", &format!("ch_{i}"), 1_048_576);
        }
        for width in [590.0, 470.0, 900.0] {
            let mut page = Page::new(Some(root.clone()));
            page.screen = vec2(width, 2000.0);
            let out = page.settled();
            let rect = |text: &str| rect_of(&out, text).unwrap_or_else(|| panic!("{text} is drawn at {width}: {:?}", texts_of(&out)));
            let (switch, line, button) = (rect(KEEP_RECORDING), rect("3 saved recordings · 3.0 MB"), rect(OPEN_FOLDER));
            // Row 1 the switch; row 2 the count and then the button, under the switch's label.
            assert!(line.top() > switch.bottom() && button.top() > switch.bottom(), "{width}: {switch:?} {line:?} {button:?}");
            assert!((line.center().y - button.center().y).abs() < 2.0, "{width}: one row: {line:?} {button:?}");
            assert!(button.left() > line.right(), "{width}: {line:?} {button:?}");
            assert!((line.left() - switch.left()).abs() < 1.0, "{width}: under the label: {switch:?} {line:?}");
            assert!(button.right() < width && switch.right() < width, "{width}: nothing is cut off: {switch:?} {button:?}");
        }
        // While the first count runs the button already stands under the label.
        let mut page = Page::new(Some(root));
        page.screen = vec2(590.0, 2000.0);
        let first = page.frame(vec![]);
        let (switch, button) = (rect_of(&first, KEEP_RECORDING).unwrap(), rect_of(&first, OPEN_FOLDER).unwrap());
        assert!(button.top() > switch.bottom() && button.left() > switch.left(), "{switch:?} {button:?}");
    }

    #[test]
    fn the_wording_is_the_planned_one() {
        assert_eq!(KEEP_RECORDING, "Keep the recording when I publish a tour, so I can edit it later");
        assert_eq!(OPEN_FOLDER, "Open folder");
        assert_eq!(RECORDINGS_STAY, "Saved recordings stay on this computer until you delete their tour or remove them from this folder.");
    }
}
