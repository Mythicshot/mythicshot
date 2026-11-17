//! The image editor window (screen 4 of the mockup, for still images).
//!
//! Every editor is its own deferred viewport holding an `Arc<Mutex<Editor>>`.
//! The canvas shows a texture made by the same tiny-skia renderer that export
//! uses; egui draws only the editing chrome on top (selection, handles, crop).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ColorImage, CursorIcon, FontId, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2, pos2, vec2};
use mythicshot_editor::{Bounds, Document, Item, MAX_DIM, MIN_DIM, ObfuscateMode, Pixmap, Pt, Rgba, RgbaImage, Shape, Style, effects, pt, render_items_onto, to_pixmap};

use mythicshot_destinations::{self as dest, Context, DestId, Sent};

use crate::hotkeys::{self, EditorAction};
use crate::icons::{self, Icon};

const ACCENT: Color32 = Color32::from_rgb(91, 141, 239);
const CANVAS_BG: Color32 = Color32::from_rgb(16, 18, 22);
const HANDLE: f32 = 8.0;
const BAR_BG: Color32 = Color32::from_rgb(27, 31, 39);
/// Toolbar icon button size and tool rail width.
const ICON: f32 = 28.0;
const RAIL: f32 = 84.0;
/// A tool button on the rail.
const TOOL_BUTTON: f32 = 36.0;
/// The rail's sections, like a mockup palette; `None` has no heading.
/// The Spotlight and Guide card tools show only when editing a recording.
const RAIL_GROUPS: [(Option<&str>, &[Tool]); 4] = [
    (None, &[Tool::Select]),
    (Some("DRAW"), &[Tool::Freehand, Tool::Arrow, Tool::Line]),
    (Some("SHAPES"), &[Tool::Circle, Tool::Group, Tool::Cone, Tool::Rectangle, Tool::Ellipse, Tool::Donut, Tool::Text, Tool::Slice, Tool::Cross, Tool::Asterisk]),
    (Some("MORE"), &[Tool::Bubble, Tool::Step, Tool::Highlight, Tool::Obfuscate, Tool::Spotlight, Tool::Card, Tool::Crop]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Freehand,
    Circle,
    Group,
    Cone,
    Donut,
    Slice,
    Cross,
    Asterisk,
    Text,
    Bubble,
    Step,
    Highlight,
    Obfuscate,
    Spotlight,
    /// A tour step's guide card; Tour view only.
    Card,
    Crop,
}

impl Tool {
    /// The name used in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Rectangle => "rectangle",
            Tool::Ellipse => "ellipse",
            Tool::Line => "line",
            Tool::Arrow => "arrow",
            Tool::Freehand => "freehand",
            Tool::Circle => "circle",
            Tool::Group => "group",
            Tool::Cone => "cone",
            Tool::Donut => "donut",
            Tool::Slice => "slice",
            Tool::Cross => "cross",
            Tool::Asterisk => "asterisk",
            Tool::Text => "text",
            Tool::Bubble => "bubble",
            Tool::Step => "step",
            Tool::Highlight => "highlight",
            Tool::Obfuscate => "obfuscate",
            Tool::Spotlight => "spotlight",
            Tool::Card => "card",
            Tool::Crop => "crop",
        }
    }

    fn from_key(k: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.key() == k)
    }

    /// The tool that draws a shape, so editing an item updates that tool's style.
    fn for_shape(s: &Shape) -> Tool {
        match s {
            Shape::Rect(_) => Tool::Rectangle,
            Shape::Ellipse(_) => Tool::Ellipse,
            Shape::Line(..) => Tool::Line,
            Shape::Arrow(..) => Tool::Arrow,
            Shape::Freehand(_) => Tool::Freehand,
            Shape::Circle(_) => Tool::Circle,
            Shape::Group(_) => Tool::Group,
            Shape::Cone(..) => Tool::Cone,
            Shape::Donut(_) => Tool::Donut,
            Shape::Slice(..) => Tool::Slice,
            Shape::Cross(_) => Tool::Cross,
            Shape::Asterisk(_) => Tool::Asterisk,
            Shape::Text { .. } => Tool::Text,
            Shape::Bubble { .. } => Tool::Bubble,
            Shape::Step { .. } => Tool::Step,
            Shape::Highlight(_) => Tool::Highlight,
            Shape::Obfuscate { .. } => Tool::Obfuscate,
            Shape::Spotlight { .. } => Tool::Spotlight,
            // Pasted pictures have no tool, so no style to remember.
            Shape::Image { .. } => Tool::Select,
        }
    }

    const ALL: [Tool; 21] = [
        Tool::Select,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Line,
        Tool::Arrow,
        Tool::Freehand,
        Tool::Circle,
        Tool::Group,
        Tool::Cone,
        Tool::Donut,
        Tool::Slice,
        Tool::Cross,
        Tool::Asterisk,
        Tool::Text,
        Tool::Bubble,
        Tool::Step,
        Tool::Highlight,
        Tool::Obfuscate,
        Tool::Spotlight,
        Tool::Card,
        Tool::Crop,
    ];

    fn icon(self) -> Icon {
        match self {
            Tool::Select => Icon::Select,
            Tool::Rectangle => Icon::Rect,
            Tool::Ellipse => Icon::Ellipse,
            Tool::Line => Icon::Line,
            Tool::Arrow => Icon::Arrow,
            Tool::Freehand => Icon::Freehand,
            Tool::Circle => Icon::Circle,
            Tool::Group => Icon::Group,
            Tool::Cone => Icon::Cone,
            Tool::Donut => Icon::Donut,
            Tool::Slice => Icon::Slice,
            Tool::Cross => Icon::Cross,
            Tool::Asterisk => Icon::Asterisk,
            Tool::Text => Icon::Text,
            Tool::Bubble => Icon::Bubble,
            Tool::Step => Icon::Step,
            Tool::Highlight => Icon::Highlight,
            Tool::Obfuscate => Icon::Obfuscate,
            Tool::Spotlight => Icon::Spotlight,
            Tool::Card => Icon::Card,
            Tool::Crop => Icon::Crop,
        }
    }

    fn action(self) -> EditorAction {
        match self {
            Tool::Select => EditorAction::Select,
            Tool::Rectangle => EditorAction::Rectangle,
            Tool::Ellipse => EditorAction::Ellipse,
            Tool::Line => EditorAction::Line,
            Tool::Arrow => EditorAction::Arrow,
            Tool::Freehand => EditorAction::Freehand,
            Tool::Circle => EditorAction::Circle,
            Tool::Group => EditorAction::Group,
            Tool::Cone => EditorAction::Cone,
            Tool::Donut => EditorAction::Donut,
            Tool::Slice => EditorAction::Slice,
            Tool::Cross => EditorAction::Cross,
            Tool::Asterisk => EditorAction::Asterisk,
            Tool::Text => EditorAction::Text,
            Tool::Bubble => EditorAction::Bubble,
            Tool::Step => EditorAction::Step,
            Tool::Highlight => EditorAction::Highlight,
            Tool::Obfuscate => EditorAction::Obfuscate,
            Tool::Spotlight => EditorAction::Spotlight,
            Tool::Card => EditorAction::Card,
            Tool::Crop => EditorAction::Crop,
        }
    }
}

enum Drag {
    Create { start: Pt, now: Pt },
    Freehand(Vec<Pt>),
    Move { id: u64, last: Pt },
    Resize { id: u64, handle: usize },
    Endpoint { id: u64, end: usize },
    Tail { id: u64 },
    Crop { start: Pt, now: Pt },
}

struct TextEdit {
    /// The text or bubble item being edited, or `None` for a new text item.
    item: Option<u64>,
    at: Pt,
    buffer: String,
    focused: bool,
    /// For new text in the Tour view: the step it was started on, which it is pinned to.
    step: Option<usize>,
}

/// What a paste brought from the system clipboard.
enum Pasted {
    Text(String),
    Image(RgbaImage),
}

/// A clipboard shortcut as egui reports it: the letter it stands for and, for a
/// paste, what was on the system clipboard.
struct ClipboardKey {
    key: Key,
    pasted: Option<Pasted>,
}

/// egui turns Ctrl+C, Ctrl+X and Ctrl+V into Copy, Cut and Paste events and
/// never reports them as key presses; a Ctrl+V with nothing on the clipboard
/// sends nothing at all. Take those events so the shortcuts can match them
/// like any other key.
fn take_clipboard_keys(ctx: &egui::Context) -> Vec<ClipboardKey> {
    ctx.input_mut(|i| {
        let mut keys = Vec::new();
        i.events.retain(|e| {
            let key = match e {
                egui::Event::Copy => ClipboardKey { key: Key::C, pasted: None },
                egui::Event::Cut => ClipboardKey { key: Key::X, pasted: None },
                egui::Event::Paste(text) => ClipboardKey { key: Key::V, pasted: Some(Pasted::Text(text.clone())) },
                egui::Event::PasteImage(img) => ClipboardKey { key: Key::V, pasted: Some(Pasted::Image(color_image_to_rgba(img))) },
                _ => return true,
            };
            keys.push(key);
            false
        });
        keys
    })
}

fn color_image_to_rgba(img: &egui::ColorImage) -> RgbaImage {
    let [w, h] = img.size;
    let bytes = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    RgbaImage::from_raw(w as u32, h as u32, bytes).unwrap_or_default()
}

/// What is on the system clipboard right now, for the right-click Paste here.
fn read_clipboard() -> Option<Pasted> {
    let mut c = arboard::Clipboard::new().ok()?;
    if let Ok(t) = c.get_text().map(|t| t.replace("\r\n", "\n")) {
        if !t.trim().is_empty() {
            return Some(Pasted::Text(t));
        }
    }
    let img = c.get_image().ok()?;
    RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()).map(Pasted::Image)
}

/// An annotation copied with Ctrl+C. `marker` is the text put on the system
/// clipboard with it: Ctrl+V needs something there to fire at all, and if the
/// clipboard no longer holds the marker, something else was copied since.
/// `pastes` moves each paste a bit further.
struct CopiedItem {
    shape: Shape,
    style: Style,
    marker: String,
    pastes: u32,
}

static ITEM_CLIPBOARD: std::sync::Mutex<Option<CopiedItem>> = std::sync::Mutex::new(None);
static COPIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// How far each paste lands from the last, in image pixels.
const PASTE_OFFSET: f32 = 16.0;

pub struct Editor {
    pub(crate) doc: Document,
    title: String,
    tool: Tool,
    pub(crate) style: Style,
    obfuscate: ObfuscateMode,
    pub(crate) selected: Option<u64>,
    drag: Option<Drag>,
    text: Option<TextEdit>,
    crop: Option<Bounds>,
    /// `None` fits the image to the window.
    zoom: Option<f32>,
    texture: Option<TextureHandle>,
    /// What the canvas texture shows: document version, frame, overlay style.
    shown: Option<(u64, usize, Option<mythicshot_editor::overlays::OverlayStyle>)>,
    /// Base plus every item except the one being dragged, built when a drag starts.
    drag_cache: Option<Pixmap>,
    pub keys: BTreeMap<EditorAction, String>,
    /// Output folder, pattern and format from settings; `title` is this capture's.
    pub(crate) dest: Context,
    saved: Option<PathBuf>,
    /// Last action and the time it happened, shown in the status bar.
    status: Option<(String, String)>,
    pub closed: bool,
    confirm_close: bool,
    /// Where the canvas was right-clicked, for the menu's Paste here.
    menu_at: Option<Pt>,
    /// Where the pointer is over the picture, for Ctrl+V of text or images.
    hover: Option<Pt>,
    /// Shift is held: resize pictures freely.
    shift: bool,
    /// When the property panel last checkpointed, so one slider drag is one undo step.
    prop_checkpoint: Option<(u64, Instant)>,
    last_canvas: CanvasGeom,
    /// Set when the editor holds a recording instead of a still image.
    pub(crate) video: Option<crate::video_editor::VideoState>,
    /// Remembered styles and choices; the app saves them when `prefs_changed`.
    pub prefs: crate::settings::EditorPrefs,
    pub prefs_changed: bool,
    /// A new default overlay style chosen in the GIF editor, for the app to save.
    pub overlay_default: Option<mythicshot_editor::overlays::OverlayStyle>,
}

impl Editor {
    pub fn new(img: RgbaImage, title: String, keys: BTreeMap<EditorAction, String>, dest: Context, prefs: crate::settings::EditorPrefs) -> Self {
        let tool = Tool::from_key(&prefs.tool).filter(|t| !matches!(t, Tool::Crop | Tool::Spotlight | Tool::Card)).unwrap_or(Tool::Rectangle);
        let mut editor = Self {
            doc: Document::new(img),
            title,
            tool: Tool::Rectangle,
            style: Style::default(),
            obfuscate: ObfuscateMode::Pixelate,
            selected: None,
            drag: None,
            text: None,
            crop: None,
            zoom: None,
            texture: None,
            shown: None,
            drag_cache: None,
            keys,
            dest,
            saved: None,
            status: None,
            closed: false,
            confirm_close: false,
            menu_at: None,
            hover: None,
            shift: false,
            prop_checkpoint: None,
            last_canvas: CanvasGeom { min: pos2(0.0, 0.0), zoom: 1.0 },
            video: None,
            prefs,
            prefs_changed: false,
            overlay_default: None,
        };
        editor.tool = tool;
        editor.load_tool_style(tool);
        // Test hook: open with the close prompt showing, to check its layout.
        #[cfg(debug_assertions)]
        // Test hook: start with a rectangle selected.
        #[cfg(debug_assertions)]
        if std::env::var_os("MYTHICSHOT_TEST_SELECT_RECT").is_some() {
            let id = editor.add_item(Shape::Rect(Bounds::new(mythicshot_editor::pt(40.0, 40.0), mythicshot_editor::pt(200.0, 140.0))));
            editor.tool = Tool::Select;
            editor.selected = Some(id);
        }
        if std::env::var_os("MYTHICSHOT_TEST_CONFIRM_CLOSE").is_some() {
            editor.confirm_close = true;
        }
        editor
    }

    /// Show the remembered style of `tool` in the properties row.
    fn load_tool_style(&mut self, tool: Tool) {
        if let Some(s) = self.prefs.styles.get(tool.key()) {
            self.style = Style { stroke: Rgba(s.stroke), fill: s.fill.map(Rgba), width: s.width, font_size: s.font_size, shadow: s.shadow };
            self.obfuscate = if s.blur { ObfuscateMode::Blur } else { ObfuscateMode::Pixelate };
        } else if tool != Tool::Select {
            self.style = default_style(tool);
            self.obfuscate = ObfuscateMode::Pixelate;
        }
    }

    /// Remember the current properties as `tool`'s style.
    fn remember_style(&mut self, tool: Tool) {
        if matches!(tool, Tool::Select | Tool::Crop) {
            return;
        }
        let st = self.style;
        let saved = crate::settings::SavedStyle { stroke: st.stroke.0, fill: st.fill.map(|f| f.0), width: st.width, font_size: st.font_size, shadow: st.shadow, blur: self.obfuscate == ObfuscateMode::Blur };
        if self.prefs.styles.get(tool.key()) != Some(&saved) {
            self.prefs.styles.insert(tool.key().to_string(), saved);
            self.prefs_changed = true;
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// New output settings from the app; keeps this capture's own title.
    pub fn set_output(&mut self, mut ctx: Context) {
        ctx.title = std::mem::take(&mut self.dest.title);
        self.dest = ctx;
    }

    pub(crate) fn notify(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), chrono::Local::now().format("%H:%M:%S").to_string()));
    }

    /// The Guide card tool is the current tool.
    pub(crate) fn tool_is_card(&self) -> bool {
        self.tool == Tool::Card
    }

    /// Back to the Select tool, for when the Guide card tool cannot be used.
    pub(crate) fn use_select_tool(&mut self) {
        self.set_tool(Tool::Select);
    }

    pub(crate) fn text_editing(&self) -> bool {
        self.text.is_some()
    }

    /// Items on the canvas now: all of them for an image, those whose frame
    /// range includes the current frame for a recording, and in the Tour view
    /// those plus the items pinned to the step.
    pub(crate) fn display_items(&self) -> Vec<Item> {
        match &self.video {
            Some(v) if v.view == crate::video_editor::VideoView::Tour => crate::video::step_items(self.doc.items(), v.current),
            Some(v) => crate::video::visible(self.doc.items(), v.current),
            None => self.doc.items().to_vec(),
        }
    }

    /// Topmost item under `p` among those on the canvas now.
    fn hit(&self, p: Pt, tol: f32) -> Option<u64> {
        self.display_items().iter().rev().find(|i| i.hit(p, tol)).map(|i| i.id)
    }

    /// Add an item; on a recording it shows from the current frame for a while,
    /// and in the Tour view it is pinned to the step.
    pub(crate) fn add_item(&mut self, shape: Shape) -> u64 {
        if let Some(src) = self.tour_src() {
            return self.add_pinned(shape, src);
        }
        let id = self.doc.add(shape, self.style);
        if let Some(r) = self.default_range() {
            self.doc.update(id, |it| it.frames = Some(r));
        }
        id
    }

    pub(crate) fn select(&mut self, id: Option<u64>) {
        if id.is_some() {
            self.select_card(false);
        }
        self.selected = id;
        if let Some(it) = id.and_then(|i| self.doc.item(i)) {
            self.style = it.style;
            if let Shape::Obfuscate { mode, .. } = it.shape {
                self.obfuscate = mode;
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            let unexported = self.video.as_ref().is_some_and(|v| !v.exported);
            if self.doc.dirty || unexported {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm_close = true;
            } else {
                self.closed = true;
            }
        }
        self.shift = ctx.input(|i| i.modifiers.shift);
        // A pick for the guide card's spot ends when the window loses the keyboard.
        if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false)))) {
            self.cancel_card_pick();
        }
        // A press off the guide card ends typing on it before anything else sees the press.
        let press = ctx.input(|i| if i.pointer.any_pressed() { i.pointer.press_origin() } else { None });
        self.end_card_typing_on_press(&ctx, press);
        // While the "Publish tour" dialog is open it gets the keys, and the editor behind it is inert.
        let modal = self.publish_picker_open();
        if !modal {
            self.video_keys(&ctx);
            self.shortcuts(&ctx);
        }
        self.video_tick(&ctx);
        if modal {
            ui.disable();
        }
        self.toolbar(ui);
        self.status_bar(ui);
        self.timeline(ui);
        self.tool_rail(ui);
        self.canvas(ui);
        self.text_editor(&ctx);
        if self.confirm_close {
            self.confirm_close_dialog(&ctx);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if self.card_escape() {
                // The guide card stopped typing or was unselected.
            } else if self.text.take().is_none() && self.crop.take().is_none() && self.drag.take().is_none() {
                self.select(None);
            }
            self.drag_cache = None;
        }
        if self.text.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        if self.crop.is_some() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            self.apply_crop();
            return;
        }
        // Shortcuts with modifiers first, so Ctrl+C is not taken as C.
        let mut bound: Vec<(EditorAction, Modifiers, Key)> = self
            .keys
            .iter()
            .filter_map(|(a, b)| hotkeys::egui_shortcut(b).map(|(m, k)| (*a, m, k)))
            .collect();
        bound.sort_by_key(|(_, m, _)| std::cmp::Reverse(m.ctrl as u8 + m.alt as u8 + m.shift as u8 + m.mac_cmd as u8));
        let mut clipboard_keys = take_clipboard_keys(ctx);
        for (action, m, k) in bound {
            let event = clipboard_keys.iter().position(|c| c.key == k && m.ctrl && !m.alt && !m.shift);
            if let Some(i) = event.filter(|_| action == EditorAction::Paste) {
                if let Some(pasted) = clipboard_keys.swap_remove(i).pasted {
                    self.paste(pasted, self.hover);
                }
                break;
            }
            if event.is_some() || ctx.input_mut(|i| i.consume_key(m, k)) {
                self.run(action);
                break;
            }
        }
    }

    fn run(&mut self, action: EditorAction) {
        if let Some(tool) = Tool::ALL.into_iter().find(|t| t.action() == action) {
            // The screenshot editor has no Spotlight tool, and guide cards live in the Tour view.
            let usable = match tool {
                Tool::Spotlight => self.video.is_some(),
                Tool::Card => self.in_tour_view(),
                _ => true,
            };
            if usable {
                self.set_tool(tool);
            }
            return;
        }
        match action {
            EditorAction::Undo => {
                self.doc.undo();
                self.fix_selection();
                self.reload_frame();
            }
            EditorAction::Redo => {
                self.doc.redo();
                self.fix_selection();
                self.reload_frame();
            }
            EditorAction::Delete => {
                if let Some(id) = self.selected.take() {
                    self.doc.remove(id);
                } else if self.card_selected() {
                    self.remove_card();
                }
            }
            EditorAction::Copy if self.selected.is_some() => self.copy_item(),
            EditorAction::Cut => {
                if self.selected.is_some() {
                    self.copy_item();
                    self.run(EditorAction::Delete);
                }
            }
            EditorAction::Paste => self.paste_item(None),
            EditorAction::Duplicate => self.duplicate(),
            EditorAction::Copy if self.video.is_some() => self.copy_frame(),
            EditorAction::Save if self.video.is_some() => self.start_export(None),
            EditorAction::Copy => self.copy(),
            EditorAction::Save => self.save(),
            _ => {}
        }
    }

    fn set_tool(&mut self, tool: Tool) {
        if tool != self.tool {
            // Another tool ends a pick for the guide card's spot.
            self.cancel_card_pick();
        }
        self.tool = tool;
        self.crop = None;
        if tool != Tool::Select {
            self.selected = None;
            self.load_tool_style(tool);
        }
        if !matches!(tool, Tool::Select | Tool::Card) {
            self.select_card(false);
        }
        if tool != Tool::Crop && self.prefs.tool != tool.key() {
            self.prefs.tool = tool.key().to_string();
            self.prefs_changed = true;
        }
    }

    fn fix_selection(&mut self) {
        if self.selected.is_some_and(|id| self.doc.item(id).is_none()) {
            self.selected = None;
        }
    }

    fn copy(&mut self) {
        self.send(DestId::Clipboard);
    }

    /// Copy the selected annotation to Mythicshot's own clipboard, shared by
    /// every editor window, with a marker on the system clipboard.
    fn copy_item(&mut self) {
        let Some(it) = self.selected.and_then(|id| self.doc.item(id)) else { return };
        let n = COPIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let marker = format!("Mythicshot annotation {n}");
        // Tests leave the real clipboard alone.
        if !cfg!(test) {
            let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(marker.clone()));
        }
        if let Ok(mut c) = ITEM_CLIPBOARD.lock() {
            *c = Some(CopiedItem { shape: it.shape.clone(), style: it.style, marker, pastes: 0 });
        }
        self.notify("Copied the annotation");
    }

    /// Paste the copied annotation, a little further down and right each time.
    /// `clipboard` is the system clipboard's text when Ctrl+V came from egui's
    /// paste event; nothing is pasted if it is not this copy's marker.
    fn paste_item(&mut self, clipboard: Option<&str>) {
        let Some((shape, style, n)) = ITEM_CLIPBOARD.lock().ok().and_then(|mut c| {
            let c = c.as_mut().filter(|c| clipboard.is_none_or(|t| t == c.marker))?;
            c.pastes += 1;
            Some((c.shape.clone(), c.style, c.pastes))
        }) else {
            return;
        };
        self.add_copy(shape, style, n);
    }

    /// The canvas right-click menu: what can be done to the annotation under
    /// the pointer, or to the picture when there is none.
    fn context_menu(&mut self, ui: &mut egui::Ui) {
        let hint = |ed: &Self, a: EditorAction| hotkeys::display(ed.keys.get(&a).map(String::as_str).unwrap_or(""));
        let item = |ui: &mut egui::Ui, label: &str, shortcut: String| ui.add(egui::Button::new(label).shortcut_text(shortcut)).clicked();
        let can_paste = ITEM_CLIPBOARD.lock().is_ok_and(|c| c.is_some()) || read_clipboard().is_some();
        if let Some(id) = self.selected {
            if item(ui, "Copy", hint(self, EditorAction::Copy)) {
                self.copy_item();
                ui.close();
            }
            if item(ui, "Cut", hint(self, EditorAction::Cut)) {
                self.run(EditorAction::Cut);
                ui.close();
            }
            if item(ui, "Duplicate", hint(self, EditorAction::Duplicate)) {
                self.duplicate();
                ui.close();
            }
            if ui.add_enabled(can_paste, egui::Button::new("Paste here")).clicked() {
                self.paste_here();
                ui.close();
            }
            ui.separator();
            if self.doc.item(id).and_then(|it| it.shape.text()).is_some() && ui.button("Edit text").clicked() {
                self.edit_text(id);
                ui.close();
            }
            if ui.button("Bring to front").clicked() {
                self.doc.bring_to_front(id);
                ui.close();
            }
            if item(ui, "Delete", hint(self, EditorAction::Delete)) {
                self.run(EditorAction::Delete);
                ui.close();
            }
        } else {
            if ui.add_enabled(can_paste, egui::Button::new("Paste here").shortcut_text(hint(self, EditorAction::Paste))).clicked() {
                self.paste_here();
                ui.close();
            }
            let copy_label = if self.video.is_some() { "Copy frame" } else { "Copy image" };
            if item(ui, copy_label, hint(self, EditorAction::Copy)) {
                self.run(EditorAction::Copy);
                ui.close();
            }
        }
    }

    /// Paste the copied annotation with its top-left corner where the canvas
    /// was right-clicked.
    fn paste_here(&mut self) {
        match read_clipboard() {
            Some(pasted) => self.paste(pasted, self.menu_at),
            // The clipboard can't be read: fall back to Mythicshot's own copy.
            None => self.paste_annotation_at(self.menu_at),
        }
    }

    /// Paste what came from the system clipboard, at `at` (image pixels) if
    /// given: Mythicshot's own annotation (its marker), text as a text
    /// annotation, or a picture as an image element.
    fn paste(&mut self, pasted: Pasted, at: Option<Pt>) {
        match pasted {
            Pasted::Text(t) if ITEM_CLIPBOARD.lock().is_ok_and(|c| c.as_ref().is_some_and(|c| c.marker == t)) => {
                if at.is_some() && at == self.menu_at { self.paste_annotation_at(at) } else { self.paste_item(Some(&t)) }
            }
            Pasted::Text(t) if !t.trim().is_empty() => self.paste_text(t.trim_end().to_string(), at),
            Pasted::Text(_) => {}
            Pasted::Image(img) => self.paste_image(&img, at),
        }
    }

    /// Mythicshot's copied annotation with its top-left corner at `at`.
    fn paste_annotation_at(&mut self, at: Option<Pt>) {
        let Some(at) = at else { return self.paste_item(None) };
        let Some((mut shape, style)) = ITEM_CLIPBOARD.lock().ok().and_then(|c| c.as_ref().map(|c| (c.shape.clone(), c.style))) else { return };
        let b = Item { id: 0, shape: shape.clone(), style, frames: None, pin: None }.bounds().norm();
        shape.translate(at.x - b.x0, at.y - b.y0);
        self.add_copy(shape, style, 0);
    }

    /// Text from another app becomes a text annotation in the text tool's
    /// style, starting at `at`, or centred on the picture.
    fn paste_text(&mut self, text: String, at: Option<Pt>) {
        let (current, obfuscate) = (self.style, self.obfuscate);
        self.load_tool_style(Tool::Text);
        let style = self.style;
        (self.style, self.obfuscate) = (current, obfuscate);
        let (w, h) = mythicshot_editor::measure_text(&text, style.font_size);
        let at = at.unwrap_or(pt((self.doc.width() as f32 - w) / 2.0, (self.doc.height() as f32 - h) / 2.0));
        self.add_copy(Shape::Text { at, text }, style, 0);
    }

    /// A picture from the clipboard becomes an image element, shrunk to fit
    /// inside the picture if it is bigger, starting at `at` or centred.
    fn paste_image(&mut self, img: &RgbaImage, at: Option<Pt>) {
        if img.width() == 0 || img.height() == 0 {
            return;
        }
        let (dw, dh) = (self.doc.width() as f32, self.doc.height() as f32);
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let scale = (dw * 0.9 / iw).min(dh * 0.9 / ih).min(1.0);
        let (w, h) = (iw * scale, ih * scale);
        let at = at.unwrap_or(pt((dw - w) / 2.0, (dh - h) / 2.0));
        let shape = Shape::Image { rect: Bounds::new(at, pt(at.x + w, at.y + h)), image: mythicshot_editor::ImageData::from_rgba(img) };
        self.add_copy(shape, Style::default(), 0);
    }

    /// Ctrl+D: a copy of the selection just beside it, without the clipboard.
    fn duplicate(&mut self) {
        let Some(it) = self.selected.and_then(|id| self.doc.item(id)) else { return };
        let (shape, style) = (it.shape.clone(), it.style);
        self.add_copy(shape, style, 1);
    }

    /// Add `shape` moved `n` steps down and right, and select it so it can be
    /// dragged into place.
    fn add_copy(&mut self, mut shape: Shape, style: Style, n: u32) {
        // Spotlights belong to recordings; one copied there can reach a screenshot editor through the shared clipboard.
        if matches!(shape, Shape::Spotlight { .. }) && self.video.is_none() {
            self.notify("Spotlights are for recordings only");
            return;
        }
        if self.drawing_blocked() {
            return;
        }
        let step = PASTE_OFFSET * n as f32;
        shape.translate(step, step);
        let previous = self.style;
        self.style = style;
        let id = self.add_item(shape);
        self.style = previous;
        self.set_tool(Tool::Select);
        self.select(Some(id));
    }

    /// Send the flattened image to a destination and report the result here.
    fn send(&mut self, d: DestId) {
        let result = dest::send(d, &self.doc.flatten(), &self.dest);
        self.sent(result);
    }

    fn sent(&mut self, result: Result<Sent, String>) {
        match result {
            Ok(Sent::Saved(p)) => self.saved_to(p),
            Ok(Sent::Cancelled) => {}
            Ok(Sent::Copied) => {
                // On the clipboard counts as kept: closing won't ask to save,
                // unless the picture changes again.
                self.doc.mark_saved();
                self.notify(Sent::Copied.describe());
            }
            Ok(sent) => self.notify(sent.describe()),
            Err(e) => self.notify(e),
        }
    }

    fn saved_to(&mut self, path: PathBuf) {
        self.doc.mark_saved();
        self.notify(format!("Saved {}", path.display()));
        self.title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.saved = Some(path);
    }

    /// Save over the last saved file, or to a new file named by the pattern.
    fn save(&mut self) {
        match self.saved.clone() {
            Some(p) => match dest::save(&self.doc.flatten(), &p, self.dest.format, self.dest.jpeg_quality) {
                Ok(()) => self.saved_to(p),
                Err(e) => self.notify(e),
            },
            None => self.send(DestId::File),
        }
    }

    fn apply_crop(&mut self) {
        if let Some(b) = self.crop.take() {
            match self.video.as_mut() {
                // A recording keeps every frame whole; the crop applies at export.
                Some(v) => v.edit.crop = Some(b.norm()),
                None => {
                    self.doc.crop(b);
                    self.fix_selection();
                }
            }
        }
    }

    fn effect(&mut self, f: impl FnOnce(&RgbaImage) -> RgbaImage) {
        self.doc.apply(f);
        self.selected = None;
    }

    // ---- panels -------------------------------------------------------------

    /// Two rows like Greenshot's editor: actions on top, then the properties of
    /// the selected item (or of the next one drawn).
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("editor-toolbar").frame(egui::Frame::new().fill(BAR_BG).inner_margin(egui::Margin::symmetric(6, 4))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if self.video.is_some() {
                    let exp = icons::button(ui, Icon::Export, ICON, false, "Export (GIF, APNG, MP4, WebM)");
                    egui::Popup::menu(&exp).show(|ui| self.export_menu(ui));
                    if icons::button(ui, Icon::Copy, ICON, false, "Copy this frame").clicked() {
                        self.copy_frame();
                    }
                    if icons::button(ui, Icon::Folder, ICON, false, "Open captures folder").clicked() {
                        crate::platform::open_folder(&self.dest.output_dir);
                    }
                } else if icons::button(ui, Icon::Save, ICON, false, &self.key_hint(EditorAction::Save)).clicked() {
                    self.save();
                }
                if self.video.is_none() {
                    if icons::button(ui, Icon::SaveAs, ICON, false, "Save as…").clicked() {
                        self.send(DestId::FileAs);
                    }
                    if icons::button(ui, Icon::Copy, ICON, false, &self.key_hint(EditorAction::Copy)).clicked() {
                        self.copy();
                    }
                    let send = icons::button(ui, Icon::Send, ICON, false, "Send to…");
                    egui::Popup::menu(&send).show(|ui| {
                        for (d, icon) in [(DestId::Print, Icon::Print), (DestId::Email, Icon::Email), (DestId::Clipboard, Icon::Copy)] {
                            if icons::menu_row(ui, icon, d.label()).on_hover_text(d.help()).clicked() {
                                self.send(d);
                                ui.close();
                            }
                        }
                    });
                    if icons::button(ui, Icon::Folder, ICON, false, "Open captures folder").clicked() {
                        crate::platform::open_folder(&self.dest.output_dir);
                    }
                }
                ui.separator();
                let (can_undo, can_redo) = (self.doc.can_undo(), self.doc.can_redo());
                if ui.add_enabled_ui(can_undo, |ui| icons::button(ui, Icon::Undo, ICON, false, &self.key_hint(EditorAction::Undo))).inner.clicked() {
                    self.run(EditorAction::Undo);
                }
                if ui.add_enabled_ui(can_redo, |ui| icons::button(ui, Icon::Redo, ICON, false, &self.key_hint(EditorAction::Redo))).inner.clicked() {
                    self.run(EditorAction::Redo);
                }
                ui.separator();
                if self.video.is_none() {
                    let fx = icons::button(ui, Icon::Effects, ICON, false, "Effects (flatten annotations into the image)");
                    egui::Popup::menu(&fx).show(|ui| self.effects_menu(ui));
                    ui.separator();
                }
                self.properties_row(ui);
            });
        });
    }

    fn effects_menu(&mut self, ui: &mut egui::Ui) {
        let mut pick: Option<Box<dyn FnOnce(&RgbaImage) -> RgbaImage>> = None;
        if ui.button("Drop shadow").clicked() {
            pick = Some(Box::new(effects::drop_shadow));
        }
        if ui.button("Torn edge").clicked() {
            pick = Some(Box::new(effects::torn_edge));
        }
        if ui.button("Border (current colour and width)").clicked() {
            let (w, c) = (self.style.width.round().max(1.0) as u32, self.style.stroke.0);
            pick = Some(Box::new(move |img: &RgbaImage| effects::border(img, w, c)));
        }
        ui.separator();
        if ui.button("Greyscale").clicked() {
            pick = Some(Box::new(effects::greyscale));
        }
        if ui.button("Invert").clicked() {
            pick = Some(Box::new(effects::invert));
        }
        ui.separator();
        ui.menu_button("Resize", |ui| {
            for p in [25, 50, 75, 125, 150, 200] {
                if ui.button(format!("{p}%")).clicked() {
                    pick = Some(Box::new(move |img: &RgbaImage| effects::resize(img, p)));
                }
            }
        });
        if ui.button("Rotate right").clicked() {
            pick = Some(Box::new(effects::rotate_cw));
        }
        if ui.button("Rotate left").clicked() {
            pick = Some(Box::new(effects::rotate_ccw));
        }
        if let Some(f) = pick {
            self.effect(f);
            ui.close();
        }
    }

    /// Colour, fill, width, text size, shadow and, for a selection, its actions.
    fn properties_row(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 6.0;
        // A guide card has no colours or widths of its own: the tour's style draws it.
        if self.in_tour_view() && (self.tool == Tool::Card || self.card_selected()) {
            return self.card_properties(ui);
        }
        let before = self.style;
        let before_mode = self.obfuscate;

        let mut c = to_color32(self.style.stroke);
        if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::OnlyBlend).on_hover_text("Line and text colour").changed() {
            self.style.stroke = from_color32(c);
        }
        let mut fill_on = self.style.fill.is_some();
        if ui.toggle_value(&mut fill_on, "Fill").on_hover_text("Fill rectangles, ellipses, text boxes and bubbles").changed() {
            self.style.fill = fill_on.then_some(Rgba([255, 255, 255, 255]));
        }
        if let Some(f) = self.style.fill {
            let mut c = to_color32(f);
            if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::OnlyBlend).on_hover_text("Fill colour").changed() {
                self.style.fill = Some(from_color32(c));
            }
        }
        ui.add(egui::DragValue::new(&mut self.style.width).range(1.0..=20.0).speed(0.1).suffix(" px")).on_hover_text("Line width (drag or type)");
        ui.add(egui::DragValue::new(&mut self.style.font_size).range(10.0..=96.0).speed(0.3).suffix(" pt")).on_hover_text("Text and step label size");
        if icons::button(ui, Icon::Shadow, ICON, self.style.shadow, "Shadow").clicked() {
            self.style.shadow = !self.style.shadow;
        }
        if self.tool == Tool::Obfuscate || self.selected_is_obfuscate() {
            egui::ComboBox::from_id_salt("obfuscate-mode")
                .width(80.0)
                .selected_text(if self.obfuscate == ObfuscateMode::Pixelate { "Pixelate" } else { "Blur" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.obfuscate, ObfuscateMode::Pixelate, "Pixelate");
                    ui.selectable_value(&mut self.obfuscate, ObfuscateMode::Blur, "Blur");
                });
        }

        if self.tool == Tool::Spotlight || self.selected_is_spotlight() {
            self.spotlight_slider(ui);
        }

        // Remember the change as the style of whatever tool it applies to: the
        // selected item's tool, or the current tool for the next item drawn.
        if self.style != before || self.obfuscate != before_mode {
            let owner = self.selected.and_then(|id| self.doc.item(id)).map(|it| Tool::for_shape(&it.shape)).unwrap_or(self.tool);
            self.remember_style(owner);
        }
        if let Some(id) = self.selected {
            if self.style != before || self.obfuscate != before_mode {
                let recent = self.prop_checkpoint.is_some_and(|(pid, at)| pid == id && at.elapsed() < Duration::from_millis(700));
                if !recent {
                    self.doc.checkpoint();
                }
                self.prop_checkpoint = Some((id, Instant::now()));
                let (style, mode) = (self.style, self.obfuscate);
                self.doc.update(id, |it| {
                    it.style = style;
                    if let Shape::Obfuscate { mode: m, .. } = &mut it.shape {
                        *m = mode;
                    }
                });
            }
            ui.separator();
            if self.doc.item(id).and_then(|it| it.shape.text()).is_some() && icons::button(ui, Icon::EditText, ICON, false, "Edit text").clicked() {
                self.edit_text(id);
            }
            if icons::button(ui, Icon::Front, ICON, false, "Bring to front").clicked() {
                self.doc.bring_to_front(id);
            }
            if icons::button(ui, Icon::Delete, ICON, false, &self.key_hint(EditorAction::Delete)).clicked() {
                self.run(EditorAction::Delete);
            }
        }
    }

    fn key_hint(&self, action: EditorAction) -> String {
        let b = self.keys.get(&action).map(String::as_str).unwrap_or("");
        if b.is_empty() { action.label().to_string() } else { format!("{}  ({})", action.label(), hotkeys::display(b)) }
    }

    /// A narrow column of tool icons, grouped like Greenshot's.
    fn tool_rail(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("editor-tools").resizable(false).exact_size(RAIL).frame(egui::Frame::new().fill(BAR_BG).inner_margin(egui::Margin::symmetric(4, 6))).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                for (title, tools) in RAIL_GROUPS {
                    if let Some(title) = title {
                        ui.add_space(4.0);
                        ui.vertical_centered(|ui| ui.label(egui::RichText::new(title).size(10.0).strong().color(ACCENT)));
                    }
                    let tools: Vec<Tool> = tools.iter().copied().filter(|t| !matches!(t, Tool::Spotlight | Tool::Card) || self.video.is_some()).collect();
                    for row in tools.chunks(2) {
                        ui.horizontal(|ui| {
                            for &tool in row {
                                self.rail_button(ui, tool);
                            }
                        });
                    }
                }
            });
        });
    }

    /// A tool button with its key in the corner, like a palette.
    fn rail_button(&mut self, ui: &mut egui::Ui, tool: Tool) {
        let binding = self.keys.get(&tool.action()).map(String::as_str).unwrap_or("");
        let tip = format!("{}\n{}", self.key_hint(tool.action()), tool_hint(tool));
        // Guide cards belong to tour steps: the tool works only in the Tour view.
        let usable = tool != Tool::Card || self.in_tour_view();
        let resp = ui.add_enabled_ui(usable, |ui| icons::button(ui, tool.icon(), TOOL_BUTTON, self.tool == tool, &tip)).inner.on_disabled_hover_text("Open the Tour view to add guide cards");
        let key = hotkeys::display(binding);
        if !key.is_empty() && key.chars().count() <= 2 {
            let corner = resp.rect.right_bottom() - vec2(2.0, 1.0);
            let color = if self.tool == tool { Color32::WHITE } else { Color32::from_rgb(140, 148, 164) };
            ui.painter().text(corner, egui::Align2::RIGHT_BOTTOM, key, FontId::proportional(9.0), color);
        }
        if resp.clicked() {
            self.set_tool(tool);
        }
    }

    /// Size, last action with its time, and zoom, like Greenshot's status bar.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("editor-status").frame(egui::Frame::new().fill(BAR_BG).inner_margin(egui::Margin::symmetric(8, 3))).show(ui, |ui| {
            ui.horizontal(|ui| {
                let muted = Color32::from_rgb(163, 170, 184);
                ui.label(egui::RichText::new(format!("{} × {}", self.doc.width(), self.doc.height())).monospace().color(muted));
                ui.separator();
                let dirty = match (&self.saved, self.doc.dirty) {
                    (None, _) => "Not saved yet",
                    (Some(_), true) => "Unsaved changes",
                    (Some(_), false) => "Saved",
                };
                if self.in_tour_view() {
                    ui.label(egui::RichText::new(crate::tour_view::STATUS).color(muted));
                    ui.separator();
                    self.card_undo_status(ui);
                }
                // A long message (an export path) is cut with an ellipsis before the zoom controls and shown whole on hover.
                let text = match &self.status {
                    Some((msg, clock)) => format!("{clock} – {msg}"),
                    None => dirty.to_string(),
                };
                ui.scope(|ui| {
                    ui.set_max_width(status_width(ui.available_width()));
                    // A cut label shows its whole text as a tooltip by itself.
                    ui.add(egui::Label::new(egui::RichText::new(&text).color(muted)).truncate());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let z = self.zoom.unwrap_or(self.last_canvas.zoom);
                    let label = if self.zoom.is_none() { format!("Fit {:.0}%", z * 100.0) } else { format!("{:.0}%", z * 100.0) };
                    egui::ComboBox::from_id_salt("zoom").width(84.0).selected_text(label).show_ui(ui, |ui| {
                        if ui.selectable_label(self.zoom.is_none(), "Fit to window").clicked() {
                            self.zoom = None;
                        }
                        for p in [25.0, 50.0, 75.0, 100.0, 150.0, 200.0, 400.0] {
                            if ui.selectable_label(self.zoom == Some(p / 100.0), format!("{p:.0}%")).clicked() {
                                self.zoom = Some(p / 100.0);
                            }
                        }
                    });
                    if icons::button(ui, Icon::Actual, 22.0, self.zoom == Some(1.0), "Actual size (100%)").clicked() {
                        self.zoom = Some(1.0);
                    }
                    if icons::button(ui, Icon::Fit, 22.0, self.zoom.is_none(), "Fit to window (Ctrl + mouse wheel zooms)").clicked() {
                        self.zoom = None;
                    }
                });
            });
        });
    }

    fn selected_is_spotlight(&self) -> bool {
        self.selected.and_then(|id| self.doc.item(id)).is_some_and(|it| matches!(it.shape, Shape::Spotlight { .. }))
    }

    /// "Spotlight · darken": one strength for every spotlight in the recording,
    /// one undo step per drag, remembered for the next spotlight and recording.
    fn spotlight_slider(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected.and_then(|id| self.doc.item(id)).and_then(|it| match it.shape {
            Shape::Spotlight { dim, .. } => Some(dim),
            _ => None,
        });
        let mut dim = selected.unwrap_or(self.prefs.spotlight_dim).clamp(MIN_DIM, MAX_DIM);
        ui.label("Spotlight · darken");
        let resp = ui.add(egui::Slider::new(&mut dim, MIN_DIM..=MAX_DIM).suffix("%")).on_hover_text("How dark everything outside a spotlight gets. One strength for every spotlight.");
        let dragging = self.video.as_ref().is_some_and(|v| v.tour.dim_dragging);
        if resp.changed() {
            self.apply_spotlight_dim(dim, !dragging);
        }
        if let Some(v) = self.video.as_mut() {
            v.tour.dim_dragging = resp.dragged();
        }
    }

    /// Set every spotlight in the recording to `dim`, in a new undo step when `checkpoint`, and remember it.
    pub(crate) fn apply_spotlight_dim(&mut self, dim: u8, checkpoint: bool) {
        let ids: Vec<u64> = self.doc.items().iter().filter(|it| matches!(it.shape, Shape::Spotlight { .. })).map(|it| it.id).collect();
        if !ids.is_empty() && checkpoint {
            self.doc.checkpoint();
        }
        for id in ids {
            self.doc.update(id, |it| {
                if let Shape::Spotlight { dim: d, .. } = &mut it.shape {
                    *d = dim;
                }
            });
        }
        if self.prefs.spotlight_dim != dim {
            self.prefs.spotlight_dim = dim;
            self.prefs_changed = true;
        }
    }

    fn selected_is_obfuscate(&self) -> bool {
        self.selected.and_then(|id| self.doc.item(id)).is_some_and(|it| matches!(it.shape, Shape::Obfuscate { .. }))
    }

    // ---- canvas -------------------------------------------------------------

    fn canvas(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS_BG)).show(ui, |ui| {
            let (w, h) = (self.doc.width() as f32, self.doc.height() as f32);
            let avail = ui.available_size();
            let fit = ((avail.x - 32.0) / w).min((avail.y - 32.0) / h).clamp(0.05, 1.0);
            // Ctrl + wheel zooms.
            let wheel = ui.input(|i| if i.modifiers.command && ui.is_enabled() { i.smooth_scroll_delta.y } else { 0.0 });
            if wheel != 0.0 {
                let z = self.zoom.unwrap_or(fit);
                self.zoom = Some((z * (1.0 + wheel * 0.002)).clamp(0.05, 8.0));
            }
            let z = self.zoom.unwrap_or(fit);
            self.refresh_texture(ui.ctx());
            egui::ScrollArea::both().auto_shrink(false).scroll_source(egui::scroll_area::ScrollSource { mouse_wheel: wheel == 0.0, ..Default::default() }).show(ui, |ui| {
                let size = vec2(w * z, h * z);
                let avail = ui.available_size();
                let total = vec2(size.x.max(avail.x), size.y.max(avail.y));
                let (rect, resp) = ui.allocate_exact_size(total, Sense::click_and_drag());
                let img_rect = Rect::from_min_size(rect.min + ((total - size) / 2.0).max(Vec2::ZERO), size);
                let to_img = |p: egui::Pos2| pt((p.x - img_rect.min.x) / z, (p.y - img_rect.min.y) / z);
                let to_scr = |p: Pt| img_rect.min + vec2(p.x * z, p.y * z);

                if let Some(tex) = &self.texture {
                    ui.painter().image(tex.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                ui.painter().rect_stroke(img_rect, 0.0, Stroke::new(1.0, Color32::from_gray(60)), StrokeKind::Outside);

                self.hover = resp.hover_pos().map(to_img).filter(|p| p.x >= 0.0 && p.y >= 0.0 && p.x <= w && p.y <= h);
                self.handle_pointer(&resp, z, &to_img);
                // Right-click selects what is under the pointer, then shows the menu.
                if resp.secondary_clicked() {
                    if let Some(p) = resp.interact_pointer_pos().map(to_img) {
                        let hit = self.hit(p, 6.0 / z);
                        if hit.is_some() {
                            self.set_tool(Tool::Select);
                        }
                        self.select(hit);
                        self.menu_at = Some(p);
                    }
                }
                resp.context_menu(|ui| self.context_menu(ui));
                self.draw_chrome(ui, img_rect, z, &to_scr);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(self.cursor(resp.hover_pos().map(to_img), z));
                }
            });
        });
    }

    fn cursor(&self, p: Option<Pt>, z: f32) -> CursorIcon {
        match self.tool {
            Tool::Select => match (p, self.selected) {
                (Some(p), Some(id)) if self.handle_at(id, p, z).is_some() => CursorIcon::Grab,
                (Some(p), _) if self.hit(p, 6.0 / z).is_some() => CursorIcon::Move,
                _ => CursorIcon::Default,
            },
            Tool::Text => CursorIcon::Text,
            _ => CursorIcon::Crosshair,
        }
    }

    fn handle_pointer(&mut self, resp: &egui::Response, z: f32, to_img: &dyn Fn(egui::Pos2) -> Pt) {
        let tol = 6.0 / z;
        if resp.drag_started() {
            // Where the button went down, not where the pointer is once it has
            // moved far enough to count as a drag, so quick drags still catch
            // a small handle.
            let origin = resp.ctx.input(|i| i.pointer.press_origin());
            if let Some(p) = origin.or(resp.interact_pointer_pos()).map(to_img) {
                self.start_drag(p, z, tol);
            }
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos().map(to_img) {
                self.continue_drag(p);
            }
        }
        if resp.drag_stopped() {
            self.finish_drag();
        }
        if resp.double_clicked() {
            if let Some(id) = resp.interact_pointer_pos().map(to_img).and_then(|p| self.hit(p, tol)) {
                if self.doc.item(id).and_then(|it| it.shape.text()).is_some() {
                    self.select(Some(id));
                    self.edit_text(id);
                    return;
                }
            }
        }
        if resp.clicked() {
            let Some(p) = resp.interact_pointer_pos().map(to_img) else { return };
            // A picked spot, a click on the guide card, or the Guide card tool.
            if self.in_tour_view() && self.card_click(p) {
                return;
            }
            match self.tool {
                Tool::Text => match self.hit(p, tol).filter(|id| matches!(self.doc.item(*id).map(|i| &i.shape), Some(Shape::Text { .. }))) {
                    Some(id) => self.edit_text(id),
                    None if self.drawing_blocked() => {}
                    None => self.text = Some(TextEdit { item: None, at: p, buffer: String::new(), focused: false, step: self.tour_src() }),
                },
                Tool::Step if self.drawing_blocked() => {}
                Tool::Step => {
                    let n = self.doc.next_step_number();
                    let id = self.add_item(Shape::Step { center: p, number: n });
                    self.selected = Some(id);
                }
                _ => {
                    let hit = self.hit(p, tol);
                    if self.tool == Tool::Select || hit.is_some() {
                        if hit.is_some() && self.tool != Tool::Select {
                            self.tool = Tool::Select;
                        }
                        self.select(hit);
                    }
                }
            }
        }
    }

    fn start_drag(&mut self, p: Pt, z: f32, tol: f32) {
        self.text = None;
        match self.tool {
            Tool::Select => {
                let handle = self.selected.and_then(|id| self.handle_at(id, p, z).map(|h| (id, h)));
                if let Some((id, h)) = handle {
                    self.doc.checkpoint();
                    self.drag = Some(match h {
                        HandleKind::Corner(i) => Drag::Resize { id, handle: i },
                        HandleKind::End(e) => Drag::Endpoint { id, end: e },
                        HandleKind::Tail => Drag::Tail { id },
                    });
                    self.build_cache(Some(id));
                } else if let Some(id) = self.hit(p, tol) {
                    self.select(Some(id));
                    self.doc.checkpoint();
                    self.drag = Some(Drag::Move { id, last: p });
                    self.build_cache(Some(id));
                } else {
                    self.select(None);
                }
            }
            Tool::Crop => self.drag = Some(Drag::Crop { start: p, now: p }),
            // Tour view without a step on this frame: nothing to pin a new item to.
            _ if self.drawing_blocked() => {}
            Tool::Freehand => {
                self.drag = Some(Drag::Freehand(vec![p]));
                self.build_cache(None);
            }
            Tool::Text | Tool::Step | Tool::Card => {}
            _ => {
                self.drag = Some(Drag::Create { start: p, now: p });
                self.build_cache(None);
            }
        }
    }

    fn continue_drag(&mut self, p: Pt) {
        let p = pt(p.x.clamp(-50.0, self.doc.width() as f32 + 50.0), p.y.clamp(-50.0, self.doc.height() as f32 + 50.0));
        match &mut self.drag {
            Some(Drag::Create { now, .. }) | Some(Drag::Crop { now, .. }) => *now = p,
            Some(Drag::Freehand(ps)) => {
                if ps.last().is_none_or(|l| (l.x - p.x).abs() + (l.y - p.y).abs() >= 1.5) {
                    ps.push(p);
                }
            }
            Some(Drag::Move { id, last }) => {
                let (dx, dy, id) = (p.x - last.x, p.y - last.y, *id);
                *last = p;
                self.doc.update(id, |it| it.shape.translate(dx, dy));
            }
            Some(Drag::Resize { id, handle }) => {
                let (id, h, free) = (*id, *handle, self.shift);
                self.doc.update(id, |it| {
                    if let Some(b) = it.shape.rect() {
                        let mut nb = resize_bounds(b.norm(), h, p);
                        // Pictures keep their shape from a corner unless Shift is held.
                        match &it.shape {
                            Shape::Image { image, .. } if !free => nb = keep_aspect(nb, h, image.width() as f32 / image.height().max(1) as f32),
                            Shape::Circle(_) | Shape::Donut(_) => nb = keep_square(nb, h),
                            _ => {}
                        }
                        it.shape.set_rect(nb);
                    }
                });
            }
            Some(Drag::Endpoint { id, end }) => {
                let (id, end) = (*id, *end);
                self.doc.update(id, |it| {
                    if let Shape::Line(a, b) | Shape::Arrow(a, b) | Shape::Cone(a, b) | Shape::Slice(a, b) = &mut it.shape {
                        *(if end == 0 { a } else { b }) = p;
                    }
                });
            }
            Some(Drag::Tail { id }) => {
                let id = *id;
                self.doc.update(id, |it| {
                    if let Shape::Bubble { tail, .. } = &mut it.shape {
                        *tail = p;
                    }
                });
            }
            None => {}
        }
    }

    fn finish_drag(&mut self) {
        let drag = self.drag.take();
        self.drag_cache = None;
        match drag {
            Some(Drag::Create { start, now }) => {
                if let Some(shape) = self.shape_from_drag(start, now) {
                    let is_bubble = matches!(shape, Shape::Bubble { .. });
                    let id = self.add_item(shape);
                    self.selected = Some(id);
                    if is_bubble {
                        self.edit_text(id);
                    }
                }
            }
            Some(Drag::Freehand(ps)) if ps.len() > 1 => {
                let id = self.add_item(Shape::Freehand(ps));
                self.selected = Some(id);
            }
            Some(Drag::Crop { start, now }) => {
                let b = Bounds::new(start, now);
                if b.width() > 3.0 && b.height() > 3.0 {
                    self.crop = Some(b.norm());
                }
            }
            _ => {}
        }
    }

    fn shape_from_drag(&self, a: Pt, b: Pt) -> Option<Shape> {
        if (a.x - b.x).abs() + (a.y - b.y).abs() < 4.0 {
            return None;
        }
        let r = Bounds::new(a, b).norm();
        Some(match self.tool {
            Tool::Rectangle => Shape::Rect(r),
            Tool::Ellipse => Shape::Ellipse(r),
            Tool::Line => Shape::Line(a, b),
            Tool::Arrow => Shape::Arrow(a, b),
            Tool::Circle => Shape::Circle(square_from(a, b)),
            Tool::Donut => Shape::Donut(square_from(a, b)),
            Tool::Group => Shape::Group(r),
            Tool::Cross => Shape::Cross(r),
            Tool::Asterisk => Shape::Asterisk(r),
            Tool::Cone => Shape::Cone(a, b),
            Tool::Slice => Shape::Slice(a, b),
            Tool::Highlight => Shape::Highlight(r),
            Tool::Obfuscate => Shape::Obfuscate { rect: r, mode: self.obfuscate },
            Tool::Spotlight => Shape::Spotlight { rect: r, dim: self.prefs.spotlight_dim.clamp(MIN_DIM, MAX_DIM) },
            Tool::Bubble => Shape::Bubble { rect: r, tail: pt(r.x0 + r.width() * 0.25, r.y1 + (r.height() * 0.5).max(30.0)), text: String::new() },
            _ => return None,
        })
    }

    fn edit_text(&mut self, id: u64) {
        if let Some(it) = self.doc.item(id) {
            let at = match &it.shape {
                Shape::Text { at, .. } => *at,
                Shape::Bubble { rect, .. } => pt(rect.norm().x0, rect.norm().y0),
                _ => return,
            };
            self.text = Some(TextEdit { item: Some(id), at, buffer: it.shape.text().unwrap_or_default().to_string(), focused: false, step: None });
        }
    }

    fn commit_text(&mut self) {
        let Some(t) = self.text.take() else { return };
        match t.item {
            Some(id) => {
                if self.doc.item(id).and_then(|it| it.shape.text()) != Some(t.buffer.as_str()) {
                    self.doc.checkpoint();
                    self.doc.update(id, |it| it.shape.set_text(t.buffer));
                }
            }
            None if !t.buffer.trim().is_empty() => {
                let shape = Shape::Text { at: t.at, text: t.buffer };
                let id = match t.step {
                    // Pinned to the step it was typed on, even if another step is shown now.
                    Some(src) => self.add_pinned(shape, src),
                    None if self.drawing_blocked() => return,
                    None => self.add_item(shape),
                };
                self.selected = Some(id);
            }
            None => {}
        }
    }

    fn text_editor(&mut self, ctx: &egui::Context) {
        let Some(at) = self.text.as_ref().map(|t| t.at) else { return };
        // Enter commits; Shift+Enter adds a line.
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            return self.commit_text();
        }
        let screen = self.last_to_scr(at);
        let mut commit = false;
        egui::Area::new(egui::Id::unique("editor-text")).fixed_pos(screen + vec2(0.0, -4.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let t = self.text.as_mut().expect("text edit");
                let size = (self.style.font_size * self.zoom.unwrap_or(1.0)).clamp(12.0, 40.0);
                let resp = ui.add(egui::TextEdit::multiline(&mut t.buffer).desired_width(280.0).desired_rows(2).font(FontId::proportional(size)).hint_text("Type, then Enter"));
                if !t.focused {
                    resp.request_focus();
                    t.focused = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Done").clicked() {
                        commit = true;
                    }
                    ui.label(egui::RichText::new("Enter done · Shift+Enter new line · Esc cancel").weak().size(11.0));
                });
            });
        });
        if commit {
            self.commit_text();
        }
    }

    /// Screen position of an image point using the canvas from the last frame.
    fn last_to_scr(&self, p: Pt) -> egui::Pos2 {
        let c = self.last_canvas;
        c.min + vec2(p.x * c.zoom, p.y * c.zoom)
    }

    fn confirm_close_dialog(&mut self, ctx: &egui::Context) {
        let is_video = self.video.is_some();
        // Roomy and centred: the title, the message, then three wide buttons
        // with space between them, Save (or Export) as the main one.
        const BUTTON: egui::Vec2 = egui::vec2(132.0, 36.0);
        const GAP: f32 = 14.0;
        let modal = egui::Modal::new(egui::Id::unique("confirm-close")).frame(egui::Frame::popup(&ctx.global_style()).inner_margin(24.0)).show(ctx, |ui| {
            ui.set_width(3.0 * BUTTON.x + 2.0 * GAP + 24.0);
            let (title, text) = if is_video {
                ("Export this recording?", "Closing without exporting deletes the recording.")
            } else {
                ("Save this capture?", "It has changes that are not saved yet.")
            };
            ui.vertical_centered(|ui| {
                ui.heading(title);
                ui.add_space(8.0);
                ui.label(text);
            });
            ui.add_space(22.0);
            let mut choice = None;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                let row = 3.0 * BUTTON.x + 2.0 * GAP;
                ui.add_space(((ui.available_width() - row) / 2.0).max(0.0));
                let primary = egui::Button::new(egui::RichText::new(if is_video { "Export and close" } else { "Save" }).strong()).fill(Color32::from_rgb(47, 111, 235)).min_size(BUTTON);
                if ui.add(primary).clicked() {
                    choice = Some(0);
                }
                if ui.add(egui::Button::new(if is_video { "Discard" } else { "Don't save" }).min_size(BUTTON)).clicked() {
                    choice = Some(1);
                }
                if ui.add(egui::Button::new("Cancel").min_size(BUTTON)).clicked() {
                    choice = Some(2);
                }
            });
            choice
        });
        match modal.inner {
            Some(0) if is_video => {
                if let Some(v) = self.video.as_mut() {
                    v.close_after_export = true;
                }
                self.start_export(None);
                self.confirm_close = false;
            }
            Some(0) => {
                self.save();
                if !self.doc.dirty {
                    self.closed = true;
                }
                self.confirm_close = false;
            }
            Some(1) => {
                self.closed = true;
                self.confirm_close = false;
            }
            Some(_) => self.confirm_close = false,
            None if modal.should_close() => self.confirm_close = false,
            None => {}
        }
    }

    // ---- rendering ----------------------------------------------------------

    fn build_cache(&mut self, exclude: Option<u64>) {
        let mut pm = to_pixmap(self.doc.base());
        let items: Vec<Item> = self.display_items().into_iter().filter(|i| Some(i.id) != exclude).collect();
        render_items_onto(&mut pm, &items);
        self.drag_cache = Some(pm);
    }

    /// Key and click overlays on a recording frame, the same way export draws them.
    /// None in the Tour view, which shows the step as exported, without pointer, ripple, halo or keys.
    fn draw_video_overlays(&self, pm: &mut Pixmap) {
        if let Some(v) = self.video.as_ref().filter(|v| v.view == crate::video_editor::VideoView::Timeline) {
            let meta = &v.rec.frames[v.current];
            mythicshot_editor::overlays::draw_overlays(pm, meta.t_ms, meta.cursor, &v.events, &v.edit.overlays);
        }
    }

    fn refresh_texture(&mut self, ctx: &egui::Context) {
        let pm = match (&self.drag, &self.drag_cache) {
            (Some(drag), Some(cache)) => {
                let extra: Option<Item> = match drag {
                    Drag::Create { start, now } => self.shape_from_drag(*start, *now).map(|shape| Item { id: 0, shape, style: self.style, frames: None, pin: None }),
                    Drag::Freehand(ps) => Some(Item { id: 0, shape: Shape::Freehand(ps.clone()), style: self.style, frames: None, pin: None }),
                    Drag::Move { id, .. } | Drag::Resize { id, .. } | Drag::Endpoint { id, .. } | Drag::Tail { id } => self.doc.item(*id).cloned(),
                    Drag::Crop { .. } => None,
                };
                let mut pm = drag_frame(self.doc.base(), cache, extra, || self.display_items());
                self.draw_video_overlays(&mut pm);
                self.shown = None;
                pm
            }
            _ => {
                // The Tour view draws no overlays and adds the step's pinned items: a different picture.
                let overlays = self.video.as_ref().filter(|v| v.view == crate::video_editor::VideoView::Timeline).map(|v| v.edit.overlays);
                let key = (self.doc.version, self.video.as_ref().map(|v| v.current).unwrap_or(0), overlays);
                if self.shown == Some(key) && self.texture.is_some() {
                    return;
                }
                let mut pm = to_pixmap(self.doc.base());
                render_items_onto(&mut pm, &self.display_items());
                self.draw_video_overlays(&mut pm);
                self.shown = Some(key);
                pm
            }
        };
        let img = ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
        match &mut self.texture {
            Some(t) if t.size() == [pm.width() as usize, pm.height() as usize] => t.set(img, TextureOptions::LINEAR),
            _ => self.texture = Some(ctx.load_texture("editor-canvas", img, TextureOptions::LINEAR)),
        }
    }

    fn draw_chrome(&mut self, ui: &egui::Ui, img_rect: Rect, z: f32, to_scr: &dyn Fn(Pt) -> egui::Pos2) {
        self.last_canvas = CanvasGeom { min: img_rect.min, zoom: z };
        let painter = ui.painter_at(ui.clip_rect());
        let scr_rect = |b: Bounds| {
            let n = b.norm();
            Rect::from_min_max(to_scr(pt(n.x0, n.y0)), to_scr(pt(n.x1, n.y1)))
        };
        // Crop preview: dim everything outside.
        let crop = match &self.drag {
            Some(Drag::Crop { start, now }) => Some(Bounds::new(*start, *now).norm()),
            _ => self.crop.or_else(|| self.video.as_ref().and_then(|v| v.edit.crop)),
        };
        if let Some(b) = crop {
            let r = scr_rect(b);
            let dim = Color32::from_black_alpha(150);
            painter.rect_filled(Rect::from_min_max(img_rect.min, pos2(img_rect.max.x, r.min.y)), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(img_rect.min.x, r.max.y), img_rect.max), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(img_rect.min.x, r.min.y), pos2(r.min.x, r.max.y)), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(r.max.x, r.min.y), pos2(img_rect.max.x, r.max.y)), 0.0, dim);
            painter.rect_stroke(r, 0.0, Stroke::new(2.0, ACCENT), StrokeKind::Outside);
            if self.crop.is_some() {
                let mut apply = false;
                let mut cancel = false;
                egui::Area::new(egui::Id::unique("crop-buttons")).fixed_pos(r.left_bottom() + vec2(0.0, 8.0)).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        apply = ui.button("Apply crop (Enter)").clicked();
                        cancel = ui.button("Cancel (Esc)").clicked();
                    });
                });
                if apply {
                    self.apply_crop();
                } else if cancel {
                    self.crop = None;
                }
            }
        }
        // Tour view: the step's hotspot as the tour draws it. Editor chrome, never exported.
        self.draw_tour_hotspot(ui.ctx(), &painter, to_scr);
        // Tour view: the step's guide card, drawn over the canvas and never into the frame.
        if self.in_tour_view() {
            self.draw_guide_card(ui, &painter, z, to_scr);
        }
        // Selection outline and handles.
        if let Some(it) = self.selected.and_then(|id| self.doc.item(id)) {
            let b = it.bounds();
            painter.rect_stroke(scr_rect(b).expand(3.0), 2.0, Stroke::new(1.0, ACCENT), StrokeKind::Outside);
            for (pos, _) in handles(it) {
                let c = to_scr(pos);
                painter.rect_filled(Rect::from_center_size(c, vec2(HANDLE, HANDLE)), 1.0, Color32::WHITE);
                painter.rect_stroke(Rect::from_center_size(c, vec2(HANDLE, HANDLE)), 1.0, Stroke::new(1.0, ACCENT), StrokeKind::Outside);
            }
        }
    }

    fn handle_at(&self, id: u64, p: Pt, z: f32) -> Option<HandleKind> {
        let it = self.doc.item(id)?;
        let r = HANDLE / z;
        handles(it).into_iter().find(|(h, _)| (h.x - p.x).abs() <= r && (h.y - p.y).abs() <= r).map(|(_, k)| k)
    }
}

#[derive(Clone, Copy)]
enum HandleKind {
    Corner(usize),
    End(usize),
    Tail,
}

/// Handle positions: eight around a box, two on a line, plus a bubble's tail.
fn handles(it: &Item) -> Vec<(Pt, HandleKind)> {
    match &it.shape {
        Shape::Line(a, b) | Shape::Arrow(a, b) | Shape::Cone(a, b) | Shape::Slice(a, b) => vec![(*a, HandleKind::End(0)), (*b, HandleKind::End(1))],
        shape => {
            let Some(b) = shape.rect().map(|b| b.norm()) else { return Vec::new() };
            let (mx, my) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
            let mut v: Vec<(Pt, HandleKind)> = [pt(b.x0, b.y0), pt(mx, b.y0), pt(b.x1, b.y0), pt(b.x1, my), pt(b.x1, b.y1), pt(mx, b.y1), pt(b.x0, b.y1), pt(b.x0, my)]
                .into_iter()
                .enumerate()
                .map(|(i, p)| (p, HandleKind::Corner(i)))
                .collect();
            if let Shape::Bubble { tail, .. } = shape {
                v.push((*tail, HandleKind::Tail));
            }
            v
        }
    }
}

/// The canvas while something is dragged: the cache (base plus every other
/// item) with the dragged item on top. A spotlight darkens the whole frame,
/// so dragging one redraws from the base with every item on the canvas
/// (`shown`) and the dragged one in its place, instead of darkening the cache again.
fn drag_frame(base: &RgbaImage, cache: &Pixmap, extra: Option<Item>, shown: impl FnOnce() -> Vec<Item>) -> Pixmap {
    match extra {
        Some(it) if matches!(it.shape, Shape::Spotlight { .. }) => {
            let mut items: Vec<Item> = shown().into_iter().filter(|i| i.id != it.id).collect();
            items.push(it);
            let mut pm = to_pixmap(base);
            render_items_onto(&mut pm, &items);
            pm
        }
        Some(it) => {
            let mut pm = cache.clone();
            render_items_onto(&mut pm, std::slice::from_ref(&it));
            pm
        }
        None => cache.clone(),
    }
}

/// Move the edges a handle controls to `p`. Handles go clockwise from top-left.
fn resize_bounds(mut b: Bounds, handle: usize, p: Pt) -> Bounds {
    if [0, 6, 7].contains(&handle) {
        b.x0 = p.x;
    }
    if [2, 3, 4].contains(&handle) {
        b.x1 = p.x;
    }
    if [0, 1, 2].contains(&handle) {
        b.y0 = p.y;
    }
    if [4, 5, 6].contains(&handle) {
        b.y1 = p.y;
    }
    b
}

/// A square from `a` towards `b`, as big as the longer side of the drag.
fn square_from(a: Pt, b: Pt) -> Bounds {
    let side = (b.x - a.x).abs().max((b.y - a.y).abs());
    Bounds::new(a, pt(a.x + side.copysign(b.x - a.x), a.y + side.copysign(b.y - a.y))).norm()
}

/// Keep `b` square after `handle` moved: corners keep the shape, and a side
/// handle sets both sides.
fn keep_square(b: Bounds, handle: usize) -> Bounds {
    let mut out = keep_aspect(b, handle, 1.0);
    match handle {
        1 | 5 => out.x1 = out.x0 + (out.y1 - out.y0).abs(),
        3 | 7 => out.y1 = out.y0 + (out.x1 - out.x0).abs(),
        _ => {}
    }
    out
}

/// For a corner handle, grow the smaller side so `b` has the width/height
/// ratio `aspect`, keeping the opposite corner where it is.
fn keep_aspect(b: Bounds, handle: usize, aspect: f32) -> Bounds {
    if ![0, 2, 4, 6].contains(&handle) || aspect <= 0.0 {
        return b;
    }
    let (w, h) = (b.x1 - b.x0, b.y1 - b.y0);
    let h2 = h.abs().max(w.abs() / aspect);
    let (w, h) = ((h2 * aspect).copysign(w), h2.copysign(h));
    let mut out = b;
    // The dragged corner moves; the opposite one stays.
    if [0, 6].contains(&handle) {
        out.x0 = b.x1 - w;
    } else {
        out.x1 = b.x0 + w;
    }
    if [0, 2].contains(&handle) {
        out.y0 = b.y1 - h;
    } else {
        out.y1 = b.y0 + h;
    }
    out
}

/// What a tool draws with before the user picks colours: the mockup shapes
/// get a white outline over a see-through fill, the asterisk is solid white.
fn default_style(tool: Tool) -> Style {
    match tool {
        Tool::Circle | Tool::Group | Tool::Cone | Tool::Donut | Tool::Slice | Tool::Cross => {
            Style { stroke: Rgba::WHITE, fill: Some(Rgba([229, 72, 77, 110])), shadow: false, ..Style::default() }
        }
        Tool::Asterisk => Style { stroke: Rgba::WHITE, shadow: false, ..Style::default() },
        _ => Style::default(),
    }
}

fn tool_hint(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => "Click an item to select it. Drag to move, drag a handle to resize, double-click text to edit.",
        Tool::Text => "Click where the text goes, type, then Enter.",
        Tool::Step => "Click to drop the next numbered step.",
        Tool::Bubble => "Drag out the bubble, then type. Drag the tail handle to point it.",
        Tool::Crop => "Drag the area to keep, then Enter.",
        Tool::Obfuscate => "Drag over anything private to pixelate or blur it.",
        Tool::Highlight => "Drag over text to highlight it.",
        Tool::Spotlight => "Drag around what matters; everything else is darkened.",
        Tool::Card => "Click the picture to give this tour step its guide card, then type on the card.",
        Tool::Freehand => "Draw with the mouse held down.",
        Tool::Circle => "Drag out a circle. It stays round.",
        Tool::Group => "Drag around an area to mark it as a group.",
        Tool::Cone => "Drag from the point of the cone to its wide end.",
        Tool::Donut => "Drag out a ring. It stays round.",
        Tool::Slice => "Drag from the point of the slice to the middle of its curved edge.",
        Tool::Cross => "Drag over something to cross it out.",
        Tool::Asterisk => "Drag out an asterisk.",
        _ => "Drag on the image to draw.",
    }
}

/// Room the zoom controls take at the right of the status bar: the zoom menu, two buttons and spacing.
const STATUS_ZOOM_W: f32 = 150.0;

/// The width the status message may use when `available` is left in the status bar, so it never
/// runs under the zoom controls.
fn status_width(available: f32) -> f32 {
    (available - STATUS_ZOOM_W).max(0.0)
}

fn to_color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0[0], c.0[1], c.0[2], c.0[3])
}

fn from_color32(c: Color32) -> Rgba {
    Rgba(c.to_srgba_unmultiplied())
}

/// Where the image sat on screen last frame, for placing the text box.
#[derive(Clone, Copy)]
struct CanvasGeom {
    min: egui::Pos2,
    zoom: f32,
}

/// One editor window and its viewport.
pub struct EditorWindow {
    pub id: egui::ViewportId,
    pub editor: Arc<Mutex<Editor>>,
}

impl EditorWindow {
    pub fn new(editor: Editor) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { id: egui::ViewportId::from_hash_of(("editor", n)), editor: Arc::new(Mutex::new(editor)) }
    }

    pub fn is_closed(&self) -> bool {
        self.editor.lock().map(|e| e.closed).unwrap_or(true)
    }

    pub fn show(&self, ctx: &egui::Context, icon: Arc<egui::IconData>, first_frame: bool) {
        let title = self.editor.lock().map(|e| format!("{} – Mythicshot", e.title())).unwrap_or_default();
        let builder = egui::ViewportBuilder::default().with_title(title).with_inner_size([1280.0, 820.0]).with_min_inner_size([760.0, 480.0]).with_icon(icon);
        let editor = self.editor.clone();
        let id = self.id;
        ctx.show_viewport_deferred(id, builder, move |ui, _class| {
            if let Ok(mut e) = editor.lock() {
                e.ui(ui);
            }
        });
        if first_frame {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{EditorPrefs, Settings};

    fn editor(prefs: EditorPrefs) -> Editor {
        let s = Settings::default();
        Editor::new(RgbaImage::new(64, 48), "t".into(), BTreeMap::new(), s.dest_context("t"), prefs)
    }

    /// Feed `events` to the editor's shortcuts the way a window would.
    fn press(ed: &mut Editor, ctx: &egui::Context, events: Vec<egui::Event>) {
        let input = egui::RawInput { events, ..Default::default() };
        ctx.run_ui(input, |ui| ed.shortcuts(ui.ctx())).textures_delta.clear();
    }

    fn ctrl(key: Key) -> egui::Event {
        // On Windows egui reports Ctrl as both ctrl and command.
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::CTRL | Modifiers::COMMAND }
    }

    #[test]
    fn the_status_message_stops_before_the_zoom_controls() {
        assert_eq!(status_width(900.0), 900.0 - STATUS_ZOOM_W);
        assert_eq!(status_width(40.0), 0.0, "never a negative width on a narrow window");
    }

    /// Ctrl+C, X and V arrive as egui's Copy, Cut and Paste events, never as
    /// key presses, so the test sends exactly that.
    #[test]
    fn copy_paste_duplicate_and_cut_annotations() {
        let mut ed = editor(EditorPrefs::default());
        ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        let ctx = egui::Context::default();
        let rect = |x: f32| Shape::Rect(Bounds::new(mythicshot_editor::pt(x, 10.0), mythicshot_editor::pt(x + 20.0, 30.0)));
        ed.style.stroke = Rgba::YELLOW;
        let id = ed.add_item(rect(5.0));
        ed.style.stroke = Rgba::RED;
        ed.select(Some(id));

        press(&mut ed, &ctx, vec![egui::Event::Copy]);
        let marker = ITEM_CLIPBOARD.lock().unwrap().as_ref().unwrap().marker.clone();
        press(&mut ed, &ctx, vec![egui::Event::Paste(marker.clone())]);
        press(&mut ed, &ctx, vec![egui::Event::Paste(marker.clone())]);
        let items = ed.doc.items();
        assert_eq!(items.len(), 3);
        // Each paste keeps the copy's colour and lands a step further.
        assert!(items.iter().all(|it| it.style.stroke == Rgba::YELLOW));
        assert_eq!(items[1].shape, { let mut s = rect(5.0); s.translate(16.0, 16.0); s });
        assert_eq!(items[2].shape, { let mut s = rect(5.0); s.translate(32.0, 32.0); s });

        // The paste is selected, so Ctrl+D (an ordinary key press) duplicates it.
        assert_eq!(ed.selected, Some(ed.doc.items()[2].id));
        press(&mut ed, &ctx, vec![ctrl(Key::D)]);
        assert_eq!(ed.doc.items().len(), 4);
        press(&mut ed, &ctx, vec![egui::Event::Cut]);
        assert_eq!(ed.doc.items().len(), 3);
        assert_eq!(ed.selected, None);
        // Ctrl+Z brings back the cut one.
        press(&mut ed, &ctx, vec![ctrl(Key::Z)]);
        assert_eq!(ed.doc.items().len(), 4);

        // Text copied in another app becomes a text annotation, selected.
        press(&mut ed, &ctx, vec![egui::Event::Paste("Hello\r\nworld\n".into())]);
        let last = ed.doc.items().last().unwrap();
        assert!(matches!(&last.shape, Shape::Text { text, .. } if text == "Hello\r\nworld"));
        assert_eq!(ed.selected, Some(last.id));
    }

    #[test]
    fn picture_corners_keep_the_shape() {
        let b = |x0, y0, x1, y1| Bounds { x0, y0, x1, y1 };
        // A 3:2 picture dragged from bottom-right to 400 wide, 200 tall: height grows to 266.67.
        let r = keep_aspect(b(0.0, 0.0, 400.0, 200.0), 4, 1.5);
        assert_eq!((r.x0, r.y0, r.x1), (0.0, 0.0, 400.0));
        assert!((r.y1 - 266.67).abs() < 0.01);
        // From top-left the bottom-right corner stays put.
        let r = keep_aspect(b(-50.0, 100.0, 300.0, 300.0), 0, 1.5);
        assert_eq!((r.x1, r.y1), (300.0, 300.0));
        assert!(((r.x1 - r.x0) / (r.y1 - r.y0) - 1.5).abs() < 0.001);
        // Edge handles stretch freely.
        assert_eq!(keep_aspect(b(0.0, 0.0, 400.0, 200.0), 3, 1.5).y1, 200.0);
    }

    /// Ask the editor's window to close, the way the title bar's X does.
    fn request_close(ed: &mut Editor, ctx: &egui::Context) {
        let mut input = egui::RawInput::default();
        input.viewports.insert(egui::ViewportId::ROOT, egui::ViewportInfo { events: vec![egui::ViewportEvent::Close], ..Default::default() });
        ctx.run_ui(input, |ui| ed.ui(ui)).textures_delta.clear();
    }

    #[test]
    fn copying_the_picture_means_closing_does_not_ask_to_save() {
        let ctx = egui::Context::default();
        let rect = Shape::Rect(Bounds::new(mythicshot_editor::pt(5.0, 5.0), mythicshot_editor::pt(20.0, 20.0)));
        // Edited and neither saved nor copied: closing asks.
        let mut ed = editor(EditorPrefs::default());
        ed.add_item(rect.clone());
        request_close(&mut ed, &ctx);
        assert!(ed.confirm_close && !ed.closed);
        // Copied to the clipboard: closing just closes.
        let mut ed = editor(EditorPrefs::default());
        ed.add_item(rect.clone());
        ed.sent(Ok(Sent::Copied));
        request_close(&mut ed, &ctx);
        assert!(ed.closed && !ed.confirm_close);
        // Changed again after copying: it asks again.
        let mut ed = editor(EditorPrefs::default());
        ed.add_item(rect.clone());
        ed.sent(Ok(Sent::Copied));
        ed.add_item(rect);
        request_close(&mut ed, &ctx);
        assert!(ed.confirm_close && !ed.closed);
    }

    #[test]
    fn pasted_pictures_become_image_elements_that_fit() {
        let mut ed = editor(EditorPrefs::default());
        ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        let ctx = egui::Context::default();
        // A small picture keeps its size and lands in the middle of the 64×48 capture.
        let small = egui::ColorImage::new([20, 10], vec![egui::Color32::GREEN; 200]);
        press(&mut ed, &ctx, vec![egui::Event::PasteImage(Arc::new(small))]);
        let it = ed.doc.items().last().unwrap().clone();
        let Shape::Image { rect, image } = &it.shape else { panic!("not an image: {:?}", it.shape) };
        assert_eq!((image.width(), image.height()), (20, 10));
        assert_eq!((rect.x0, rect.y0, rect.x1, rect.y1), (22.0, 19.0, 42.0, 29.0));
        assert_eq!(ed.selected, Some(it.id));
        // A bigger one is shrunk to fit inside, keeping its shape.
        let big = egui::ColorImage::new([640, 480], vec![egui::Color32::BLUE; 640 * 480]);
        press(&mut ed, &ctx, vec![egui::Event::PasteImage(Arc::new(big))]);
        let b = ed.doc.items().last().unwrap().bounds();
        assert!(b.width() <= 64.0 * 0.9 + 0.01 && b.height() <= 48.0 * 0.9 + 0.01, "{b:?}");
        assert!((b.width() / b.height() - 640.0 / 480.0).abs() < 0.01);
        // It is drawn: the middle of the capture is now blue.
        let flat = ed.doc.flatten();
        assert_eq!(flat.get_pixel(32, 24).0, [0, 0, 255, 255]);
    }

    #[test]
    fn tool_styles_carry_over_to_the_next_capture() {
        let mut first = editor(EditorPrefs::default());
        first.set_tool(Tool::Text);
        first.style.stroke = Rgba([255, 255, 255, 255]);
        first.style.font_size = 32.0;
        first.style.fill = Some(Rgba([0, 0, 0, 200]));
        first.remember_style(Tool::Text);
        assert!(first.prefs_changed);

        // The next capture opens with the same tool and its remembered style.
        let mut next = editor(first.prefs.clone());
        assert_eq!(next.tool, Tool::Text);
        assert_eq!(next.style.font_size, 32.0);
        assert_eq!(next.style.stroke, Rgba([255, 255, 255, 255]));
        assert_eq!(next.style.fill, Some(Rgba([0, 0, 0, 200])));
        // Other tools keep their own defaults.
        next.set_tool(Tool::Rectangle);
        assert_eq!(next.style, Style::default());
        next.set_tool(Tool::Text);
        assert_eq!(next.style.font_size, 32.0);
    }

    #[test]
    fn editing_a_selected_item_updates_its_tool_style() {
        let mut e = editor(EditorPrefs::default());
        e.set_tool(Tool::Rectangle);
        let id = e.doc.add(Shape::Arrow(pt(1.0, 1.0), pt(20.0, 20.0)), e.style);
        e.selected = Some(id);
        e.style.width = 9.0;
        let owner = Tool::for_shape(&e.doc.item(id).unwrap().shape);
        e.remember_style(owner);
        assert_eq!(e.prefs.styles.get("arrow").map(|s| s.width), Some(9.0));
        assert!(e.prefs.styles.get("rectangle").is_none());
    }

    /// A spotlight darkens the whole frame, so dragging one must not draw it
    /// over a cache that already holds the other spotlight's darkening.
    #[test]
    fn dragging_one_of_two_spotlights_previews_like_a_normal_render() {
        let base = RgbaImage::from_pixel(100, 80, image::Rgba([200, 200, 200, 255]));
        let spot = |id, x0, y0, x1, y1| Item { id, shape: Shape::Spotlight { rect: Bounds { x0, y0, x1, y1 }, dim: 60 }, style: Style::default(), frames: None, pin: None };
        let red = Style { stroke: Rgba([255, 0, 0, 255]), fill: Some(Rgba([255, 0, 0, 255])), shadow: false, ..Style::default() };
        let mark = Item { id: 3, shape: Shape::Rect(Bounds { x0: 70.0, y0: 5.0, x1: 90.0, y1: 15.0 }), style: red, frames: None, pin: None };
        let items = vec![spot(1, 10.0, 10.0, 30.0, 30.0), spot(2, 50.0, 40.0, 90.0, 70.0), mark];
        let mut normal = to_pixmap(&base);
        render_items_onto(&mut normal, &items);
        for dragged in [1, 2] {
            let mut cache = to_pixmap(&base);
            let others: Vec<Item> = items.iter().filter(|i| i.id != dragged).cloned().collect();
            render_items_onto(&mut cache, &others);
            let it = items.iter().find(|i| i.id == dragged).cloned();
            let preview = drag_frame(&base, &cache, it, || items.clone());
            assert!(preview.data() == normal.data(), "spotlight {dragged}: the preview differs from a normal render");
        }
        // A new spotlight being drawn (id 0) joins the others the same way.
        let new = spot(0, 40.0, 0.0, 60.0, 20.0);
        let mut with_new = to_pixmap(&base);
        render_items_onto(&mut with_new, &[items.clone(), vec![new.clone()]].concat());
        let mut cache = to_pixmap(&base);
        render_items_onto(&mut cache, &items);
        assert!(drag_frame(&base, &cache, Some(new), || items.clone()).data() == with_new.data());
    }

    /// A spotlight copied in a recording editor can reach the screenshot
    /// editor through the shared clipboard; it has no spotlights, so pasting or duplicating one adds nothing.
    #[test]
    fn the_screenshot_editor_refuses_pasted_spotlights() {
        let mut ed = editor(EditorPrefs::default());
        let spot = Shape::Spotlight { rect: Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }, dim: 60 };
        ed.add_copy(spot.clone(), Style::default(), 1);
        assert!(ed.doc.items().is_empty(), "a pasted spotlight is refused");
        let id = ed.doc.add(spot, Style::default());
        ed.select(Some(id));
        ed.duplicate();
        assert_eq!(ed.doc.items().len(), 1, "nor is it duplicated");
        // Everything else still pastes.
        ed.add_copy(Shape::Rect(Bounds { x0: 1.0, y0: 1.0, x1: 9.0, y1: 9.0 }), Style::default(), 0);
        assert_eq!(ed.doc.items().len(), 2);
    }

    /// Text typed on one tour step stays on that step even when the shown step changes before it is committed.
    #[test]
    fn new_text_is_pinned_to_the_step_it_was_started_on() {
        let mut ed = editor(EditorPrefs::default());
        ed.text = Some(TextEdit { item: None, at: pt(3.0, 4.0), buffer: "Here".into(), focused: true, step: Some(4) });
        ed.commit_text();
        let it = ed.doc.items().last().expect("the text was added");
        assert_eq!((it.pin, it.frames), (Some(4), None));
        assert_eq!(ed.selected, Some(it.id));
    }

    #[test]
    fn prefs_round_trip_through_the_settings_file() {
        let mut s = Settings::default();
        s.editor.tool = "highlight".into();
        s.editor.export_format = "apng".into();
        s.editor.styles.insert("text".into(), crate::settings::SavedStyle { font_size: 40.0, ..Default::default() });
        let back: Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(back.editor, s.editor);
    }
}
