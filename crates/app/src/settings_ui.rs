//! The Preferences page of the main window: the General, Output and Hotkeys tabs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use chrono::{DateTime, TimeZone};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Stroke, vec2};

use mythicshot_destinations::{DEFAULT_PATTERN, DestId, ImageFormat, expand_pattern};

use crate::theme::{self, Strong as _};
use crate::hotkeys::{self, Action, EditorAction};
use crate::project::{self, ConvertState, FfmpegState, Form, ProjectInfo, Removed, Store, Totals, form_text, over_limit, over_limit_sentence, parse_limit, problem_text, remove_question, row_title, usage_fraction, usage_line};
use crate::publish_ui::size_text;
use crate::settings::{Settings, TrayLeftClick};

const BAR_HEIGHT: f32 = 8.0;
/// How long until the page looks again whether ffmpeg was found, while it is being looked for.
const FFMPEG_LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(200);
/// The room a row's title keeps before the row's details go under it.
const TITLE_MIN_WIDTH: f32 = 120.0;
/// How tall the list of saved recordings gets before it scrolls.
const LIST_MAX_HEIGHT: f32 = 180.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    General,
    Output,
    Hotkeys,
}

impl Tab {
    /// The page's sections, left to right: its tab row, and the Preferences menu of every window.
    pub const ALL: [Tab; 3] = [Tab::General, Tab::Output, Tab::Hotkeys];

    pub fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Output => "Output",
            Tab::Hotkeys => "Hotkeys",
        }
    }
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
    let (lister, remover) = (ui.ctx().clone(), ui.ctx().clone());
    let hooks = Hooks {
        open_folder: &mut |folder| crate::platform::open_folder(folder),
        ffmpeg: project::ffmpeg_state(),
        epoch: project::epoch(),
        list: &mut |root| list_in_background(root, lister.clone()),
        remove: &mut |root, dir| remove_in_background(root, dir, remover.clone()),
    };
    ui_with(ui, state, view, hooks)
}

/// What the page asks of the world outside it. A test hands in its own, so it opens no folder,
/// depends on no ffmpeg and decides itself what is listed and removed.
struct Hooks<'a> {
    /// Show a folder in the file manager.
    open_folder: &'a mut dyn FnMut(&Path),
    /// How the search for ffmpeg stands.
    ffmpeg: FfmpegState,
    /// [`project::epoch`]: it moves when a project is saved or removed, or a conversion starts or ends.
    epoch: u64,
    /// Read the saved recordings of a projects folder, off the UI thread.
    list: &'a mut dyn FnMut(&Path) -> Receiver<Listed>,
    /// Remove, from the store in the first folder, the project in the second, off the UI thread.
    remove: &'a mut dyn FnMut(&Path, &Path) -> Receiver<Removal>,
}

/// What a removal answers: what the store did, or its reason for not doing it.
type Removal = Result<Removed, String>;

/// [`ui`], with what it asks of the world handed in.
fn ui_with(ui: &mut egui::Ui, state: &mut SettingsState, view: View<'_>, mut hooks: Hooks<'_>) -> Vec<UiAction> {
    let mut out = Vec::new();
    ui.horizontal(|ui| {
        for tab in Tab::ALL {
            let selected = state.tab == tab;
            if ui.add(egui::Button::selectable(selected, tab.label()).min_size(vec2(96.0, 30.0))).clicked() {
                state.tab = tab;
                out.push(UiAction::StopListening);
            }
        }
    });
    ui.separator();
    ui.add_space(6.0);
    // The settings file could not be used as it was at start-up.
    if let Some(problem) = &view.settings.load_problem {
        theme::caution_box(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(problem.sentence()).color(theme::CAUTION_TEXT)).wrap());
        });
        ui.add_space(8.0);
    }
    // A limit that was typed and then left by the click on another tab: its row is not drawn
    // again to see the field lose its focus, so what stands in the field is saved here.
    if state.tab != Tab::Output && state.saved.save_typed_limit(view.settings) {
        out.push(UiAction::SettingsChanged);
    }
    match state.tab {
        Tab::General => general(ui, view, &mut out),
        Tab::Output => output_tab(ui, view, &mut state.saved, &mut hooks, &mut out),
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
    ui.label(RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION"))).size(12.0).color(theme::MUTED));
}

fn output_tab(ui: &mut egui::Ui, view: View<'_>, saved: &mut SavedRecordings, hooks: &mut Hooks<'_>, out: &mut Vec<UiAction>) {
    // The tab is opened when it was not drawn in the pass before this one.
    let pass = ui.ctx().cumulative_pass_nr();
    let opened = saved.drawn_in.is_none_or(|last| pass > last + 1);
    saved.drawn_in = Some(pass);
    // Everything the disk is asked happens here, before anything is drawn, and on other threads.
    saved.refresh(hooks.epoch, opened, hooks.list);
    saved.build(view.settings.store_recordings_as_video, hooks.ffmpeg, &chrono::Local::now());
    // The search for ffmpeg ends on another thread, which does not know this page: look again
    // shortly, so the line and the rows settle without the mouse being moved.
    if hooks.ffmpeg == FfmpegState::Checking && view.settings.store_recordings_as_video {
        ui.ctx().request_repaint_after(FFMPEG_LOOK_AGAIN);
    }
    egui::ScrollArea::vertical().show(ui, |ui| output_page(ui, view, saved, hooks, out));
}

fn output_page(ui: &mut egui::Ui, view: View<'_>, saved: &mut SavedRecordings, hooks: &mut Hooks<'_>, out: &mut Vec<UiAction>) {
    let s = view.settings;
    let mut changed = false;
    ui.heading("Output");
    ui.add_space(10.0);

    section_label(ui, "After capture");
    ui.label(RichText::new("Every ticked destination runs, in this order. \"Ask every time\" shows a menu instead.").color(theme::MUTED).size(12.0));
    ui.add_space(4.0);
    for d in DestId::ALL {
        let mut on = s.has_destination(*d);
        if ui.checkbox(&mut on, d.label()).on_hover_text(d.help()).changed() {
            s.toggle_destination(*d);
            changed = true;
        }
    }

    ui.add_space(14.0);
    section_label(ui, "Saving");
    ui.add_space(4.0);
    ui.label("Captures folder");
    ui.horizontal(|ui| {
        let mut dir = s.output_dir.display().to_string();
        if ui.add(egui::TextEdit::singleline(&mut dir).desired_width(ui.available_width() - 90.0)).changed() {
            s.output_dir = dir.into();
            changed = true;
        }
        if ui.button("Browse…").clicked() {
            if let Some(p) = crate::dialogs::pick_folder(rfd::FileDialog::new().set_directory(&s.output_dir), "captures folder") {
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
    ui.label(RichText::new(format!("Example: {example}.{}", s.image_format.extension())).color(theme::MUTED).size(12.0));
    ui.label(RichText::new("Tokens: ${YYYY} ${MM} ${DD} ${hh} ${mm} ${ss} ${title} ${user}. Use / for subfolders.").color(theme::MUTED).size(12.0));
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
    section_label(ui, "Recording");
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
    ui.label(RichText::new("GIFs are saved to the captures folder with the file name pattern above.").color(theme::MUTED).size(12.0));

    ui.add_space(14.0);
    section_label(ui, "Tour publishing");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(TOUR_SITE_MOVED);
        if ui.link(OPEN_WORKSPACE).clicked() {
            out.push(UiAction::OpenWorkspace);
        }
    });
    ui.add_space(8.0);
    // Turning the switch off deletes nothing: the section under it stays as it is.
    changed |= ui.checkbox(&mut s.keep_published_recordings, KEEP_RECORDING).changed();
    // The rest belongs to the switch: it stands under the switch's label.
    let mut under = ui.available_rect_before_wrap();
    under.min.x += ui.spacing().icon_width + ui.spacing().icon_spacing;
    ui.scope_builder(egui::UiBuilder::new().max_rect(under), |ui| changed |= saved_section(ui, s, saved, hooks));
    ui.add_space(8.0);
    if changed {
        out.push(UiAction::SettingsChanged);
    }
}

/// What the Output tab shows under the keep switch: the video switch and what it says about
/// ffmpeg, the limit, what the saved recordings take, and the list. True when a setting was
/// changed. Nothing here reads the disk, and no switch removes or converts anything.
fn saved_section(ui: &mut egui::Ui, s: &mut Settings, saved: &mut SavedRecordings, hooks: &mut Hooks<'_>) -> bool {
    let mut changed = ui.checkbox(&mut s.store_recordings_as_video, STORE_AS_VIDEO).changed();
    // The line promises compression, so it is there only while the switch is on.
    if s.store_recordings_as_video {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
            ui.add(egui::Label::new(RichText::new(ffmpeg_line(hooks.ffmpeg)).color(theme::MUTED).size(12.0)).wrap());
        });
    }
    ui.add_space(6.0);
    changed |= limit_row(ui, s, saved);
    ui.add_space(4.0);

    let limit = s.saved_recordings_warn_gb;
    if let Some(t) = saved.totals() {
        let over = over_limit(t.bytes, limit);
        if let Some(fraction) = usage_fraction(t.bytes, limit) {
            usage_bar(ui, fraction, over);
        }
        ui.add(egui::Label::new(RichText::new(usage_line(t.count, t.bytes, limit)).color(theme::MUTED)).extend()).on_hover_text(RECORDINGS_STAY);
        if over {
            theme::caution_box(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(over_limit_sentence(t.bytes, limit)).color(theme::CAUTION_TEXT)).wrap());
            });
        }
    }
    if let Some(root) = saved.root()
        && ui.button(OPEN_FOLDER).on_hover_text(RECORDINGS_STAY).clicked()
    {
        (hooks.open_folder)(root);
    }
    if let Some(note) = &saved.note {
        theme::caution_line(ui, note, theme::BODY_SIZE, theme::Fit::Wrap);
    }
    saved_list(ui, saved, hooks);
    changed
}

/// The limit: the words, a field of four characters, and the unit. True when a limit was
/// saved that is not the one saved before. Nothing is saved while it is typed: Enter or leaving
/// the field saves what stands there when that is a limit, and Escape or anything that is no
/// limit puts the saved one back.
///
/// The field can be left without this row being drawn to see it: by a click on another tab or
/// page, or by the window closing. That something was typed is remembered, and it is saved
/// ([`SavedRecordings::save_typed_limit`]) the next time the page finds the field without the
/// focus.
fn limit_row(ui: &mut egui::Ui, s: &mut Settings, saved: &mut SavedRecordings) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(WARN_WHEN);
        let id = ui.make_persistent_id("saved-recordings-limit");
        // Escape leaves the field too, and is the one way of leaving it that saves nothing.
        // The focus is gone before the row is drawn then, as it is after a Tab.
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            saved.limit_typed = false;
        }
        if !ui.memory(|m| m.has_focus(id)) {
            changed |= saved.save_typed_limit(s);
            saved.limit_text = limit_text(s.saved_recordings_warn_gb);
        }
        let field = egui::TextEdit::singleline(&mut saved.limit_text).id(id).char_limit(4).desired_width(40.0);
        let field = ui.add(field).on_hover_text(LIMIT_HOVER);
        saved.limit_typed |= field.changed();
        if field.lost_focus() {
            changed |= saved.save_typed_limit(s);
        }
        ui.label(WARN_UNIT);
    });
    changed
}

/// The usage bar: `fraction` of the width is filled, in the warning's colour past the limit.
fn usage_bar(ui: &mut egui::Ui, fraction: f32, over: bool) {
    let (track, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAR_HEIGHT), egui::Sense::hover());
    if !ui.is_rect_visible(track) {
        return;
    }
    let round = CornerRadius::same(4);
    ui.painter().rect_filled(track, round, theme::METER_TRACK);
    if fraction > 0.0 {
        let used = egui::Rect::from_min_size(track.min, vec2(track.width() * fraction.clamp(0.0, 1.0), track.height()));
        ui.painter().rect_filled(used, round, if over { theme::METER_OVER } else { theme::METER_USED });
    }
}

/// The list of saved recordings, in a frame that scrolls by itself past its height. A row's
/// button only asks; the question's own button is the one that has a recording removed.
fn saved_list(ui: &mut egui::Ui, saved: &mut SavedRecordings, hooks: &mut Hooks<'_>) {
    enum Click {
        Ask(PathBuf),
        Remove,
        Keep,
    }
    let mut click = None;
    {
        // A question that was just asked is brought into view, once; after that the list is the user's to scroll.
        let reveal = std::mem::take(&mut saved.reveal);
        let Some(listing) = saved.shown.as_ref().filter(|l| !l.rows.is_empty()) else { return };
        let (asking, busy) = (saved.asking.as_deref(), saved.removing.is_some());
        theme::card_frame().inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("saved-recordings").max_height(LIST_MAX_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
                for row in &listing.rows {
                    // A row and its question are widgets of their own, so the keyboard focus
                    // of the button that asked is never on the button that removes.
                    if asking == Some(row.dir.as_path()) {
                        match ui.push_id(("question", &row.dir), |ui| question(ui, row, reveal)).inner {
                            Some(true) => click = Some(Click::Remove),
                            Some(false) => click = Some(Click::Keep),
                            None => {}
                        }
                    } else if ui.push_id(("row", &row.dir), |ui| list_row(ui, row, !busy)).inner {
                        click = Some(Click::Ask(row.dir.clone()));
                    }
                }
            });
        });
    }
    match click {
        Some(Click::Ask(dir)) => saved.ask(&dir),
        Some(Click::Keep) => saved.keep(),
        Some(Click::Remove) => {
            // The store is this page's own; the folder is the one the question was about.
            if let Some(root) = saved.root().map(Path::to_path_buf) {
                saved.confirm(&mut |dir| (hooks.remove)(&root, dir));
            }
        }
        None => {}
    }
}

/// One row: the title, then the day, the form and the size, then the button. Where that
/// would leave the title too little room, the details go under the title instead. True when
/// the button was clicked.
fn list_row(ui: &mut egui::Ui, row: &Row, enabled: bool) -> bool {
    let detail = RichText::new(row.detail()).color(theme::MUTED).size(12.0);
    let button_width = text_width(ui, REMOVE_RECORDING, egui::TextStyle::Button.resolve(ui.style())) + 2.0 * ui.spacing().button_padding.x;
    let detail_width = text_width(ui, &row.detail(), egui::FontId::proportional(12.0));
    let one_line = ui.available_width() - button_width - detail_width - 2.0 * ui.spacing().item_spacing.x >= TITLE_MIN_WIDTH;
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            clicked = ui.add_enabled(enabled, egui::Button::new(REMOVE_RECORDING)).clicked();
            if one_line {
                ui.add(egui::Label::new(detail.clone()).extend());
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::Label::new(&row.title).truncate()).on_hover_text(&row.title);
            });
        });
    });
    if !one_line {
        ui.add(egui::Label::new(detail).truncate());
        ui.add_space(2.0);
    }
    clicked
}

/// How wide `text` is on one line in `font`.
fn text_width(ui: &egui::Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER).size().x
}

/// The question in the place of its row, scrolled into view when `reveal` says so.
/// Some(true) for "Remove recording", Some(false) for "Keep it" or Escape.
fn question(ui: &mut egui::Ui, row: &Row, reveal: bool) -> Option<bool> {
    let mut answer = None;
    let frame = theme::danger_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(remove_question(&row.title)).color(theme::DANGER_TEXT)).wrap());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.add(theme::danger_button(REMOVE_RECORDING)).clicked() {
                answer = Some(true);
            }
            if ui.button(KEEP_IT).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    if reveal {
        frame.response.scroll_to_me(None);
    }
    answer
}

/// What the Output tab says where the tour site settings used to be.
pub const TOUR_SITE_MOVED: &str = "Tour site settings are on the Workspace page.";
pub const OPEN_WORKSPACE: &str = "Open Workspace";

/// The switch of the Output tab: a tour's recording is kept on this computer when it is published.
pub const KEEP_RECORDING: &str = "Keep the recording when I publish a tour, so I can edit it later";
pub const OPEN_FOLDER: &str = "Open folder";
/// The hover text of the usage line and of "Open folder".
pub const RECORDINGS_STAY: &str = "Saved recordings stay on this computer until you remove them here or delete their tour.";

/// The switch under the keep switch: kept recordings are stored again as video, in the background.
pub const STORE_AS_VIDEO: &str = "Store kept recordings as video to save space";
/// The line under that switch, for each way the search for ffmpeg can stand.
pub const FFMPEG_FOUND: &str = "ffmpeg was found on this computer. Recordings are compressed in the background after a tour is published.";
pub const FFMPEG_NOT_FOUND: &str = "ffmpeg was not found on this computer, so recordings are stored without video compression. Install ffmpeg and start Mythicshot again.";
pub const FFMPEG_LOOKING: &str = "Looking for ffmpeg…";
/// The limit row: these words, the field, and the unit.
pub const WARN_WHEN: &str = "Warn me when saved recordings take more than";
pub const WARN_UNIT: &str = "GB";
pub const LIMIT_HOVER: &str = "Leave empty or enter 0 for no warning.";
/// The button of a row of the list, and the yes of its question.
pub const REMOVE_RECORDING: &str = "Remove recording";
/// The no of the question, in the Tours page's word.
pub const KEEP_IT: &str = "Keep it";
/// What is said after a removal that waits for an editor, and after one that failed.
pub const REMOVED_WHEN_CLOSED: &str = "This recording is open in an editor. It is removed when that editor is closed.";
pub const NOT_REMOVED: &str = "The recording could not be removed. Close anything that has it open and try again.";

fn ffmpeg_line(ffmpeg: FfmpegState) -> &'static str {
    match ffmpeg {
        FfmpegState::Checking => FFMPEG_LOOKING,
        FfmpegState::Found => FFMPEG_FOUND,
        FfmpegState::NotFound => FFMPEG_NOT_FOUND,
    }
}

/// What the limit field holds for a saved limit: nothing for "no warning".
fn limit_text(gb: u32) -> String {
    if gb == 0 { String::new() } else { gb.to_string() }
}

/// The saved recordings as one read of the disk found them: every project of the store, and the
/// folder of the one that was being stored as video at that moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listed {
    pub projects: Vec<ProjectInfo>,
    pub converting: Option<PathBuf>,
}

/// The saved recordings as the page shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Newest first.
    pub rows: Vec<Row>,
    pub totals: Totals,
}

/// One saved recording in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The project's folder, as the store's own list named it: what a removal is asked for.
    pub dir: PathBuf,
    pub title: String,
    /// The day it was saved, as the Tours page writes one. None when the project file names none.
    pub saved: Option<String>,
    /// How the recording is kept, or what is wrong with it.
    pub form: &'static str,
    pub size: String,
}

impl Row {
    /// The date, the form and the size, as the row shows them after the title.
    fn detail(&self) -> String {
        let saved = self.saved.iter().map(String::as_str);
        saved.chain([self.form, self.size.as_str()]).collect::<Vec<_>>().join(" · ")
    }
}

/// Whether the conversion queue would store this recording as video: the switch is on, ffmpeg
/// was found, and the recording is kept as frames, has nothing wrong with it, was not tried
/// before and its frames file is large enough (the folder's size, which the row shows, also
/// counts the project file and the pictures).
fn will_convert(p: &ProjectInfo, video_on: bool, ffmpeg: FfmpegState) -> bool {
    video_on && ffmpeg == FfmpegState::Found && p.form == Form::Frames && p.convert == ConvertState::NotTried && p.problem.is_none() && p.frames_bytes >= project::MIN_BYTES
}

/// The list and the totals for what was read. Reads nothing itself.
fn listing<Tz: TimeZone>(listed: &Listed, video_on: bool, ffmpeg: FfmpegState, now: &DateTime<Tz>) -> Listing
where
    Tz::Offset: std::fmt::Display,
{
    let mut projects: Vec<&ProjectInfo> = listed.projects.iter().collect();
    // Newest first; recordings saved in the same millisecond in the store's own order.
    projects.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then_with(|| a.dir.cmp(&b.dir)));
    // A tour has several saved recordings when several folders share its folder.
    let shared = |p: &ProjectInfo| listed.projects.iter().filter(|other| other.dir.parent() == p.dir.parent()).count() > 1;
    let rows = projects.iter().map(|p| {
        let converting = listed.converting.as_deref() == Some(p.dir.as_path());
        let form = match p.problem {
            Some(problem) => problem_text(problem),
            None => form_text(p.form, p.convert, converting, will_convert(p, video_on, ffmpeg)),
        };
        // A project file that cannot be read names no day.
        let saved = (p.saved_at > 0).then(|| crate::tours_ui::day_label(p.saved_at, now)).flatten();
        Row { dir: p.dir.clone(), title: row_title(p, shared(p)), saved, form, size: size_text(p.bytes) }
    });
    let rows = rows.collect();
    let totals = Totals { count: projects.len(), bytes: projects.iter().fold(0, |sum, p| sum.saturating_add(p.bytes)) };
    Listing { rows, totals }
}

/// The saved recordings as the Output tab shows them: read on a background thread, and kept
/// until a project is saved or removed or the tab is opened again. Drawing never reads the disk.
#[derive(Default)]
pub struct SavedRecordings {
    /// The projects folder, looked up when first needed; inside, None when this user has none.
    root: Option<Option<PathBuf>>,
    /// What the last read found. None until the first one is back.
    read: Option<Listed>,
    /// The list and the totals made from it.
    shown: Option<Listing>,
    /// The video switch and the state of ffmpeg `shown` was made for.
    built_for: Option<(bool, FfmpegState)>,
    /// [`crate::project::epoch`] when the newest read was started.
    listed_at: Option<u64>,
    /// The read that is running.
    listing: Option<Receiver<Listed>>,
    /// The pass the tab was last drawn in.
    drawn_in: Option<u64>,
    /// The text in the limit field while it is being typed in.
    limit_text: String,
    /// The limit field holds something that was typed and is neither saved nor given up yet.
    limit_typed: bool,
    /// The row whose question is showing: a folder of the list, never one from anywhere else.
    pub asking: Option<PathBuf>,
    /// The question was asked since the list was last drawn, and is to be scrolled into view.
    reveal: bool,
    /// The removal that is running.
    pub removing: Option<Receiver<Removal>>,
    /// What the last removal has to say, if anything.
    pub note: Option<String>,
}

impl SavedRecordings {
    /// For the projects folder `root`; None stands for a user whose folder cannot be found.
    #[cfg(test)]
    fn at(root: Option<PathBuf>) -> SavedRecordings {
        SavedRecordings { root: Some(root), ..Default::default() }
    }

    /// Save what was typed into the limit field and is not saved yet, when that is a limit;
    /// what is no limit keeps the saved one. True when a limit was saved that is not the one
    /// saved before. Asked when the field is found without the focus, or is not drawn at all.
    fn save_typed_limit(&mut self, s: &mut Settings) -> bool {
        if !std::mem::take(&mut self.limit_typed) {
            return false;
        }
        match parse_limit(&self.limit_text).filter(|gb| *gb != s.saved_recordings_warn_gb) {
            Some(gb) => {
                s.saved_recordings_warn_gb = gb;
                true
            }
            None => false,
        }
    }

    /// The folder "Open folder" shows: the store's own, never a path read from a project.
    fn root(&self) -> Option<&Path> {
        self.root.as_ref()?.as_deref()
    }

    /// How many recordings are saved and what they take. None while the first read is running;
    /// a user without a projects folder has no saved recordings.
    fn totals(&self) -> Option<Totals> {
        match self.root() {
            Some(_) => self.shown.as_ref().map(|l| l.totals),
            None => Some(Totals::default()),
        }
    }

    /// Take a finished removal and a finished read, and start a read with `list` when the tab
    /// was just `opened` or a project was saved or removed since the last one (`epoch` moved).
    /// One read runs at a time, and what was read before stays shown meanwhile.
    fn refresh(&mut self, epoch: u64, opened: bool, list: &mut dyn FnMut(&Path) -> Receiver<Listed>) {
        let root = self.root.get_or_insert_with(default_root).clone();
        if opened {
            // Coming back to the tab: a question and a note from the last visit are over.
            self.asking = None;
            self.note = None;
        }
        self.take_removal();
        if let Some(running) = &self.listing {
            match running.try_recv() {
                Ok(listed) => {
                    self.read = Some(listed);
                    self.built_for = None;
                }
                Err(TryRecvError::Empty) => return,
                // The read died: what is shown stays until the next reason to read.
                Err(TryRecvError::Disconnected) => {}
            }
            self.listing = None;
        }
        let Some(root) = root else { return };
        if opened || self.listed_at != Some(epoch) {
            self.listed_at = Some(epoch);
            self.listing = Some(list(&root));
        }
    }

    /// Take what a finished removal answered. Whatever it was, the list is read again.
    fn take_removal(&mut self) {
        let Some(running) = &self.removing else { return };
        let said = match running.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(said) => said,
            // The removal died without an answer: nothing is known to be gone.
            Err(TryRecvError::Disconnected) => Err(String::new()),
        };
        self.removing = None;
        self.listed_at = None;
        self.note = match said {
            // Gone, or gone already: the list says the rest.
            Ok(Removed::Now | Removed::NotThere) => None,
            Ok(Removed::WhenItsEditorCloses) => Some(REMOVED_WHEN_CLOSED.to_string()),
            // The store's reason is for its log; the page has its own sentence.
            Err(_) => Some(NOT_REMOVED.to_string()),
        };
    }

    /// Make the list from what was read, when that is new or the video switch or the state of
    /// ffmpeg changed since it was made. Reads nothing.
    fn build<Tz: TimeZone>(&mut self, video_on: bool, ffmpeg: FfmpegState, now: &DateTime<Tz>)
    where
        Tz::Offset: std::fmt::Display,
    {
        let Some(read) = &self.read else { return };
        if self.built_for == Some((video_on, ffmpeg)) {
            return;
        }
        let made = listing(read, video_on, ffmpeg, now);
        // A question about a recording that is no longer listed is over.
        if self.asking.as_ref().is_some_and(|dir| !made.rows.iter().any(|r| &r.dir == dir)) {
            self.asking = None;
        }
        self.shown = Some(made);
        self.built_for = Some((video_on, ffmpeg));
    }

    /// "Remove recording" on the row of `dir`: its question shows. Nothing is removed yet.
    fn ask(&mut self, dir: &Path) {
        if self.removing.is_none() && self.lists(dir) {
            self.asking = Some(dir.to_path_buf());
            self.reveal = true;
            self.note = None;
        }
    }

    /// "Keep it": the question goes, and nothing else happens.
    fn keep(&mut self) {
        self.asking = None;
    }

    /// The question was answered with "Remove recording": `remove` is asked for the folder the
    /// question was about, and only when the list as it is shown still holds that folder.
    fn confirm(&mut self, remove: &mut dyn FnMut(&Path) -> Receiver<Removal>) {
        let Some(dir) = self.asking.take() else { return };
        if self.removing.is_none() && self.lists(&dir) {
            self.note = None;
            self.removing = Some(remove(&dir));
        }
    }

    /// Whether the list as it is shown holds the project in `dir`.
    fn lists(&self, dir: &Path) -> bool {
        self.shown.as_ref().is_some_and(|l| l.rows.iter().any(|r| r.dir == dir))
    }
}

/// This user's projects folder. A test never looks into the real one.
fn default_root() -> Option<PathBuf> {
    if cfg!(test) { None } else { Store::default_root() }
}

/// Read the projects in `root` on a thread of its own, once, and draw again when it is done.
/// Reading creates nothing.
fn list_in_background(root: &Path, ctx: egui::Context) -> Receiver<Listed> {
    let (tx, rx) = std::sync::mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        // Which project is being stored as video is asked with the list, never while drawing.
        let listed = Listed { projects: Store::at(root).list(), converting: project::converting_now() };
        let _ = tx.send(listed);
        ctx.request_repaint();
    });
    rx
}

/// Remove the project in `dir` from the store in `root` on a thread of its own, and draw again
/// when it is done. The store checks the folder once more itself.
fn remove_in_background(root: &Path, dir: &Path, ctx: egui::Context) -> Receiver<Removal> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (root, dir) = (root.to_path_buf(), dir.to_path_buf());
    std::thread::spawn(move || {
        let _ = tx.send(Store::at(root).remove_project(&dir));
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
        theme::caution_box(ui, |ui| {
            ui.set_width(ui.available_width());
            if snipping {
                ui.label(RichText::new("Windows is using the Print Screen key for Snipping Tool").color(theme::CAUTION_TEXT).bold());
                ui.label(RichText::new("Turn off Settings › Accessibility › Keyboard › \"Use the Print screen key to open screen capture\", then restart Mythicshot, or pick other keys below.").color(theme::CAUTION_TEXT));
            } else {
                ui.label(RichText::new("Another app is holding the Print Screen shortcuts").color(theme::CAUTION_TEXT).bold());
                ui.label(RichText::new("Another screenshot tool is probably running. Close it or turn off its shortcuts, then restart Mythicshot, or pick other keys below.").color(theme::CAUTION_TEXT));
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
        ui.label(RichText::new("These work inside the editor window only.").color(theme::MUTED).size(12.0));
        ui.add_space(4.0);
        for &action in EditorAction::ALL {
            row(ui, state, &view, Bind::Editor(action), view.settings.editor_binding(action), out);
        }
    });
}

fn group_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    section_label(ui, title);
    ui.add_space(4.0);
}

/// The label over a section of the page, in sentence case.
fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(theme::SMALL_SIZE).color(theme::MUTED).bold());
}

fn row(ui: &mut egui::Ui, state: &mut SettingsState, view: &View<'_>, action: Bind, binding: &str, out: &mut Vec<UiAction>) {
    let listening = state.listening == Some(action);
    let error = view.errors.get(&action);
    let fill = if listening {
        theme::SELECTION
    } else if error.is_some() {
        theme::CAUTION_TINT
    } else {
        theme::SURFACE
    };
    egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(theme::RADIUS_MD)).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
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
                    Stroke::new(1.0, theme::ACCENT)
                } else if error.is_some() {
                    Stroke::new(1.0, theme::CAUTION_BORDER)
                } else {
                    Stroke::new(1.0, theme::BORDER_STRONG)
                };
                let button = egui::Button::new(RichText::new(text).monospace()).stroke(stroke).min_size(vec2(170.0, 32.0));
                if ui.add(button).on_hover_text("Click, then press the new shortcut").clicked() {
                    out.push(if listening { UiAction::StopListening } else { UiAction::Listen(action) });
                }
                if listening {
                    ui.label(RichText::new("Esc cancels · Backspace clears").color(theme::MUTED).size(12.0));
                } else if let Some(e) = error {
                    theme::caution_line(ui, e, theme::SMALL_SIZE, theme::Fit::Truncate).on_hover_text(e);
                }
            });
        });
    });
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Problem, ProjectKey};
    use std::sync::mpsc::channel;

    const SCREEN: egui::Vec2 = vec2(600.0, 2000.0);
    /// The folder of a site, as the store names one.
    const SITE: &str = "0123456789abcdef";
    const OTHER_SITE: &str = "fedcba9876543210";
    const SERVER: &str = "https://tours.example.com";
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    const FFMPEG_LINES: [&str; 3] = [FFMPEG_FOUND, FFMPEG_NOT_FOUND, FFMPEG_LOOKING];

    /// A folder the store lists as a project taking `bytes` on the disk. The store counts a
    /// project it cannot open like any other, so the file need not be a real one.
    fn project(root: &Path, site: &str, tour: &str, chapter: &str, bytes: usize) {
        let dir = root.join(site).join(tour).join(chapter);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("project.json"), vec![b' '; bytes]).unwrap();
    }

    /// Save a small real recording as the chapter `chapter` of `tour`, as a publish does.
    fn save_project(store: &Store, tour: &str, chapter: &str, tour_title: &str, title: &str) -> ProjectInfo {
        let recordings = store.root().parent().unwrap().join("recordings");
        std::fs::create_dir_all(&recordings).unwrap();
        let dir = tempfile::tempdir_in(&recordings).unwrap().keep();
        let rec = mythicshot_recording::Recording::synthetic_in(&dir, 64, 48, &[0, 500, 1000], 1500).unwrap();
        let edit = crate::video::Edit::new(&rec, Default::default());
        let snap = project::Snapshot { profile: "", rec: &rec, title, tour_title, steps: 2, edit: &edit, events: &[], items: &[] };
        store.save(&key(tour, chapter), &snap).unwrap().info
    }

    fn key(tour: &str, chapter: &str) -> ProjectKey {
        ProjectKey { server: SERVER.into(), tour_id: tour.into(), chapter_id: chapter.into() }
    }

    /// What the store's list says of a saved recording, with no disk behind it.
    fn info(site: &str, tour: &str, chapter: &str, tour_title: &str, title: &str, saved_at: i64, bytes: u64) -> ProjectInfo {
        let dir = Path::new("projects").join(site).join(tour).join(chapter);
        ProjectInfo { profile: String::new(), key: key(tour, chapter), title: title.into(), tour_title: tour_title.into(), saved_at, bytes, frames_bytes: bytes, dir, problem: None, form: Form::Frames, convert: ConvertState::NotTried }
    }

    fn listed(projects: Vec<ProjectInfo>) -> Listed {
        Listed { projects, converting: None }
    }

    fn at(day: &str) -> DateTime<chrono::FixedOffset> {
        DateTime::parse_from_rfc3339(day).unwrap()
    }

    fn ms(day: &str) -> i64 {
        at(day).timestamp_millis()
    }

    /// The day the page writes for a time, as the Tours page writes it today.
    fn day(ms: i64) -> String {
        crate::tours_ui::day_label(ms, &chrono::Local::now()).unwrap()
    }

    /// The mouse wheel turned a good way down.
    fn wheel_down() -> egui::Event {
        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -400.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel }
    }

    /// An answer that is there at once.
    fn ready<T>(value: T) -> Receiver<T> {
        let (tx, rx) = channel();
        tx.send(value).unwrap();
        rx
    }

    /// A read of the store in `root` that is back at once, and how many were started.
    fn read(root: &Path, started: &mut usize) -> Receiver<Listed> {
        *started += 1;
        ready(listed(Store::at(root.to_path_buf()).list()))
    }

    /// The usage line of a state as the page would show it, with no limit set.
    fn line(saved: &mut SavedRecordings) -> Option<String> {
        saved.build(true, FfmpegState::NotFound, &chrono::Utc::now());
        saved.totals().map(|t| usage_line(t.count, t.bytes, 0))
    }

    fn shapes_of(out: &egui::FullOutput) -> Vec<&egui::Shape> {
        fn collect<'a>(shape: &'a egui::Shape, into: &mut Vec<&'a egui::Shape>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                other => into.push(other),
            }
        }
        let mut shapes = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut shapes));
        shapes
    }

    /// Every text that was drawn, and where.
    fn drawn(out: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        let text = |s: &&egui::Shape| match s {
            // Where the letters are: a text set from the right starts left of its position.
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
            _ => None,
        };
        shapes_of(out).iter().filter_map(text).collect()
    }

    fn texts_of(out: &egui::FullOutput) -> Vec<String> {
        drawn(out).into_iter().map(|(text, _)| text).collect()
    }

    /// Everywhere `text` was drawn, from the top.
    fn rects_of(out: &egui::FullOutput, text: &str) -> Vec<egui::Rect> {
        let mut rects: Vec<egui::Rect> = drawn(out).into_iter().filter(|(t, _)| t == text).map(|(_, r)| r).collect();
        rects.sort_by(|a, b| a.top().total_cmp(&b.top()));
        rects
    }

    /// Where `text` was drawn.
    fn rect_of(out: &egui::FullOutput, text: &str) -> Option<egui::Rect> {
        rects_of(out, text).first().copied()
    }

    /// The usage bar as it was drawn: its track, and what is filled with which colour.
    fn bar_of(out: &egui::FullOutput) -> Option<(egui::Rect, egui::Rect, Color32)> {
        let rect = |fills: &[Color32]| {
            shapes_of(out).iter().find_map(|s| match s {
                egui::Shape::Rect(r) if fills.contains(&r.fill) => Some((r.rect, r.fill)),
                _ => None,
            })
        };
        let (track, _) = rect(&[theme::METER_TRACK])?;
        let (used, fill) = rect(&[theme::METER_USED, theme::METER_OVER]).unwrap_or((egui::Rect::from_min_size(track.min, vec2(0.0, track.height())), theme::METER_USED));
        Some((track, used, fill))
    }

    /// The Output tab drawn without a window: its settings, its state, and what it asked of
    /// the app and of the world.
    struct Page {
        ctx: egui::Context,
        settings: Settings,
        state: SettingsState,
        actions: Vec<UiAction>,
        opened: Vec<PathBuf>,
        screen: egui::Vec2,
        ffmpeg: FfmpegState,
        /// The count of saves and removals the page is told: its own, so that the tests running beside it do not move it.
        epoch: u64,
        /// What a read answers instead of the store's own list.
        fake: Option<Listed>,
        /// How many reads were started.
        reads: usize,
        /// Every removal that was asked for: the store's folder and the project's.
        removals: Vec<(PathBuf, PathBuf)>,
        /// What a removal answers instead of the store's own.
        answer: Option<Removal>,
    }

    impl Page {
        fn new(root: Option<PathBuf>) -> Page {
            let state = SettingsState { tab: Tab::Output, listening: None, saved: SavedRecordings::at(root) };
            Page {
                ctx: crate::theme::test_context(),
                settings: Settings::default(),
                state,
                actions: Vec::new(),
                opened: Vec::new(),
                screen: SCREEN,
                ffmpeg: FfmpegState::NotFound,
                epoch: 0,
                fake: None,
                reads: 0,
                removals: Vec::new(),
                answer: None,
            }
        }

        /// A page whose store holds what `listed` says, with no disk behind it.
        fn showing(listed: Listed) -> Page {
            let mut page = Page::new(Some(PathBuf::from("projects")));
            page.fake = Some(listed);
            page
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen)), events, ..Default::default() };
            let errors = HashMap::new();
            let (settings, state, actions, opened) = (&mut self.settings, &mut self.state, &mut self.actions, &mut self.opened);
            let (fake, reads, removals, answer, ffmpeg, epoch) = (&self.fake, &mut self.reads, &mut self.removals, &self.answer, self.ffmpeg, self.epoch);
            let ctx = self.ctx.clone();
            let mut out = self.ctx.run_ui(input, |ui| {
                let view = View { settings: &mut *settings, errors: &errors, snipping_tool: None, autostart: false };
                let hooks = Hooks {
                    open_folder: &mut |folder| opened.push(folder.to_path_buf()),
                    ffmpeg,
                    epoch,
                    list: &mut |root| {
                        *reads += 1;
                        match fake {
                            Some(listed) => ready(listed.clone()),
                            None => list_in_background(root, ctx.clone()),
                        }
                    },
                    remove: &mut |root, dir| {
                        removals.push((root.to_path_buf(), dir.to_path_buf()));
                        match answer {
                            Some(answer) => ready(answer.clone()),
                            None => remove_in_background(root, dir, ctx.clone()),
                        }
                    },
                };
                actions.extend(ui_with(ui, &mut *state, view, hooks));
            });
            out.textures_delta.clear();
            out
        }

        /// Draw until every read and removal is back and shown.
        fn settled(&mut self) -> egui::FullOutput {
            for _ in 0..500 {
                self.frame(vec![]);
                if self.state.saved.listing.is_none() && self.state.saved.removing.is_none() {
                    // A scroll area learns its size from the frame before.
                    self.frame(vec![]);
                    self.frame(vec![]);
                    return self.frame(vec![]);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("the list never came back");
        }

        fn click_at(&mut self, at: egui::Pos2) {
            let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            for events in [vec![egui::Event::PointerMoved(at)], vec![press(true)], vec![press(false)], vec![]] {
                self.frame(events);
            }
        }

        /// Click the `nth` place `text` is drawn, counted from the top.
        fn click_nth(&mut self, text: &str, nth: usize) {
            let out = self.settled();
            let at = rects_of(&out, text).get(nth).unwrap_or_else(|| panic!("{text} is drawn {} times: {:?}", nth + 1, texts_of(&out))).center();
            self.click_at(at);
        }

        fn click(&mut self, text: &str) {
            self.click_nth(text, 0);
        }

        /// Replace what the limit field holds by typing `text` into it.
        fn type_limit(&mut self, text: &str) {
            let out = self.settled();
            // The unit stands right of the field, also when the field itself holds "GB".
            let unit = rects_of(&out, WARN_UNIT).into_iter().max_by(|a, b| a.left().total_cmp(&b.left())).unwrap_or_else(|| panic!("the limit row is drawn: {:?}", texts_of(&out)));
            self.click_at(egui::pos2(unit.left() - 24.0, unit.center().y));
            let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
            self.frame(vec![key(egui::Key::A, egui::Modifiers::COMMAND)]);
            self.frame(vec![if text.is_empty() { key(egui::Key::Backspace, egui::Modifiers::NONE) } else { egui::Event::Text(text.to_string()) }]);
            self.frame(vec![]);
        }

        /// Press and let go of a key.
        fn key(&mut self, key: egui::Key) -> egui::FullOutput {
            let event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
            self.frame(vec![event(true)]);
            self.frame(vec![event(false)]);
            self.frame(vec![])
        }

        /// Type `text` into the limit field and press Enter.
        fn set_limit(&mut self, text: &str) {
            self.type_limit(text);
            self.key(egui::Key::Enter);
        }

        /// Move the keyboard focus with Tab until it is on the widget at `at`.
        fn focus_at(&mut self, at: egui::Pos2) {
            for _ in 0..400 {
                self.key(egui::Key::Tab);
                let focused = self.ctx.memory(|m| m.focused()).and_then(|id| self.ctx.read_response(id));
                if focused.is_some_and(|r| r.rect.contains(at)) {
                    return;
                }
            }
            panic!("the keyboard never reached {at:?}");
        }

        fn changes(&self) -> usize {
            self.actions.iter().filter(|a| matches!(a, UiAction::SettingsChanged)).count()
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
    fn the_video_switch_is_on_by_default_and_turning_it_off_is_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("projects"));
        let saved = save_project(&store, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding");
        let mut page = Page::new(Some(store.root().to_path_buf()));
        assert!(page.settings.store_recordings_as_video);
        let texts = texts_of(&page.settled());
        assert!(texts.iter().any(|t| t == STORE_AS_VIDEO), "{texts:?}");
        assert!(page.actions.is_empty(), "drawing changes nothing");
        // Off: the app is told, and it saves the settings. The keep switch is not touched.
        page.click(STORE_AS_VIDEO);
        assert!(!page.settings.store_recordings_as_video && page.settings.keep_published_recordings);
        assert!(matches!(page.actions[..], [UiAction::SettingsChanged]));
        // On again, the same way.
        page.click(STORE_AS_VIDEO);
        assert!(page.settings.store_recordings_as_video);
        assert_eq!((page.changes(), page.actions.len()), (2, 2));
        // Both switches off: nothing is removed and nothing is asked of the store but its list.
        page.click(STORE_AS_VIDEO);
        page.click(KEEP_RECORDING);
        page.settled();
        assert!(!page.settings.store_recordings_as_video && !page.settings.keep_published_recordings);
        assert!(page.removals.is_empty() && page.opened.is_empty());
        assert_eq!(store.list(), [saved]);
    }

    #[test]
    fn the_line_under_it_says_whether_ffmpeg_was_found_or_is_still_being_looked_for() {
        assert_eq!(ffmpeg_line(FfmpegState::Found), FFMPEG_FOUND);
        assert_eq!(ffmpeg_line(FfmpegState::NotFound), FFMPEG_NOT_FOUND);
        assert_eq!(ffmpeg_line(FfmpegState::Checking), FFMPEG_LOOKING);
        for (state, says) in [(FfmpegState::Found, FFMPEG_FOUND), (FfmpegState::NotFound, FFMPEG_NOT_FOUND), (FfmpegState::Checking, FFMPEG_LOOKING)] {
            let mut page = Page::showing(listed(vec![]));
            page.ffmpeg = state;
            let out = page.settled();
            let texts = texts_of(&out);
            // Exactly the one line, under its switch and above the limit row.
            assert_eq!(texts.iter().filter(|t| FFMPEG_LINES.contains(&t.as_str())).collect::<Vec<_>>(), [says], "{state:?}");
            let (switch, line, limit) = (rect_of(&out, STORE_AS_VIDEO).unwrap(), rect_of(&out, says).unwrap(), rect_of(&out, WARN_WHEN).unwrap());
            assert!(switch.bottom() <= line.top() && line.bottom() <= limit.top(), "{state:?}: {switch:?} {line:?} {limit:?}");
            assert!(line.right() <= page.screen.x, "{state:?}: the line wraps inside the page: {line:?}");
            // The search goes on: the next frame says what it found, with nothing read again for it.
            let reads = page.reads;
            page.ffmpeg = FfmpegState::Found;
            assert!(texts_of(&page.frame(vec![])).iter().any(|t| t == FFMPEG_FOUND));
            assert_eq!(page.reads, reads);
            // With the switch off nothing is compressed, so the line that says so is not there.
            page.click(STORE_AS_VIDEO);
            let texts = texts_of(&page.settled());
            assert!(!texts.iter().any(|t| FFMPEG_LINES.contains(&t.as_str())), "{state:?}: {texts:?}");
        }
    }

    #[test]
    fn typing_a_limit_saves_it_and_typing_letters_keeps_the_last_one() {
        let mut page = Page::showing(listed(vec![]));
        let out = page.settled();
        assert_eq!(page.settings.saved_recordings_warn_gb, 5);
        // The row: the words, the field with the saved limit, the unit.
        let (words, field, unit) = (rect_of(&out, WARN_WHEN).unwrap(), rect_of(&out, "5").unwrap(), rect_of(&out, WARN_UNIT).unwrap());
        assert!(words.right() < field.left() && field.right() < unit.left(), "{words:?} {field:?} {unit:?}");
        assert!(page.actions.is_empty());
        // Nothing is saved while it is typed: on the way from 5 to 10 the limit is never "none" and never 1.
        page.type_limit("");
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (5, 0, ""));
        page.frame(vec![egui::Event::Text("1".into())]);
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (5, 0, "1"));
        page.frame(vec![egui::Event::Text("0".into())]);
        for _ in 0..5 {
            page.frame(vec![]);
        }
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (5, 0, "10"));
        // Enter saves it, once.
        page.key(egui::Key::Enter);
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes()), (10, 1));
        page.settled();
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (10, 1, "10"));
        // Leaving the field saves it too.
        page.type_limit("12");
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes()), (10, 1));
        page.click(WARN_WHEN);
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes()), (12, 2));
        // Letters, a decimal and a minus sign are not a limit: the last one stays, nothing is
        // saved, and the saved limit stands in the field again.
        for bad in ["ab", "5.5", "-1", "GB", "1e3"] {
            page.type_limit(bad);
            assert_eq!(page.state.saved.limit_text, bad, "what was typed stays in the field while it is typed in");
            page.key(egui::Key::Enter);
            let texts = texts_of(&page.settled());
            assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (12, 2, "12"), "{bad}");
            assert!(texts.iter().any(|t| t == "12"), "{bad}: {texts:?}");
        }
        // Escape puts the saved limit back and saves nothing, also when a number was typed.
        page.type_limit("3");
        page.key(egui::Key::Escape);
        page.settled();
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (12, 2, "12"));
        // The field holds four characters: what is typed past them never arrives.
        page.set_limit("98765");
        page.settled();
        assert_eq!((page.settings.saved_recordings_warn_gb, page.state.saved.limit_text.as_str()), (9876, "9876"));
        // Empty and 0 are both "no warning", and the field shows it empty.
        page.set_limit("");
        assert_eq!(page.settings.saved_recordings_warn_gb, 0);
        page.set_limit("7");
        page.set_limit("0");
        page.settled();
        assert_eq!((page.settings.saved_recordings_warn_gb, page.state.saved.limit_text.as_str()), (0, ""));
        // The same number again saves nothing.
        let changes = page.changes();
        page.set_limit("0");
        page.set_limit("");
        assert_eq!(page.changes(), changes);
        assert_eq!(page.actions.len(), page.changes(), "the field asks for nothing but a save");
        assert!(page.removals.is_empty());
        assert_eq!((limit_text(0), limit_text(5), limit_text(9999)), (String::new(), "5".to_string(), "9999".to_string()));
    }

    #[test]
    fn a_limit_typed_and_left_by_clicking_another_tab_is_saved() {
        let mut page = Page::showing(listed(vec![]));
        page.settled();
        page.type_limit("12");
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes()), (5, 0));
        // The click that leaves the field shows another tab, so the field is not drawn again.
        page.click("General");
        assert_eq!(page.state.tab, Tab::General);
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes()), (12, 1));
        // Once, and the field holds it when the tab is shown again.
        page.settled();
        page.click("Output");
        let texts = texts_of(&page.settled());
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (12, 1, "12"));
        assert!(texts.iter().any(|t| t == "12"), "{texts:?}");
    }

    #[test]
    fn letters_typed_and_left_by_clicking_another_tab_keep_the_saved_limit() {
        let mut page = Page::showing(listed(vec![]));
        page.settled();
        for bad in ["ab", "5.5", "-1"] {
            page.type_limit(bad);
            page.click("General");
            page.settled();
            assert_eq!((page.settings.saved_recordings_warn_gb, page.changes()), (5, 0), "{bad}");
            page.click("Output");
            page.settled();
            assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (5, 0, "5"), "{bad}");
        }
    }

    #[test]
    fn escape_and_then_another_tab_saves_nothing() {
        let mut page = Page::showing(listed(vec![]));
        page.settled();
        page.type_limit("12");
        page.key(egui::Key::Escape);
        page.click("General");
        page.settled();
        page.click("Output");
        page.settled();
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (5, 0, "5"));
    }

    #[test]
    fn a_limit_typed_before_the_page_went_away_is_saved_when_the_page_is_back() {
        let mut page = Page::showing(listed(vec![]));
        page.settled();
        page.type_limit("12");
        // Another page of the window is shown, or the window is hidden: nothing of this page is drawn.
        for _ in 0..3 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, page.screen)), ..Default::default() };
            let _ = page.ctx.run_ui(input, |_| {});
        }
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes()), (5, 0));
        page.settled();
        assert_eq!((page.settings.saved_recordings_warn_gb, page.changes(), page.state.saved.limit_text.as_str()), (12, 1, "12"));
    }

    #[test]
    fn an_empty_limit_shows_no_bar_and_no_warning() {
        let mut page = Page::showing(listed(vec![info(SITE, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding", 1, 6 * GB)]));
        page.settings.saved_recordings_warn_gb = 0;
        let out = page.settled();
        let texts = texts_of(&out);
        assert!(texts.iter().any(|t| t == "1 saved recording · 6.0 GB"), "{texts:?}");
        assert!(bar_of(&out).is_none(), "no limit, no bar");
        assert!(!texts.iter().any(|t| t.starts_with("Saved recordings take")), "{texts:?}");
        // The field is empty, and the row around it is still there.
        assert!(page.state.saved.limit_text.is_empty() && texts.iter().any(|t| t == WARN_WHEN) && texts.iter().any(|t| t == WARN_UNIT), "{texts:?}");
        // A limit is typed: the bar and the warning come without anything being read again.
        let reads = page.reads;
        page.set_limit("5");
        let out = page.settled();
        let texts = texts_of(&out);
        assert!(bar_of(&out).is_some() && texts.iter().any(|t| t == &over_limit_sentence(6 * GB, 5)), "{texts:?}");
        assert!(texts.iter().any(|t| t == "1 saved recording · 6.0 GB of 5 GB"), "{texts:?}");
        assert_eq!(page.reads, reads);
    }

    #[test]
    fn the_bar_is_full_and_the_warning_shows_past_the_limit() {
        // Under the limit: a quarter of the bar, and no warning.
        let mut page = Page::showing(listed(vec![info(SITE, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding", 1, GB)]));
        page.settings.saved_recordings_warn_gb = 4;
        let out = page.settled();
        let texts = texts_of(&out);
        let (track, used, fill) = bar_of(&out).expect("a limit, a bar");
        assert!((used.width() / track.width() - 0.25).abs() < 0.01 && fill == theme::METER_USED, "{track:?} {used:?}");
        assert!((used.left() - track.left()).abs() < 0.5 && track.width() > 200.0 && track.right() <= page.screen.x, "{track:?} {used:?}");
        assert!(texts.iter().any(|t| t == "1 saved recording · 1.0 GB of 4 GB") && !texts.iter().any(|t| t.starts_with("Saved recordings take")), "{texts:?}");
        // Exactly the limit: full, and still no warning.
        page.settings.saved_recordings_warn_gb = 1;
        let out = page.settled();
        let (track, used, fill) = bar_of(&out).unwrap();
        assert!((used.width() - track.width()).abs() < 0.5 && fill == theme::METER_USED, "{track:?} {used:?}");
        assert!(!texts_of(&out).iter().any(|t| t.starts_with("Saved recordings take")));
        // Past it: full and no fuller, in the warning's colour, with the planned sentence under the line.
        let projects = vec![info(SITE, "tour_a", "chap_a", "Customer onboarding", "Invite your team", 2, 5 * GB), info(SITE, "tour_a", "chap_b", "Customer onboarding", "Add a domain", 1, 429_496_730)];
        let mut page = Page::showing(listed(projects));
        let out = page.settled();
        let (track, used, fill) = bar_of(&out).unwrap();
        assert!((used.width() - track.width()).abs() < 0.5 && fill == theme::METER_OVER, "{track:?} {used:?}");
        let sentence = "Saved recordings take 5.4 GB, more than the 5 GB you set. Nothing is removed automatically: remove recordings you no longer need in Preferences.";
        let (line, warning, button) = (rect_of(&out, "2 saved recordings · 5.4 GB of 5 GB").unwrap(), rect_of(&out, sentence).unwrap_or_else(|| panic!("{:?}", texts_of(&out))), rect_of(&out, OPEN_FOLDER).unwrap());
        assert!(track.bottom() <= line.top() && line.bottom() <= warning.top() && warning.bottom() <= button.top(), "{track:?} {line:?} {warning:?} {button:?}");
        assert!(warning.right() <= page.screen.x, "the sentence wraps inside the page: {warning:?}");
        // The warning removes nothing and asks for nothing.
        assert!(page.removals.is_empty() && page.actions.is_empty() && page.state.saved.asking.is_none());
    }

    #[test]
    fn the_list_shows_title_chapter_date_form_and_size_for_every_address() {
        let now = at("2026-11-18T12:00:00+00:00");
        let nov_18 = ms("2026-11-18T09:30:00+00:00");
        let mut video = info(SITE, "tour_a", "chap_a", "Customer onboarding", "Invite your team", nov_18, 33_764_147);
        (video.form, video.convert) = (Form::Video, ConvertState::Done);
        let large = info(SITE, "tour_a", "chap_b", "Customer onboarding", "Add a domain", ms("2026-11-02T08:00:00+00:00"), 172 * MB);
        let small = info(SITE, "tour_b", "chap_a", "Billing", "Billing", ms("2026-03-09T08:00:00+00:00"), 3 * MB);
        // Another site: its own address, and its own tour that happens to have the same id.
        let mut other = info(OTHER_SITE, "tour_a", "chap_a", "Release notes", "Release notes", ms("2025-12-31T12:00:00+00:00"), 12 * MB);
        other.key.server = "https://other.example.com".into();
        let mut failed = info(OTHER_SITE, "tour_c", "chap_a", "Kept as frames", "Kept as frames", nov_18 - 1, 200 * MB);
        failed.convert = ConvertState::NotSmaller;
        let listed = Listed { projects: vec![small.clone(), failed.clone(), other.clone(), large.clone(), video.clone()], converting: None };
        let row = |p: &ProjectInfo, title: &str, saved: &str, form: &'static str, size: &str| Row { dir: p.dir.clone(), title: title.into(), saved: Some(saved.into()), form, size: size.into() };

        // ffmpeg found and the switch on: newest first, every address, each with its five parts.
        let made = listing(&listed, true, FfmpegState::Found, &now);
        let rows = [
            row(&video, "Customer onboarding · Invite your team", "18 Nov", "video", "32.2 MB"),
            row(&failed, "Kept as frames", "18 Nov", "not compressed", "200 MB"),
            row(&large, "Customer onboarding · Add a domain", "2 Nov", "not compressed yet", "172 MB"),
            row(&small, "Billing", "9 Mar", "not compressed", "3.0 MB"),
            row(&other, "Release notes", "31 Dec 2025", "not compressed yet", "12.0 MB"),
        ];
        assert_eq!(made.rows, rows);
        assert_eq!(made.totals, Totals { count: 5, bytes: 33_764_147 + (172 + 3 + 12 + 200) * MB });
        assert_eq!(made.rows[0].detail(), "18 Nov · video · 32.2 MB");

        // "yet" is said only of a recording the queue will take: not without ffmpeg, not while
        // it is still looked for, and not with the switch off. A video stays a video.
        for (video_on, ffmpeg) in [(true, FfmpegState::NotFound), (true, FfmpegState::Checking), (false, FfmpegState::Found), (false, FfmpegState::NotFound)] {
            let forms: Vec<&str> = listing(&listed, video_on, ffmpeg, &now).rows.iter().map(|r| r.form).collect();
            assert_eq!(forms, ["video", "not compressed", "not compressed", "not compressed", "not compressed"], "{video_on} {ffmpeg:?}");
        }
        // Exactly the smallest size the queue takes, and one byte less.
        // The queue measures the frames file, not the folder with its project file and pictures.
        let edge = |bytes, frames_bytes| listing(&Listed { projects: vec![ProjectInfo { frames_bytes, ..info(SITE, "tour_a", "chap_a", "A", "A", nov_18, bytes) }], converting: None }, true, FfmpegState::Found, &now).rows[0].form;
        assert_eq!((edge(project::MIN_BYTES, project::MIN_BYTES), edge(project::MIN_BYTES + MB, project::MIN_BYTES - 1), project::MIN_BYTES), ("not compressed yet", "not compressed", 8 * MB));
        assert_eq!((edge(9 * MB, project::MIN_BYTES), edge(20 * MB, 0)), ("not compressed yet", "not compressed"));

        // The one being stored as video when the list was read says so, whatever else is true of it.
        let converting = Listed { converting: Some(large.dir.clone()), ..listed.clone() };
        for (video_on, ffmpeg) in [(true, FfmpegState::Found), (false, FfmpegState::NotFound)] {
            let made = listing(&converting, video_on, ffmpeg, &now);
            assert_eq!(made.rows[2], row(&large, "Customer onboarding · Add a domain", "2 Nov", "compressing…", "172 MB"));
            assert_eq!(made.rows.iter().filter(|r| r.form == "compressing…").count(), 1);
        }

        // A recording with a problem names it where the form would be, and is counted like any other.
        let with = |problem| ProjectInfo { problem: Some(problem), ..large.clone() };
        let (mut nameless, mut newer) = (with(Problem::Damaged), with(Problem::Newer));
        (nameless.title, nameless.tour_title, nameless.saved_at, nameless.dir) = (String::new(), String::new(), 0, large.dir.with_file_name("chap_x"));
        (newer.form, newer.dir) = (Form::Video, large.dir.with_file_name("chap_y"));
        let broken = Listed { projects: vec![nameless.clone(), newer.clone(), with(Problem::FramesMissing)], converting: Some(large.dir.clone()) };
        let made = listing(&broken, true, FfmpegState::Found, &now);
        assert_eq!(made.rows.iter().map(|r| r.form).collect::<Vec<_>>(), ["frames missing", "saved by a newer version", "damaged"]);
        // No titles and no day in the file: the row says so in words, and shows no day from 1970.
        assert_eq!(made.rows[2], Row { dir: nameless.dir.clone(), title: "Untitled recording".into(), saved: None, form: "damaged", size: "172 MB".into() });
        assert_eq!(made.rows[2].detail(), "damaged · 172 MB");
        assert_eq!(made.totals, Totals { count: 3, bytes: 3 * 172 * MB });
        // Nothing saved: no rows.
        assert_eq!(listing(&Listed::default(), true, FfmpegState::Found, &now), Listing::default());

        // On the page: every row with its title, its details and its button.
        let mut page = Page::showing(listed.clone());
        page.ffmpeg = FfmpegState::Found;
        let out = page.settled();
        let texts = texts_of(&out);
        let expected = listing(&listed, true, FfmpegState::Found, &chrono::Local::now());
        for r in &expected.rows {
            let (title, detail) = (rect_of(&out, &r.title).unwrap_or_else(|| panic!("{} is drawn: {texts:?}", r.title)), rect_of(&out, &r.detail()).unwrap_or_else(|| panic!("{} is drawn: {texts:?}", r.detail())));
            assert!((title.center().y - detail.center().y).abs() < 4.0 && title.right() < detail.left(), "one row: {title:?} {detail:?}");
        }
        assert_eq!(rects_of(&out, REMOVE_RECORDING).len(), 5, "{texts:?}");
        assert!(texts.iter().any(|t| t == &format!("{} · video · 32.2 MB", day(nov_18))), "{texts:?}");
        assert!(texts.iter().any(|t| t == "5 saved recordings · 419 MB of 5 GB"), "{texts:?}");
    }

    #[test]
    fn a_chapter_is_shown_only_when_the_tour_has_several() {
        // Saved by the store itself: a tour with two chapters, and one with a single chapter
        // whose title is not the tour's.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("projects"));
        save_project(&store, "tour_a", "chap_a", "Customer onboarding", "Invite your team");
        save_project(&store, "tour_a", "chap_b", "Customer onboarding", "Customer onboarding");
        save_project(&store, "tour_b", "chap_a", "Billing", "Change your plan");
        let mut page = Page::new(Some(store.root().to_path_buf()));
        let texts = texts_of(&page.settled());
        for title in ["Customer onboarding · Invite your team", "Customer onboarding · Customer onboarding", "Billing"] {
            assert_eq!(texts.iter().filter(|t| t.as_str() == title).count(), 1, "{title}: {texts:?}");
        }
        assert!(!texts.iter().any(|t| t.contains("Change your plan")), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("3 saved recordings · ")), "{texts:?}");
        // One of the two is removed by hand and the tab opened again: the tour has one, and names no chapter.
        std::fs::remove_dir_all(store.root().join(store.list()[0].dir.parent().unwrap()).join("chap_b")).unwrap();
        let mut page = Page::new(Some(store.root().to_path_buf()));
        let texts = texts_of(&page.settled());
        assert!(texts.iter().any(|t| t == "Customer onboarding") && !texts.iter().any(|t| t.contains("Invite your team")), "{texts:?}");
        // The same tour id on two sites is two tours.
        let now = chrono::Utc::now();
        let two_sites = listed(vec![info(SITE, "tour_a", "chap_a", "Customer onboarding", "Invite your team", 2, MB), info(OTHER_SITE, "tour_a", "chap_b", "Customer onboarding", "Add a domain", 1, MB)]);
        let titles: Vec<String> = listing(&two_sites, true, FfmpegState::Found, &now).rows.into_iter().map(|r| r.title).collect();
        assert_eq!(titles, ["Customer onboarding", "Customer onboarding"]);
    }

    #[test]
    fn the_list_is_read_once_per_epoch_and_never_while_drawing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("projects");
        project(&root, SITE, "tour_a", "ch_a", 2_097_152);
        let mut saved = SavedRecordings::at(Some(root.clone()));
        let mut started = 0;
        // Nothing is shown before the first read is back.
        assert_eq!(line(&mut saved), None);
        saved.refresh(7, true, &mut |root| read(root, &mut started));
        assert_eq!((started, line(&mut saved)), (1, None));
        // Drawn again and again: the list is read once and the disk is left alone, also when
        // the folder changed behind the app's back.
        project(&root, SITE, "tour_a", "ch_b", 1_048_576);
        for _ in 0..50 {
            saved.refresh(7, false, &mut |root| read(root, &mut started));
        }
        assert_eq!((started, line(&mut saved).as_deref()), (1, Some("1 saved recording · 2.0 MB")));
        assert_eq!(saved.shown.as_ref().unwrap().rows.len(), 1);
        // The video switch and the search for ffmpeg change what a row says, and read nothing.
        let now = chrono::Utc::now();
        for (video_on, ffmpeg) in [(false, FfmpegState::Found), (true, FfmpegState::Checking), (true, FfmpegState::Found), (true, FfmpegState::Found)] {
            saved.build(video_on, ffmpeg, &now);
            saved.refresh(7, false, &mut |root| read(root, &mut started));
            assert_eq!((started, saved.built_for), (1, Some((video_on, ffmpeg))));
        }
        // A project was saved or removed: read again, the earlier list staying until it is back.
        saved.refresh(8, false, &mut |root| read(root, &mut started));
        assert_eq!((started, line(&mut saved).as_deref()), (2, Some("1 saved recording · 2.0 MB")));
        saved.refresh(8, false, &mut |root| read(root, &mut started));
        assert_eq!((started, line(&mut saved).as_deref()), (2, Some("2 saved recordings · 3.0 MB")));
        // The tab is opened again: read again, whatever the counter says.
        std::fs::remove_dir_all(root.join(SITE).join("tour_a").join("ch_a")).unwrap();
        saved.refresh(8, true, &mut |root| read(root, &mut started));
        saved.refresh(8, false, &mut |root| read(root, &mut started));
        assert_eq!((started, line(&mut saved).as_deref()), (3, Some("1 saved recording · 1.0 MB")));

        // The page itself: one read however often it is drawn, and none from clicking about in it.
        let mut page = Page::showing(listed(vec![info(SITE, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding", 1, 20 * MB)]));
        for _ in 0..50 {
            page.frame(vec![]);
        }
        page.click(REMOVE_RECORDING);
        page.click(KEEP_IT);
        page.click(STORE_AS_VIDEO);
        page.set_limit("7");
        assert_eq!(page.reads, 1);
        // A project is saved or removed, or a conversion starts or ends: one more read.
        page.epoch += 1;
        for _ in 0..50 {
            page.frame(vec![]);
        }
        assert_eq!(page.reads, 2);
    }

    #[test]
    fn one_read_runs_at_a_time_and_a_change_during_it_is_read_afterwards() {
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
            saved.refresh(epoch, opened, &mut |root| read(root, &mut started));
        }
        assert_eq!((started, line(&mut saved)), (1, None));
        // It comes back: it is shown, and the change it may have missed is read.
        let four = (0..4).map(|i| info(SITE, "tour_a", &format!("chap_{i}"), "Customer onboarding", "Customer onboarding", i, MB)).collect();
        tx.send(listed(four)).unwrap();
        saved.refresh(2, false, &mut |root| read(root, &mut started));
        assert_eq!((started, line(&mut saved).as_deref()), (2, Some("4 saved recordings · 4.0 MB")));
        saved.refresh(2, false, &mut |root| read(root, &mut started));
        assert_eq!((started, line(&mut saved).as_deref()), (2, Some("No saved recordings")));
        // A read that died is not started over and over, and what is shown stays.
        let (dead, rx) = channel::<Listed>();
        drop(dead);
        let mut gone = Some(rx);
        saved.refresh(3, false, &mut |_| {
            started += 1;
            gone.take().unwrap()
        });
        for _ in 0..5 {
            saved.refresh(3, false, &mut |root| read(root, &mut started));
        }
        assert_eq!((started, line(&mut saved).as_deref()), (3, Some("No saved recordings")));
    }

    #[test]
    fn without_a_projects_folder_nothing_is_read_and_no_folder_is_offered() {
        let mut saved = SavedRecordings::at(None);
        let mut started = 0;
        assert_eq!(line(&mut saved).as_deref(), Some("No saved recordings"));
        saved.refresh(1, true, &mut |root| read(root, &mut started));
        saved.refresh(2, false, &mut |root| read(root, &mut started));
        assert_eq!((started, line(&mut saved).as_deref(), saved.root()), (0, Some("No saved recordings"), None));
        // A state the tests did not give a folder looks for none, so no test reads the real one.
        let mut fresh = SavedRecordings::default();
        fresh.refresh(1, true, &mut |root| read(root, &mut started));
        assert_eq!((started, fresh.root()), (0, None));
        // On the page: the switches, the limit and the line; no button and no list.
        let mut page = Page::new(None);
        let texts = texts_of(&page.settled());
        for text in [KEEP_RECORDING, STORE_AS_VIDEO, WARN_WHEN, "No saved recordings"] {
            assert!(texts.iter().any(|t| t == text), "{text}: {texts:?}");
        }
        assert!(!texts.iter().any(|t| t == OPEN_FOLDER || t == REMOVE_RECORDING), "{texts:?}");
        assert_eq!(page.reads, 0);
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
        // Reading and drawing made no folder; making it is left to the helper that opens it.
        assert!(!root.exists());
        // With recordings saved, and with the switch off: the same folder, not a project's.
        project(&root, SITE, "tour_a", "ch_a", 1_048_576);
        page.settings.keep_published_recordings = false;
        page.click(OPEN_FOLDER);
        assert_eq!(page.opened, [root.clone(), root]);
        assert!(page.removals.is_empty());
    }

    #[test]
    fn remove_asks_first_and_keep_it_removes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("projects"));
        let saved = save_project(&store, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding");
        let mut page = Page::new(Some(store.root().to_path_buf()));
        let question = "Remove the recording of \"Customer onboarding\"? The tour stays online, but it can no longer be edited from this computer.";
        let texts = texts_of(&page.settled());
        assert!(!texts.iter().any(|t| t == question || t == KEEP_IT), "nothing is asked before the button: {texts:?}");
        // The button asks; it removes nothing.
        page.click(REMOVE_RECORDING);
        let out = page.settled();
        let texts = texts_of(&out);
        assert_eq!(page.state.saved.asking.as_deref(), Some(saved.dir.as_path()));
        let (asked, yes, no) = (rect_of(&out, question).unwrap_or_else(|| panic!("{texts:?}")), rect_of(&out, REMOVE_RECORDING).unwrap(), rect_of(&out, KEEP_IT).unwrap());
        assert_eq!(rects_of(&out, REMOVE_RECORDING).len(), 1, "the row's button gives way to the question's: {texts:?}");
        assert!(asked.bottom() <= yes.top() && (yes.center().y - no.center().y).abs() < 2.0, "{asked:?} {yes:?} {no:?}");
        assert!(asked.right() <= page.screen.x && yes.right() <= page.screen.x && no.right() <= page.screen.x);
        assert!(page.removals.is_empty() && page.state.saved.removing.is_none());
        assert_eq!(store.list(), std::slice::from_ref(&saved));
        // "Keep it": the question goes and the recording stays, also after drawing on.
        page.click(KEEP_IT);
        let texts = texts_of(&page.settled());
        assert!(!texts.iter().any(|t| t == question || t == KEEP_IT) && texts.iter().any(|t| t == REMOVE_RECORDING), "{texts:?}");
        assert!(page.removals.is_empty() && page.state.saved.asking.is_none() && page.state.saved.note.is_none());
        // Asked again and left unanswered while both switches are turned off: still nothing.
        page.click(REMOVE_RECORDING);
        page.click(STORE_AS_VIDEO);
        page.click(KEEP_RECORDING);
        page.settled();
        assert!(page.removals.is_empty() && page.actions.iter().all(|a| matches!(a, UiAction::SettingsChanged)));
        assert_eq!(store.list(), std::slice::from_ref(&saved));
        assert!(saved.dir.join("project.json").is_file() && saved.dir.join("frames.zst").is_file());
        // Leaving the tab and coming back drops a question nobody answered.
        page.state.tab = Tab::General;
        page.frame(vec![]);
        page.frame(vec![]);
        page.state.tab = Tab::Output;
        assert!(!texts_of(&page.settled()).iter().any(|t| t == question));
        assert!(page.state.saved.asking.is_none() && page.removals.is_empty());
    }

    #[test]
    fn a_confirmed_removal_asks_the_store_for_exactly_that_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("projects"));
        let first = save_project(&store, "tour_a", "chap_a", "Customer onboarding", "Invite your team");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = save_project(&store, "tour_b", "chap_a", "Billing", "Billing");
        let mut page = Page::new(Some(store.root().to_path_buf()));
        let out = page.settled();
        // Newest first: the second row is the recording saved first.
        let (billing, onboarding) = (rect_of(&out, "Billing").unwrap(), rect_of(&out, "Customer onboarding").unwrap());
        assert!(billing.top() < onboarding.top());
        page.click_nth(REMOVE_RECORDING, 1);
        let texts = texts_of(&page.settled());
        assert!(texts.iter().any(|t| t == &remove_question("Customer onboarding")), "{texts:?}");
        // The question's own button, which stands where the row was.
        assert_eq!(page.state.saved.asking.as_deref(), Some(first.dir.as_path()));
        page.click_nth(REMOVE_RECORDING, 1);
        let texts = texts_of(&page.settled());
        // The store was asked once, for that row's folder as its own list named it, in its own folder.
        assert_eq!(page.removals, [(store.root().to_path_buf(), first.dir.clone())]);
        assert!(!first.dir.exists() && second.dir.join("frames.zst").is_file());
        assert_eq!(store.list(), std::slice::from_ref(&second));
        // The list was read again: the row is gone, the other stays, and nothing more is said.
        assert!(!texts.iter().any(|t| t == "Customer onboarding" || t == KEEP_IT) && texts.iter().any(|t| t == "Billing"), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("1 saved recording · ")), "{texts:?}");
        assert!(page.state.saved.note.is_none() && page.state.saved.asking.is_none() && page.state.saved.removing.is_none());

        // A folder that is not in the list as it is shown is never asked about, and never handed on.
        let mut asked = Vec::new();
        let mut hand_on = |dir: &Path| {
            asked.push(dir.to_path_buf());
            ready(Ok(Removed::Now))
        };
        let saved = &mut page.state.saved;
        for elsewhere in [store.root().to_path_buf(), first.dir.clone(), second.dir.join("pictures"), tmp.path().to_path_buf(), PathBuf::from("C:\\Windows")] {
            saved.ask(&elsewhere);
            assert!(saved.asking.is_none(), "{elsewhere:?}");
            saved.asking = Some(elsewhere);
            saved.confirm(&mut hand_on);
            assert!(saved.asking.is_none() && saved.removing.is_none());
        }
        // Without a question there is nothing to confirm.
        saved.confirm(&mut hand_on);
        // A question whose recording left the list before it was answered is over.
        saved.ask(&second.dir);
        assert_eq!(saved.asking.as_deref(), Some(second.dir.as_path()));
        saved.read = Some(Listed::default());
        saved.built_for = None;
        saved.build(true, FfmpegState::NotFound, &chrono::Utc::now());
        assert!(saved.asking.is_none());
        saved.confirm(&mut hand_on);
        assert!(asked.is_empty(), "{asked:?}");
        assert!(second.dir.join("frames.zst").is_file());
    }

    #[test]
    fn one_removal_runs_at_a_time_and_a_recording_that_was_already_gone_only_refreshes_the_list() {
        let a = info(SITE, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding", 2, 20 * MB);
        let b = info(SITE, "tour_b", "chap_a", "Billing", "Billing", 1, 20 * MB);
        let mut saved = SavedRecordings::at(Some(PathBuf::from("projects")));
        let reads = std::cell::Cell::new(0);
        let both = listed(vec![a.clone(), b.clone()]);
        let mut list = |_: &Path| {
            reads.set(reads.get() + 1);
            ready(both.clone())
        };
        let now = chrono::Utc::now();
        saved.refresh(1, true, &mut list);
        saved.refresh(1, false, &mut list);
        saved.build(true, FfmpegState::NotFound, &now);
        // A removal that takes its time.
        let (tx, rx) = channel();
        let (mut slow, mut asked) = (Some(rx), Vec::new());
        saved.ask(&a.dir);
        saved.confirm(&mut |dir| {
            asked.push(dir.to_path_buf());
            slow.take().unwrap()
        });
        assert!(saved.removing.is_some() && saved.asking.is_none());
        // Meanwhile no other row can be asked about, and nothing else is handed on.
        saved.ask(&b.dir);
        assert!(saved.asking.is_none());
        saved.asking = Some(b.dir.clone());
        saved.confirm(&mut |dir| {
            asked.push(dir.to_path_buf());
            ready(Ok(Removed::Now))
        });
        assert_eq!(asked, std::slice::from_ref(&a.dir));
        saved.refresh(1, false, &mut list);
        assert!(saved.removing.is_some());
        // The store found nothing there: no error is said, and the list is read again.
        let before = reads.get();
        tx.send(Ok(Removed::NotThere)).unwrap();
        saved.refresh(1, false, &mut list);
        assert!(saved.removing.is_none() && saved.note.is_none());
        assert_eq!(reads.get(), before + 1);
        // The same for a removal that worked.
        saved.refresh(1, false, &mut list);
        saved.ask(&b.dir);
        saved.confirm(&mut |_| ready(Ok(Removed::Now)));
        let before = reads.get();
        saved.refresh(1, false, &mut list);
        assert!(saved.removing.is_none() && saved.note.is_none());
        assert_eq!(reads.get(), before + 1);
    }

    #[test]
    fn a_removal_that_fails_says_so_and_the_row_stays() {
        let project = info(SITE, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding", 1, 20 * MB);
        let mut page = Page::showing(listed(vec![project.clone()]));
        // The store's own reason is for its log: the page says its own sentence.
        page.answer = Some(Err("this saved recording could not be removed: C:\\Users\\somebody\\projects is in use.".into()));
        page.click(REMOVE_RECORDING);
        page.click(REMOVE_RECORDING);
        let out = page.settled();
        let texts = texts_of(&out);
        assert_eq!(page.removals, [(PathBuf::from("projects"), project.dir.clone())]);
        let sentence = "The recording could not be removed. Close anything that has it open and try again.";
        assert_eq!(page.state.saved.note.as_deref(), Some(sentence));
        assert!(texts.iter().any(|t| t == sentence), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("somebody") || t.contains("in use")), "{texts:?}");
        // The row stays, with its button, and can be tried again; trying takes the sentence away.
        assert!(texts.iter().any(|t| t == "Customer onboarding") && texts.iter().any(|t| t == REMOVE_RECORDING) && !texts.iter().any(|t| t == KEEP_IT), "{texts:?}");
        assert!(rect_of(&out, sentence).unwrap().right() <= page.screen.x);
        page.click(REMOVE_RECORDING);
        let texts = texts_of(&page.settled());
        assert!(page.state.saved.note.is_none() && !texts.iter().any(|t| t == sentence) && texts.iter().any(|t| t == KEEP_IT), "{texts:?}");
        assert_eq!(page.removals.len(), 1);

        // A removal that died without an answer says the same, and nothing is taken to be gone.
        let mut saved = SavedRecordings::at(Some(PathBuf::from("projects")));
        let one = listed(vec![project.clone()]);
        saved.refresh(1, true, &mut |_| ready(one.clone()));
        saved.refresh(1, false, &mut |_| ready(one.clone()));
        saved.build(true, FfmpegState::NotFound, &chrono::Utc::now());
        saved.ask(&project.dir);
        saved.confirm(&mut |_| channel().1);
        saved.refresh(1, false, &mut |_| ready(one.clone()));
        assert_eq!((saved.note.as_deref(), saved.removing.is_none()), (Some(NOT_REMOVED), true));
        assert!(saved.lists(&project.dir));
    }

    #[test]
    fn a_project_open_in_an_editor_says_it_goes_when_the_editor_closes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("projects"));
        let saved = save_project(&store, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding");
        let other = save_project(&store, "tour_b", "chap_a", "Billing", "Billing");
        // An editor has it open.
        let open = store.load(&saved.key).unwrap();
        let mut page = Page::new(Some(store.root().to_path_buf()));
        let out = page.settled();
        let nth = if rect_of(&out, "Customer onboarding").unwrap().top() < rect_of(&out, "Billing").unwrap().top() { 0 } else { 1 };
        page.click_nth(REMOVE_RECORDING, nth);
        assert_eq!(page.state.saved.asking.as_deref(), Some(saved.dir.as_path()));
        page.click_nth(REMOVE_RECORDING, nth);
        let texts = texts_of(&page.settled());
        assert_eq!(page.removals, [(store.root().to_path_buf(), saved.dir.clone())]);
        // The planned sentence; the row is gone from the list, and the editor still has its frames.
        let sentence = "This recording is open in an editor. It is removed when that editor is closed.";
        assert_eq!(page.state.saved.note.as_deref(), Some(sentence));
        assert!(texts.iter().any(|t| t == sentence) && !texts.iter().any(|t| t == "Customer onboarding") && texts.iter().any(|t| t == "Billing"), "{texts:?}");
        assert!(saved.dir.join("frames.zst").is_file());
        assert_eq!(store.list(), std::slice::from_ref(&other));
        // The editor closes: the store removes it, and the page asked for nothing more.
        drop(open);
        assert!(!saved.dir.exists() && other.dir.join("frames.zst").is_file());
        page.settled();
        assert_eq!(page.removals.len(), 1);
    }

    /// The page's width in the main window at its default size, and at its smallest: the
    /// window less the navigation and the margins of the panel the page stands in.
    const PAGE_WIDTH: f32 = 760.0 - 170.0 - 16.0;
    const PAGE_WIDTH_MIN: f32 = 640.0 - 170.0 - 16.0;

    #[test]
    fn the_page_draws_the_switches_the_bar_and_the_list_at_the_default_window_size() {
        assert_eq!((crate::main_window::SIZE[0], PAGE_WIDTH, PAGE_WIDTH_MIN), (760.0, 574.0, 454.0));
        let projects = vec![
            info(SITE, "tour_a", "chap_a", "Customer onboarding for the whole support team", "Invite your team and give everybody a role", ms("2026-11-18T09:30:00+00:00"), 172 * MB),
            info(SITE, "tour_a", "chap_b", "Customer onboarding for the whole support team", "Add a domain", ms("2026-11-02T09:30:00+00:00"), 33 * MB),
            // The longest a row's details get: another year, the longest word, the widest size.
            ProjectInfo { problem: Some(Problem::Newer), ..info(SITE, "tour_b", "chap_a", "Billing and invoices for the finance team", "Billing", ms("2025-12-28T09:30:00+00:00"), 161_061_273_600) },
        ];
        let total = 161_061_273_600 + (172 + 33) * MB;
        // The page at the default window size, at the smallest, a little under that, and wide.
        for width in [PAGE_WIDTH, PAGE_WIDTH_MIN, PAGE_WIDTH_MIN - 14.0, 900.0] {
            let mut page = Page::showing(listed(projects.clone()));
            page.ffmpeg = FfmpegState::Found;
            // The first frame reads nothing itself: the line and the list come when the read is back.
            page.screen = vec2(width, 2000.0);
            let first = texts_of(&page.frame(vec![]));
            for text in [KEEP_RECORDING, STORE_AS_VIDEO, FFMPEG_FOUND, WARN_WHEN, OPEN_FOLDER] {
                assert!(first.iter().any(|t| t == text), "{width}: {text} is drawn at once: {first:?}");
            }
            assert!(!first.iter().any(|t| t.contains("saved recordings ·") || t == REMOVE_RECORDING), "{width}: {first:?}");
            let out = page.settled();
            let texts = texts_of(&out);
            let rect = |text: &str| rect_of(&out, text).unwrap_or_else(|| panic!("{text} is drawn at {width}: {texts:?}"));
            let (track, _, _) = bar_of(&out).unwrap_or_else(|| panic!("the bar is drawn at {width}"));
            let heading = rect("Tour publishing");
            let (keep, video, ffmpeg, limit, unit) = (rect(KEEP_RECORDING), rect(STORE_AS_VIDEO), rect(FFMPEG_FOUND), rect(WARN_WHEN), rect(WARN_UNIT));
            let (line, warning, button) = (rect(&usage_line(3, total, 5)), rect(&over_limit_sentence(total, 5)), rect(OPEN_FOLDER));
            let title = rect("Customer onboarding for the whole support team · Invite your team and give everybody a role");
            let removes = rects_of(&out, REMOVE_RECORDING);
            // From the top, in the planned order.
            let order = [heading, keep, video, ffmpeg, limit, track, line, warning, button, title];
            for pair in order.windows(2) {
                assert!(pair[0].bottom() <= pair[1].top() + 0.5, "{width}: {:?} stands above {:?}", pair[0], pair[1]);
            }
            // Everything under the keep switch stands under its label, and the ffmpeg line under its own switch's.
            assert!(video.left() >= keep.left() - 0.5 && ffmpeg.left() >= video.left() - 0.5, "{width}: {keep:?} {video:?} {ffmpeg:?}");
            for part in [limit, line, button, track, title] {
                assert!((part.left() - keep.left()).abs() < 12.0, "{width}: under the label: {keep:?} {part:?}");
            }
            // The limit row is one row, the unit after the field.
            assert!(unit.left() > limit.right() && (unit.center().y - limit.center().y).abs() < 2.0, "{width}: {limit:?} {unit:?}");
            // Nothing is cut off at the right.
            assert_eq!(removes.len(), 3, "{width}: {texts:?}");
            for part in [keep, video, ffmpeg, limit, unit, line, warning, button, track].iter().chain(&removes) {
                assert!(part.right() <= width, "{width}: {part:?}");
            }
            // Every row: its title has room, and its details stand after the title when they
            // fit there and under it when they do not; nothing runs into anything else.
            let expected = listing(&listed(projects.clone()), true, FfmpegState::Found, &chrono::Local::now());
            assert_eq!(expected.rows.iter().map(|r| r.form).collect::<Vec<_>>(), ["not compressed yet", "not compressed yet", "saved by a newer version"]);
            assert!(expected.rows[2].detail().ends_with(" 2025 · saved by a newer version · 150.0 GB"), "{}", expected.rows[2].detail());
            let mut under = 0;
            for (row, remove) in expected.rows.iter().zip(&removes) {
                let (title, detail) = (rect(&row.title), rect(&row.detail()));
                assert!(title.right() <= remove.left() && (title.center().y - remove.center().y).abs() < 4.0, "{width}: {title:?} {remove:?}");
                assert!(detail.right() <= width && detail.left() >= title.left() - 0.5, "{width}: {detail:?}");
                if (detail.center().y - remove.center().y).abs() < 4.0 {
                    assert!(title.right() <= detail.left() && detail.right() < remove.left(), "{width}: {title:?} {detail:?} {remove:?}");
                    assert!(detail.left() - title.left() >= 110.0, "{width}: the title keeps its room: {title:?} {detail:?}");
                } else {
                    under += 1;
                    assert!(detail.top() >= title.bottom() - 1.0 && detail.top() >= remove.bottom() - 6.0, "{width}: {title:?} {detail:?} {remove:?}");
                    assert!(remove.left() - title.left() >= 110.0, "{width}: the title keeps its room: {title:?} {remove:?}");
                }
            }
            // Wide, every row is one line; at the smallest window the longest details go under their title.
            if width == 900.0 {
                assert_eq!(under, 0);
            }
            if width <= PAGE_WIDTH_MIN {
                assert!(under >= 1, "{width}");
            }
            // A question takes the place of its row and fits the same width.
            page.click_nth(REMOVE_RECORDING, 0);
            let out = page.settled();
            let question = drawn(&out).into_iter().find(|(t, _)| t.starts_with("Remove the recording of")).unwrap().1;
            let (yes, no) = (rect_of(&out, REMOVE_RECORDING).unwrap(), rect_of(&out, KEEP_IT).unwrap());
            assert!(question.right() <= width && yes.right() <= width && no.right() <= width, "{width}: {question:?} {yes:?} {no:?}");
            assert_eq!(rects_of(&out, REMOVE_RECORDING).len(), 3, "{width}: two rows and the question");
            assert!(page.removals.is_empty());
        }
    }

    #[test]
    fn the_page_draws_again_by_itself_while_ffmpeg_is_looked_for() {
        let delay = |out: &egui::FullOutput| out.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        let mut page = Page::showing(listed(vec![info(SITE, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding", 1, 20 * MB)]));
        page.ffmpeg = FfmpegState::Checking;
        let out = page.settled();
        assert!(texts_of(&out).iter().any(|t| t == FFMPEG_LOOKING));
        // Soon, and not at once: the search takes a moment, and the page does not spin for it.
        let wait = delay(&out);
        assert!(wait >= std::time::Duration::from_millis(100) && wait <= std::time::Duration::from_millis(300), "{wait:?}");
        // The search is over, either way: the page says so and asks for nothing more.
        for (state, says) in [(FfmpegState::Found, FFMPEG_FOUND), (FfmpegState::NotFound, FFMPEG_NOT_FOUND)] {
            page.ffmpeg = state;
            let out = page.settled();
            assert!(texts_of(&out).iter().any(|t| t == says));
            assert!(delay(&out) > std::time::Duration::from_secs(5), "{state:?}: {:?}", delay(&out));
        }
        // With the video switch off the search matters to nothing on the page.
        page.ffmpeg = FfmpegState::Checking;
        page.settings.store_recordings_as_video = false;
        assert!(delay(&page.settled()) > std::time::Duration::from_secs(5));
    }

    #[test]
    fn escape_answers_keep_it() {
        let project = info(SITE, "tour_a", "chap_a", "Customer onboarding", "Customer onboarding", 1, 20 * MB);
        let mut page = Page::showing(listed(vec![project.clone()]));
        page.answer = Some(Ok(Removed::Now));
        page.click(REMOVE_RECORDING);
        assert_eq!(page.state.saved.asking.as_deref(), Some(project.dir.as_path()));
        page.key(egui::Key::Escape);
        let texts = texts_of(&page.settled());
        assert!(page.state.saved.asking.is_none() && page.removals.is_empty() && page.state.saved.removing.is_none());
        assert!(!texts.iter().any(|t| t == KEEP_IT) && texts.iter().any(|t| t == REMOVE_RECORDING), "{texts:?}");
        // Escape with no question showing does nothing.
        page.key(egui::Key::Escape);
        assert!(page.removals.is_empty() && page.actions.is_empty());
    }

    #[test]
    fn a_double_click_on_a_rows_button_only_asks() {
        let projects: Vec<ProjectInfo> = (0..3).map(|i| info(SITE, &format!("tour_{i}"), "chap_a", &format!("Recording {i}"), "Chapter", 1_000 - i, 20 * MB)).collect();
        for width in [PAGE_WIDTH, PAGE_WIDTH_MIN, 900.0] {
            for nth in 0..3 {
                let mut page = Page::showing(listed(projects.clone()));
                page.answer = Some(Ok(Removed::Now));
                page.screen = vec2(width, 2000.0);
                let out = page.settled();
                let at = rects_of(&out, REMOVE_RECORDING)[nth].center();
                // Twice and then twice more in the same place, as fast as frames come.
                let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
                page.frame(vec![egui::Event::PointerMoved(at)]);
                for _ in 0..4 {
                    page.frame(vec![press(true)]);
                    page.frame(vec![press(false)]);
                }
                page.settled();
                assert!(page.removals.is_empty() && page.state.saved.removing.is_none(), "{width} {nth}: {:?}", page.removals);
                assert_eq!(page.state.saved.asking.as_deref(), Some(projects[nth].dir.as_path()), "{width} {nth}");
                assert_eq!(page.state.saved.shown.as_ref().unwrap().rows.len(), 3);
            }
        }
    }

    #[test]
    fn enter_after_asking_with_the_keyboard_does_not_confirm() {
        let projects: Vec<ProjectInfo> = (0..3).map(|i| info(SITE, &format!("tour_{i}"), "chap_a", &format!("Recording {i}"), "Chapter", 1_000 - i, 20 * MB)).collect();
        for nth in 0..3 {
            let mut page = Page::showing(listed(projects.clone()));
            page.answer = Some(Ok(Removed::Now));
            let out = page.settled();
            // Tab to the row's button and press Enter: the question shows.
            page.focus_at(rects_of(&out, REMOVE_RECORDING)[nth].center());
            page.key(egui::Key::Enter);
            assert_eq!(page.state.saved.asking.as_deref(), Some(projects[nth].dir.as_path()), "{nth}");
            // Enter and Space again, with nothing else done: no button of the question has the keyboard.
            for key in [egui::Key::Enter, egui::Key::Enter, egui::Key::Space, egui::Key::Enter] {
                page.key(key);
            }
            page.settled();
            assert!(page.removals.is_empty() && page.state.saved.removing.is_none(), "{nth}: {:?}", page.removals);
            assert_eq!(page.state.saved.asking.as_deref(), Some(projects[nth].dir.as_path()), "{nth}");
        }
    }

    #[test]
    fn the_question_scrolls_itself_into_view_inside_the_list() {
        let projects: Vec<ProjectInfo> = (0..40).map(|i| info(SITE, &format!("tour_{i:02}"), "chap_a", &format!("Recording {i:02}"), "Chapter", 1_000_000 - i, 20 * MB)).collect();
        let mut page = Page::showing(listed(projects.clone()));
        let out = page.settled();
        // Ask about the last row the list shows without scrolling: the question is taller
        // than the row it replaces, so where the row stood its buttons would be cut off.
        let list_bottom = out.shapes.iter().find(|c| matches!(&c.shape, egui::Shape::Text(t) if t.galley.text() == "Recording 00")).unwrap().clip_rect.bottom();
        // The list may also paint the top of a row it cuts off; that row is not one it shows.
        let buttons: Vec<egui::Rect> = rects_of(&out, REMOVE_RECORDING).into_iter().filter(|r| r.bottom() <= list_bottom).collect();
        let (last, nth) = (*buttons.last().unwrap(), buttons.len() - 1);
        assert!((5..12).contains(&nth) && list_bottom - last.bottom() < 30.0, "{nth}: {last:?} in a list that ends at {list_bottom}");
        page.click_at(last.center());
        assert_eq!(page.state.saved.asking.as_deref(), Some(projects[nth].dir.as_path()));
        // The list glides there over some frames.
        for _ in 0..60 {
            page.frame(vec![]);
        }
        let out = page.settled();
        // The question and both its buttons are inside what the list shows.
        let mut seen = 0;
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape
                && (t.galley.text() == KEEP_IT || t.galley.text().starts_with("Remove the recording of"))
            {
                seen += 1;
                let rect = t.galley.rect.translate(t.pos.to_vec2());
                assert!(clipped.clip_rect.contains_rect(rect), "{}: {rect:?} in {:?}", t.galley.text(), clipped.clip_rect);
            }
        }
        assert_eq!(seen, 2, "{:?}", texts_of(&out));
        // It is shown once and then left alone: the list can be scrolled away from it, to its end.
        page.frame(vec![egui::Event::PointerMoved(last.center())]);
        for _ in 0..60 {
            page.frame(vec![wheel_down()]);
        }
        let out = page.settled();
        assert!(rect_of(&out, "Recording 39").is_some() && rect_of(&out, KEEP_IT).is_none(), "{:?}", texts_of(&out));
        assert!(page.state.saved.asking.is_some() && page.removals.is_empty());
    }

    #[test]
    fn the_section_fits_the_default_window_without_the_list_pushing_the_page_out() {
        // The page as the default window shows it: 590 wide and about 470 tall beside the navigation.
        let projects = (0..40).map(|i| info(SITE, &format!("tour_{i}"), "chap_a", &format!("Tour {i}"), "Chapter", 1_000 + i, 20 * MB)).collect();
        let mut page = Page::showing(listed(projects));
        page.screen = vec2(PAGE_WIDTH, 470.0);
        let out = page.settled();
        // Whatever is drawn is inside the window.
        for (text, rect) in drawn(&out) {
            assert!(rect.right() <= PAGE_WIDTH + 0.5 && rect.top() >= -0.5, "{text}: {rect:?}");
        }
        // Scrolled to its end, the page shows the section whole: the list's height is bounded.
        let at = egui::pos2(300.0, 100.0);
        page.frame(vec![egui::Event::PointerMoved(at)]);
        for _ in 0..40 {
            page.frame(vec![wheel_down()]);
        }
        let out = page.settled();
        let texts = texts_of(&out);
        for text in [STORE_AS_VIDEO, WARN_WHEN, OPEN_FOLDER, "40 saved recordings · 800 MB of 5 GB", "Tour 39"] {
            let rect = rect_of(&out, text).unwrap_or_else(|| panic!("{text} is on the page: {texts:?}"));
            assert!(rect.top() >= 0.0 && rect.bottom() <= 470.0, "{text}: {rect:?}");
        }
    }

    #[test]
    fn two_hundred_rows_draw_inside_the_scroll_area() {
        let projects: Vec<ProjectInfo> = (0..200).map(|i| info(SITE, &format!("tour_{i:03}"), "chap_a", &format!("Recording {i:03}"), "Chapter", 1_000_000 - i, 20 * MB)).collect();
        let mut page = Page::showing(listed(projects));
        let out = page.settled();
        let texts = texts_of(&out);
        assert_eq!(page.state.saved.shown.as_ref().unwrap().rows.len(), 200);
        assert!(texts.iter().any(|t| t == "200 saved recordings · 3.9 GB of 5 GB"), "{texts:?}");
        // On a page tall enough for all of them, only the rows inside the list's height are drawn.
        let rows: Vec<(String, egui::Rect)> = drawn(&out).into_iter().filter(|(t, _)| t.starts_with("Recording ")).collect();
        assert!(rows.iter().any(|(t, _)| t == "Recording 000") && !rows.iter().any(|(t, _)| t == "Recording 199"), "newest first, and the last is not drawn");
        assert!((3..20).contains(&rows.len()), "{} rows are drawn", rows.len());
        let (top, bottom) = (rows.iter().map(|(_, r)| r.top()).fold(f32::MAX, f32::min), rows.iter().map(|(_, r)| r.bottom()).fold(f32::MIN, f32::max));
        assert!(bottom - top <= LIST_MAX_HEIGHT + 40.0, "{top} to {bottom}");
        // Nothing of the list is painted outside its own height, whatever the page's.
        let button = rect_of(&out, OPEN_FOLDER).unwrap();
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape
                && (t.galley.text().starts_with("Recording ") || t.galley.text() == REMOVE_RECORDING)
            {
                assert!(clipped.clip_rect.height() <= LIST_MAX_HEIGHT + 1.0 && clipped.clip_rect.top() >= button.bottom(), "{:?}", clipped.clip_rect);
            }
        }
        // The list scrolls by itself: its last row can be reached and removed like any other.
        let at = rows[1].1.center();
        page.frame(vec![egui::Event::PointerMoved(at)]);
        for _ in 0..200 {
            page.frame(vec![wheel_down()]);
        }
        let out = page.settled();
        let last = rect_of(&out, "Recording 199").unwrap_or_else(|| panic!("the last row is reached: {:?}", texts_of(&out)));
        assert!(last.bottom() - top <= LIST_MAX_HEIGHT + 40.0, "{last:?}");
        assert!(rect_of(&out, OPEN_FOLDER).is_some(), "the page did not move");
    }

    #[test]
    fn the_wording_is_the_planned_one() {
        assert_eq!(KEEP_RECORDING, "Keep the recording when I publish a tour, so I can edit it later");
        assert_eq!(STORE_AS_VIDEO, "Store kept recordings as video to save space");
        assert_eq!(FFMPEG_FOUND, "ffmpeg was found on this computer. Recordings are compressed in the background after a tour is published.");
        assert_eq!(FFMPEG_NOT_FOUND, "ffmpeg was not found on this computer, so recordings are stored without video compression. Install ffmpeg and start Mythicshot again.");
        assert_eq!(FFMPEG_LOOKING, "Looking for ffmpeg…");
        assert_eq!((WARN_WHEN, WARN_UNIT), ("Warn me when saved recordings take more than", "GB"));
        assert_eq!(LIMIT_HOVER, "Leave empty or enter 0 for no warning.");
        assert_eq!(OPEN_FOLDER, "Open folder");
        assert_eq!((REMOVE_RECORDING, KEEP_IT), ("Remove recording", "Keep it"));
        assert_eq!(REMOVED_WHEN_CLOSED, "This recording is open in an editor. It is removed when that editor is closed.");
        assert_eq!(NOT_REMOVED, "The recording could not be removed. Close anything that has it open and try again.");
        assert_eq!(RECORDINGS_STAY, "Saved recordings stay on this computer until you remove them here or delete their tour.");
        // The lines and the words that come from the rules of the saved recordings.
        assert_eq!(usage_line(3, 1_288_490_189, 5), "3 saved recordings · 1.2 GB of 5 GB");
        assert_eq!(usage_line(3, 1_288_490_189, 0), "3 saved recordings · 1.2 GB");
        assert_eq!(usage_line(1, 172 * MB, 5), "1 saved recording · 172 MB of 5 GB");
        assert_eq!(usage_line(0, 0, 5), "No saved recordings");
        assert_eq!(over_limit_sentence(5_798_205_850, 5), "Saved recordings take 5.4 GB, more than the 5 GB you set. Nothing is removed automatically: remove recordings you no longer need in Preferences.");
        assert_eq!(remove_question("Customer onboarding"), "Remove the recording of \"Customer onboarding\"? The tour stays online, but it can no longer be edited from this computer.");
        let forms = [form_text(Form::Video, ConvertState::Done, false, false), form_text(Form::Frames, ConvertState::NotTried, false, true), form_text(Form::Frames, ConvertState::NotTried, true, true), form_text(Form::Frames, ConvertState::NotTried, false, false)];
        assert_eq!(forms, ["video", "not compressed yet", "compressing…", "not compressed"]);
        assert_eq!([Problem::Damaged, Problem::Newer, Problem::FramesMissing].map(problem_text), ["damaged", "saved by a newer version", "frames missing"]);
    }
}
