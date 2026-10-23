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
use mythicshot_editor::{Bounds, Document, Item, ObfuscateMode, Pixmap, Pt, Rgba, RgbaImage, Shape, Style, effects, pt, render_items_onto, to_pixmap};

use mythicshot_destinations::{self as dest, Context, DestId, Sent};

use crate::hotkeys::{self, EditorAction};
use crate::icons::{self, Icon};

const ACCENT: Color32 = Color32::from_rgb(91, 141, 239);
const CANVAS_BG: Color32 = Color32::from_rgb(16, 18, 22);
const HANDLE: f32 = 8.0;
const BAR_BG: Color32 = Color32::from_rgb(27, 31, 39);
/// Toolbar icon button size and tool rail width.
const ICON: f32 = 28.0;
const RAIL: f32 = 44.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Freehand,
    Text,
    Bubble,
    Step,
    Highlight,
    Obfuscate,
    Crop,
}

impl Tool {
    const ALL: [Tool; 12] = [
        Tool::Select,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Line,
        Tool::Arrow,
        Tool::Freehand,
        Tool::Text,
        Tool::Bubble,
        Tool::Step,
        Tool::Highlight,
        Tool::Obfuscate,
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
            Tool::Text => Icon::Text,
            Tool::Bubble => Icon::Bubble,
            Tool::Step => Icon::Step,
            Tool::Highlight => Icon::Highlight,
            Tool::Obfuscate => Icon::Obfuscate,
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
            Tool::Text => EditorAction::Text,
            Tool::Bubble => EditorAction::Bubble,
            Tool::Step => EditorAction::Step,
            Tool::Highlight => EditorAction::Highlight,
            Tool::Obfuscate => EditorAction::Obfuscate,
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
}

pub struct Editor {
    doc: Document,
    title: String,
    tool: Tool,
    style: Style,
    obfuscate: ObfuscateMode,
    selected: Option<u64>,
    drag: Option<Drag>,
    text: Option<TextEdit>,
    crop: Option<Bounds>,
    /// `None` fits the image to the window.
    zoom: Option<f32>,
    texture: Option<TextureHandle>,
    shown_version: Option<u64>,
    /// Base plus every item except the one being dragged, built when a drag starts.
    drag_cache: Option<Pixmap>,
    pub keys: BTreeMap<EditorAction, String>,
    /// Output folder, pattern and format from settings; `title` is this capture's.
    dest: Context,
    saved: Option<PathBuf>,
    /// Last action and the time it happened, shown in the status bar.
    status: Option<(String, String)>,
    pub closed: bool,
    confirm_close: bool,
    /// When the property panel last checkpointed, so one slider drag is one undo step.
    prop_checkpoint: Option<(u64, Instant)>,
    last_canvas: CanvasGeom,
}

impl Editor {
    pub fn new(img: RgbaImage, title: String, keys: BTreeMap<EditorAction, String>, dest: Context) -> Self {
        Self {
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
            shown_version: None,
            drag_cache: None,
            keys,
            dest,
            saved: None,
            status: None,
            closed: false,
            confirm_close: false,
            prop_checkpoint: None,
            last_canvas: CanvasGeom { min: pos2(0.0, 0.0), zoom: 1.0 },
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

    fn notify(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), chrono::Local::now().format("%H:%M:%S").to_string()));
    }

    fn select(&mut self, id: Option<u64>) {
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
            if self.doc.dirty {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm_close = true;
            } else {
                self.closed = true;
            }
        }
        self.shortcuts(&ctx);
        self.toolbar(ui);
        self.status_bar(ui);
        self.tool_rail(ui);
        self.canvas(ui);
        self.text_editor(&ctx);
        if self.confirm_close {
            self.confirm_close_dialog(&ctx);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if self.text.take().is_none() && self.crop.take().is_none() && self.drag.take().is_none() {
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
        for (action, m, k) in bound {
            if ctx.input_mut(|i| i.consume_key(m, k)) {
                self.run(action);
                break;
            }
        }
    }

    fn run(&mut self, action: EditorAction) {
        if let Some(tool) = Tool::ALL.into_iter().find(|t| t.action() == action) {
            self.set_tool(tool);
            return;
        }
        match action {
            EditorAction::Undo => {
                self.doc.undo();
                self.fix_selection();
            }
            EditorAction::Redo => {
                self.doc.redo();
                self.fix_selection();
            }
            EditorAction::Delete => {
                if let Some(id) = self.selected.take() {
                    self.doc.remove(id);
                }
            }
            EditorAction::Copy => self.copy(),
            EditorAction::Save => self.save(),
            _ => {}
        }
    }

    fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.crop = None;
        if tool != Tool::Select {
            self.selected = None;
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

    /// Send the flattened image to a destination and report the result here.
    fn send(&mut self, d: DestId) {
        match dest::send(d, &self.doc.flatten(), &self.dest) {
            Ok(Sent::Saved(p)) => self.saved_to(p),
            Ok(Sent::Cancelled) => {}
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
            self.doc.crop(b);
            self.fix_selection();
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
                if icons::button(ui, Icon::Save, ICON, false, &self.key_hint(EditorAction::Save)).clicked() {
                    self.save();
                }
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
                ui.separator();
                let (can_undo, can_redo) = (self.doc.can_undo(), self.doc.can_redo());
                if ui.add_enabled_ui(can_undo, |ui| icons::button(ui, Icon::Undo, ICON, false, &self.key_hint(EditorAction::Undo))).inner.clicked() {
                    self.run(EditorAction::Undo);
                }
                if ui.add_enabled_ui(can_redo, |ui| icons::button(ui, Icon::Redo, ICON, false, &self.key_hint(EditorAction::Redo))).inner.clicked() {
                    self.run(EditorAction::Redo);
                }
                ui.separator();
                let fx = icons::button(ui, Icon::Effects, ICON, false, "Effects (flatten annotations into the image)");
                egui::Popup::menu(&fx).show(|ui| self.effects_menu(ui));
                ui.separator();
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
            ui.spacing_mut().item_spacing.y = 2.0;
            for tool in Tool::ALL {
                if matches!(tool, Tool::Rectangle | Tool::Text | Tool::Highlight | Tool::Crop) {
                    ui.add_space(3.0);
                    ui.separator();
                    ui.add_space(3.0);
                }
                let tip = format!("{}\n{}", self.key_hint(tool.action()), tool_hint(tool));
                if icons::button(ui, tool.icon(), RAIL - 8.0, self.tool == tool, &tip).clicked() {
                    self.set_tool(tool);
                }
            }
        });
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
                match &self.status {
                    Some((msg, clock)) => ui.label(egui::RichText::new(format!("{clock} – {msg}")).color(muted)),
                    None => ui.label(egui::RichText::new(dirty).color(muted)),
                };
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
            let wheel = ui.input(|i| if i.modifiers.command { i.smooth_scroll_delta.y } else { 0.0 });
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

                self.handle_pointer(&resp, z, &to_img);
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
                (Some(p), _) if self.doc.hit(p, 6.0 / z).is_some() => CursorIcon::Move,
                _ => CursorIcon::Default,
            },
            Tool::Text => CursorIcon::Text,
            _ => CursorIcon::Crosshair,
        }
    }

    fn handle_pointer(&mut self, resp: &egui::Response, z: f32, to_img: &dyn Fn(egui::Pos2) -> Pt) {
        let tol = 6.0 / z;
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos().map(to_img) {
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
            if let Some(id) = resp.interact_pointer_pos().map(to_img).and_then(|p| self.doc.hit(p, tol)) {
                if self.doc.item(id).and_then(|it| it.shape.text()).is_some() {
                    self.select(Some(id));
                    self.edit_text(id);
                    return;
                }
            }
        }
        if resp.clicked() {
            let Some(p) = resp.interact_pointer_pos().map(to_img) else { return };
            match self.tool {
                Tool::Text => match self.doc.hit(p, tol).filter(|id| matches!(self.doc.item(*id).map(|i| &i.shape), Some(Shape::Text { .. }))) {
                    Some(id) => self.edit_text(id),
                    None => self.text = Some(TextEdit { item: None, at: p, buffer: String::new(), focused: false }),
                },
                Tool::Step => {
                    let n = self.doc.next_step_number();
                    let id = self.doc.add(Shape::Step { center: p, number: n }, self.style);
                    self.selected = Some(id);
                }
                _ => {
                    let hit = self.doc.hit(p, tol);
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
                } else if let Some(id) = self.doc.hit(p, tol) {
                    self.select(Some(id));
                    self.doc.checkpoint();
                    self.drag = Some(Drag::Move { id, last: p });
                    self.build_cache(Some(id));
                } else {
                    self.select(None);
                }
            }
            Tool::Crop => self.drag = Some(Drag::Crop { start: p, now: p }),
            Tool::Freehand => {
                self.drag = Some(Drag::Freehand(vec![p]));
                self.build_cache(None);
            }
            Tool::Text | Tool::Step => {}
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
                let (id, h) = (*id, *handle);
                self.doc.update(id, |it| {
                    if let Some(b) = it.shape.rect() {
                        it.shape.set_rect(resize_bounds(b.norm(), h, p));
                    }
                });
            }
            Some(Drag::Endpoint { id, end }) => {
                let (id, end) = (*id, *end);
                self.doc.update(id, |it| {
                    if let Shape::Line(a, b) | Shape::Arrow(a, b) = &mut it.shape {
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
                    let id = self.doc.add(shape, self.style);
                    self.selected = Some(id);
                    if is_bubble {
                        self.edit_text(id);
                    }
                }
            }
            Some(Drag::Freehand(ps)) if ps.len() > 1 => {
                let id = self.doc.add(Shape::Freehand(ps), self.style);
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
            Tool::Highlight => Shape::Highlight(r),
            Tool::Obfuscate => Shape::Obfuscate { rect: r, mode: self.obfuscate },
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
            self.text = Some(TextEdit { item: Some(id), at, buffer: it.shape.text().unwrap_or_default().to_string(), focused: false });
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
                let id = self.doc.add(Shape::Text { at: t.at, text: t.buffer }, self.style);
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
        let modal = egui::Modal::new(egui::Id::unique("confirm-close")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading("Save this capture?");
            ui.label("It has changes that are not saved yet.");
            ui.add_space(10.0);
            let mut choice = None;
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(0);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
            choice
        });
        match modal.inner {
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
        let items: Vec<Item> = self.doc.items().iter().filter(|i| Some(i.id) != exclude).cloned().collect();
        render_items_onto(&mut pm, &items);
        self.drag_cache = Some(pm);
    }

    fn refresh_texture(&mut self, ctx: &egui::Context) {
        let pm = match (&self.drag, &self.drag_cache) {
            (Some(drag), Some(cache)) => {
                let extra: Option<Item> = match drag {
                    Drag::Create { start, now } => self.shape_from_drag(*start, *now).map(|shape| Item { id: 0, shape, style: self.style, frames: None }),
                    Drag::Freehand(ps) => Some(Item { id: 0, shape: Shape::Freehand(ps.clone()), style: self.style, frames: None }),
                    Drag::Move { id, .. } | Drag::Resize { id, .. } | Drag::Endpoint { id, .. } | Drag::Tail { id } => self.doc.item(*id).cloned(),
                    Drag::Crop { .. } => None,
                };
                let mut pm = cache.clone();
                if let Some(it) = extra {
                    render_items_onto(&mut pm, std::slice::from_ref(&it));
                }
                self.shown_version = None;
                pm
            }
            _ => {
                if self.shown_version == Some(self.doc.version) && self.texture.is_some() {
                    return;
                }
                let mut pm = to_pixmap(self.doc.base());
                render_items_onto(&mut pm, self.doc.items());
                self.shown_version = Some(self.doc.version);
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
            _ => self.crop,
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
        Shape::Line(a, b) | Shape::Arrow(a, b) => vec![(*a, HandleKind::End(0)), (*b, HandleKind::End(1))],
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

fn tool_hint(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => "Click an item to select it. Drag to move, drag a handle to resize, double-click text to edit.",
        Tool::Text => "Click where the text goes, type, then Enter.",
        Tool::Step => "Click to drop the next numbered step.",
        Tool::Bubble => "Drag out the bubble, then type. Drag the tail handle to point it.",
        Tool::Crop => "Drag the area to keep, then Enter.",
        Tool::Obfuscate => "Drag over anything private to pixelate or blur it.",
        Tool::Highlight => "Drag over text to highlight it.",
        Tool::Freehand => "Draw with the mouse held down.",
        _ => "Drag on the image to draw.",
    }
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
