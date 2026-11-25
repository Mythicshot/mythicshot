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

use crate::theme::{self, Strong as _};
use crate::hotkeys::{self, Action, EditorAction};
use crate::menu_bar::{self, AppRequest, Entry};
use crate::icons::{self, Icon};

const HANDLE: f32 = 8.0;
/// Toolbar icon button size and tool rail width.
const ICON: f32 = 28.0;
const RAIL: f32 = 84.0;
/// A tool button on the rail.
const TOOL_BUTTON: f32 = 36.0;
/// The width each of the toolbar's overflow chevrons keeps for itself.
const CUE_W: f32 = 14.0;
/// The toolbar's button for the settings panel, and the panel's heading. Approved wording.
pub(crate) const PROPERTIES: &str = "Properties";
/// The settings file's names of the tools taken off the rail: Cross (it had Z), Asterisk (F)
/// and Donut (D). Shapes they made still open, draw and edit; no new ones are made, and their
/// keys belong to nothing.
const REMOVED_TOOLS: [&str; 3] = ["cross", "asterisk", "donut"];
/// The rail's sections, like a mockup palette; `None` has no heading.
/// The Spotlight and Guide card tools show only when editing a recording.
const RAIL_GROUPS: [(Option<&str>, &[Tool]); 4] = [
    (None, &[Tool::Select]),
    (Some("Draw"), &[Tool::Freehand, Tool::Arrow, Tool::Line]),
    (Some("Shapes"), &[Tool::Circle, Tool::Group, Tool::Cone, Tool::Rectangle, Tool::Ellipse, Tool::Text, Tool::Slice]),
    (Some("More"), &[Tool::Bubble, Tool::Step, Tool::Picture, Tool::Highlight, Tool::Obfuscate, Tool::Spotlight, Tool::Card, Tool::Crop]),
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
    Slice,
    Text,
    Bubble,
    Step,
    Highlight,
    Obfuscate,
    Spotlight,
    /// A tour step's guide card; Tour view only.
    Card,
    Crop,
    /// Place a picture file on the capture: an action that asks for the file, never a mode.
    Picture,
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
            Tool::Slice => "slice",
            Tool::Text => "text",
            Tool::Bubble => "bubble",
            Tool::Step => "step",
            Tool::Highlight => "highlight",
            Tool::Obfuscate => "obfuscate",
            Tool::Spotlight => "spotlight",
            Tool::Card => "card",
            Tool::Crop => "crop",
            Tool::Picture => "picture",
        }
    }

    fn from_key(k: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.key() == k)
    }

    /// The tool the editor opens on for `k`, the last tool used as the settings file names it:
    /// that tool, or Select for one of the tools taken off the rail (Cross, Asterisk and Donut),
    /// or the Rectangle for a word no version knew. Crop, Spotlight and guide cards are never
    /// where the editor opens, nor Picture, which is never a tool in use.
    fn opening(k: &str) -> Tool {
        if REMOVED_TOOLS.contains(&k) {
            return Tool::Select;
        }
        Tool::from_key(k).filter(|t| !matches!(t, Tool::Crop | Tool::Spotlight | Tool::Card | Tool::Picture)).unwrap_or(Tool::Rectangle)
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
            Shape::Slice(..) => Tool::Slice,
            Shape::Text { .. } => Tool::Text,
            Shape::Bubble { .. } => Tool::Bubble,
            Shape::Step { .. } => Tool::Step,
            Shape::Highlight(_) => Tool::Highlight,
            Shape::Obfuscate { .. } => Tool::Obfuscate,
            Shape::Spotlight { .. } => Tool::Spotlight,
            // Pasted pictures have no tool, so no style to remember; nor have the shapes whose
            // tools left the rail, which older pictures and recordings still hold.
            Shape::Image { .. } | Shape::Donut(_) | Shape::Cross(_) | Shape::Asterisk(_) => Tool::Select,
        }
    }

    const ALL: [Tool; 19] = [
        Tool::Select,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Line,
        Tool::Arrow,
        Tool::Freehand,
        Tool::Circle,
        Tool::Group,
        Tool::Cone,
        Tool::Slice,
        Tool::Text,
        Tool::Bubble,
        Tool::Step,
        Tool::Highlight,
        Tool::Obfuscate,
        Tool::Spotlight,
        Tool::Card,
        Tool::Crop,
        Tool::Picture,
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
            Tool::Slice => Icon::Slice,
            Tool::Text => Icon::Text,
            Tool::Bubble => Icon::Bubble,
            Tool::Step => Icon::Step,
            Tool::Highlight => Icon::Highlight,
            Tool::Obfuscate => Icon::Obfuscate,
            Tool::Spotlight => Icon::Spotlight,
            Tool::Card => Icon::Card,
            Tool::Crop => Icon::Crop,
            Tool::Picture => Icon::Picture,
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
            Tool::Slice => EditorAction::Slice,
            Tool::Text => EditorAction::Text,
            Tool::Bubble => EditorAction::Bubble,
            Tool::Step => EditorAction::Step,
            Tool::Highlight => EditorAction::Highlight,
            Tool::Obfuscate => EditorAction::Obfuscate,
            Tool::Spotlight => EditorAction::Spotlight,
            Tool::Card => EditorAction::Card,
            Tool::Crop => EditorAction::Crop,
            Tool::Picture => EditorAction::Picture,
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

/// Ask for a picture file to place, with the dialog "Open image" uses ([`crate::open_image::pick`],
/// through the stub-able dialog helper). A test build records the dialog and answers with the
/// file a test put in [`tests::PICKED`], if any.
fn pick_picture(dir: &std::path::Path) -> Option<PathBuf> {
    let picked = crate::open_image::pick(dir);
    #[cfg(test)]
    if let Some(answer) = tests::PICKED.with(|p| p.borrow_mut().take()) {
        return Some(answer);
    }
    picked
}

/// What is on the system clipboard right now, for the right-click Paste here.
fn read_clipboard() -> Option<Pasted> {
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed("read clipboard") {
        return None;
    }
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

/// The number the next editor gets, for its own widget ids.
static NEXT_EDITOR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// How long a held status message stays when the user does nothing in the editor.
const HELD_FOR: std::time::Duration = std::time::Duration::from_secs(30);

/// A status message that stays in front of later ones.
struct Held {
    text: String,
    clock: String,
    /// When the status bar first drew it; None until it has.
    since: Option<Instant>,
}

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
    /// A message the status bar keeps showing whatever else is reported: see [`Editor::notify_held`].
    held: Option<Held>,
    /// A file the status message offers to show in the file manager ("Show in folder"); cleared by the next message.
    pub(crate) status_reveal: Option<PathBuf>,
    /// Sentences shown after "Show in folder" (a warning about the file); cleared by the next message.
    pub(crate) status_more: Option<String>,
    /// A page the status message offers to open in the browser ("Open site"); cleared by the next message.
    pub(crate) status_open: Option<PathBuf>,
    pub closed: bool,
    pub(crate) confirm_close: bool,
    /// Where the canvas was right-clicked, for the menu's Paste here.
    menu_at: Option<Pt>,
    /// Where the pointer is over the picture, for Ctrl+V of text or images.
    hover: Option<Pt>,
    /// Shift is held: resize pictures freely.
    shift: bool,
    /// When the property panel last checkpointed, so one slider drag is one undo step.
    prop_checkpoint: Option<(u64, Instant)>,
    last_canvas: CanvasGeom,
    /// The "Publish image" or "Publish recording" window, while it is open. Declared before
    /// `video`: dropping it cancels its job before the recording is let go of.
    pub(crate) share: Option<crate::share_ui::ShareDialog>,
    /// Set when the editor holds a recording instead of a still image.
    pub(crate) video: Option<crate::video_editor::VideoState>,
    /// Remembered styles and choices; the app saves them when `prefs_changed`.
    pub prefs: crate::settings::EditorPrefs,
    pub prefs_changed: bool,
    /// A new default overlay style chosen in the GIF editor, for the app to save.
    pub overlay_default: Option<mythicshot_editor::overlays::OverlayStyle>,
    /// The global shortcuts, for the menu bar to show beside Capture region and Record region.
    pub global_keys: BTreeMap<Action, String>,
    /// What this window's menu bar asked of the app, in order; the app takes them each frame.
    pub app_requests: Vec<AppRequest>,
    /// "Export…" was chosen in the menu bar: the toolbar's export panel opens on the next frame.
    open_export: bool,
    /// One of the menu bar's menus was open in the last frame: Esc closes it and does nothing else.
    menu_open: bool,
    /// The menu bar is being used (a menu is open, or the pointer is on the bar): typing on a text
    /// or a guide card goes on, and gets the keyboard back once the menu is done.
    pub(crate) menu_hold: bool,
    /// Something is being captured or recorded: Capture region and Record region are greyed out.
    pub busy: bool,
    /// Quitting asked this editor its close question: its answer, until the quit goes on or stops.
    pub(crate) quit: Option<QuitAnswer>,
    /// The toolbar row as it was last drawn, for its overflow chevrons.
    toolbar_scroll: ToolbarRow,
    /// The Properties panel of the image editor or of the Timeline view is open (the Tour view's
    /// is its own): taken from the preference of that view kind when the view opens.
    pub(crate) panel_open: bool,
    /// This editor's number, for the ids of its Properties panel: editor windows share one egui
    /// context, and each panel keeps its own scroll.
    instance: u64,
    /// Where the canvas was in the last frame, for the box a text is typed in.
    canvas_rect: Option<Rect>,
    /// A drag of files over this window: what it holds, for the overlay over the canvas.
    picture_drop: crate::open_image::DropState,
    /// The picture files being read to be placed on this capture, this editor's newest winning.
    placing: crate::place_picture::Placing,
}

/// The toolbar row's scrolling as of the last frame: how wide its content and its view are, how far
/// it is scrolled, where its chevrons were drawn (left, right), and a scroll a chevron asked for.
#[derive(Debug, Clone, Copy, Default)]
struct ToolbarRow {
    content_w: f32,
    view_w: f32,
    offset: f32,
    cues: [Option<Rect>; 2],
    /// The room kept at each end for a chevron (left, right), drawn or not: only while the row overflows.
    areas: [Option<Rect>; 2],
    scroll_to: Option<f32>,
}

/// An editor's answer to its close question when Quit asked it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitAnswer {
    /// The question is still up.
    Waiting,
    /// Saved, exported (or exporting) or discarded: the app may go once every editor agrees.
    Go,
    /// Cancel: the app stays, and nothing is closed.
    Stay,
}

/// What Quit does first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitStart {
    /// Nothing would be lost: the app goes now.
    Exit,
    /// Editors that would lose something show their close question; see [`quit_progress`].
    Asked,
    /// A recording, an export, a publish or a GIF being saved is running: the app stays and says so.
    StillRunning,
}

/// How a quit that asked stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitProgress {
    Waiting,
    /// Every editor asked agreed and nothing runs any more: the app goes now.
    Exit,
    /// One said Cancel: every question is gone and nothing was closed.
    Cancelled,
}

/// What the app says when Quit finds work still running.
pub const STILL_RUNNING: &str = "Mythicshot is still recording, exporting or publishing. Quit again when it is done.";

/// Quit (the File menu, the tray): go at once when nothing would be lost; ask every editor that
/// would lose something its close question, as its close button does; and never drop work that is
/// running (`running`: a recording or a GIF being saved, outside the editors).
pub fn start_quit<G: std::ops::DerefMut<Target = Editor>>(editors: &mut [G], running: bool) -> QuitStart {
    if running || editors.iter().any(|e| e.exporting()) {
        return QuitStart::StillRunning;
    }
    let mut asked = false;
    for e in editors.iter_mut().filter(|e| !e.closed && e.asks_before_closing()) {
        e.quit = Some(QuitAnswer::Waiting);
        e.confirm_close = true;
        asked = true;
    }
    if asked { QuitStart::Asked } else { QuitStart::Exit }
}

/// After [`start_quit`] asked: wait for every answer and for an export chosen in a question to end.
/// A Cancel ends every question and closes nothing.
pub fn quit_progress<G: std::ops::DerefMut<Target = Editor>>(editors: &mut [G]) -> QuitProgress {
    let answers: Vec<QuitAnswer> = editors.iter().filter(|e| !e.closed).filter_map(|e| e.quit).collect();
    if answers.contains(&QuitAnswer::Stay) {
        for e in editors.iter_mut() {
            if e.quit.take().is_some() {
                e.confirm_close = false;
            }
        }
        return QuitProgress::Cancelled;
    }
    if answers.contains(&QuitAnswer::Waiting) || editors.iter().any(|e| !e.closed && e.exporting()) {
        return QuitProgress::Waiting;
    }
    QuitProgress::Exit
}

impl Editor {
    pub fn new(img: RgbaImage, title: String, keys: BTreeMap<EditorAction, String>, dest: Context, prefs: crate::settings::EditorPrefs) -> Self {
        let tool = Tool::opening(&prefs.tool);
        let panel_open = prefs.image_properties_open;
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
            status_reveal: None,
            status_more: None,
            status_open: None,
            closed: false,
            confirm_close: false,
            held: None,
            menu_at: None,
            hover: None,
            shift: false,
            prop_checkpoint: None,
            last_canvas: CanvasGeom { min: pos2(0.0, 0.0), zoom: 1.0 },
            share: None,
            video: None,
            prefs,
            prefs_changed: false,
            overlay_default: None,
            global_keys: BTreeMap::new(),
            app_requests: Vec::new(),
            open_export: false,
            menu_open: false,
            menu_hold: false,
            busy: false,
            quit: None,
            toolbar_scroll: ToolbarRow::default(),
            panel_open,
            instance: NEXT_EDITOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            canvas_rect: None,
            picture_drop: crate::open_image::DropState::default(),
            placing: crate::place_picture::Placing::default(),
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
        self.status_reveal = None;
        self.status_more = None;
        self.status_open = None;
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

    /// Whether closing asks first: for changes that are not saved, and for a recording that was
    /// never exported. A recording opened from a saved project never asks: the question says
    /// the recording is deleted, which would be untrue, and edits that were not sent to the
    /// site are discarded.
    pub(crate) fn asks_before_closing(&self) -> bool {
        match &self.video {
            Some(v) if v.origin.is_some() => false,
            Some(v) => self.doc.dirty || !v.exported,
            None => self.doc.dirty,
        }
    }

    /// Whether closing could drop edits without asking: a recording opened from a saved project,
    /// whose edits not sent to the site Quit discards. The app cannot tell from here whether any
    /// were made, so an app update waits for every such editor to be closed instead: it never
    /// loses them.
    pub(crate) fn may_hold_unsent_edits(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.origin.is_some())
    }

    /// The status bar's message.
    #[cfg(test)]
    pub(crate) fn status_text(&self) -> Option<&str> {
        self.held.as_ref().map(|h| h.text.as_str()).or(self.status.as_ref().map(|(text, _)| text.as_str()))
    }

    /// Report something the user must get to see: it stays in the status bar, in front of
    /// anything reported after it, until the user's first click or key press in this editor,
    /// or until it has been on screen for [`HELD_FOR`]. After that it is an ordinary message.
    pub(crate) fn notify_held(&mut self, msg: impl Into<String>) {
        let text: String = msg.into();
        self.notify(text.clone());
        let clock = self.status.as_ref().map(|(_, clock)| clock.clone()).unwrap_or_default();
        self.held = Some(Held { text, clock, since: None });
    }

    /// What the status bar shows at `now`: the held message while it is held, else the last
    /// one reported. `acted` is whether the user clicked or pressed a key in this frame.
    fn status_now(&mut self, now: Instant, acted: bool) -> Option<(String, String)> {
        if let Some(held) = self.held.as_mut() {
            let since = *held.since.get_or_insert(now);
            if acted || now.saturating_duration_since(since) >= HELD_FOR {
                self.held = None;
            }
        }
        match &self.held {
            Some(held) => Some((held.text.clone(), held.clock.clone())),
            None => self.status.clone(),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        theme::window_background(ui);
        let ctx = ui.ctx().clone();
        self.placing.set_wake(&ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.asks_before_closing() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            self.ask_to_close();
        }
        self.shift = ctx.input(|i| i.modifiers.shift);
        // A pick for the guide card's spot ends when the window loses the keyboard, but not while a
        // button is held: the release would otherwise land as a click that unselects the card.
        if ctx.input(|i| !i.pointer.any_down() && i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false)))) {
            self.cancel_card_pick();
        }
        // A press off the guide card ends typing on it; noted now, done once this frame's keys reached the card.
        let press = ctx.input(|i| if i.pointer.any_pressed() { i.pointer.press_origin() } else { None });
        // While the "Publish tour", "Add to site" or "Publish to" window is open it gets the keys, and the editor behind it is inert.
        let modal = self.publish_picker_open() || self.site_dialog_open() || self.share.is_some();
        // The close question takes the keys too (Escape dismisses it), but keeps clicks as they were.
        if !modal && !self.confirm_close {
            self.video_keys(&ctx);
            self.shortcuts(&ctx);
        }
        self.video_tick(&ctx);
        if modal {
            inert(ui);
        }
        let menu = menu_bar::bar(ui, "editor-menu-bar", &self.menu_state());
        self.menu_open = menu.open;
        let on_bar = ctx.input(|i| i.pointer.press_origin().or(i.pointer.hover_pos()).is_some_and(|p| menu.rect.contains(p)));
        self.menu_hold = menu.open || on_bar;
        if let Some(entry) = menu.picked {
            self.menu_entry(entry, &ctx);
        }
        self.toolbar(ui);
        self.status_bar(ui);
        self.timeline(ui);
        self.properties_panel(ui);
        self.tool_rail(ui);
        self.canvas(ui);
        // A picture file dragged over the canvas, and the pictures that have been read since.
        self.picture_drop(&ctx, modal || self.confirm_close);
        self.take_placed_pictures();
        // After the canvas, so a key typed in the same frame as the press still reaches the card first.
        // A press on the menu bar, or in one of its menus, leaves the typing on.
        if !self.menu_hold {
            self.end_card_typing_on_press(&ctx, press);
        }
        self.text_editor(&ctx);
        self.share_window(&ctx);
        if self.confirm_close {
            self.confirm_close_dialog(&ctx);
        }
    }

    /// Close the window as its close button does: at once, or after the close question when
    /// something would be lost.
    fn ask_to_close(&mut self) {
        if self.asks_before_closing() {
            self.confirm_close = true;
        } else {
            self.closed = true;
        }
    }

    /// What the menu bar shows in this window now.
    pub(crate) fn menu_state(&self) -> menu_bar::State {
        let (global, keys) = (&self.global_keys, &self.keys);
        let keys = menu_bar::Keys::from(|a| global.get(&a).cloned().unwrap_or_default(), |a| keys.get(&a).cloned().unwrap_or_default());
        // While a text or a guide card's text is typed, its keys are the text's (as `shortcuts`
        // leaves them): undo, redo, copy and delete would act on the picture instead.
        let typing = self.text.is_some() || self.card_typing_id().is_some();
        menu_bar::State {
            busy: self.busy,
            // The image editor saves; the recording editor exports, from its export panel.
            save: self.video.is_none(),
            export: self.video.is_some(),
            publish: self.can_publish_tour(),
            undo: self.doc.can_undo() && !typing,
            redo: self.doc.can_redo() && !typing,
            // The selection, or else the picture or the frame, as Ctrl+C copies.
            copy: !typing,
            delete: (self.selected.is_some() || self.card_selected()) && !typing,
            // Every view has its Properties panel.
            properties: Some((true, self.properties_open())),
            keys,
        }
    }

    /// Run a menu entry the way this window's own button or key runs it.
    fn menu_entry(&mut self, entry: Entry, ctx: &egui::Context) {
        match entry {
            Entry::Save => self.run(EditorAction::Save),
            Entry::Undo => self.run(EditorAction::Undo),
            Entry::Redo => self.run(EditorAction::Redo),
            Entry::Copy => self.run(EditorAction::Copy),
            Entry::Delete => self.run(EditorAction::Delete),
            Entry::Export => self.open_export = self.video.is_some(),
            Entry::PublishTour => self.open_publish_picker(),
            Entry::Properties => self.toggle_properties(),
            Entry::CloseWindow => self.ask_to_close(),
            Entry::CaptureRegion | Entry::RecordRegion if self.busy => {}
            entry => {
                if let Some(request) = entry.app_request() {
                    self.app_requests.push(request);
                    // The app takes it on its own frame: wake it.
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            }
        }
    }

    /// The Properties panel of the view shown is open.
    pub(crate) fn properties_open(&self) -> bool {
        match &self.video {
            Some(v) if v.view == crate::video_editor::VideoView::Tour => v.tour.settings.open,
            _ => self.panel_open,
        }
    }

    /// Open or close the Properties panel of the view shown (the toolbar's button, the Edit menu,
    /// the panel's own close button), and remember it for the next time a view of that kind opens:
    /// the image editor, the Timeline view and the Tour view each on its own, in any window.
    pub(crate) fn toggle_properties(&mut self) {
        let open = !self.properties_open();
        let remembered = match self.video.as_mut() {
            Some(v) if v.view == crate::video_editor::VideoView::Tour => {
                v.tour.settings.open = open;
                &mut self.prefs.tour_properties_open
            }
            Some(_) => {
                self.panel_open = open;
                &mut self.prefs.timeline_properties_open
            }
            None => {
                self.panel_open = open;
                &mut self.prefs.image_properties_open
            }
        };
        if *remembered != open {
            *remembered = open;
            self.prefs_changed = true;
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // With a menu of the menu bar open, Esc is left to it: it closes the menu, and only that.
        if !self.menu_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if self.card_escape() {
                // The guide card stopped typing or was unselected.
            } else if self.pulse_escape() {
                // The picture no longer takes the step's pulse.
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
        if action == EditorAction::Picture {
            return self.picture_tool();
        }
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
                self.pulses_from_history();
                self.fix_selection();
                self.reload_frame();
            }
            EditorAction::Redo => {
                self.doc.redo();
                self.pulses_from_history();
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
        #[cfg(test)]
        mythicshot_destinations::stubs::stubbed(format!("clipboard text {marker}"));
        #[cfg(not(test))]
        let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(marker.clone()));
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

    /// Whether a picture may be placed now: wherever a picture may be pasted, so everywhere but
    /// the Tour view while it shows no step to pin the picture to.
    pub(crate) fn can_place_picture(&self) -> bool {
        !self.drawing_blocked()
    }

    /// The rail's Picture tool, or its key: ask for a picture file with the dialog "Open image"
    /// uses (its filters and the captures folder), and place the one chosen at the middle of the
    /// part of the capture the canvas shows. An action, not a mode: the tool in use stays (until
    /// the picture is placed, which selects it with the Select tool, as a drop does).
    pub(crate) fn picture_tool(&mut self) {
        if !self.can_place_picture() {
            return;
        }
        let Some(path) = pick_picture(&self.dest.output_dir) else { return };
        let centre = self.visible_centre();
        self.place_picture_file(path, centre);
    }

    /// What a picture asked for now would be placed on: see [`crate::place_picture::Target`].
    fn place_target(&self) -> crate::place_picture::Target {
        crate::place_picture::Target { capture: (self.doc.width(), self.doc.height()), view: self.video.as_ref().map(|v| (v.view == crate::video_editor::VideoView::Tour, self.tour_src())) }
    }

    /// Read the picture file at `path` in the background ([`crate::open_image::load`], within
    /// its limits), to be placed centred on `centre` (capture pixels) once it arrives.
    pub(crate) fn place_picture_file(&mut self, path: PathBuf, centre: Pt) {
        if !self.can_place_picture() {
            return;
        }
        let target = self.place_target();
        self.placing.request(path, centre, target);
    }

    /// The point of the capture (in its pixels) under `p`, a point of the window, as the canvas
    /// was last drawn; it may lie outside the capture.
    fn capture_point(&self, p: egui::Pos2) -> Pt {
        let c = self.last_canvas;
        pt((p.x - c.min.x) / c.zoom, (p.y - c.min.y) / c.zoom)
    }

    /// The middle of the part of the capture the canvas shows, in capture pixels; the capture's
    /// middle before the canvas was first drawn.
    fn visible_centre(&self) -> Pt {
        match self.canvas_rect {
            Some(r) => self.capture_point(r.center()),
            None => pt(self.doc.width() as f32 / 2.0, self.doc.height() as f32 / 2.0),
        }
    }

    /// Where the pointer is in this window as a file is dropped. While the system drags a file
    /// the window is told nothing of the pointer, so the system's own cursor position is asked
    /// for and turned into a point of this window; egui's last pointer position otherwise.
    fn drop_point(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        #[cfg(not(test))]
        if let (Some((x, y)), Some(inner)) = (crate::platform::cursor_pos(), ctx.input(|i| i.viewport().inner_rect)) {
            let ppp = ctx.pixels_per_point();
            return Some(pos2(x as f32 / ppp - inner.min.x, y as f32 / ppp - inner.min.y));
        }
        ctx.input(|i| i.pointer.latest_pos())
    }

    /// A file dragged over the window: the drop overlay over the canvas only (it takes the
    /// picture, or refuses anything else, or everything while nothing may be placed or a dialog
    /// is open: `blocked`, saying so over a picture), and a dropped picture read to be placed
    /// where it was dropped.
    fn picture_drop(&mut self, ctx: &egui::Context, blocked: bool) {
        let Some(area) = self.canvas_rect else { return };
        let allowed = !blocked && self.can_place_picture();
        let words = crate::open_image::Words { accept: crate::place_picture::PLACE_HERE, blocked: crate::place_picture::CANNOT_PLACE_NOW };
        if let Some(path) = self.picture_drop.frame_from_input_for(ctx, area, words, allowed) {
            let centre = self.drop_point(ctx).filter(|p| area.contains(*p)).map(|p| self.capture_point(p)).unwrap_or_else(|| self.visible_centre());
            self.place_picture_file(path, centre);
        }
    }

    /// Place the pictures that have arrived, or say why a file could not be opened, the way the
    /// editor reports its other failures. A picture for a capture that has changed meanwhile
    /// (cropped or resized, another view or step shown), or for a closed editor, is dropped
    /// without a word.
    fn take_placed_pictures(&mut self) {
        for placed in self.placing.poll() {
            if self.closed {
                continue;
            }
            match placed {
                crate::place_picture::Placed::Picture { image, centre, target } if target == self.place_target() => {
                    let size = (image.width() as f32, image.height() as f32);
                    let rect = crate::place_picture::placed_rect(size, centre, target.capture);
                    // One undo step, selected with the Select tool, as a pasted picture is.
                    self.add_copy(Shape::Image { rect, image }, Style::default(), 0);
                }
                crate::place_picture::Placed::Failed { sentence, target } if target == self.place_target() => self.notify(sentence),
                _ => {}
            }
        }
    }

    /// Whether an app update must wait for this editor: it exports or publishes, it is reading
    /// a picture to place, or it holds a saved recording whose edits not sent to the site a
    /// close would drop.
    pub(crate) fn holds_update(&self) -> bool {
        !self.closed && (self.exporting() || self.may_hold_unsent_edits() || self.placing.pending())
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
        egui::Panel::top("editor-toolbar").frame(egui::Frame::new().fill(theme::SURFACE).inner_margin(egui::Margin::symmetric(6, 4))).show(ui, |ui| {
            ui.horizontal(|ui| {
                // "Properties" keeps its room at the far right; on a narrow window the rest of the
                // row scrolls under it (wheel or drag) instead of pushing it out of the window.
                let font = egui::TextStyle::Button.resolve(ui.style());
                let button = ui.fonts_mut(|f| f.layout_no_wrap(PROPERTIES.into(), font, theme::TEXT).size().x) + 2.0 * ui.spacing().button_padding.x;
                let room = (ui.available_width() - button - 2.0 * ui.spacing().item_spacing.x - 8.0).max(0.0);
                // When the row is wider than its room, a chevron at each end shows that buttons are hidden
                // that way and scrolls there; both keep their room, so they never cover a button.
                let overflow = self.toolbar_scroll.content_w > room + 0.5;
                let cue_w = if overflow { CUE_W } else { 0.0 };
                let gap = ui.spacing().item_spacing.x;
                let view = if overflow { (room - 2.0 * (cue_w + gap)).max(0.0) } else { room };
                let height = ICON;
                // The room is laid out taking nothing; a side with buttons hidden takes clicks once that
                // is known (below), never the keyboard focus: Tab would land on it, and the editor's
                // keys wait while something has the focus.
                let left = overflow.then(|| ui.allocate_exact_size(vec2(cue_w, height), egui::Sense::hover()).0);
                let mut area = egui::ScrollArea::horizontal().id_salt("editor-toolbar-row").max_width(view).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
                if let Some(x) = self.toolbar_scroll.scroll_to.take() {
                    area = area.horizontal_scroll_offset(x);
                }
                let out = area.show(ui, |ui| {
                    ui.horizontal(|ui| self.toolbar_row(ui));
                });
                let right = overflow.then(|| ui.allocate_exact_size(vec2(cue_w, height), egui::Sense::hover()).0);
                let (content_w, view_w, offset) = (out.content_size.x, out.inner_rect.width(), out.state.clamped_offset().x);
                let hidden = [overflow && offset > 0.5, overflow && offset + view_w < content_w - 0.5];
                let step = (view_w * 0.75).max(ICON);
                // The row's width this frame against the room decided from the last: when the answer
                // changed, the cue (or the room kept for it) is wrong this frame, so draw another now.
                if (content_w > room + 0.5) != overflow {
                    ui.ctx().request_repaint();
                }
                let areas = [left, right];
                let mut cues = [None, None];
                for (i, area) in areas.into_iter().enumerate() {
                    let Some(rect) = area else { continue };
                    if !hidden[i] {
                        continue;
                    }
                    let resp = ui.interact(rect, ui.scope_id().with(("editor-toolbar-cue", i)), egui::Sense::click() - egui::Sense::FOCUSABLE);
                    paint_chevron(ui.painter(), rect, i == 1, if resp.hovered() { theme::TEXT } else { theme::MUTED });
                    cues[i] = Some(rect);
                    if resp.clicked() {
                        let to = if i == 0 { offset - step } else { offset + step };
                        self.toolbar_scroll.scroll_to = Some(to.clamp(0.0, (content_w - view_w).max(0.0)));
                        ui.ctx().request_repaint();
                    }
                }
                let scroll_to = self.toolbar_scroll.scroll_to;
                self.toolbar_scroll = ToolbarRow { content_w, view_w, offset, cues, areas, scroll_to };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.properties_button(ui));
            });
        });
    }

    /// The toolbar's actions, left to right: saving or exporting, undo and redo, effects, and the
    /// properties of the selection or of the next item drawn.
    fn toolbar_row(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 2.0;
        if self.video.is_some() {
            let exp = icons::button(ui, Icon::Export, ICON, false, "Export (GIF, APNG, MP4, WebM)");
            // "Export…" in the File menu opens this same panel.
            if std::mem::take(&mut self.open_export) {
                egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&exp));
            }
            egui::Popup::menu(&exp).style(theme::menu_bar_style).show(|ui| self.export_menu(ui));
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
            egui::Popup::menu(&send).style(theme::menu_style).show(|ui| {
                for (d, icon) in [(DestId::Print, Icon::Print), (DestId::Email, Icon::Email), (DestId::Clipboard, Icon::Copy)] {
                    if icons::menu_row(ui, icon, d.label()).on_hover_text(d.help()).clicked() {
                        self.send(d);
                        ui.close();
                    }
                }
                ui.separator();
                if icons::menu_row(ui, Icon::Export, crate::share_ui::MENU_ENTRY).clicked() {
                    self.open_share_image();
                    ui.close();
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
            let fx = icons::button(ui, Icon::Effects, ICON, false, EFFECTS_HINT);
            egui::Popup::menu(&fx).style(theme::menu_style).show(|ui| self.effects_menu(ui));
            ui.separator();
        }
        self.properties_row(ui);
    }

    /// "Properties" at the far right of the toolbar: lit while the panel is open. Every view has
    /// its panel, so the button is always usable.
    fn properties_button(&mut self, ui: &mut egui::Ui) {
        let button = egui::Button::selectable(self.properties_open(), PROPERTIES).min_size(vec2(0.0, ICON));
        if ui.add(button).clicked() {
            self.toggle_properties();
        }
    }

    fn effects_menu(&mut self, ui: &mut egui::Ui) {
        if let Some(f) = self.effect_buttons(ui, Place::Toolbar) {
            self.effect(f);
            ui.close();
        }
    }

    /// The effects: the one list behind the toolbar's Effects menu and the panel's "Effects", giving
    /// the effect clicked. The menu holds Resize as a submenu and lines between its groups; the
    /// panel has Resize as a row of its own ([`resize_buttons`]).
    fn effect_buttons(&self, ui: &mut egui::Ui, place: Place) -> Option<Effect> {
        let menu = place == Place::Toolbar;
        let mut pick: Option<Effect> = None;
        if applied(ui.button("Drop shadow"), place) {
            pick = Some(Box::new(effects::drop_shadow));
        }
        if applied(ui.button("Torn edge"), place) {
            pick = Some(Box::new(effects::torn_edge));
        }
        if applied(ui.button("Border (current colour and width)"), place) {
            let (w, c) = (self.style.width.round().max(1.0) as u32, self.style.stroke.0);
            pick = Some(Box::new(move |img: &RgbaImage| effects::border(img, w, c)));
        }
        if menu {
            ui.separator();
        }
        if applied(ui.button("Greyscale"), place) {
            pick = Some(Box::new(effects::greyscale));
        }
        if applied(ui.button("Invert"), place) {
            pick = Some(Box::new(effects::invert));
        }
        if menu {
            ui.separator();
            ui.menu_button("Resize", |ui| {
                if let Some(p) = resize_buttons(ui, place) {
                    pick = Some(resize_effect(p));
                }
            });
        }
        if applied(ui.button("Rotate right"), place) {
            pick = Some(Box::new(effects::rotate_cw));
        }
        if applied(ui.button("Rotate left"), place) {
            pick = Some(Box::new(effects::rotate_ccw));
        }
        pick
    }

    /// Colour, fill, width, text size, shadow and, for a selection, its actions.
    fn properties_row(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 6.0;
        // A guide card has no colours or widths of its own: the tour's style draws it.
        if self.in_tour_view() && (self.tool == Tool::Card || self.card_selected()) {
            return self.card_properties(ui);
        }
        self.style_controls(ui, Place::Toolbar);
        if let Some(id) = self.selected {
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

    /// The style of the selection, or of the current tool when nothing is selected: the one set of
    /// controls behind the toolbar's properties row and the panel's "This shape". The toolbar lays
    /// them out in a row; the panel gives each a line with its label. A change is applied the same
    /// way from either place (see [`Editor::commit_style`]). True when something changed.
    fn style_controls(&mut self, ui: &mut egui::Ui, place: Place) -> bool {
        let before = (self.style, self.obfuscate);
        in_row(ui, place, "Line and text colour", |ui| colour_button(ui, &mut self.style.stroke, "Line and text colour"));
        match place {
            Place::Toolbar => fill_controls(ui, place, &mut self.style),
            Place::Panel => {
                ui.horizontal(|ui| fill_controls(ui, place, &mut self.style));
            }
        }
        in_row(ui, place, "Line width", |ui| ui.add(egui::DragValue::new(&mut self.style.width).range(1.0..=20.0).speed(0.1).suffix(" px")).on_hover_text("Line width (drag or type)"));
        in_row(ui, place, "Text size", |ui| ui.add(egui::DragValue::new(&mut self.style.font_size).range(10.0..=96.0).speed(0.3).suffix(" pt")).on_hover_text("Text and step label size"));
        match place {
            Place::Toolbar => {
                if icons::button(ui, Icon::Shadow, ICON, self.style.shadow, SHADOW).clicked() {
                    self.style.shadow = !self.style.shadow;
                }
            }
            Place::Panel => {
                ui.checkbox(&mut self.style.shadow, SHADOW);
            }
        }
        if self.shows_obfuscate() {
            let salt = if place == Place::Toolbar { "obfuscate-mode" } else { "obfuscate-mode-panel" };
            in_row(ui, place, "Hide as", |ui| {
                egui::ComboBox::from_id_salt(salt)
                    .popup_style(theme::menu_style.into())
                    .width(80.0)
                    .selected_text(if self.obfuscate == ObfuscateMode::Pixelate { "Pixelate" } else { "Blur" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.obfuscate, ObfuscateMode::Pixelate, "Pixelate");
                        ui.selectable_value(&mut self.obfuscate, ObfuscateMode::Blur, "Blur");
                    });
            });
        }
        if self.shows_spotlight() {
            match place {
                Place::Toolbar => self.spotlight_slider(ui),
                Place::Panel => {
                    ui.horizontal(|ui| {
                        row_label(ui, "Spotlight strength");
                        self.dim_slider(ui);
                    });
                }
            }
        }
        self.commit_style(before)
    }

    /// After the style controls ran: remember a change as the style of whatever tool it applies to
    /// (the selected item's tool, or the current tool for the next item drawn), and apply it to the
    /// selection in one undo step per burst of changes. True when something changed.
    fn commit_style(&mut self, before: (Style, ObfuscateMode)) -> bool {
        if (self.style, self.obfuscate) == before {
            return false;
        }
        let owner = self.selected.and_then(|id| self.doc.item(id)).map(|it| Tool::for_shape(&it.shape)).unwrap_or(self.tool);
        self.remember_style(owner);
        if let Some(id) = self.selected {
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
        true
    }

    /// "Pixelate" or "Blur" is offered: for the Obfuscate tool or a selected obfuscate shape.
    fn shows_obfuscate(&self) -> bool {
        self.tool == Tool::Obfuscate || self.selected_is_obfuscate()
    }

    /// The spotlight strength is offered: for the Spotlight tool or a selected spotlight.
    fn shows_spotlight(&self) -> bool {
        self.tool == Tool::Spotlight || self.selected_is_spotlight()
    }

    /// The Properties panel of the image editor or of the Timeline view, docked at the right of
    /// the canvas when open. The Tour view draws its own ([`Editor::tour_settings_panel`]).
    fn properties_panel(&mut self, ui: &mut egui::Ui) {
        if self.in_tour_view() || !self.panel_open {
            return;
        }
        let mut changed = false;
        let close = crate::tour_cards::properties_shell(ui, ("editor-properties", self.instance), |ui| {
            changed = if self.video.is_some() { self.timeline_properties(ui) } else { self.image_properties(ui) };
        });
        // The toolbar was drawn before the panel: draw again at once, so it shows the change too.
        if changed {
            ui.ctx().request_repaint();
        }
        if close {
            self.toggle_properties();
        }
    }

    /// The image editor's panel: "This shape", "Picture" and "In this editor". True when something changed.
    fn image_properties(&mut self, ui: &mut egui::Ui) -> bool {
        crate::tour_cards::section(ui, "This shape");
        let mut changed = self.style_controls(ui, Place::Panel);
        crate::tour_cards::section(ui, "Picture");
        ui.label(egui::RichText::new(format!("{} × {}", self.doc.width(), self.doc.height())).color(theme::MUTED));
        // The toolbar's Effects button says what an effect does to the annotations; so do these.
        row_label(ui, "Resize").on_hover_text(EFFECTS_HINT);
        let mut pick = ui.horizontal_wrapped(|ui| resize_buttons(ui, Place::Panel)).inner.map(resize_effect);
        row_label(ui, "Effects").on_hover_text(EFFECTS_HINT);
        if let Some(f) = ui.horizontal_wrapped(|ui| self.effect_buttons(ui, Place::Panel)).inner {
            pick = Some(f);
        }
        if let Some(f) = pick {
            self.effect(f);
            changed = true;
        }
        crate::tour_cards::section(ui, "In this editor");
        let zoom = self.zoom;
        ui.horizontal(|ui| {
            row_label(ui, "Zoom");
            self.zoom_choice(ui, "zoom-panel");
        });
        changed | (zoom != self.zoom)
    }

    /// The zoom choices: the status bar's and the panel's "Zoom".
    fn zoom_choice(&mut self, ui: &mut egui::Ui, salt: &str) {
        let z = self.zoom.unwrap_or(self.last_canvas.zoom);
        let label = if self.zoom.is_none() { format!("Fit {:.0}%", z * 100.0) } else { format!("{:.0}%", z * 100.0) };
        egui::ComboBox::from_id_salt(salt).popup_style(theme::menu_style.into()).width(84.0).selected_text(label).show_ui(ui, |ui| {
            if ui.selectable_label(self.zoom.is_none(), "Fit to window").clicked() {
                self.zoom = None;
            }
            for p in [25.0, 50.0, 75.0, 100.0, 150.0, 200.0, 400.0] {
                if ui.selectable_label(self.zoom == Some(p / 100.0), format!("{p:.0}%")).clicked() {
                    self.zoom = Some(p / 100.0);
                }
            }
        });
    }

    fn key_hint(&self, action: EditorAction) -> String {
        let b = self.keys.get(&action).map(String::as_str).unwrap_or("");
        if b.is_empty() { action.label().to_string() } else { format!("{}  ({})", action.label(), hotkeys::display(b)) }
    }

    /// A narrow column of tool icons, grouped like Greenshot's.
    fn tool_rail(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("editor-tools").resizable(false).exact_size(RAIL).frame(egui::Frame::new().fill(theme::SURFACE).inner_margin(egui::Margin::symmetric(4, 6))).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                for (title, tools) in RAIL_GROUPS {
                    if let Some(title) = title {
                        ui.add_space(4.0);
                        ui.vertical_centered(|ui| ui.label(egui::RichText::new(title).size(theme::SMALL_SIZE).bold().color(theme::MUTED)));
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
        // Guide cards belong to tour steps: the tool works only in the Tour view. A picture is
        // placed wherever one may be pasted.
        let usable = match tool {
            Tool::Card => self.in_tour_view(),
            Tool::Picture => self.can_place_picture(),
            _ => true,
        };
        let disabled_tip = if tool == Tool::Card { "Open the Tour view to add guide cards" } else { tip.as_str() };
        let resp = ui.add_enabled_ui(usable, |ui| icons::button(ui, tool.icon(), TOOL_BUTTON, self.tool == tool, &tip)).inner.on_disabled_hover_text(disabled_tip);
        let key = hotkeys::display(binding);
        if !key.is_empty() && key.chars().count() <= 2 {
            let corner = resp.rect.right_bottom() - vec2(2.0, 1.0);
            let color = if self.tool == tool { theme::ON_PRIMARY } else { theme::MUTED };
            ui.painter().text(corner, egui::Align2::RIGHT_BOTTOM, key, FontId::proportional(9.0), color);
        }
        if resp.clicked() {
            if tool == Tool::Picture {
                self.picture_tool();
            } else {
                self.set_tool(tool);
            }
        }
    }

    /// Size, last action with its time, and zoom, like Greenshot's status bar.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("editor-status").frame(egui::Frame::new().fill(theme::SURFACE).inner_margin(egui::Margin::symmetric(8, 3))).show(ui, |ui| {
            ui.horizontal(|ui| {
                let muted = theme::MUTED;
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
                let acted = ui.ctx().input(|i| i.pointer.any_pressed() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::Text(_) | egui::Event::Paste(_))));
                let shown = self.status_now(Instant::now(), acted);
                let text = match &shown {
                    Some((msg, clock)) => format!("{clock} – {msg}"),
                    None => dirty.to_string(),
                };
                // The links and the sentences after them belong to the last message, not to a held one.
                let last = shown.as_ref().filter(|_| self.held.is_none());
                let reveal = last.and(self.status_reveal.clone());
                let more = last.and(self.status_more.clone());
                let open = last.and(self.status_open.clone());
                // Hovering any part of the message shows all of it.
                let whole = match &more {
                    Some(m) => format!("{text} · Show in folder. {m}"),
                    None => text.clone(),
                };
                ui.scope(|ui| {
                    // Room is kept for "· Show in folder" when the message offers it, and half of what is left
                    // for the sentences after it.
                    let link_w = (if reveal.is_some() { STATUS_REVEAL_W } else { 0.0 }) + (if open.is_some() { STATUS_OPEN_W } else { 0.0 });
                    let room = (status_width(ui.available_width()) - link_w).max(0.0);
                    ui.set_max_width(if more.is_some() { room / 2.0 } else { room });
                    let label = ui.add(egui::Label::new(egui::RichText::new(&text).color(muted)).truncate());
                    if more.is_some() {
                        label.on_hover_text(&whole);
                    }
                });
                if let Some(path) = reveal {
                    ui.label(egui::RichText::new("·").color(muted));
                    // A link is a focusable widget, so Tab reaches it and Enter or Space follows it.
                    if ui.link("Show in folder").on_hover_text(path.display().to_string()).clicked() {
                        crate::platform::reveal(&path);
                    }
                }
                if let Some(page) = open {
                    ui.label(egui::RichText::new("·").color(muted));
                    if ui.link("Open site").on_hover_text("Open the site's index page in the browser").clicked() {
                        crate::platform::open_path(&page);
                    }
                }
                if let Some(m) = &more {
                    ui.scope(|ui| {
                        ui.set_max_width(status_width(ui.available_width()));
                        theme::caution_line(ui, m, theme::BODY_SIZE, theme::Fit::Truncate).on_hover_text(&whole);
                    });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    self.zoom_choice(ui, "zoom");
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
        ui.label("Spotlight · darken");
        self.dim_slider(ui);
    }

    /// The spotlight strength slider: the toolbar's (after "Spotlight · darken") and the panel's
    /// "Spotlight strength".
    fn dim_slider(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected.and_then(|id| self.doc.item(id)).and_then(|it| match it.shape {
            Shape::Spotlight { dim, .. } => Some(dim),
            _ => None,
        });
        let mut dim = selected.unwrap_or(self.prefs.spotlight_dim).clamp(MIN_DIM, MAX_DIM);
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
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::GROUND)).show(ui, |ui| {
            self.canvas_rect = Some(ui.max_rect());
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
                    ui.painter().image(tex.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), theme::IMAGE_TINT);
                }
                ui.painter().rect_stroke(img_rect, 0.0, Stroke::new(1.0, theme::BORDER), StrokeKind::Outside);

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
                egui::Popup::context_menu(&resp).style(theme::menu_style).show(|ui| self.context_menu(ui));
                self.draw_chrome(ui, img_rect, z, &to_scr);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(self.cursor(resp.hover_pos().map(to_img), z));
                }
            });
        });
    }

    fn cursor(&self, p: Option<Pt>, z: f32) -> CursorIcon {
        if self.placing_pulse().is_some() {
            return CursorIcon::Crosshair;
        }
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
        // While the step's pulse is being moved, the picture is the pulse's.
        if self.pulse_pointer(resp, to_img) {
            return;
        }
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
            Tool::Group => Shape::Group(r),
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
        let hold = self.menu_hold;
        let mut commit = false;
        let mut area = egui::Area::new(egui::Id::unique(TEXT_AREA)).fixed_pos(screen + vec2(0.0, -4.0)).order(egui::Order::Foreground);
        // With the Properties panel open the box stays over the canvas, never over the panel's rows.
        // With it closed it is placed exactly as it always was.
        if self.properties_open()
            && let Some(canvas) = self.canvas_rect
        {
            area = area.constrain_to(canvas);
        }
        area.show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let t = self.text.as_mut().expect("text edit");
                let size = (self.style.font_size * self.zoom.unwrap_or(1.0)).clamp(12.0, 40.0);
                let resp = ui.add(egui::TextEdit::multiline(&mut t.buffer).desired_width(280.0).desired_rows(2).font(FontId::proportional(size)).hint_text("Type, then Enter"));
                if hold && !resp.has_focus() {
                    // The menu bar took the keyboard: the text gets it back when the menu is done.
                    t.focused = false;
                } else if !t.focused && !hold {
                    resp.request_focus();
                    t.focused = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Done").clicked() {
                        commit = true;
                    }
                    ui.label(egui::RichText::new("Enter done · Shift+Enter new line · Esc cancel").weak().size(theme::SMALL_SIZE));
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
        let modal = crate::share_ui::modal(egui::Id::unique("confirm-close")).frame(egui::Frame::popup(&ctx.global_style()).inner_margin(24.0)).show(ctx, |ui| {
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
                let primary = theme::primary_button(if is_video { "Export and close" } else { "Save" }).min_size(BUTTON).corner_radius(theme::control_radius(BUTTON.y));
                if ui.add(primary).clicked() {
                    choice = Some(0);
                }
                if ui.add(egui::Button::new(if is_video { "Discard" } else { "Don't save" }).min_size(BUTTON).corner_radius(theme::control_radius(BUTTON.y))).clicked() {
                    choice = Some(1);
                }
                if ui.add(egui::Button::new("Cancel").min_size(BUTTON).corner_radius(theme::control_radius(BUTTON.y))).clicked() {
                    choice = Some(2);
                }
            });
            choice
        });
        // Asked by Quit: the answer goes to the app, and nothing closes until every editor agreed.
        if self.quit.is_some() {
            match modal.inner {
                Some(0) if is_video => {
                    self.start_export(None);
                    (self.quit, self.confirm_close) = (Some(QuitAnswer::Go), false);
                }
                Some(0) => {
                    self.save();
                    if !self.doc.dirty {
                        (self.quit, self.confirm_close) = (Some(QuitAnswer::Go), false);
                    }
                }
                Some(1) => (self.quit, self.confirm_close) = (Some(QuitAnswer::Go), false),
                Some(_) => (self.quit, self.confirm_close) = (Some(QuitAnswer::Stay), false),
                None if modal.should_close() => (self.quit, self.confirm_close) = (Some(QuitAnswer::Stay), false),
                None => {}
            }
            return;
        }
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
            let dim = theme::SCRIM_STRONG;
            painter.rect_filled(Rect::from_min_max(img_rect.min, pos2(img_rect.max.x, r.min.y)), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(img_rect.min.x, r.max.y), img_rect.max), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(img_rect.min.x, r.min.y), pos2(r.min.x, r.max.y)), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(r.max.x, r.min.y), pos2(img_rect.max.x, r.max.y)), 0.0, dim);
            theme::selection_outline(&painter, r, 0.0, 2.0);
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
            theme::selection_outline(&painter, scr_rect(b).expand(3.0), 2.0, 1.0);
            for (pos, _) in handles(it) {
                theme::selection_handle(&painter, to_scr(pos), HANDLE);
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
/// get a white outline over a see-through fill.
fn default_style(tool: Tool) -> Style {
    match tool {
        Tool::Circle | Tool::Group | Tool::Cone | Tool::Slice => Style { stroke: Rgba::WHITE, fill: Some(Rgba([229, 72, 77, 110])), shadow: false, ..Style::default() },
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
        Tool::Slice => "Drag from the point of the slice to the middle of its curved edge.",
        Tool::Picture => "Choose a picture to place on the capture.",
        _ => "Drag on the image to draw.",
    }
}

/// Room the zoom controls take at the right of the status bar: the zoom menu, two buttons and spacing.
const STATUS_ZOOM_W: f32 = 150.0;

/// The width the status message may use when `available` is left in the status bar, so it never
/// runs under the zoom controls.
/// Width kept beside the status message for "· Show in folder".
const STATUS_REVEAL_W: f32 = 112.0;
/// Width kept beside the status message for "· Open site".
const STATUS_OPEN_W: f32 = 84.0;

fn status_width(available: f32) -> f32 {
    (available - STATUS_ZOOM_W).max(0.0)
}

/// A chevron pointing right (or left) in the middle of `r`: the toolbar's sign that buttons are hidden that way.
fn paint_chevron(p: &egui::Painter, r: Rect, right: bool, c: Color32) {
    let (h, w) = (r.height().min(16.0) / 2.0, 3.5);
    let (tip, back) = if right { (r.center().x + w, r.center().x - w) } else { (r.center().x - w, r.center().x + w) };
    let y = r.center().y;
    p.add(egui::Shape::line(vec![pos2(back, y - h / 1.5), pos2(tip, y), pos2(back, y + h / 1.5)], egui::Stroke::new(2.0, c)));
}

/// Where a control shared by the toolbar and the Properties panel is drawn. Both are bound to the
/// same state, so a change in one shows in the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// In a row of the toolbar (or of the Timeline view's controls, or of the export panel).
    Toolbar,
    /// On a line of the Properties panel, with its label.
    Panel,
}

/// An effect on the picture, as the Effects menu and the panel give it.
type Effect = Box<dyn FnOnce(&RgbaImage) -> RgbaImage>;

/// The id of the box a text is typed in.
const TEXT_AREA: &str = "editor-text";

/// The sizes Resize offers, in percent.
const RESIZE: [u32; 6] = [25, 50, 75, 125, 150, 200];

/// The shadow switch's label and hover text.
const SHADOW: &str = "Shadow";

/// The Effects button's hover text, which the panel's "Effects" and "Resize" carry too.
const EFFECTS_HINT: &str = "Effects (flatten annotations into the image)";

/// A label at the start of a line of the Properties panel, muted as the Tour view's are.
pub(crate) fn row_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(egui::RichText::new(text).color(theme::MUTED))
}

/// Whether a button that changes the picture or the recording was clicked. In the panel, which
/// stays open (unlike the toolbar's menus), a button clicked with the mouse lets go of the keyboard
/// focus, so a later Space or Enter does not apply it again; reached with Tab, it keeps the focus.
pub(crate) fn applied(resp: egui::Response, place: Place) -> bool {
    let clicked = resp.clicked();
    if clicked && place == Place::Panel && resp.ctx.input(|i| i.pointer.any_click()) {
        resp.surrender_focus();
    }
    clicked
}

/// A control in `place`: on its own in the toolbar, after its label on a line of the panel.
pub(crate) fn in_row<R>(ui: &mut egui::Ui, place: Place, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    match place {
        Place::Toolbar => add(ui),
        Place::Panel => {
            ui.horizontal(|ui| {
                row_label(ui, label);
                add(ui)
            })
            .inner
        }
    }
}

/// An on/off setting: a toggle button in the toolbar, a check box in the panel, with one label.
pub(crate) fn switch(ui: &mut egui::Ui, place: Place, on: &mut bool, label: &str) -> egui::Response {
    match place {
        Place::Toolbar => ui.toggle_value(on, label),
        Place::Panel => ui.checkbox(on, label),
    }
}

/// A colour button with `hover` as its hover text; it changes `c` when a colour is picked.
fn colour_button(ui: &mut egui::Ui, c: &mut Rgba, hover: &str) {
    let mut shown = to_color32(*c);
    if egui::color_picker::color_edit_button_srgba(ui, &mut shown, egui::color_picker::Alpha::OnlyBlend).on_hover_text(hover).changed() {
        *c = from_color32(shown);
    }
}

/// "Fill" and, while it is on, the fill colour.
fn fill_controls(ui: &mut egui::Ui, place: Place, style: &mut Style) {
    let mut on = style.fill.is_some();
    if switch(ui, place, &mut on, "Fill").on_hover_text("Fill rectangles, ellipses, text boxes and bubbles").changed() {
        style.fill = on.then_some(Rgba([255, 255, 255, 255]));
    }
    if let Some(mut f) = style.fill {
        colour_button(ui, &mut f, "Fill colour");
        style.fill = Some(f);
    }
}

/// The Resize sizes as buttons (the Effects menu's submenu, the panel's "Resize"): the one clicked.
fn resize_buttons(ui: &mut egui::Ui, place: Place) -> Option<u32> {
    let mut pick = None;
    for p in RESIZE {
        if applied(ui.button(format!("{p}%")), place) {
            pick = Some(p);
        }
    }
    pick
}

fn resize_effect(p: u32) -> Effect {
    Box::new(move |img: &RgbaImage| effects::resize(img, p))
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

/// The smallest an editor window can be made, in points.
pub(crate) const EDITOR_MIN_SIZE: [f32; 2] = [760.0, 480.0];

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
        let builder = editor_builder(title, icon);
        let editor = self.editor.clone();
        let id = self.id;
        ctx.show_viewport_deferred(id, builder, move |ui, _class| {
            if let Ok(mut e) = editor.lock() {
                e.ui(ui);
            }
        });
        if first_frame {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
            theme::dark_title_bar(ctx, id);
        }
    }
}

/// An editor window's viewport. It takes files dropped on it (a picture is placed on the capture),
/// said outright rather than left to the platform, as for the main window.
fn editor_builder(title: String, icon: Arc<egui::IconData>) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default().with_title(title).with_inner_size([1280.0, 820.0]).with_min_inner_size(EDITOR_MIN_SIZE).with_icon(icon).with_drag_and_drop(true)
}

/// Make `ui` and everything drawn in it take no clicks, drags, scrolls, hovers or keys, as
/// [`egui::Ui::disable`] does, but without its fading: every window of the app can be see-through
/// (for the live draw window), so a faded background would show the desktop behind the window.
/// Transparent-capable windows must never paint their background with reduced opacity.
fn inert(ui: &mut egui::Ui) {
    let opacity = ui.opacity();
    ui.disable();
    ui.set_opacity(opacity);
}

/// Checks for a frame drawn with a dialog open over an editor: what the editor painted, whether
/// anything dims the window, and clicks the way a mouse makes them. The video editor's tests use them too.
#[cfg(test)]
pub(crate) mod dialog_checks {
    use super::*;

    /// The window the tests draw into, as big as the editor opens.
    pub(crate) const WINDOW: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 820.0));

    pub(crate) fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput { screen_rect: Some(WINDOW), events, ..Default::default() }
    }

    /// What one frame painted: the shapes on the editor's own layer, and every shape of the frame;
    /// and whether the editor's window was left taking input.
    pub(crate) struct Painted {
        pub(crate) editor: Vec<egui::Shape>,
        pub(crate) all: Vec<egui::Shape>,
        pub(crate) enabled: bool,
    }

    pub(crate) fn frame(ed: &mut Editor, ctx: &egui::Context, events: Vec<egui::Event>) -> Painted {
        let (mut editor, mut enabled) = (Vec::new(), true);
        let mut out = ctx.run_ui(input(events), |ui| {
            ed.ui(ui);
            enabled = ui.is_enabled();
            let layer = ui.layer_id();
            editor = ctx.graphics(|g| g.get(layer).map(|l| l.all_entries().map(|c| c.shape.clone()).collect()).unwrap_or_default());
        });
        out.textures_delta.clear();
        Painted { editor: flat(editor), all: flat(out.shapes.into_iter().map(|c| c.shape).collect()), enabled }
    }

    fn flat(shapes: Vec<egui::Shape>) -> Vec<egui::Shape> {
        let mut out = Vec::new();
        for s in shapes {
            match s {
                egui::Shape::Vec(v) => out.extend(flat(v)),
                s => out.push(s),
            }
        }
        out
    }

    /// The filled rectangles of at least 4000 square points: the panels' backgrounds and the canvas
    /// (a rectangle with a fully transparent fill paints only its border, if any).
    pub(crate) fn big_fills(shapes: &[egui::Shape]) -> Vec<(Rect, Color32)> {
        shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Rect(r) if r.rect.area() >= 4000.0 && r.fill.a() > 0 => Some((r.rect, r.fill)),
                _ => None,
            })
            .collect()
    }

    /// The editor painted solid with a dialog open: the panels' backgrounds are exactly the ones
    /// it paints without the dialog (`solid`), none of them see-through, and the toolbar, rail,
    /// status bar and canvas backgrounds are all there.
    pub(crate) fn assert_solid(with_dialog: &Painted, solid: &Painted) {
        let fills = big_fills(&with_dialog.editor);
        assert!(fills.iter().filter(|(_, c)| *c == theme::SURFACE).count() >= 3, "the toolbar, rail and status bar backgrounds: {fills:?}");
        assert!(fills.iter().any(|(_, c)| *c == theme::GROUND), "the canvas background: {fills:?}");
        assert!(fills.iter().all(|(_, c)| c.a() == 255), "nothing of the editor's background is see-through: {fills:?}");
        assert_eq!(fills, big_fills(&solid.editor), "the same backgrounds as with no dialog open");
        // The picture itself is not faded either.
        for s in &with_dialog.editor {
            if let egui::Shape::Mesh(m) = s {
                assert!(m.vertices.iter().all(|v| v.color.a() == 255), "a faded picture");
            }
        }
    }

    /// Nothing laid over the whole window shows: no dimming layer behind a dialog.
    pub(crate) fn assert_not_dimmed(p: &Painted) {
        for s in &p.all {
            if let egui::Shape::Rect(r) = s {
                assert!(!(r.rect.contains_rect(WINDOW) && r.fill.a() > 0 && r.fill.a() < 255), "a dimming layer over the window: {r:?}");
            }
        }
    }

    /// A left click at `at`: the press in one frame, the release in the next, and one frame more.
    pub(crate) fn click(ed: &mut Editor, ctx: &egui::Context, at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(ed, ctx, vec![egui::Event::PointerMoved(at), button(true)]);
        frame(ed, ctx, vec![button(false)]);
        frame(ed, ctx, vec![]);
    }

    /// The window has no dimming over it, a click far outside the dialog lands on nothing, and the editor
    /// behind is solid and inert while `open` (the dialog) is shown: a click on a tool, a hover over
    /// it, a tool's key and a scroll over the canvas change nothing.
    pub(crate) fn assert_inert_and_solid(make: impl Fn() -> Editor, open: impl Fn(&mut Editor), is_open: impl Fn(&Editor) -> bool) {
        // Find a tool button on the rail: a point a click on which changes the tool with no dialog open.
        // The search starts at the rail's own top, below the menu bar and the toolbar, so it never
        // clicks a toolbar button (Save as… would open the system's file dialog).
        let top = {
            let (mut ed, ctx) = (make(), crate::theme::test_context());
            frame(&mut ed, &ctx, vec![]);
            let painted = frame(&mut ed, &ctx, vec![]);
            big_fills(&painted.editor).into_iter().find(|(r, c, ..)| *c == theme::SURFACE && r.left() <= 0.5 && (r.width() - RAIL).abs() <= 1.0).map(|(r, _)| r.top()).expect("the rail")
        };
        let rail = (0..80).map(|i| pos2(RAIL / 2.0, top + 4.0 + i as f32 * 8.0)).find(|&at| {
            let (mut ed, ctx) = (make(), crate::theme::test_context());
            frame(&mut ed, &ctx, vec![]);
            let before = ed.tool;
            click(&mut ed, &ctx, at);
            ed.tool != before
        });
        let rail = rail.expect("a tool button on the rail");

        let ctx = crate::theme::test_context();
        let mut ed = make();
        ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        frame(&mut ed, &ctx, vec![]);
        let solid = frame(&mut ed, &ctx, vec![]);
        assert!(!is_open(&ed), "the dialog is not open yet");
        open(&mut ed);
        frame(&mut ed, &ctx, vec![]);
        let shown = frame(&mut ed, &ctx, vec![]);
        assert!(is_open(&ed), "the dialog is open");
        // The editor is disabled as before, so none of its widgets takes input, and only the fading is gone.
        assert!(solid.enabled && !shown.enabled, "the editor is disabled while the dialog is open");
        assert_solid(&shown, &solid);
        assert_not_dimmed(&shown);

        // Pointing at a tool highlights nothing: the editor paints exactly what it paints with no pointer.
        let hovered = frame(&mut ed, &ctx, vec![egui::Event::PointerMoved(rail)]);
        let away = frame(&mut ed, &ctx, vec![egui::Event::PointerGone]);
        assert_eq!(format!("{:?}", hovered.editor), format!("{:?}", away.editor), "a hover highlight behind the dialog");

        let (tool, zoom) = (ed.tool, ed.zoom);
        click(&mut ed, &ctx, rail);
        let keys: Vec<egui::Event> = [Key::A, Key::E, Key::T, Key::S, Key::R].into_iter().map(|key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }).collect();
        frame(&mut ed, &ctx, keys);
        let canvas = pos2(WINDOW.center().x, WINDOW.center().y);
        frame(&mut ed, &ctx, vec![egui::Event::PointerMoved(canvas), egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, 300.0), modifiers: Modifiers::CTRL | Modifiers::COMMAND, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel }]);
        frame(&mut ed, &ctx, vec![]);
        assert_eq!((ed.tool, ed.zoom), (tool, zoom), "the editor behind the dialog reacted");
        assert!(is_open(&ed), "a click on the editor behind the dialog closed it");
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
        let ctx = crate::theme::test_context();
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
    fn a_held_message_stays_until_the_first_action_or_half_a_minute() {
        let start = Instant::now();
        let at = |secs: u64| start + std::time::Duration::from_secs(secs);
        let shown = |ed: &mut Editor, secs: u64, acted: bool| ed.status_now(at(secs), acted).map(|(text, _)| text);
        // Nothing the editor reports afterwards takes its place while the user does nothing.
        let mut ed = editor(EditorPrefs::default());
        ed.notify_held("This recording was opened without the cover logo.");
        ed.notify("Exporting… 40%");
        assert_eq!(ed.status_text(), Some("This recording was opened without the cover logo."));
        // The half minute counts from when it was first on screen, not from when it was reported.
        assert_eq!(shown(&mut ed, 100, false).as_deref(), Some("This recording was opened without the cover logo."));
        assert_eq!(shown(&mut ed, 129, false).as_deref(), Some("This recording was opened without the cover logo."));
        assert_eq!(shown(&mut ed, 130, false).as_deref(), Some("Exporting… 40%"));
        assert_eq!(shown(&mut ed, 131, false).as_deref(), Some("Exporting… 40%"));
        // The user's first click or key press lets the next message through at once.
        let mut ed = editor(EditorPrefs::default());
        ed.notify_held("This recording was opened without the cover logo.");
        assert_eq!(shown(&mut ed, 0, false).as_deref(), Some("This recording was opened without the cover logo."));
        ed.notify("Copied this frame to the clipboard");
        assert_eq!(shown(&mut ed, 1, false).as_deref(), Some("This recording was opened without the cover logo."));
        assert_eq!(shown(&mut ed, 2, true).as_deref(), Some("Copied this frame to the clipboard"));
        // With nothing reported after it, it simply stays, as any message does.
        let mut ed = editor(EditorPrefs::default());
        ed.notify_held("This recording was opened without the cover logo.");
        assert_eq!(shown(&mut ed, 0, true).as_deref(), Some("This recording was opened without the cover logo."));
        assert_eq!(shown(&mut ed, 500, false).as_deref(), Some("This recording was opened without the cover logo."));
        // The status bar draws it, and a key press in the window is the first action.
        let ctx = crate::theme::test_context();
        let mut ed = editor(EditorPrefs::default());
        ed.notify_held("This recording was opened without the cover logo.");
        ed.notify("Saved");
        ctx.run_ui(egui::RawInput::default(), |ui| ed.ui(ui)).textures_delta.clear();
        assert!(ed.held.as_ref().is_some_and(|h| h.since.is_some()));
        let key = egui::Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        ctx.run_ui(egui::RawInput { events: vec![key], ..Default::default() }, |ui| ed.ui(ui)).textures_delta.clear();
        assert!(ed.held.is_none());
        assert_eq!(ed.status_text(), Some("Saved"));
    }

    #[test]
    fn copying_the_picture_means_closing_does_not_ask_to_save() {
        let ctx = crate::theme::test_context();
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
        let ctx = crate::theme::test_context();
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

    // ---- The editor behind a dialog: solid, undimmed and inert ----

    fn share_dialog() -> crate::share_ui::ShareDialog {
        // Blocked from the start, so nothing is asked of any site.
        crate::share_ui::ShareDialog::new(crate::publish::share::Kind::Image, String::new(), "t", String::new(), false, Some("No server is set.".into()), None)
    }

    /// The bug: with "Publish to…" open the editor was drawn half see-through, and the
    /// desktop behind the window showed through it.
    #[test]
    fn the_editor_behind_publish_to_stays_solid_undimmed_and_inert() {
        let make = || editor(EditorPrefs::default());
        dialog_checks::assert_inert_and_solid(make, |ed| ed.share = Some(share_dialog()), |ed| ed.share.is_some());
    }

    /// The editor paints its whole window opaque in the theme, alone and with a dialog open,
    /// and the dialog's own frame is opaque surface-raised.
    #[test]
    fn the_editor_window_and_its_dialog_are_opaque() {
        let ctx = crate::theme::test_context();
        let mut ed = editor(EditorPrefs::default());
        // Frames a second apart, so a dialog has faded in.
        let run = |ed: &mut Editor| {
            let input = egui::RawInput { time: Some(ctx.input(|i| i.time) + 1.0), ..dialog_checks::input(vec![]) };
            let mut out = ctx.run_ui(input, |ui| ed.ui(ui));
            out.textures_delta.clear();
            out
        };
        run(&mut ed);
        crate::theme::checks::assert_window_opaque(&run(&mut ed), dialog_checks::WINDOW, "the editor");
        ed.share = Some(share_dialog());
        run(&mut ed);
        let out = run(&mut ed);
        crate::theme::checks::assert_window_opaque(&out, dialog_checks::WINDOW, "the editor with a dialog open");
        crate::theme::checks::assert_dialog_opaque(&out, "the share dialog over the editor");
        // Filled in pixel by pixel at 125% scaling: not one pixel lets the desktop through.
        let mut ed = editor(EditorPrefs::default());
        assert_eq!(crate::snapshot::uncovered(&crate::theme::test_context(), dialog_checks::WINDOW.size(), 1.25, 3, |ui| ed.ui(ui)), 0, "the editor");
        ed.share = Some(share_dialog());
        assert_eq!(crate::snapshot::uncovered(&crate::theme::test_context(), dialog_checks::WINDOW.size(), 1.25, 3, |ui| ed.ui(ui)), 0, "the editor with a dialog");
    }

    /// An editor on a plain `bg` screenshot with one rectangle selected. The rectangle is drawn in
    /// the picture's own colour, without a shadow, so only the selection shows on the picture.
    fn selection_on(bg: egui::Color32) -> Editor {
        let shot = RgbaImage::from_pixel(320, 200, image::Rgba([bg.r(), bg.g(), bg.b(), 255]));
        let s = Settings::default();
        let mut ed = Editor::new(shot, "t".into(), BTreeMap::new(), s.dest_context("t"), EditorPrefs::default());
        let style = Style { stroke: mythicshot_editor::Rgba([bg.r(), bg.g(), bg.b(), 255]), fill: None, width: 1.0, font_size: 20.0, shadow: false };
        let id = ed.doc.add(Shape::Rect(Bounds { x0: 80.0, y0: 50.0, x1: 240.0, y1: 150.0 }), style);
        ed.select(Some(id));
        ed
    }

    /// The selection on the canvas reads on any screenshot: on a white one and on a near-black
    /// one, across each side of the outline and across each handle there is a pixel with at least
    /// 3:1 contrast against the picture.
    #[test]
    fn the_selection_reads_on_a_white_and_on_a_near_black_screenshot() {
        let ppp = 1.25;
        let mut failed = Vec::new();
        for (name, bg) in crate::theme::tests::PICTURES {
            let mut ed = selection_on(bg);
            let image = crate::snapshot::picture(&crate::theme::test_context(), dialog_checks::WINDOW.size(), ppp, 3, |ui| ed.ui(ui));
            let geom = ed.last_canvas;
            let scr = |x: f32, y: f32| geom.min + vec2(x, y) * geom.zoom;
            let (l, t, r, b) = (80.0, 50.0, 240.0, 150.0);
            let (qx, qy) = (l + (r - l) / 4.0, t + (b - t) / 4.0);
            for (side, a, out) in [("top", scr(qx, t), vec2(0.0, -10.0)), ("bottom", scr(qx, b), vec2(0.0, 10.0)), ("left", scr(l, qy), vec2(-10.0, 0.0)), ("right", scr(r, qy), vec2(10.0, 0.0))] {
                if !crate::theme::tests::reads_against(&image, ppp, a + out * 0.15, a + out, bg) {
                    failed.push(format!("the outline on {name}, {side} side"));
                }
            }
            // Each handle: from inside the rectangle (where nothing else is drawn) into its middle.
            let it = ed.doc.item(ed.selected.unwrap()).unwrap().clone();
            let middle = scr((l + r) / 2.0, (t + b) / 2.0);
            for (pos, _) in handles(&it) {
                let c = scr(pos.x, pos.y);
                let inward = (middle - c).normalized();
                if !crate::theme::tests::reads_against(&image, ppp, c + inward * (HANDLE / 2.0 + 4.0), c, bg) {
                    failed.push(format!("the handle at {pos:?} on {name}"));
                }
            }
        }
        assert!(failed.is_empty(), "lost on the screenshot: {failed:?}");
    }

    /// Every label of the editor fits what holds it: the rail's group titles (Draw, Shapes,
    /// More) in its 84 points, the toolbar's property row with a shape selected and with the
    /// typing hint while a text is typed, and the status bar.
    #[test]
    fn the_editors_labels_fit_its_rail_toolbar_and_status_bar() {
        let ctx = crate::theme::test_context();
        let frames = |ed: &mut Editor| {
            let mut last = None;
            for _ in 0..3 {
                let input = egui::RawInput { time: Some(ctx.input(|i| i.time) + 1.0), ..dialog_checks::input(vec![]) };
                let mut out = ctx.run_ui(input, |ui| ed.ui(ui));
                out.textures_delta.clear();
                last = Some(out);
            }
            last.unwrap()
        };
        let texts = |out: &egui::FullOutput| {
            let mut all = Vec::new();
            fn collect(s: &egui::Shape, into: &mut Vec<(String, Rect)>) {
                match s {
                    egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                    egui::Shape::Text(t) => into.push((t.galley.text().to_string(), t.visual_bounding_rect())),
                    _ => {}
                }
            }
            out.shapes.iter().for_each(|c| collect(&c.shape, &mut all));
            all
        };
        // A shape selected: its properties in the toolbar.
        let mut ed = editor(EditorPrefs::default());
        let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        ed.select(Some(id));
        let out = frames(&mut ed);
        assert!(crate::theme::checks::assert_texts_fit(&out, "the editor with a shape selected") >= 3);
        let shown = texts(&out);
        for title in ["Draw", "Shapes", "More"] {
            let (_, r) = shown.iter().find(|(t, _)| t == title).unwrap_or_else(|| panic!("the rail shows {title}: {shown:?}"));
            assert!(r.left() >= 0.0 && r.right() <= RAIL, "{title} at {r:?} fits the {RAIL}-point rail");
        }
        // A text being typed: the hint in the toolbar.
        let mut ed = editor(EditorPrefs::default());
        ed.tool = Tool::Text;
        ed.text = Some(TextEdit { item: None, at: pt(3.0, 4.0), buffer: "Here".into(), focused: true, step: None });
        let out = frames(&mut ed);
        assert!(shown_has(&texts(&out), "Esc cancel"), "the typing hint shows");
        assert!(crate::theme::checks::assert_texts_fit(&out, "the editor while a text is typed") >= 3);
        fn shown_has(shown: &[(String, Rect)], part: &str) -> bool {
            shown.iter().any(|(t, _)| t.contains(part))
        }
    }

    /// One frame of `ed` in a window `size` points big, with `events`, half a second after the last.
    pub(crate) fn sized_frame(ed: &mut Editor, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size)), time: Some(ctx.input(|i| i.time) + 0.5), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| ed.ui(ui));
        out.textures_delta.clear();
        out
    }

    /// The toolbar's Save as… asks for the system's file dialog, which a test build never opens:
    /// the stub answers as if cancelled. The right-click Paste reads no real clipboard.
    #[test]
    fn save_as_and_paste_here_never_reach_the_desktop() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ed = editor(EditorPrefs::default());
        ed.dest.output_dir = tmp.path().to_path_buf();
        mythicshot_destinations::stubs::take();
        ed.send(DestId::FileAs);
        assert_eq!(ed.saved, None);
        let asked = mythicshot_destinations::stubs::take();
        assert!(matches!(asked.as_slice(), [a] if a.starts_with("save as dialog: ")), "{asked:?}");
        assert!(read_clipboard().is_none());
        assert_eq!(mythicshot_destinations::stubs::take(), ["read clipboard"]);
    }

    /// Click the button that says `label` in one frame's drawing, as a user does.
    fn click_label(ed: &mut Editor, ctx: &egui::Context, label: &str) {
        let size = dialog_checks::WINDOW.size();
        sized_frame(ed, ctx, size, vec![]);
        let out = sized_frame(ed, ctx, size, vec![]);
        let at = crate::menu_bar::tests::last_text_rect(&out, label).unwrap_or_else(|| panic!("{label} is shown")).center();
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(ed, ctx, size, events), at);
    }

    #[test]
    fn quit_with_nothing_to_lose_goes_at_once_and_asks_nothing() {
        let (mut a, mut b) = (editor(EditorPrefs::default()), editor(EditorPrefs::default()));
        assert_eq!(start_quit(&mut [&mut a, &mut b], false), QuitStart::Exit);
        assert!(!a.confirm_close && !b.confirm_close && a.quit.is_none());
        assert_eq!(start_quit::<&mut Editor>(&mut [], false), QuitStart::Exit, "no editor open");
    }

    #[test]
    fn quit_with_an_unsaved_capture_asks_and_cancel_keeps_everything() {
        let ctx = crate::theme::test_context();
        let mut clean = editor(EditorPrefs::default());
        let mut ed = editor(EditorPrefs::default());
        ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        assert_eq!(start_quit(&mut [&mut clean, &mut ed], false), QuitStart::Asked);
        assert!(ed.confirm_close && !clean.confirm_close, "only the editor that would lose something asks");
        assert_eq!(quit_progress(&mut [&mut clean, &mut ed]), QuitProgress::Waiting);
        // Its own close question, as its close button shows it.
        sized_frame(&mut ed, &ctx, dialog_checks::WINDOW.size(), vec![]);
        let out = sized_frame(&mut ed, &ctx, dialog_checks::WINDOW.size(), vec![]);
        assert!(crate::share_ui::tests::text_rect(&out, "Save this capture?").is_some());
        click_label(&mut ed, &ctx, "Cancel");
        assert_eq!(quit_progress(&mut [&mut clean, &mut ed]), QuitProgress::Cancelled);
        assert!(!ed.closed && !ed.confirm_close && ed.quit.is_none() && ed.doc.items().len() == 1, "nothing closed or lost");
        assert!(!clean.closed);
    }

    #[test]
    fn quit_goes_only_when_every_editor_agrees_and_closes_nothing_before() {
        let ctx = crate::theme::test_context();
        let make = || {
            let mut ed = editor(EditorPrefs::default());
            ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
            ed
        };
        // One says Don't save, the other Cancel: the app stays and neither window closed.
        let (mut a, mut b) = (make(), make());
        assert_eq!(start_quit(&mut [&mut a, &mut b], false), QuitStart::Asked);
        click_label(&mut a, &ctx, "Don't save");
        assert_eq!(quit_progress(&mut [&mut a, &mut b]), QuitProgress::Waiting);
        assert!(!a.closed, "not closed while the other editor has not answered");
        click_label(&mut b, &ctx, "Cancel");
        assert_eq!(quit_progress(&mut [&mut a, &mut b]), QuitProgress::Cancelled);
        assert!(!a.closed && !b.closed && a.doc.items().len() == 1 && b.doc.items().len() == 1);
        // Both say Don't save: the app goes.
        let (mut a, mut b) = (make(), make());
        start_quit(&mut [&mut a, &mut b], false);
        click_label(&mut a, &ctx, "Don't save");
        click_label(&mut b, &ctx, "Don't save");
        assert_eq!(quit_progress(&mut [&mut a, &mut b]), QuitProgress::Exit);
        // Save saves first, then counts as agreed.
        let tmp = tempfile::tempdir().unwrap();
        let mut c = make();
        c.dest.output_dir = tmp.path().to_path_buf();
        start_quit(&mut [&mut c], false);
        click_label(&mut c, &ctx, "Save");
        assert!(c.saved.as_ref().is_some_and(|p| p.is_file()));
        assert_eq!(quit_progress(&mut [&mut c]), QuitProgress::Exit);
    }

    #[test]
    fn quit_while_a_recording_or_a_gif_is_saved_keeps_the_app_and_asks_nothing() {
        let mut ed = editor(EditorPrefs::default());
        ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        assert_eq!(start_quit(&mut [&mut ed], true), QuitStart::StillRunning);
        assert!(!ed.confirm_close && ed.quit.is_none() && !ed.closed);
    }

    /// While a text is typed, Edit's Undo, Redo, Copy and Delete are disabled (the keys belong to
    /// the text, as `shortcuts` leaves them); opening the menu keeps the text, and typing goes on
    /// once the menu is closed.
    #[test]
    fn the_edit_menu_waits_while_a_text_is_typed() {
        let ctx = crate::theme::test_context();
        let size = dialog_checks::WINDOW.size();
        let mut ed = editor(EditorPrefs::default());
        ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        ed.select(Some(id));
        ed.tool = Tool::Text;
        ed.text = Some(TextEdit { item: None, at: pt(3.0, 4.0), buffer: String::new(), focused: false, step: None });
        sized_frame(&mut ed, &ctx, size, vec![]);
        sized_frame(&mut ed, &ctx, size, vec![egui::Event::Text("Hel".into())]);
        let find = |ed: &Editor, e: Entry| crate::menu_bar::tests::find(&menu_bar::menus(&ed.menu_state()), e).enabled;
        for e in [Entry::Undo, Entry::Copy, Entry::Delete] {
            assert!(!find(&ed, e), "{e:?} while typing");
        }
        // Opened and used while typing: the disabled entries do nothing, and the text stays.
        crate::menu_bar::tests::choose(&mut |events| sized_frame(&mut ed, &ctx, size, events), "Edit", "Delete");
        crate::menu_bar::tests::choose(&mut |events| sized_frame(&mut ed, &ctx, size, events), "Edit", "Undo");
        assert_eq!(ed.doc.items().len(), 1, "nothing was deleted or undone");
        assert_eq!(ed.text.as_ref().map(|t| t.buffer.as_str()), Some("Hel"), "the text is kept and still typed");
        // The menu closed with the click; the text gets the keyboard back.
        for _ in 0..2 {
            sized_frame(&mut ed, &ctx, size, vec![]);
        }
        sized_frame(&mut ed, &ctx, size, vec![egui::Event::Text("lo".into())]);
        assert_eq!(ed.text.as_ref().map(|t| t.buffer.as_str()), Some("Hello"), "typing goes on after the menu");
        // Done typing: the entries work again.
        ed.text = None;
        assert!(find(&ed, Entry::Undo) && find(&ed, Entry::Copy) && find(&ed, Entry::Delete));
    }

    /// While something is being captured or recorded, an editor's Capture region and Record region
    /// are greyed out and do nothing, like the main window's tiles.
    #[test]
    fn the_editors_capture_entries_wait_while_something_is_captured() {
        let ctx = crate::theme::test_context();
        let size = dialog_checks::WINDOW.size();
        let mut ed = editor(EditorPrefs::default());
        ed.busy = true;
        let menus = menu_bar::menus(&ed.menu_state());
        assert!(!crate::menu_bar::tests::find(&menus, Entry::CaptureRegion).enabled && !crate::menu_bar::tests::find(&menus, Entry::RecordRegion).enabled);
        for entry in ["Capture region", "Record region"] {
            crate::menu_bar::tests::choose(&mut |events| sized_frame(&mut ed, &ctx, size, events), "File", entry);
        }
        assert!(ed.app_requests.is_empty());
        ed.busy = false;
        crate::menu_bar::tests::choose(&mut |events| sized_frame(&mut ed, &ctx, size, events), "File", "Capture region");
        assert_eq!(ed.app_requests, vec![AppRequest::Run(crate::tray::Command::Region)]);
    }

    #[test]
    fn the_image_editors_menus_offer_its_save_undo_copy_and_delete() {
        let mut ed = editor(EditorPrefs::default());
        ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        ed.global_keys = Action::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        let find = |ed: &Editor, e: Entry| crate::menu_bar::tests::find(&menu_bar::menus(&ed.menu_state()), e);
        // Saving is this editor's; exporting and publishing a tour are the recording editor's.
        assert!(find(&ed, Entry::Save).enabled && !find(&ed, Entry::Export).enabled && !find(&ed, Entry::PublishTour).enabled);
        assert_eq!(find(&ed, Entry::Save).shortcut, "Ctrl+S");
        assert_eq!(find(&ed, Entry::CaptureRegion).shortcut, "PrtSc");
        // Nothing to undo, redo or delete yet; the picture can always be copied, as Ctrl+C copies it.
        assert!(!find(&ed, Entry::Undo).enabled && !find(&ed, Entry::Redo).enabled && !find(&ed, Entry::Delete).enabled);
        assert!(find(&ed, Entry::Copy).enabled);
        // The image editor has its Properties panel: the entry is usable, unchecked while it is closed.
        let props = find(&ed, Entry::Properties);
        assert!(props.enabled && props.checked == Some(false));
        let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        ed.select(Some(id));
        assert!(find(&ed, Entry::Undo).enabled && find(&ed, Entry::Delete).enabled && !find(&ed, Entry::Redo).enabled);
    }

    #[test]
    fn the_image_editors_menu_entries_do_what_its_keys_and_buttons_do() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let make = || {
            let mut ed = editor(EditorPrefs::default());
            ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
            ed.dest.output_dir = tmp.path().to_path_buf();
            ed
        };
        let size = dialog_checks::WINDOW.size();
        let choose = |ed: &mut Editor, menu: &str, entry: &str| crate::menu_bar::tests::choose(&mut |events| sized_frame(ed, &ctx, size, events), menu, entry);
        let rect = Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 });

        // Undo and Redo, as Ctrl+Z and Ctrl+Y.
        let mut ed = make();
        ed.add_item(rect.clone());
        choose(&mut ed, "Edit", "Undo");
        assert!(ed.doc.items().is_empty() && ed.doc.can_redo());
        choose(&mut ed, "Edit", "Redo");
        assert_eq!(ed.doc.items().len(), 1);
        // Copy and Delete act on the selection, as Ctrl+C and Delete do.
        let id = ed.doc.items()[0].id;
        ed.use_select_tool();
        ed.select(Some(id));
        choose(&mut ed, "Edit", "Copy");
        assert_eq!(ed.status_text(), Some("Copied the annotation"));
        choose(&mut ed, "Edit", "Delete");
        assert!(ed.doc.items().is_empty() && ed.selected.is_none());
        // Save, as Ctrl+S: a new file named by the pattern, in the captures folder.
        choose(&mut ed, "File", "Save");
        let saved = ed.saved.clone().expect("saved");
        assert!(saved.starts_with(tmp.path()) && saved.is_file(), "{saved:?}");
        // Close window, as the window's close button: at once when nothing would be lost...
        choose(&mut ed, "File", "Close window");
        assert!(ed.closed && !ed.confirm_close);
        // ...and after the close question when something would.
        let mut ed = make();
        ed.add_item(rect.clone());
        choose(&mut ed, "File", "Close window");
        assert!(ed.confirm_close && !ed.closed);
        // What only the app can do goes to it, as the hotkeys and the tray menu ask for it.
        let mut ed = make();
        for (menu, entry) in [("File", "Capture region"), ("File", "Record region"), ("File", "Open tours"), ("Preferences", "Hotkeys"), ("Preferences", "Workspace"), ("Help", "About Mythicshot"), ("Help", "Open the log folder"), ("Help", "Check for updates"), ("File", "Quit Mythicshot")] {
            choose(&mut ed, menu, entry);
        }
        use crate::tray::Command;
        assert_eq!(ed.app_requests, vec![
            AppRequest::Run(Command::Region),
            AppRequest::Run(Command::Record),
            AppRequest::Page(crate::main_window::Page::Tours),
            AppRequest::Preferences(crate::settings_ui::Tab::Hotkeys),
            AppRequest::Page(crate::main_window::Page::Workspace),
            AppRequest::Preferences(crate::settings_ui::Tab::General),
            AppRequest::LogFolder,
            AppRequest::CheckForUpdates,
            AppRequest::Run(Command::Exit),
        ]);
        assert!(!ed.closed && ed.doc.items().is_empty(), "nothing else happened in the editor");
        // A disabled entry does nothing: Export here.
        choose(&mut ed, "File", "Export…");
        assert!(!ed.open_export && !ed.prefs_changed && ed.app_requests.len() == 9);
        // Properties opens and closes the panel, as the toolbar's button does.
        choose(&mut ed, "Edit", "Properties");
        assert!(ed.properties_open() && ed.prefs.image_properties_open && ed.prefs_changed && ed.app_requests.len() == 9);
    }

    /// The menu bar takes no keys: with a menu open or not, the editor's own shortcuts work and a
    /// text being typed gets its letters; and Esc closes an open menu before anything else.
    #[test]
    fn the_menu_bar_leaves_the_editors_keys_and_typing_alone() {
        let ctx = crate::theme::test_context();
        let size = dialog_checks::WINDOW.size();
        let key = |k: Key, modifiers: Modifiers| egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers };
        let mut ed = editor(EditorPrefs::default());
        ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        sized_frame(&mut ed, &ctx, size, vec![]);
        // A tool's key and Ctrl+Z, with the menus closed.
        sized_frame(&mut ed, &ctx, size, vec![key(Key::E, Modifiers::NONE)]);
        assert_eq!(ed.tool, Tool::Ellipse);
        ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        sized_frame(&mut ed, &ctx, size, vec![ctrl(Key::Z)]);
        assert!(ed.doc.items().is_empty(), "Ctrl+Z undid");
        // Typing a text: the letters go to the text, not to the tools.
        ed.tool = Tool::Text;
        ed.text = Some(TextEdit { item: None, at: pt(3.0, 4.0), buffer: String::new(), focused: false, step: None });
        sized_frame(&mut ed, &ctx, size, vec![]);
        sized_frame(&mut ed, &ctx, size, vec![]);
        sized_frame(&mut ed, &ctx, size, vec![egui::Event::Text("re".into()), key(Key::R, Modifiers::NONE)]);
        assert_eq!(ed.tool, Tool::Text, "R reached the tools while a text was typed");
        assert_eq!(ed.text.as_ref().map(|t| t.buffer.as_str()), Some("re"));
        ed.text = None;
        // With the File menu open, Esc closes the menu and leaves the selection alone.
        let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        ed.use_select_tool();
        ed.select(Some(id));
        let open = crate::menu_bar::tests::open_menu(&mut |events| sized_frame(&mut ed, &ctx, size, events), "File");
        assert!(crate::menu_bar::tests::last_text_rect(&open, "Quit Mythicshot").is_some(), "the menu is open");
        sized_frame(&mut ed, &ctx, size, vec![key(Key::Escape, Modifiers::NONE)]);
        let closed = sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(crate::menu_bar::tests::last_text_rect(&closed, "Quit Mythicshot").is_none(), "Esc closed the menu");
        assert_eq!(ed.selected, Some(id), "and did not reach the editor");
        // The next Esc is the editor's again.
        sized_frame(&mut ed, &ctx, size, vec![key(Key::Escape, Modifiers::NONE)]);
        assert_eq!(ed.selected, None);
    }

    // ---- The Properties panel of the image editor ----

    /// The Properties panel as drawn: the column of surface, as wide as the panel, at the right.
    pub(crate) fn panel_of(out: &egui::FullOutput) -> Option<Rect> {
        crate::theme::checks::fills(out).into_iter().find(|(r, c, _)| *c == theme::SURFACE && (r.width() - crate::tour_cards::PANEL_W).abs() <= 2.0 && r.height() > 200.0).map(|(r, ..)| r)
    }

    /// The texts drawn inside `panel` as far as they show, top to bottom: each with where it is.
    pub(crate) fn panel_texts(out: &egui::FullOutput, panel: Rect) -> Vec<(String, Rect)> {
        fn walk(clip: Rect, s: &egui::Shape, into: &mut Vec<(String, Rect, Rect)>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(clip, s, into)),
                egui::Shape::Text(t) => into.push((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()), clip)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        out.shapes.iter().for_each(|c| walk(c.clip_rect, &c.shape, &mut all));
        let mut inside: Vec<(String, Rect)> = all.into_iter().filter(|(_, r, clip)| panel.contains(r.center()) && clip.contains(r.center())).map(|(t, r, _)| (t, r)).collect();
        inside.sort_by(|a, b| a.1.top().total_cmp(&b.1.top()));
        inside
    }

    /// Where `text` is drawn inside `panel`, if it is.
    pub(crate) fn in_panel(out: &egui::FullOutput, panel: Rect, text: &str) -> Option<Rect> {
        panel_texts(out, panel).into_iter().find(|(t, _)| t == text).map(|(_, r)| r)
    }

    /// The panel's section titles, top to bottom: its texts as big as its own heading.
    pub(crate) fn panel_headings(out: &egui::FullOutput, panel: Rect) -> Vec<String> {
        let texts = panel_texts(out, panel);
        let heading = texts.iter().find(|(t, _)| t == PROPERTIES).map(|(_, r)| r.height()).expect("the panel's heading");
        texts.into_iter().filter(|(_, r)| (r.height() - heading).abs() < 0.5).map(|(t, _)| t).collect()
    }

    /// The labels in `order` are all in the panel, each below the one before.
    pub(crate) fn assert_in_order(out: &egui::FullOutput, panel: Rect, order: &[&str], what: &str) {
        let ys: Vec<f32> = order.iter().map(|t| in_panel(out, panel, t).unwrap_or_else(|| panic!("{what}: the panel shows {t:?}: {:?}", panel_texts(out, panel))).top()).collect();
        for (w, t) in ys.windows(2).zip(order.windows(2)) {
            assert!(w[0] < w[1], "{what}: {} comes before {}: {ys:?}", t[0], t[1]);
        }
    }

    /// The Properties button lit: the selection fill behind its label in the toolbar.
    pub(crate) fn button_lit(out: &egui::FullOutput) -> bool {
        let label = crate::share_ui::tests::shapes_of(out).into_iter().find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == PROPERTIES && t.pos.y < 90.0 => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        });
        let label = label.expect("the Properties button");
        crate::theme::checks::fills(out).iter().any(|(r, c, _)| *c == theme::SELECTION && r.contains(label.center()) && r.area() < 20_000.0)
    }

    /// The settings file written and read back, as at the next start.
    pub(crate) fn restarted(prefs: &EditorPrefs) -> EditorPrefs {
        let mut settings = Settings::default();
        settings.editor = prefs.clone();
        let back: Settings = toml::from_str(&toml::to_string_pretty(&settings).unwrap()).unwrap();
        back.editor
    }

    /// The Properties button and the Edit menu's entry open and close the image editor's panel, the
    /// button is lit while it is open, and the panel's own close button closes it. Whether it is open
    /// is remembered for the next image editor, apart from the recording editor's views.
    #[test]
    fn the_properties_button_and_the_edit_menu_open_and_close_the_image_editors_panel() {
        let ctx = crate::theme::test_context();
        let size = dialog_checks::WINDOW.size();
        let mut ed = editor(EditorPrefs::default());
        assert!(!ed.properties_open(), "closed by default");
        sized_frame(&mut ed, &ctx, size, vec![]);
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(panel_of(&out).is_none() && !button_lit(&out));
        let at = crate::share_ui::tests::text_rect(&out, PROPERTIES).expect("the button is shown");
        assert!(at.right() > size.x - 100.0 && at.top() > 20.0 && at.bottom() < 80.0, "at the far right of the toolbar, under the menu bar: {at:?}");
        let click = |ed: &mut Editor, at: egui::Pos2| crate::menu_bar::tests::click_at(&mut |events| sized_frame(ed, &ctx, size, events), at);

        // Opened with the button: drawn at the right, beside the picture, and the button lit.
        click(&mut ed, at.center());
        assert!(ed.properties_open() && ed.prefs.image_properties_open && ed.prefs_changed);
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        let panel = panel_of(&out).expect("the panel is open");
        assert!((panel.right() - size.x).abs() <= 1.0, "docked at the right edge: {panel:?}");
        let canvas = crate::theme::checks::fills(&out).into_iter().filter(|(r, c, _)| *c == theme::GROUND && r.height() > 200.0 && r.width() < size.x - 10.0).map(|(r, ..)| r).max_by(|a, b| a.area().total_cmp(&b.area())).expect("the canvas");
        assert!(canvas.right() <= panel.left() + 1.0, "beside the picture, never over it: {canvas:?} {panel:?}");
        assert!(button_lit(&out), "the button is lit while the panel is open");
        assert!(menu_bar::tests::find(&menu_bar::menus(&ed.menu_state()), Entry::Properties).checked == Some(true));
        // The Tour view's and the Timeline view's settings are their own.
        assert!(ed.prefs.tour_properties_open && !ed.prefs.timeline_properties_open);
        let next = editor(restarted(&ed.prefs));
        assert!(next.properties_open(), "open in the next image editor, after a restart");

        // Closed with the button, then opened and closed with the Edit menu.
        click(&mut ed, at.center());
        assert!(!ed.properties_open() && !ed.prefs.image_properties_open);
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(panel_of(&out).is_none() && !button_lit(&out));
        assert!(!editor(restarted(&ed.prefs)).properties_open(), "closed after a restart");
        crate::menu_bar::tests::choose(&mut |events| sized_frame(&mut ed, &ctx, size, events), "Edit", "Properties");
        assert!(ed.properties_open() && ed.prefs.image_properties_open);
        crate::menu_bar::tests::choose(&mut |events| sized_frame(&mut ed, &ctx, size, events), "Edit", "Properties");
        assert!(!ed.properties_open() && !ed.prefs.image_properties_open);

        // Closed with its own close button, at the right of its heading.
        ed.toggle_properties();
        sized_frame(&mut ed, &ctx, size, vec![]);
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        let panel = panel_of(&out).unwrap();
        let heading = in_panel(&out, panel, PROPERTIES).expect("the panel's heading");
        click(&mut ed, pos2(panel.right() - 12.0 - 11.0, heading.center().y));
        assert!(!ed.properties_open() && !ed.prefs.image_properties_open);
    }

    /// The image editor's panel: exactly its three sections in order, each control on its line in
    /// order; "Hide as" and "Spotlight strength" only where the toolbar shows theirs.
    #[test]
    fn the_image_editors_panel_shows_this_shape_the_picture_and_the_editor_in_order() {
        let ctx = crate::theme::test_context();
        let size = egui::vec2(1280.0, 1600.0);
        let frames = |ed: &mut Editor| {
            for _ in 0..2 {
                sized_frame(ed, &ctx, size, vec![]);
            }
            sized_frame(ed, &ctx, size, vec![])
        };
        let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
        let out = frames(&mut ed);
        let panel = panel_of(&out).expect("open");
        assert_eq!(panel_headings(&out, panel), ["Properties", "This shape", "Picture", "In this editor"]);
        let order = ["Properties", "This shape", "Line and text colour", "Fill", "Line width", "Text size", "Shadow", "Picture", "64 × 48", "Resize", "25%", "200%", "Effects", "Drop shadow", "Rotate left", "In this editor", "Zoom"];
        assert_in_order(&out, panel, &order, "the image editor's panel");
        for absent in ["Hide as", "Pixelate", "Spotlight strength"] {
            assert!(in_panel(&out, panel, absent).is_none(), "{absent} only for an obfuscate shape or a spotlight");
        }
        // The effects as the Effects menu lists them, Resize being a line of its own.
        for e in ["Drop shadow", "Torn edge", "Border (current colour and width)", "Greyscale", "Invert", "Rotate right", "Rotate left"] {
            assert!(in_panel(&out, panel, e).is_some(), "{e}");
        }
        for p in RESIZE {
            assert!(in_panel(&out, panel, &format!("{p}%")).is_some(), "{p}%");
        }
        // The fill colour only while Fill is on.
        let swatches = |out: &egui::FullOutput, area: Rect| crate::theme::checks::fills(out).iter().filter(|(r, c, _)| *c == Color32::WHITE && area.contains(r.center()) && r.area() < 2_000.0).count();
        assert_eq!(swatches(&out, panel), 0);
        ed.style.fill = Some(Rgba([255, 255, 255, 255]));
        let out = frames(&mut ed);
        assert!(swatches(&out, panel_of(&out).unwrap()) >= 1, "the fill colour button");

        // The Obfuscate tool, and a selected obfuscate shape: "Hide as" in the panel and the choice in the toolbar.
        let toolbar = |out: &egui::FullOutput, text: &str| crate::share_ui::tests::shapes_of(out).into_iter().any(|s| matches!(s, egui::Shape::Text(t) if t.galley.text() == text && t.pos.y < 90.0));
        let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
        ed.set_tool(Tool::Obfuscate);
        let out = frames(&mut ed);
        let panel = panel_of(&out).unwrap();
        assert_in_order(&out, panel, &["Shadow", "Hide as", "Picture"], "the Obfuscate tool");
        assert!(toolbar(&out, "Pixelate"), "the toolbar shows its choice too");
        ed.set_tool(Tool::Select);
        let out = frames(&mut ed);
        assert!(in_panel(&out, panel_of(&out).unwrap(), "Hide as").is_none() && !toolbar(&out, "Pixelate"), "not for the Select tool with nothing selected");
        let id = ed.add_item(Shape::Obfuscate { rect: Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }, mode: ObfuscateMode::Blur });
        ed.select(Some(id));
        let out = frames(&mut ed);
        assert!(in_panel(&out, panel_of(&out).unwrap(), "Hide as").is_some() && toolbar(&out, "Blur"), "for a selected obfuscate shape, in both places");

        // The Spotlight tool: "Spotlight strength" in the panel where the toolbar shows its slider.
        ed.select(None);
        ed.tool = Tool::Spotlight;
        let out = frames(&mut ed);
        let panel = panel_of(&out).unwrap();
        assert_in_order(&out, panel, &["Shadow", "Spotlight strength", "Picture"], "the Spotlight tool");
        assert!(toolbar(&out, "Spotlight · darken"));
        ed.tool = Tool::Rectangle;
        let out = frames(&mut ed);
        assert!(in_panel(&out, panel_of(&out).unwrap(), "Spotlight strength").is_none() && !toolbar(&out, "Spotlight · darken"));
    }

    /// A change made in the panel is the change the toolbar makes: the same state, shown in both
    /// places, remembered as the tool's style, and one undo step, undone and redone alike.
    #[test]
    fn a_change_in_the_image_editors_panel_is_the_toolbars_and_undoes_alike() {
        let size = dialog_checks::WINDOW.size();
        let swatches = |out: &egui::FullOutput, area: Rect| crate::theme::checks::fills(out).iter().filter(|(r, c, _)| *c == Color32::WHITE && area.contains(r.center()) && r.area() < 2_000.0).count();
        // Fill switched on with the panel's check box, or with the toolbar's button, on a fresh editor each.
        let mut results = Vec::new();
        for in_the_panel in [true, false] {
            let ctx = crate::theme::test_context();
            let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
            let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
            ed.select(Some(id));
            sized_frame(&mut ed, &ctx, size, vec![]);
            let out = sized_frame(&mut ed, &ctx, size, vec![]);
            let panel = panel_of(&out).unwrap();
            let band = Rect::from_min_max(pos2(0.0, 20.0), pos2(size.x, 90.0));
            assert_eq!((swatches(&out, panel), swatches(&out, band)), (0, 0), "no fill yet");
            let at = if in_the_panel {
                in_panel(&out, panel, "Fill").expect("the panel's Fill")
            } else {
                crate::share_ui::tests::shapes_of(&out).into_iter().find_map(|s| match s {
                    egui::Shape::Text(t) if t.galley.text() == "Fill" && band.contains(t.pos) => Some(t.galley.rect.translate(t.pos.to_vec2())),
                    _ => None,
                }).expect("the toolbar's Fill")
            };
            crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), at.center());
            let fill = Some(Rgba([255, 255, 255, 255]));
            assert_eq!(ed.style.fill, fill, "panel {in_the_panel}: the state both read");
            assert_eq!(ed.doc.item(id).unwrap().style.fill, fill, "panel {in_the_panel}: the selection");
            assert_eq!(ed.prefs.styles.get("rectangle").map(|s| s.fill), Some(fill.map(|f| f.0)), "panel {in_the_panel}: the rectangle's remembered style");
            let out = sized_frame(&mut ed, &ctx, size, vec![]);
            assert!(swatches(&out, panel) >= 1 && swatches(&out, band) >= 1, "panel {in_the_panel}: the fill colour shows in both places");
            // One undo step: undone, the shape is there without fill; redone, with.
            ed.run(EditorAction::Undo);
            let undone = (ed.doc.items().len(), ed.doc.item(id).map(|it| it.style.fill));
            ed.run(EditorAction::Redo);
            let redone = (ed.doc.items().len(), ed.doc.item(id).map(|it| it.style.fill));
            results.push((undone, redone));
        }
        assert_eq!(results[0], results[1], "the panel and the toolbar undo and redo alike");
        assert_eq!(results[0], ((1, Some(None)), (1, Some(Some(Rgba([255, 255, 255, 255]))))));

        // Shadow in the panel: the same state as the toolbar's shadow button, one undo step.
        let ctx = crate::theme::test_context();
        let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
        let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        ed.select(Some(id));
        let shadow = ed.style.shadow;
        sized_frame(&mut ed, &ctx, size, vec![]);
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        let at = in_panel(&out, panel_of(&out).unwrap(), "Shadow").unwrap();
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), at.center());
        assert_eq!((ed.style.shadow, ed.doc.item(id).unwrap().style.shadow), (!shadow, !shadow));
        ed.run(EditorAction::Undo);
        assert_eq!((ed.doc.items().len(), ed.doc.item(id).unwrap().style.shadow), (1, shadow));

        // An effect and a resize from the panel: the same undo step as from the Effects menu.
        let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
        sized_frame(&mut ed, &ctx, size, vec![]);
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        let at = in_panel(&out, panel_of(&out).unwrap(), "50%").unwrap();
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), at.center());
        assert_eq!((ed.doc.width(), ed.doc.height()), (32, 24), "resized to half");
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(in_panel(&out, panel_of(&out).unwrap(), "32 × 24").is_some(), "the picture's size follows");
        ed.run(EditorAction::Undo);
        assert_eq!((ed.doc.width(), ed.doc.height()), (64, 48));
        let mut by_menu = editor(EditorPrefs::default());
        by_menu.effect(resize_effect(50));
        assert_eq!((by_menu.doc.width(), by_menu.doc.can_undo()), (32, true));

        // The zoom chosen in the panel is the status bar's.
        let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
        sized_frame(&mut ed, &ctx, size, vec![]);
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        let panel = panel_of(&out).unwrap();
        let combo = panel_texts(&out, panel).into_iter().find(|(t, _)| t.starts_with("Fit ")).expect("the panel's zoom").1;
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), combo.center());
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        let choice = crate::menu_bar::tests::last_text_rect(&out, "200%").expect("the zoom choices");
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), choice.center());
        assert_eq!(ed.zoom, Some(2.0));
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        let status = crate::share_ui::tests::shapes_of(&out).into_iter().filter(|s| matches!(s, egui::Shape::Text(t) if t.galley.text() == "200%" && t.pos.y > size.y - 40.0)).count();
        assert!(status >= 1, "the status bar shows the zoom chosen in the panel");
    }

    /// A change made in the panel asks for another frame at once, so the toolbar (drawn before the
    /// panel) shows it without waiting for the next input.
    #[test]
    fn a_change_in_the_panel_draws_the_toolbar_again_at_once() {
        let ctx = crate::theme::test_context();
        let size = dialog_checks::WINDOW.size();
        let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
        for _ in 0..3 {
            sized_frame(&mut ed, &ctx, size, vec![]);
        }
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        let at = in_panel(&out, panel_of(&out).unwrap(), "Shadow").unwrap().center();
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        sized_frame(&mut ed, &ctx, size, vec![egui::Event::PointerMoved(at), button(true)]);
        let before = ed.style.shadow;
        let released = sized_frame(&mut ed, &ctx, size, vec![button(false)]);
        assert_ne!(ed.style.shadow, before, "the click changed the shadow");
        assert!(repaint_asked(&released), "the frame of the change asked for another");
    }

    /// Every label of the image editor's panel fits it at 100%, with each conditional line shown, in
    /// the editor's own window and at the narrowest the editor allows.
    #[test]
    fn the_image_editors_panel_labels_fit_it() {
        for size in [dialog_checks::WINDOW.size(), egui::vec2(EDITOR_MIN_SIZE[0], EDITOR_MIN_SIZE[1])] {
            for tool in [Tool::Obfuscate, Tool::Spotlight, Tool::Text] {
                let ctx = crate::theme::test_context();
                let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
                ed.tool = tool;
                ed.style.fill = Some(Rgba([255, 255, 255, 255]));
                for _ in 0..2 {
                    sized_frame(&mut ed, &ctx, size, vec![]);
                }
                let out = sized_frame(&mut ed, &ctx, size, vec![]);
                let what = format!("the image editor's panel, {tool:?}, {size:?}");
                let panel = panel_of(&out).unwrap_or_else(|| panic!("{what}: open"));
                let texts = panel_texts(&out, panel);
                assert!(texts.len() >= 10, "{what}: {texts:?}");
                for (t, r) in &texts {
                    assert!(panel.expand(0.5).contains_rect(*r), "{what}: {t:?} at {r:?} sticks out of the panel {panel:?}");
                }
                assert!(crate::theme::checks::assert_texts_fit(&out, &what) >= 3);
            }
        }
    }

    /// The toolbar's overflow cue works as before with the panel open: none where the row fits, a
    /// chevron where buttons are hidden, and a click on it scrolls.
    /// Two editors with their panels open keep their own scroll: scrolling one leaves the other where it was.
    #[test]
    fn two_editors_panels_scroll_on_their_own() {
        let ctx = crate::theme::test_context();
        let size = egui::vec2(EDITOR_MIN_SIZE[0], EDITOR_MIN_SIZE[1]);
        let prefs = EditorPrefs { image_properties_open: true, ..EditorPrefs::default() };
        let (mut a, mut b) = (editor(prefs.clone()), editor(prefs));
        let picture_y = |ed: &mut Editor| {
            for _ in 0..2 {
                sized_frame(ed, &ctx, size, vec![]);
            }
            let out = sized_frame(ed, &ctx, size, vec![]);
            let panel = panel_of(&out).expect("open");
            in_panel(&out, panel, "Picture").expect("Picture in sight").top()
        };
        let (a0, b0) = (picture_y(&mut a), picture_y(&mut b));
        assert!((a0 - b0).abs() < 0.5, "both start at the top: {a0} {b0}");
        // Scroll A's panel down with the wheel over it.
        let over = pos2(size.x - 100.0, size.y / 2.0);
        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -100.0), modifiers: Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
        sized_frame(&mut a, &ctx, size, vec![egui::Event::PointerMoved(over), wheel]);
        for _ in 0..3 {
            sized_frame(&mut a, &ctx, size, vec![egui::Event::PointerMoved(over)]);
        }
        let a1 = picture_y(&mut a);
        assert!(a1 < a0 - 20.0, "A's panel scrolled: {a0} -> {a1}");
        let b1 = picture_y(&mut b);
        assert!((b1 - b0).abs() < 0.5, "B's panel stayed where it was: {b0} -> {b1}");
    }

    /// The panel's "Effects" and "Resize" carry the toolbar's Effects button's hover text: an effect
    /// flattens the annotations into the picture.
    #[test]
    fn the_panels_effects_and_resize_say_what_the_effects_button_says() {
        let size = egui::vec2(1280.0, 1600.0);
        for label in ["Effects", "Resize"] {
            let ctx = crate::theme::test_context();
            let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
            sized_frame(&mut ed, &ctx, size, vec![]);
            let out = sized_frame(&mut ed, &ctx, size, vec![]);
            let at = in_panel(&out, panel_of(&out).unwrap(), label).unwrap_or_else(|| panic!("{label}")).center();
            let hint = |out: &egui::FullOutput| crate::share_ui::tests::text_rect(out, EFFECTS_HINT);
            assert!(hint(&out).is_none(), "no hover text before the pointer is there");
            sized_frame(&mut ed, &ctx, size, vec![egui::Event::PointerMoved(at)]);
            for _ in 0..3 {
                sized_frame(&mut ed, &ctx, size, vec![]);
            }
            let out = sized_frame(&mut ed, &ctx, size, vec![]);
            let shown = hint(&out).unwrap_or_else(|| panic!("{label}: the hover text shows"));
            assert!((shown.center().y - at.y).abs() < 80.0, "{label}: by the label: {shown:?} {at:?}");
        }
    }

    /// Where the box a text is typed in was drawn last.
    fn text_box(ctx: &egui::Context) -> Rect {
        ctx.memory(|m| m.area_rect(egui::Id::unique(TEXT_AREA))).expect("the text box")
    }

    /// With the panel open, the box a text is typed in stays over the canvas and never covers the
    /// panel's rows; with the panel closed it is placed exactly as before, at the text's spot.
    #[test]
    fn the_text_box_keeps_off_the_panel_and_is_placed_as_before_without_it() {
        let size = egui::vec2(EDITOR_MIN_SIZE[0], EDITOR_MIN_SIZE[1]);
        let typing = |open: bool, img: RgbaImage, at: Pt| {
            let ctx = crate::theme::test_context();
            let s = Settings::default();
            let mut ed = Editor::new(img, "t".into(), BTreeMap::new(), s.dest_context("t"), EditorPrefs { image_properties_open: open, ..EditorPrefs::default() });
            ed.zoom = Some(1.0);
            ed.tool = Tool::Text;
            ed.text = Some(TextEdit { item: None, at, buffer: "Here".into(), focused: true, step: None });
            for _ in 0..3 {
                sized_frame(&mut ed, &ctx, size, vec![]);
            }
            let out = sized_frame(&mut ed, &ctx, size, vec![]);
            (text_box(&ctx), ed.last_to_scr(at) + vec2(0.0, -4.0), panel_of(&out), out)
        };
        // Closed: at the text's spot, as before.
        let (r, spot, panel, _) = typing(false, RgbaImage::new(64, 48), pt(3.0, 4.0));
        assert!(panel.is_none());
        assert!((r.min - spot).length() <= 1.0, "at the spot: {r:?} {spot:?}");
        // Near the right edge it is kept inside the window, as egui keeps it, reaching where the panel would be.
        let (r, spot, _, _) = typing(false, RgbaImage::new(64, 48), pt(60.0, 4.0));
        assert!((r.top() - spot.y).abs() <= 1.0 && (r.right() - size.x).abs() <= 1.0 && r.left() > size.x - crate::tour_cards::PANEL_W - 50.0, "kept inside the window, as before: {r:?} {spot:?}");
        // Open: inside the canvas, off the panel, at the same height.
        for at in [pt(3.0, 4.0), pt(60.0, 4.0)] {
            let (r, spot, panel, out) = typing(true, RgbaImage::new(64, 48), at);
            let panel = panel.expect("open");
            assert!(r.right() <= panel.left() + 0.5 && !r.intersects(panel.shrink(0.5)), "off the panel: {r:?} {panel:?}");
            assert!((r.top() - spot.y).abs() <= 1.0, "at the text's height: {r:?} {spot:?}");
            for row in ["Line width", "Text size"] {
                let lr = in_panel(&out, panel, row).unwrap_or_else(|| panic!("{row}"));
                assert!(!r.intersects(lr), "{row} at {lr:?} is not under the box {r:?}");
            }
            assert!(crate::theme::checks::assert_texts_fit(&out, "typing with the panel open") >= 3);
        }
    }

    /// A button of the panel that changes the picture, clicked with the mouse, does not keep the
    /// keyboard: a later Space or Enter does not apply it again. One click, one effect, one undo step.
    #[test]
    fn a_panel_effect_clicked_once_is_applied_once_whatever_keys_follow() {
        let size = egui::vec2(1280.0, 1600.0);
        for (button, after) in [("Rotate right", (48, 64)), ("50%", (32, 24)), ("Invert", (64, 48))] {
            let ctx = crate::theme::test_context();
            let mut ed = editor(EditorPrefs { image_properties_open: true, ..EditorPrefs::default() });
            ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
            sized_frame(&mut ed, &ctx, size, vec![]);
            let out = sized_frame(&mut ed, &ctx, size, vec![]);
            let at = in_panel(&out, panel_of(&out).unwrap(), button).unwrap_or_else(|| panic!("{button}")).center();
            crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), at);
            let pixel = ed.doc.flatten().get_pixel(0, 0).0;
            assert_eq!((ed.doc.width(), ed.doc.height()), after, "{button}: applied");
            assert_eq!(ctx.memory(|m| m.focused()), None, "{button}: the button let go of the keyboard");
            for key in [Key::Space, Key::Enter] {
                for pressed in [true, false] {
                    sized_frame(&mut ed, &ctx, size, vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
                }
                sized_frame(&mut ed, &ctx, size, vec![]);
            }
            assert_eq!(((ed.doc.width(), ed.doc.height()), ed.doc.flatten().get_pixel(0, 0).0), (after, pixel), "{button}: not applied again");
            ed.run(EditorAction::Undo);
            assert_eq!((ed.doc.width(), ed.doc.height(), ed.doc.can_undo()), (64, 48, false), "{button}: exactly one undo step");
        }
    }

    #[test]
    fn the_toolbar_overflow_cue_works_with_the_panel_open() {
        let ctx = crate::theme::test_context();
        let prefs = EditorPrefs { image_properties_open: true, ..EditorPrefs::default() };
        let settle = |ed: &mut Editor, size: egui::Vec2| {
            let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
            ed.select(Some(id));
            for _ in 0..3 {
                sized_frame(ed, &ctx, size, vec![]);
            }
            sized_frame(ed, &ctx, size, vec![])
        };
        let mut ed = editor(prefs.clone());
        let out = settle(&mut ed, dialog_checks::WINDOW.size());
        assert!(panel_of(&out).is_some());
        assert_eq!(ed.toolbar_scroll.cues, [None, None], "no cue when everything fits");
        let size = egui::vec2(480.0, EDITOR_MIN_SIZE[1]);
        let mut ed = editor(prefs);
        settle(&mut ed, size);
        let right = ed.toolbar_scroll.cues[1].expect("a cue at the right edge");
        let before = ed.toolbar_scroll.offset;
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), right.center());
        sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(ed.toolbar_scroll.offset > before + 1.0 && ed.toolbar_scroll.cues[0].is_some(), "the chevron scrolls: {:?}", ed.toolbar_scroll);
    }

    /// Nothing sticks out of the toolbar at the narrowest window the editor allows, at 100%, 125%
    /// and 200%: with a shape selected (the longest row of properties) and while a text is typed,
    /// every label lies inside the window and the toolbar, and the Properties button is whole, at
    /// the right, and covers nothing else.
    #[test]
    fn nothing_sticks_out_of_the_toolbar_at_the_narrowest_window_and_every_scale() {
        let size = egui::vec2(EDITOR_MIN_SIZE[0], EDITOR_MIN_SIZE[1]);
        for ppp in [1.0, 1.25, 2.0] {
            for (typing, open) in [(false, false), (true, false), (false, true), (true, true)] {
                let ctx = crate::theme::test_context();
                ctx.set_pixels_per_point(ppp);
                let mut ed = editor(EditorPrefs { image_properties_open: open, ..EditorPrefs::default() });
                if typing {
                    ed.tool = Tool::Text;
                    ed.text = Some(TextEdit { item: None, at: pt(3.0, 4.0), buffer: "Here".into(), focused: true, step: None });
                } else {
                    let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
                    ed.select(Some(id));
                }
                for _ in 0..2 {
                    sized_frame(&mut ed, &ctx, size, vec![]);
                }
                let out = sized_frame(&mut ed, &ctx, size, vec![]);
                let what = format!("the editor at {ppp}, typing {typing}, panel open {open}");
                assert_eq!(panel_of(&out).is_some(), open, "{what}");
                assert_toolbar_fits(&out, size, &what);
                assert!(crate::theme::checks::assert_texts_fit(&out, &what) >= 3);
                let mut ed2 = editor(EditorPrefs { image_properties_open: open, ..EditorPrefs::default() });
                assert_eq!(crate::snapshot::uncovered(&crate::theme::test_context(), size, ppp, 3, |ui| ed2.ui(ui)), 0, "{what}");
            }
        }
    }

    /// The painted lines (paths and segments) of a frame that lie inside `r`.
    fn lines_in(out: &egui::FullOutput, r: Rect) -> usize {
        fn walk(s: &egui::Shape, r: Rect, n: &mut usize) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, r, n)),
                egui::Shape::Path(p) if !p.points.is_empty() && p.points.iter().all(|q| r.expand(0.5).contains(*q)) => *n += 1,
                egui::Shape::LineSegment { points, .. } if points.iter().all(|q| r.expand(0.5).contains(*q)) => *n += 1,
                _ => {}
            }
        }
        let mut n = 0;
        out.shapes.iter().for_each(|c| walk(&c.shape, r, &mut n));
        n
    }

    /// An editor with a shape selected (the longest toolbar row), drawn a few frames at `size`.
    fn overflowing(ctx: &egui::Context, size: egui::Vec2) -> (Editor, egui::FullOutput) {
        let mut ed = editor(EditorPrefs::default());
        let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        ed.select(Some(id));
        for _ in 0..3 {
            sized_frame(&mut ed, ctx, size, vec![]);
        }
        let out = sized_frame(&mut ed, ctx, size, vec![]);
        (ed, out)
    }

    /// With room for the whole toolbar row, no chevron is drawn on either side.
    #[test]
    fn the_toolbar_shows_no_overflow_cue_when_everything_fits() {
        let ctx = crate::theme::test_context();
        let (ed, _) = overflowing(&ctx, dialog_checks::WINDOW.size());
        assert!(ed.toolbar_scroll.content_w > 0.0 && ed.toolbar_scroll.content_w <= ed.toolbar_scroll.view_w + 0.5, "the row fits: {:?}", ed.toolbar_scroll);
        assert_eq!(ed.toolbar_scroll.cues, [None, None], "no cue when everything fits");
    }

    /// On a window narrower than the row (smaller than the editor allows, so the test does not hang on
    /// the row's exact width) the row overflows: a chevron at the right edge, none at the left, both
    /// left of the Properties button; a click on it scrolls right and brings the left chevron; a
    /// click on that scrolls back. Scrolled to the end, the right chevron goes.
    #[test]
    fn the_toolbar_shows_a_chevron_where_buttons_are_hidden_and_a_click_scrolls() {
        let ctx = crate::theme::test_context();
        let size = egui::vec2(480.0, EDITOR_MIN_SIZE[1]);
        let (mut ed, out) = overflowing(&ctx, size);
        assert!(ed.toolbar_scroll.content_w > ed.toolbar_scroll.view_w + 0.5, "the row overflows: {:?}", ed.toolbar_scroll);
        let [left, right] = ed.toolbar_scroll.cues;
        assert!(left.is_none(), "nothing is hidden to the left at first");
        let right = right.expect("a cue at the right edge");
        assert!(lines_in(&out, right) >= 1, "the chevron is painted in {right:?}");
        let props = crate::share_ui::tests::text_rect(&out, PROPERTIES).expect("the Properties button");
        assert!(right.right() <= props.left() && right.top() < props.bottom() && right.bottom() > props.top(), "the cue {right:?} sits in the toolbar, left of Properties {props:?}");

        let before = ed.toolbar_scroll.offset;
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), right.center());
        let out = sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(ed.toolbar_scroll.offset > before + 1.0, "a click on the right chevron scrolls right: {before} -> {:?}", ed.toolbar_scroll);
        let left = ed.toolbar_scroll.cues[0].expect("a cue at the left edge once scrolled");
        assert!(lines_in(&out, left) >= 1, "the left chevron is painted in {left:?}");
        assert!(left.right() <= right.left(), "the left cue is at the left edge");

        let scrolled = ed.toolbar_scroll.offset;
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), left.center());
        sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(ed.toolbar_scroll.offset < scrolled - 1.0, "a click on the left chevron scrolls back: {scrolled} -> {:?}", ed.toolbar_scroll);

        for _ in 0..20 {
            let Some(r) = ed.toolbar_scroll.cues[1] else { break };
            crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), r.center());
            sized_frame(&mut ed, &ctx, size, vec![]);
        }
        assert!(ed.toolbar_scroll.cues[1].is_none() && ed.toolbar_scroll.cues[0].is_some(), "scrolled to the end, only the left cue: {:?}", ed.toolbar_scroll);
        assert!((ed.toolbar_scroll.offset + ed.toolbar_scroll.view_w - ed.toolbar_scroll.content_w).abs() <= 1.0, "the last button is reachable: {:?}", ed.toolbar_scroll);
    }

    /// Whether the frame asked to be drawn again at once.
    fn repaint_asked(out: &egui::FullOutput) -> bool {
        out.viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.repaint_delay == std::time::Duration::ZERO)
    }

    /// The row's cue state is decided from the width it had the frame before; when the row grows
    /// or shrinks past its room, that frame asks for another, so the cue is right without waiting
    /// for the next input. On a steady row nothing asks for one. (When it grows because buttons
    /// appear, egui asks for a frame of its own as well; the shrink is the case only this covers.)
    #[test]
    fn a_toolbar_row_that_grows_or_shrinks_past_its_room_asks_for_another_frame() {
        let rect = || Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 });
        let settle = |ed: &mut Editor, ctx: &egui::Context, size: egui::Vec2| {
            for _ in 0..3 {
                sized_frame(ed, ctx, size, vec![]);
            }
            sized_frame(ed, ctx, size, vec![])
        };
        // A window wide enough for the row with nothing selected, too narrow with a shape selected.
        let size = (500..=1200)
            .step_by(10)
            .map(|w| egui::vec2(w as f32, EDITOR_MIN_SIZE[1]))
            .find(|&size| {
                let mut plain = editor(EditorPrefs::default());
                settle(&mut plain, &crate::theme::test_context(), size);
                let mut sel = editor(EditorPrefs::default());
                let id = sel.add_item(rect());
                sel.select(Some(id));
                settle(&mut sel, &crate::theme::test_context(), size);
                plain.toolbar_scroll.cues == [None, None] && plain.toolbar_scroll.areas == [None, None] && sel.toolbar_scroll.cues[1].is_some()
            })
            .expect("a width between the two rows");
        let ctx = crate::theme::test_context();
        let mut ed = editor(EditorPrefs::default());
        let steady = settle(&mut ed, &ctx, size);
        assert!(!repaint_asked(&steady), "a steady row asks for nothing");
        let id = ed.add_item(rect());
        ed.select(Some(id));
        let grew = sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(repaint_asked(&grew), "the row grew past its room: another frame is asked for");
        sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(ed.toolbar_scroll.cues[1].is_some(), "the frame it asked for shows the cue");
        // egui asks for a frame or two of its own after new widgets appear; then the row is steady.
        assert!((0..5).any(|_| !repaint_asked(&sized_frame(&mut ed, &ctx, size, vec![]))), "the row with the cue settles");
        // Removing the selection's buttons adds no widget, so egui asks for nothing of its own here.
        ed.select(None);
        let shrank = sized_frame(&mut ed, &ctx, size, vec![]);
        assert!(repaint_asked(&shrank), "the row shrank back into its room: another frame is asked for");
        sized_frame(&mut ed, &ctx, size, vec![]);
        assert_eq!((ed.toolbar_scroll.cues, ed.toolbar_scroll.areas), ([None, None], [None, None]), "no cue and no room kept for one");
        // The editor's first frame knows no width yet: an overflowing row asks for a second at once.
        let mut first = editor(EditorPrefs::default());
        let id = first.add_item(rect());
        first.select(Some(id));
        assert!(repaint_asked(&sized_frame(&mut first, &crate::theme::test_context(), size, vec![])));
    }

    /// The widgets registered over `r` in the frame just drawn.
    fn senses_at(ctx: &egui::Context, r: Rect) -> Vec<egui::Sense> {
        ctx.viewport(|v| v.this_pass.widgets.layers().flat_map(|(_, ws)| ws.iter()).filter(|w| w.rect == r).map(|w| w.sense).collect())
    }

    /// The cue areas never take the keyboard focus (Tab would land on a blank area, and the editor's
    /// keys wait while something has the focus); a side with nothing hidden takes no click at all.
    #[test]
    fn the_toolbar_cue_areas_take_no_focus_and_a_blank_one_no_click() {
        let ctx = crate::theme::test_context();
        let size = egui::vec2(480.0, EDITOR_MIN_SIZE[1]);
        let mut ed = editor(EditorPrefs::default());
        let id = ed.add_item(Shape::Rect(Bounds { x0: 5.0, y0: 5.0, x1: 30.0, y1: 30.0 }));
        ed.select(Some(id));
        for _ in 0..4 {
            sized_frame(&mut ed, &ctx, size, vec![]);
        }
        let [left, right] = ed.toolbar_scroll.areas.map(|a| a.expect("both areas kept while the row overflows"));
        assert!(ed.toolbar_scroll.cues[0].is_none() && ed.toolbar_scroll.cues[1] == Some(right), "nothing hidden on the left yet");
        let (l, r) = (senses_at(&ctx, left), senses_at(&ctx, right));
        assert!(!l.is_empty() && !r.is_empty(), "both areas are laid out: {l:?} {r:?}");
        assert!(l.iter().chain(&r).all(|s| !s.is_focusable()), "no cue area is focusable: {l:?} {r:?}");
        assert!(l.iter().all(|s| !s.senses_click()), "the blank left area takes no click: {l:?}");
        assert!(r.iter().any(|s| s.senses_click()), "the right chevron does: {r:?}");
        // Tab around the window: the focus never lands on a cue area.
        let tab = egui::Event::Key { key: Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        for _ in 0..40 {
            sized_frame(&mut ed, &ctx, size, vec![tab.clone()]);
            if let Some(f) = ctx.memory(|m| m.focused()) {
                let at = ctx.viewport(|v| v.this_pass.widgets.get(f).map(|w| w.rect));
                assert!(at.is_none_or(|a| a != left && a != right), "Tab focused a cue area at {at:?}");
            }
        }
        // A click on the blank left area scrolls nothing.
        let offset = ed.toolbar_scroll.offset;
        crate::menu_bar::tests::click_at(&mut |events| sized_frame(&mut ed, &ctx, size, events), left.center());
        sized_frame(&mut ed, &ctx, size, vec![]);
        assert_eq!((ed.toolbar_scroll.offset, ed.toolbar_scroll.scroll_to), (offset, None));
    }

    /// The toolbar of a frame `size` points big: the band of surface just under the menu bar.
    /// Every text in it, as far as it shows, lies inside the window, and the Properties button is
    /// whole, at the right end of the band, with no other text showing under it.
    pub(crate) fn assert_toolbar_fits(out: &egui::FullOutput, size: egui::Vec2, what: &str) {
        let window = Rect::from_min_size(pos2(0.0, 0.0), size);
        // The toolbar's "Properties" (the panel's heading, when it is open, is lower down).
        let label = crate::share_ui::tests::shapes_of(out)
            .into_iter()
            .find_map(|s| match s {
                egui::Shape::Text(t) if t.galley.text() == PROPERTIES && t.pos.y < 90.0 => Some(t.galley.rect.translate(t.pos.to_vec2())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{what}: the Properties button"));
        // The button: the smallest filled or outlined rectangle around its whole label.
        let button = crate::share_ui::tests::shapes_of(out)
            .into_iter()
            .filter_map(|s| match s {
                egui::Shape::Rect(r) if (r.fill.a() > 0 || r.stroke.width > 0.0) && r.rect.contains_rect(label) && r.rect.area() < 20_000.0 => Some(r.rect),
                _ => None,
            })
            .min_by(|a, b| a.area().total_cmp(&b.area()))
            .unwrap_or_else(|| panic!("{what}: the Properties button's frame"));
        let band = crate::theme::checks::fills(out).into_iter().filter(|(r, c, _)| *c == theme::SURFACE && r.contains(button.center()) && r.width() >= size.x - 1.0).map(|(r, ..)| r).min_by(|a, b| a.area().total_cmp(&b.area()));
        let band = band.unwrap_or_else(|| panic!("{what}: the toolbar's band"));
        assert!(window.contains_rect(button) && band.expand(0.5).contains_rect(button), "{what}: the Properties button {button:?} is whole, in the toolbar {band:?}");
        assert!(button.right() >= size.x - 16.0, "{what}: at the far right: {button:?}");
        // Each text as it shows: cut to its clip, so what scrolls under the button is not counted.
        fn texts(clip: Rect, s: &egui::Shape, into: &mut Vec<(Rect, egui::epaint::TextShape)>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| texts(clip, s, into)),
                egui::Shape::Text(t) => into.push((clip, t.clone())),
                _ => {}
            }
        }
        let mut shown = Vec::new();
        out.shapes.iter().for_each(|c| texts(c.clip_rect, &c.shape, &mut shown));
        for (clip, t) in shown {
            {
                let r = t.visual_bounding_rect().intersect(clip);
                if !r.is_positive() || !band.contains(r.center()) {
                    continue;
                }
                assert!(window.expand(0.5).contains_rect(r) && band.expand(0.5).contains_rect(r), "{what}: {:?} at {r:?} leaves the toolbar {band:?}", t.galley.text());
                if t.galley.text() != PROPERTIES {
                    assert!(!r.intersects(button), "{what}: {:?} at {r:?} is under the Properties button {button:?}", t.galley.text());
                }
            }
        }
    }

    /// An editor window is told once, on its first frame, to draw its title bar dark.
    #[test]
    fn an_editor_window_is_told_once_to_draw_its_title_bar_dark() {
        let ctx = crate::theme::test_context();
        ctx.set_embed_viewports(false);
        let window = EditorWindow::new(editor(EditorPrefs::default()));
        let icon = Arc::new(egui::IconData::default());
        for (first, want) in [(true, 1), (false, 0), (false, 0)] {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| window.show(ui.ctx(), icon.clone(), first));
            out.textures_delta.clear();
            assert_eq!(crate::main_window::tests::dark_title_commands(&out, window.id), want, "first frame: {first}");
        }
    }

    /// Pictures of the editor for the owner, drawn without a window (see `snapshot`): alone, and
    /// with "Publish to…" open on its form, and with a rectangle selected on a white page and on a
    /// dark one. The pictures in it are made here: bands and blocks, like a screenshot of some
    /// app. Nothing is asked of any site.
    #[test]
    #[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
    fn snapshot_the_editor_and_the_share_dialog() {
        let shot = RgbaImage::from_fn(960, 600, |x, y| {
            let band = if y < 48 { [40, 44, 52] } else if x < 200 { [235, 237, 240] } else { [250, 250, 251] };
            let block = (260..900).contains(&x) && ((90..170).contains(&y) || (200..330).contains(&y) || (360..560).contains(&y)) && (x / 4 + y / 4) % 1 == 0;
            let c = if block { [212, 222, 240] } else { band };
            image::Rgba([c[0], c[1], c[2], 255])
        });
        let s = Settings::default();
        let make = || Editor::new(shot.clone(), "Loot window".into(), BTreeMap::new(), s.dest_context("Loot window"), EditorPrefs::default());
        let size = dialog_checks::WINDOW.size();
        let mut ed = make();
        let shot = crate::snapshot::shot(&crate::theme::test_context(), size, 1.5, 4, |ui| ed.ui(ui));
        assert_eq!(crate::snapshot::save(&shot, "editor"), 0, "the editor: nothing behind the window shows");
        // The dialog's form drawn over the editor in the same frame. Through the editor it would
        // first look for a saved token on this computer; here it is shown as it is once one is found.
        let mut ed = make();
        let details = crate::share_ui::image_details(960, 600, crate::publish::share::MediaType::Png, 412_000);
        let mut share = crate::share_ui::ShareDialog::new(crate::publish::share::Kind::Image, "https://tours.example.com".into(), "Loot window", details, false, None, None);
        let shot = crate::snapshot::shot(&crate::theme::test_context(), size, 1.5, 4, |ui| {
            ed.ui(ui);
            share.show(ui.ctx(), &mut |_| false);
        });
        assert_eq!(crate::snapshot::save(&shot, "editor-share"), 0, "the editor with the share dialog: nothing behind the window shows");
        // A selected rectangle on a white page and on a dark app, to see the selection on both.
        for (name, page, ink) in [("editor-selection-white", [255u8, 255, 255], [214u8, 218, 224]), ("editor-selection-dark", [14, 14, 18], [44, 48, 58])] {
            let shot = RgbaImage::from_fn(960, 600, |x, y| {
                let line = (60..900).contains(&x) && (y % 36) < 10 && y > 40;
                let c = if line { ink } else { page };
                image::Rgba([c[0], c[1], c[2], 255])
            });
            let mut ed = Editor::new(shot, "Selection".into(), BTreeMap::new(), s.dest_context("Selection"), EditorPrefs::default());
            let id = ed.doc.add(Shape::Rect(Bounds { x0: 240.0, y0: 160.0, x1: 620.0, y1: 400.0 }), Style::default());
            ed.select(Some(id));
            let shot = crate::snapshot::shot(&crate::theme::test_context(), size, 1.5, 4, |ui| ed.ui(ui));
            assert_eq!(crate::snapshot::save(&shot, name), 0, "{name}: nothing behind the window shows");
        }
    }

    /// Every dialog is made by the one helper that leaves the window undimmed.
    #[test]
    fn every_dialog_is_made_without_dimming() {
        fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    sources(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let made: Vec<_> = files.iter().filter(|p| std::fs::read_to_string(p).unwrap().contains(concat!("Modal", "::new("))).collect();
        assert_eq!(made.len(), 1, "{made:?}");
        assert!(made[0].ends_with("share_ui.rs"));
    }

    /// A click outside "Publish to…" closes nothing, as before the dimming went.
    #[test]
    fn a_click_outside_publish_to_does_not_close_it() {
        let ctx = crate::theme::test_context();
        let mut ed = editor(EditorPrefs::default());
        ed.share = Some(share_dialog());
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        dialog_checks::click(&mut ed, &ctx, pos2(10.0, 810.0));
        assert!(ed.share.is_some());
    }

    /// The close question has no dimming either, the editor behind it stays solid, and a click
    /// outside it still dismisses it without closing the window, as it did with the dimming.
    #[test]
    fn the_close_question_is_undimmed_and_a_click_outside_still_dismisses_it() {
        let ctx = crate::theme::test_context();
        let mut ed = editor(EditorPrefs::default());
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        let solid = dialog_checks::frame(&mut ed, &ctx, vec![]);
        ed.confirm_close = true;
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        let shown = dialog_checks::frame(&mut ed, &ctx, vec![]);
        assert!(ed.confirm_close);
        dialog_checks::assert_solid(&shown, &solid);
        dialog_checks::assert_not_dimmed(&shown);
        // A click on a tool behind the question reaches no tool: the backdrop still takes it, and dismisses the question.
        let tool = ed.tool;
        dialog_checks::click(&mut ed, &ctx, pos2(RAIL / 2.0, 120.0));
        assert_eq!(ed.tool, tool);
        assert!(!ed.confirm_close && !ed.closed);
    }

    /// With the close question open the editor takes no keys: Escape dismisses the question (as a
    /// click outside does) and the window stays, a tool key changes nothing, and Enter does
    /// nothing, as before.
    #[test]
    fn the_close_question_takes_the_keys_and_escape_dismisses_it() {
        let ctx = crate::theme::test_context();
        let key = |k: Key, modifiers: Modifiers| egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers };
        let mut ed = editor(EditorPrefs::default());
        ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        ed.add_item(Shape::Rect(Bounds::new(mythicshot_editor::pt(5.0, 5.0), mythicshot_editor::pt(20.0, 20.0))));
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        ed.confirm_close = true;
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        // A tool key: the tool stays.
        let tool = ed.tool;
        let other = Tool::ALL.into_iter().find(|t| *t != tool && !matches!(t, Tool::Spotlight | Tool::Card) && hotkeys::egui_shortcut(t.action().default_binding()).is_some()).expect("a tool with a key");
        let (m, k) = hotkeys::egui_shortcut(other.action().default_binding()).unwrap();
        dialog_checks::frame(&mut ed, &ctx, vec![key(k, m)]);
        assert_eq!(ed.tool, tool, "{other:?}'s key reached the editor behind the question");
        assert!(ed.confirm_close);
        // Enter: nothing, as before.
        dialog_checks::frame(&mut ed, &ctx, vec![key(Key::Enter, Modifiers::NONE)]);
        assert!(ed.confirm_close && !ed.closed);
        // Escape: the question goes, the window stays.
        dialog_checks::frame(&mut ed, &ctx, vec![key(Key::Escape, Modifiers::NONE)]);
        assert!(!ed.confirm_close && !ed.closed);
        // And the keys work again.
        dialog_checks::frame(&mut ed, &ctx, vec![key(k, m)]);
        assert_eq!(ed.tool, other);
    }

    // ---- The tools the rail no longer has: Cross (Z), Asterisk (F) and Donut (D) ----

    /// The keys the three removed tools had in the settings file, and their actions' names.
    const REMOVED_KEYS: [&str; 3] = ["cross", "asterisk", "donut"];

    /// The rail lists exactly these tools, in this order; the Shapes section closes up with no
    /// gap, filling its two columns from the top. No tool is named after a removed one.
    #[test]
    fn the_rail_lists_exactly_the_remaining_tools_in_order() {
        let rail: Vec<(Option<&str>, Vec<&str>)> = RAIL_GROUPS.iter().map(|(title, tools)| (*title, tools.iter().map(|t| t.key()).collect())).collect();
        assert_eq!(
            rail,
            vec![
                (None, vec!["select"]),
                (Some("Draw"), vec!["freehand", "arrow", "line"]),
                (Some("Shapes"), vec!["circle", "group", "cone", "rectangle", "ellipse", "text", "slice"]),
                (Some("More"), vec!["bubble", "step", "picture", "highlight", "obfuscate", "spotlight", "card", "crop"]),
            ]
        );
        for key in REMOVED_KEYS {
            assert!(Tool::from_key(key).is_none(), "{key} is still a tool");
            assert!(EditorAction::ALL.iter().all(|a| !format!("{a:?}").eq_ignore_ascii_case(key)), "{key} is still an editor action");
        }
        // Every tool is on the rail once.
        let on_rail: Vec<Tool> = RAIL_GROUPS.iter().flat_map(|(_, t)| t.iter().copied()).collect();
        assert_eq!(on_rail.len(), Tool::ALL.len());
        assert!(Tool::ALL.iter().all(|t| on_rail.contains(t)));
        // The rail draws the Shapes section in rows of two from the top: seven tools, four rows,
        // the last one holding Slice alone at the left.
        let ctx = crate::theme::test_context();
        let mut ed = editor(EditorPrefs::default());
        ed.keys = EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        let painted = dialog_checks::frame(&mut ed, &ctx, vec![]);
        let corner = |key: &str| {
            painted.editor.iter().find_map(|s| match s {
                // Where the letter ends: it is set from its button's right edge.
                egui::Shape::Text(t) if t.galley.text() == key && t.pos.x < RAIL => Some(t.pos + vec2(t.galley.size().x, 0.0)),
                _ => None,
            })
        };
        // Each tool's key letter sits in its button's corner.
        let at: Vec<egui::Pos2> = ["O", "G", "C", "R", "E", "T", "U"].iter().map(|k| corner(k).unwrap_or_else(|| panic!("{k} is not on the rail"))).collect();
        for pair in at.chunks(2) {
            if let [a, b] = pair {
                assert!((a.y - b.y).abs() < 0.5 && b.x > a.x, "a row of two: {pair:?}");
            }
        }
        let rows: Vec<&[egui::Pos2]> = at.chunks(2).collect();
        for w in rows.windows(2) {
            let step = w[1][0].y - w[0][0].y;
            assert!(step > TOOL_BUTTON - 1.0 && step < TOOL_BUTTON + 8.0, "rows one under the other with no gap: {at:?}");
            assert!((w[1][0].x - w[0][0].x).abs() < 0.5, "the left column: {at:?}");
        }
        for k in ["Z", "F", "D"] {
            assert!(corner(k).is_none(), "{k} is still on the rail");
        }
    }

    /// Z, F and D are no tool's key any more: pressed in the editor they do nothing, whatever is
    /// selected, and no editor action has them by default (Ctrl+Z and Ctrl+D keep theirs).
    #[test]
    fn z_f_and_d_do_nothing_in_the_editor() {
        for a in EditorAction::ALL {
            assert!(!matches!(a.default_binding(), "KeyZ" | "KeyF" | "KeyD"), "{a:?} has {}", a.default_binding());
        }
        assert_eq!((EditorAction::Undo.default_binding(), EditorAction::Duplicate.default_binding()), ("control+KeyZ", "control+KeyD"));
        let ctx = crate::theme::test_context();
        let mut ed = editor(EditorPrefs::default());
        ed.keys = Settings::default().editor_keys;
        let id = ed.add_item(Shape::Rect(Bounds::new(pt(5.0, 5.0), pt(20.0, 20.0))));
        for selected in [None, Some(id)] {
            for tool in [Tool::Rectangle, Tool::Select] {
                ed.set_tool(tool);
                ed.select(selected);
                let (items, shapes) = (ed.doc.items().len(), format!("{:?}", ed.doc.items().iter().map(|i| (&i.shape, i.style)).collect::<Vec<_>>()));
                for key in [Key::Z, Key::F, Key::D] {
                    press(&mut ed, &ctx, vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
                    assert_eq!((ed.tool, ed.selected), (tool, selected), "{key:?}");
                    assert_eq!(ed.doc.items().len(), items, "{key:?} changed the picture");
                    assert_eq!(format!("{:?}", ed.doc.items().iter().map(|i| (&i.shape, i.style)).collect::<Vec<_>>()), shapes, "{key:?} changed the picture");
                }
            }
        }
    }

    /// The Hotkeys page and the rail's tips name no removed tool.
    #[test]
    fn no_hotkey_label_or_tool_tip_names_a_removed_tool() {
        let labels: Vec<&str> = EditorAction::ALL.iter().map(|a| a.label()).collect();
        let hints: Vec<&str> = Tool::ALL.iter().map(|t| tool_hint(*t)).collect();
        for word in ["Donut", "Cross-out", "Asterisk", "asterisk", "ring", "cross it out"] {
            assert!(labels.iter().chain(&hints).all(|l| !l.contains(word)), "{word}: {labels:?} {hints:?}");
        }
    }

    /// A picture 200 by 120, white, with a Donut, a Cross and an Asterisk on it, as a capture,
    /// a recording or a project from before the tools went holds them.
    fn editor_with_removed_kinds() -> (Editor, [u64; 3]) {
        let s = Settings::default();
        let mut ed = Editor::new(RgbaImage::from_pixel(200, 120, image::Rgba([255, 255, 255, 255])), "t".into(), BTreeMap::new(), s.dest_context("t"), EditorPrefs::default());
        ed.keys = s.editor_keys;
        let style = Style { stroke: Rgba::RED, width: 4.0, ..Style::default() };
        let ids = [
            ed.doc.add(Shape::Donut(Bounds::new(pt(10.0, 10.0), pt(50.0, 50.0))), style),
            ed.doc.add(Shape::Cross(Bounds::new(pt(70.0, 10.0), pt(110.0, 50.0))), style),
            ed.doc.add(Shape::Asterisk(Bounds::new(pt(130.0, 10.0), pt(170.0, 50.0))), style),
        ];
        (ed, ids)
    }

    /// Whether some pixel of `img` inside `b` is not white: the shape there was drawn.
    fn drawn_in(img: &RgbaImage, b: Bounds) -> bool {
        (b.y0.max(0.0) as u32..(b.y1 as u32).min(img.height())).any(|y| (b.x0.max(0.0) as u32..(b.x1 as u32).min(img.width())).any(|x| img.get_pixel(x, y).0 != [255, 255, 255, 255]))
    }

    /// A point of item `id` the editor's hit test finds, away from its handles, in picture pixels.
    fn a_point_on(ed: &Editor, id: u64) -> Pt {
        let b = ed.doc.item(id).unwrap().shape.rect().unwrap().norm();
        (0..=40).flat_map(|i| (0..=40).map(move |j| pt(b.x0 + (b.x1 - b.x0) * i as f32 / 40.0, b.y0 + (b.y1 - b.y0) * j as f32 / 40.0))).find(|p| ed.hit(*p, 1.0) == Some(id) && ed.handle_at(id, *p, ed.last_canvas.zoom).is_none()).expect("a point on the shape")
    }

    /// A drag on the canvas from `a` to `b` (picture pixels), as a mouse makes it.
    fn drag(ed: &mut Editor, ctx: &egui::Context, a: Pt, b: Pt) {
        let c = ed.last_canvas;
        let at = |p: Pt| pos2(c.min.x + p.x * c.zoom, c.min.y + p.y * c.zoom);
        let button = |p: Pt, pressed| egui::Event::PointerButton { pos: at(p), button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        dialog_checks::frame(ed, ctx, vec![egui::Event::PointerMoved(at(a)), button(a, true)]);
        let mid = pt((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        dialog_checks::frame(ed, ctx, vec![egui::Event::PointerMoved(at(mid))]);
        dialog_checks::frame(ed, ctx, vec![egui::Event::PointerMoved(at(b))]);
        dialog_checks::frame(ed, ctx, vec![button(b, false)]);
        dialog_checks::frame(ed, ctx, vec![]);
    }

    /// A Donut, a Cross or an Asterisk already in a picture still draws, and a click selects it:
    /// the rail then shows Select (no tool of its own is lit, none is missing). It moves, resizes
    /// (the donut stays round), takes a new style (remembered for no tool), is deleted and comes
    /// back with undo, and the picture exported shows it.
    #[test]
    fn a_shape_the_rail_no_longer_makes_still_selects_moves_resizes_restyles_and_deletes() {
        let (ed, ids) = editor_with_removed_kinds();
        let flat = ed.doc.flatten();
        for id in ids {
            assert!(drawn_in(&flat, ed.doc.item(id).unwrap().shape.rect().unwrap().norm()), "item {id} is drawn");
            assert_eq!(Tool::for_shape(&ed.doc.item(id).unwrap().shape), Tool::Select, "its style belongs to no tool");
        }
        for (n, id) in ids.into_iter().enumerate() {
            let ctx = crate::theme::test_context();
            let (mut ed, _) = editor_with_removed_kinds();
            ed.set_tool(Tool::Rectangle);
            dialog_checks::frame(&mut ed, &ctx, vec![]);
            dialog_checks::frame(&mut ed, &ctx, vec![]);
            let c = ed.last_canvas;
            let p = a_point_on(&ed, id);
            dialog_checks::click(&mut ed, &ctx, pos2(c.min.x + p.x * c.zoom, c.min.y + p.y * c.zoom));
            assert_eq!(ed.selected, Some(id), "a click selects item {n}");
            assert_eq!(ed.tool, Tool::Select, "the rail shows Select");
            // The rail lights Select and nothing else.
            let painted = dialog_checks::frame(&mut ed, &ctx, vec![]);
            let lit = painted.editor.iter().filter(|s| matches!(s, egui::Shape::Rect(r) if r.fill == theme::PRIMARY && r.rect.right() <= RAIL && (r.rect.width() - TOOL_BUTTON).abs() < 1.0)).count();
            assert_eq!(lit, 1, "one tool lit on the rail");
            // Moved by a drag.
            let before = ed.doc.item(id).unwrap().shape.rect().unwrap().norm();
            drag(&mut ed, &ctx, p, pt(p.x + 6.0, p.y + 4.0));
            let moved = ed.doc.item(id).unwrap().shape.rect().unwrap().norm();
            assert!((moved.x0 - before.x0 - 6.0).abs() < 1.0 && (moved.y0 - before.y0 - 4.0).abs() < 1.0, "moved: {before:?} -> {moved:?}");
            // Resized from its bottom-right corner.
            let corner = pt(moved.x1, moved.y1);
            drag(&mut ed, &ctx, corner, pt(corner.x + 10.0, corner.y + 6.0));
            let resized = ed.doc.item(id).unwrap().shape.rect().unwrap().norm();
            assert!(resized.x1 > moved.x1 + 4.0 && (resized.x0 - moved.x0).abs() < 0.01, "resized: {moved:?} -> {resized:?}");
            if matches!(ed.doc.item(id).unwrap().shape, Shape::Donut(_)) {
                assert!(((resized.x1 - resized.x0) - (resized.y1 - resized.y0)).abs() < 0.5, "the donut stays round: {resized:?}");
            }
            // A new style: the item takes it, no tool remembers it.
            let before = (ed.style, ed.obfuscate);
            ed.style.width = 9.0;
            ed.style.stroke = Rgba::YELLOW;
            assert!(ed.commit_style(before));
            assert_eq!((ed.doc.item(id).unwrap().style.width, ed.doc.item(id).unwrap().style.stroke), (9.0, Rgba::YELLOW));
            assert!(REMOVED_KEYS.iter().all(|k| !ed.prefs.styles.contains_key(*k)), "{:?}", ed.prefs.styles.keys().collect::<Vec<_>>());
            // The exported picture shows it where it is now.
            assert!(drawn_in(&ed.doc.flatten(), resized));
            // Deleted with Delete, back with undo.
            ed.run(EditorAction::Delete);
            assert!(ed.doc.item(id).is_none());
            ed.run(EditorAction::Undo);
            assert!(ed.doc.item(id).is_some(), "undo brings it back");
        }
    }

    /// The last tool of an older settings file was one of the three: the editor opens on Select
    /// and keeps every remembered style.
    #[test]
    fn a_last_tool_that_was_removed_opens_on_select() {
        for key in REMOVED_KEYS {
            let mut prefs = EditorPrefs { tool: key.into(), ..EditorPrefs::default() };
            prefs.styles.insert("arrow".into(), crate::settings::SavedStyle { width: 7.0, ..Default::default() });
            prefs.styles.insert(key.into(), crate::settings::SavedStyle { width: 3.0, ..Default::default() });
            let ed = editor(prefs.clone());
            assert_eq!(ed.tool, Tool::Select, "{key}");
            assert_eq!(ed.prefs.styles, prefs.styles, "{key}: every remembered style is kept");
        }
        // Another word nobody knows still falls back as before.
        assert_eq!(editor(EditorPrefs { tool: "sculpt".into(), ..EditorPrefs::default() }).tool, Tool::Rectangle);
    }

    // ---- A picture file dropped on the editor ----

    use crate::place_picture::PLACE_HERE;

    /// An editor of a white capture `w` by `h`, its keys the default ones.
    pub(crate) fn white_editor(w: u32, h: u32) -> Editor {
        let s = Settings::default();
        let mut ed = Editor::new(RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255])), "t".into(), BTreeMap::new(), s.dest_context("t"), EditorPrefs::default());
        ed.keys = s.editor_keys;
        ed
    }

    /// One frame of the editor with `hovered` dragged over its window and `dropped` dropped on it.
    pub(crate) fn file_frame(ed: &mut Editor, ctx: &egui::Context, hovered: &[PathBuf], dropped: &[PathBuf], events: Vec<egui::Event>) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(dialog_checks::WINDOW),
            events,
            hovered_files: hovered.iter().map(|p| egui::HoveredFile { path: Some(p.clone()), mime: String::new() }).collect(),
            dropped_files: dropped.iter().map(|p| Arc::new(crate::open_image::tests::Dropped(p.clone())) as egui::DroppedFileHandle).collect(),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| ed.ui(ui));
        out.textures_delta.clear();
        out
    }

    fn texts(out: &egui::FullOutput) -> Vec<String> {
        crate::share_ui::tests::shapes_of(out).into_iter().filter_map(|s| if let egui::Shape::Text(t) = s { Some(t.galley.text().to_string()) } else { None }).collect()
    }

    /// Frames of the editor until `done` holds (at most ten seconds): a picture is read off the
    /// window's thread.
    pub(crate) fn frames_until(ed: &mut Editor, ctx: &egui::Context, done: impl Fn(&Editor) -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !done(ed) && Instant::now() < until {
            dialog_checks::frame(ed, ctx, vec![]);
            std::thread::sleep(Duration::from_millis(5));
        }
        // And a frame more, so whatever the picture's arrival changed is drawn.
        dialog_checks::frame(ed, ctx, vec![]);
    }

    /// The window point over capture pixel `p`, as the canvas was last drawn.
    fn screen_at(ed: &Editor, p: Pt) -> egui::Pos2 {
        let c = ed.last_canvas;
        pos2(c.min.x + p.x * c.zoom, c.min.y + p.y * c.zoom)
    }

    /// Drop `file` on the canvas over capture pixel `at`, the pointer there, after a frame of it
    /// being dragged; then wait for the read to end.
    pub(crate) fn drop_on(ed: &mut Editor, ctx: &egui::Context, file: &std::path::Path, at: Pt) {
        let p = screen_at(ed, at);
        file_frame(ed, ctx, &[file.to_path_buf()], &[], vec![egui::Event::PointerMoved(p)]);
        file_frame(ed, ctx, &[], &[file.to_path_buf()], vec![egui::Event::PointerMoved(p)]);
        frames_until(ed, ctx, |e| !e.placing.pending());
    }

    fn images(ed: &Editor) -> Vec<(u64, Bounds, (u32, u32))> {
        ed.doc.items().iter().filter_map(|it| match &it.shape {
            Shape::Image { rect, image } => Some((it.id, *rect, (image.width(), image.height()))),
            _ => None,
        }).collect()
    }

    /// One supported picture dragged over the editor shows the overlay that places it, over the
    /// canvas only; anything else, or several files, the refusal; no drag, none. The main window's
    /// words never show here.
    #[test]
    fn a_picture_dragged_over_an_editor_shows_the_place_overlay_over_its_canvas_and_anything_else_the_refusal() {
        use crate::open_image::{ACCEPT_TINT, DROP_TO_OPEN, ONLY_PICTURES, REFUSE_TINT};
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 8, 8, [255, 0, 0, 255]);
        let notes = tmp.path().join("notes.txt");
        std::fs::write(&notes, "x").unwrap();
        let ctx = crate::theme::test_context();
        let mut ed = white_editor(400, 300);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        let out = file_frame(&mut ed, &ctx, &[], &[], vec![]);
        assert!(!texts(&out).iter().any(|t| t == PLACE_HERE || t == ONLY_PICTURES), "no drag, no overlay");
        let canvas = ed.canvas_rect.expect("the canvas was drawn");
        for (files, words, tint) in [(vec![pic.clone()], PLACE_HERE, ACCEPT_TINT), (vec![notes.clone()], ONLY_PICTURES, REFUSE_TINT), (vec![pic.clone(), pic.clone()], ONLY_PICTURES, REFUSE_TINT)] {
            let out = file_frame(&mut ed, &ctx, &files, &[], vec![]);
            let t = texts(&out);
            assert!(t.iter().any(|t| t == words), "{files:?}: {t:?}");
            assert!(!t.iter().any(|t| t == DROP_TO_OPEN), "never the main window's words: {t:?}");
            // The tint lies exactly over the canvas: not over the menu bar, toolbar, rail or status bar.
            let tints: Vec<Rect> = crate::share_ui::tests::shapes_of(&out).into_iter().filter_map(|s| match s {
                egui::Shape::Rect(r) if r.fill == tint => Some(r.rect),
                _ => None,
            }).collect();
            assert_eq!(tints, vec![canvas], "{files:?}");
            file_frame(&mut ed, &ctx, &[], &[], vec![]);
        }
        let out = file_frame(&mut ed, &ctx, &[], &[], vec![]);
        assert!(!texts(&out).iter().any(|t| t == PLACE_HERE || t == ONLY_PICTURES), "the drag left, the overlay went");
    }

    /// A dropped picture becomes exactly one placed picture: at its natural size, centred where it
    /// was dropped, selected with the Select tool, and one undo step (undo removes it, redo puts it back).
    #[test]
    fn a_dropped_picture_is_placed_once_where_it_was_dropped_selected_and_as_one_undo_step() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 40, 20, [255, 0, 0, 255]);
        let ctx = crate::theme::test_context();
        let mut ed = white_editor(400, 300);
        ed.set_tool(Tool::Arrow);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        drop_on(&mut ed, &ctx, &pic, pt(100.0, 80.0));
        let placed = images(&ed);
        assert_eq!(placed.len(), 1, "{placed:?}");
        let (id, rect, stored) = placed[0];
        assert_eq!(stored, (40, 20), "its natural size: it fits");
        assert!((rect.x0 - 80.0).abs() < 1.5 && (rect.y0 - 70.0).abs() < 1.5 && (rect.width() - 40.0).abs() < 0.01 && (rect.height() - 20.0).abs() < 0.01, "centred on the drop point: {rect:?}");
        assert_eq!((ed.selected, ed.tool), (Some(id), Tool::Select));
        // It is drawn.
        assert_eq!(ed.doc.flatten().get_pixel(100, 80).0, [255, 0, 0, 255]);
        ed.run(EditorAction::Undo);
        assert!(images(&ed).is_empty(), "undo removes it");
        ed.run(EditorAction::Redo);
        assert_eq!(images(&ed).len(), 1, "redo puts it back");
        assert!(!ed.doc.can_redo());
        ed.run(EditorAction::Undo);
        assert!(!ed.doc.can_undo(), "it was one undo step");
    }

    /// Dropped by an edge, the picture is moved in only as far as it must to lie inside the capture.
    #[test]
    fn a_picture_dropped_by_an_edge_is_moved_inside_the_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 60, 40, [0, 0, 255, 255]);
        let ctx = crate::theme::test_context();
        let mut ed = white_editor(400, 300);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        drop_on(&mut ed, &ctx, &pic, pt(395.0, 5.0));
        let (_, rect, _) = images(&ed)[0];
        assert!((rect.x1 - 400.0).abs() < 0.01 && rect.y0.abs() < 0.01, "against the top right corner: {rect:?}");
        assert!((rect.x0 - 340.0).abs() < 0.01 && (rect.y1 - 40.0).abs() < 0.01, "{rect:?}");
    }

    /// A picture larger than the capture is scaled down to fit inside it with a margin, keeping its
    /// proportions, and its pixels are resampled to that size, so the editor keeps no more.
    #[test]
    fn a_picture_larger_than_the_capture_is_scaled_to_fit_and_stored_at_that_size() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "huge.png", 2000, 1000, [0, 160, 0, 255]);
        let ctx = crate::theme::test_context();
        let mut ed = white_editor(400, 300);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        drop_on(&mut ed, &ctx, &pic, pt(200.0, 150.0));
        let placed = images(&ed);
        assert_eq!(placed.len(), 1);
        let (_, rect, stored) = placed[0];
        assert_eq!(stored, (360, 180), "resampled to the size it is shown at: 90% of the width");
        assert!((rect.width() - 360.0).abs() < 0.01 && (rect.height() - 180.0).abs() < 0.01, "{rect:?}");
        assert!(rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= 400.0 && rect.y1 <= 300.0, "inside the capture: {rect:?}");
        assert!((rect.x0 - 20.0).abs() < 0.6, "the margin, centred: {rect:?}");
    }

    /// A file that cannot be opened is said with the usual sentence in the status bar, and
    /// nothing is placed.
    #[test]
    fn a_dropped_file_that_is_no_picture_places_nothing_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("fake.png");
        std::fs::write(&fake, "not a picture").unwrap();
        let ctx = crate::theme::test_context();
        let mut ed = white_editor(400, 300);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        drop_on(&mut ed, &ctx, &fake, pt(100.0, 100.0));
        assert!(ed.doc.items().is_empty() && !ed.doc.can_undo());
        let e = crate::open_image::load(&fake).unwrap_err();
        assert_eq!(ed.status_text(), Some(crate::open_image::could_not_open(&fake, &e).as_str()));
    }

    /// While a picture is read the editor holds an update back; once it is placed, no more.
    #[test]
    fn an_update_waits_while_a_picture_is_still_being_placed() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 8, 8, [255, 0, 0, 255]);
        let ctx = crate::theme::test_context();
        let mut ed = white_editor(400, 300);
        assert!(!ed.holds_update());
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        ed.place_picture_file(pic, pt(10.0, 10.0));
        assert!(ed.holds_update(), "the picture is being read");
        frames_until(&mut ed, &ctx, |e| !e.placing.pending());
        assert_eq!(images(&ed).len(), 1);
        assert!(!ed.holds_update());
        // A closed editor holds nothing back.
        ed.place_picture_file(crate::place_picture::tests::picture_file(tmp.path(), "b.png", 8, 8, [0, 0, 0, 255]), pt(10.0, 10.0));
        ed.closed = true;
        assert!(!ed.holds_update());
    }

    /// A picture that arrives once the editor is closed, or once its capture has changed (here
    /// cropped), is dropped without a word: nothing is placed and nothing is said.
    #[test]
    fn a_picture_arriving_for_a_closed_editor_or_a_changed_capture_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 8, 8, [255, 0, 0, 255]);
        let ctx = crate::theme::test_context();
        // Closed.
        let mut ed = white_editor(400, 300);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        ed.place_picture_file(pic.clone(), pt(10.0, 10.0));
        ed.closed = true;
        frames_until(&mut ed, &ctx, |e| !e.placing.pending());
        assert!(ed.doc.items().is_empty() && ed.status_text().is_none());
        // Cropped meanwhile: another capture.
        let mut ed = white_editor(400, 300);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        ed.place_picture_file(pic.clone(), pt(10.0, 10.0));
        ed.doc.crop(Bounds::new(pt(0.0, 0.0), pt(200.0, 150.0)));
        frames_until(&mut ed, &ctx, |e| !e.placing.pending());
        assert!(images(&ed).is_empty() && ed.status_text().is_none());
        // A failure for a changed capture says nothing either.
        let fake = tmp.path().join("fake.png");
        std::fs::write(&fake, "x").unwrap();
        ed.place_picture_file(fake, pt(10.0, 10.0));
        ed.doc.crop(Bounds::new(pt(0.0, 0.0), pt(100.0, 100.0)));
        frames_until(&mut ed, &ctx, |e| !e.placing.pending());
        assert!(ed.status_text().is_none());
    }

    /// With a dialog open over the editor a picture dragged there is refused with the reason (a
    /// picture cannot be placed now), a file that is no picture with the usual sentence, and a
    /// drop places nothing.
    #[test]
    fn a_picture_dropped_while_the_close_question_is_up_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 8, 8, [255, 0, 0, 255]);
        let ctx = crate::theme::test_context();
        let mut ed = white_editor(400, 300);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        ed.confirm_close = true;
        let out = file_frame(&mut ed, &ctx, std::slice::from_ref(&pic), &[], vec![]);
        let t = texts(&out);
        assert!(t.iter().any(|t| t == crate::place_picture::CANNOT_PLACE_NOW), "a picture, refused for the right reason: {t:?}");
        assert!(!t.iter().any(|t| t == crate::open_image::ONLY_PICTURES || t == PLACE_HERE), "{t:?}");
        // The refusal's tint, over the canvas.
        let tints = crate::share_ui::tests::shapes_of(&out).into_iter().filter(|s| matches!(s, egui::Shape::Rect(r) if r.fill == crate::open_image::REFUSE_TINT)).count();
        assert_eq!(tints, 1);
        // A file that is no picture keeps the usual sentence.
        let notes = tmp.path().join("notes.txt");
        std::fs::write(&notes, "x").unwrap();
        file_frame(&mut ed, &ctx, &[], &[], vec![]);
        let t = texts(&file_frame(&mut ed, &ctx, std::slice::from_ref(&notes), &[], vec![]));
        assert!(t.iter().any(|t| t == crate::open_image::ONLY_PICTURES) && !t.iter().any(|t| t == crate::place_picture::CANNOT_PLACE_NOW), "{t:?}");
        file_frame(&mut ed, &ctx, &[], &[], vec![]);
        file_frame(&mut ed, &ctx, std::slice::from_ref(&pic), &[], vec![]);
        file_frame(&mut ed, &ctx, &[], std::slice::from_ref(&pic), vec![]);
        assert!(!ed.placing.pending());
        std::thread::sleep(Duration::from_millis(50));
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        assert!(ed.doc.items().is_empty());
    }

    /// An editor's window takes dropped files, said outright.
    #[test]
    fn an_editor_windows_viewport_takes_dropped_files() {
        assert_eq!(editor_builder("t".into(), Arc::new(egui::IconData::default())).drag_and_drop, Some(true));
    }

    /// The editor with a picture dragged over its canvas, for the owner: the overlay that places
    /// it, the refusal of a file that is no picture, and the refusal of a picture while nothing
    /// may be placed. Run with
    /// `MYTHICSHOT_SNAPSHOTS=<absolute folder> cargo test -p mythicshot editor_drop_renders -- --ignored`.
    #[test]
    #[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
    fn editor_drop_renders() {
        let shot = RgbaImage::from_fn(960, 600, |x, y| {
            let band = if y < 48 { [40, 44, 52] } else if x < 200 { [235, 237, 240] } else { [250, 250, 251] };
            let block = (260..900).contains(&x) && ((90..170).contains(&y) || (200..330).contains(&y) || (360..560).contains(&y));
            let c = if block { [212, 222, 240] } else { band };
            image::Rgba([c[0], c[1], c[2], 255])
        });
        let s = Settings::default();
        for ppp in [1.0f32, 2.0] {
            for (label, accept, blocked) in [("editor-drop-picture", true, false), ("editor-drop-refused", false, false), ("editor-drop-blocked", false, true)] {
                let mut ed = Editor::new(shot.clone(), "Loot window".into(), BTreeMap::new(), s.dest_context("Loot window"), EditorPrefs::default());
                ed.keys = s.editor_keys.clone();
                ed.picture_drop.shown_for_render = Some(accept);
                ed.picture_drop.blocked_for_render = blocked;
                let shot = crate::snapshot::shot(&crate::theme::test_context(), dialog_checks::WINDOW.size(), ppp, 4, |ui| ed.ui(ui));
                assert_eq!(crate::snapshot::save(&shot, &format!("{label}-{ppp}x")), 0, "{label}");
            }
        }
    }

    // ---- The rail's Picture tool ----

    thread_local! {
        /// The file the stubbed "Open image" dialog answers with in this test thread, once.
        pub(crate) static PICKED: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    /// The Picture button on the rail as the editor last drew it: the point a click on which asks
    /// for the "Open image" dialog.
    fn picture_button(make: impl Fn() -> Editor) -> egui::Pos2 {
        let top = 60.0;
        (0..120).flat_map(|i| [pos2(RAIL / 4.0 + 2.0, top + i as f32 * 6.0), pos2(RAIL * 3.0 / 4.0 - 2.0, top + i as f32 * 6.0)]).find(|&at| {
            let (mut ed, ctx) = (make(), crate::theme::test_context());
            dialog_checks::frame(&mut ed, &ctx, vec![]);
            mythicshot_destinations::stubs::take();
            dialog_checks::click(&mut ed, &ctx, at);
            mythicshot_destinations::stubs::take() == ["open dialog: open image"]
        }).expect("the Picture button on the rail")
    }

    /// Clicking the Picture tool asks for a file with the dialog "Open image" uses (in a test
    /// build the stub, answering as if cancelled): nothing is placed and the tool in use stays.
    /// Answered with a picture, it is placed at the middle of the canvas as a drop places it.
    #[test]
    fn the_picture_tool_asks_for_a_file_like_open_image_and_places_it_at_the_middle_of_the_canvas() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 30, 20, [0, 0, 255, 255]);
        let make = || {
            let mut ed = white_editor(400, 300);
            ed.set_tool(Tool::Arrow);
            ed.dest.output_dir = tmp.path().to_path_buf();
            ed
        };
        let at = picture_button(make);
        // Cancelled: nothing changes.
        let ctx = crate::theme::test_context();
        let mut ed = make();
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        mythicshot_destinations::stubs::take();
        dialog_checks::click(&mut ed, &ctx, at);
        assert_eq!(mythicshot_destinations::stubs::take(), ["open dialog: open image"]);
        assert!(ed.doc.items().is_empty() && !ed.holds_update());
        assert_eq!(ed.tool, Tool::Arrow, "an action, not a mode: the tool stays");
        assert_eq!(ed.prefs.tool, "arrow", "and is the one remembered");
        // A picture chosen: placed at the middle of what the canvas shows, selected.
        PICKED.with(|p| *p.borrow_mut() = Some(pic.clone()));
        dialog_checks::click(&mut ed, &ctx, at);
        frames_until(&mut ed, &ctx, |e| !e.placing.pending());
        let placed = images(&ed);
        assert_eq!(placed.len(), 1, "{placed:?}");
        let (id, rect, stored) = placed[0];
        assert_eq!(stored, (30, 20));
        assert!((rect.x0 - 185.0).abs() < 1.5 && (rect.y0 - 140.0).abs() < 1.5, "the middle of the capture, all of it shown: {rect:?}");
        assert_eq!((ed.selected, ed.tool), (Some(id), Tool::Select));
        // Its key runs the same action, with no key of its own by default; the tool in use stays
        // until the picture is placed.
        assert_eq!(EditorAction::Picture.default_binding(), "");
        ed.set_tool(Tool::Arrow);
        mythicshot_destinations::stubs::take();
        PICKED.with(|p| *p.borrow_mut() = Some(pic.clone()));
        ed.run(EditorAction::Picture);
        assert_eq!(mythicshot_destinations::stubs::take(), ["open dialog: open image"]);
        assert_eq!(ed.tool, Tool::Arrow, "nothing changes until the picture is placed");
        frames_until(&mut ed, &ctx, |e| !e.placing.pending());
        assert_eq!((images(&ed).len(), ed.tool), (2, Tool::Select));
    }

    /// Zoomed in, the picture goes to the middle of the part of the capture the canvas shows.
    #[test]
    fn the_picture_tool_places_at_the_middle_of_the_visible_part_of_a_zoomed_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 10, 10, [0, 0, 255, 255]);
        let ctx = crate::theme::test_context();
        let mut ed = white_editor(2000, 1500);
        ed.zoom = Some(2.0);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        let canvas = ed.canvas_rect.unwrap();
        let shown_centre = ed.capture_point(canvas.center());
        PICKED.with(|p| *p.borrow_mut() = Some(pic));
        ed.picture_tool();
        frames_until(&mut ed, &ctx, |e| !e.placing.pending());
        let (_, rect, _) = images(&ed)[0];
        assert!(((rect.x0 + rect.x1) / 2.0 - shown_centre.x).abs() < 0.6 && ((rect.y0 + rect.y1) / 2.0 - shown_centre.y).abs() < 0.6, "{rect:?} around {shown_centre:?}");
        assert!(shown_centre.x < 1000.0, "the scrolled-to corner, not the capture's middle: {shown_centre:?}");
    }

    /// The tool's tip says what it does, and its row on the Hotkeys page is named.
    #[test]
    fn the_picture_tool_has_its_hint_and_its_hotkey_name() {
        assert_eq!(tool_hint(Tool::Picture), "Choose a picture to place on the capture.");
        assert_eq!(EditorAction::Picture.label(), "Picture tool");
        assert_eq!(Tool::Picture.action(), EditorAction::Picture);
        assert!(EditorAction::ALL.contains(&EditorAction::Picture));
        // Never the tool an editor opens on, even if a settings file names it.
        assert_eq!(editor(EditorPrefs { tool: "picture".into(), ..EditorPrefs::default() }).tool, Tool::Rectangle);
    }

    /// The rail with the Picture tool, for the owner: the image editor, at 100% and 200%. Run with
    /// `MYTHICSHOT_SNAPSHOTS=<absolute folder> cargo test -p mythicshot editor_rail_renders -- --ignored`.
    #[test]
    #[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
    fn editor_rail_renders() {
        let s = Settings::default();
        for ppp in [1.0f32, 2.0] {
            let mut ed = Editor::new(RgbaImage::from_pixel(960, 600, image::Rgba([250, 250, 251, 255])), "Loot window".into(), BTreeMap::new(), s.dest_context("Loot window"), EditorPrefs::default());
            ed.keys = s.editor_keys.clone();
            // The rail and the start of the canvas beside it.
            let size = egui::vec2(RAIL + 120.0, 600.0);
            let shot = crate::snapshot::shot(&crate::theme::test_context(), dialog_checks::WINDOW.size(), ppp, 4, |ui| ed.ui(ui));
            let crop = image::imageops::crop_imm(&shot.image, 0, 0, (size.x * ppp) as u32, (size.y * ppp) as u32).to_image();
            let cropped = crate::snapshot::Shot { image: crop, uncovered: shot.uncovered, uncovered_at: shot.uncovered_at };
            assert_eq!(crate::snapshot::save(&cropped, &format!("editor-rail-picture-{ppp}x")), 0);
        }
    }
}
