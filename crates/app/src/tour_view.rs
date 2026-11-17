//! The Tour view: the recording editor's second bottom-panel mode, where each
//! tour step gets its own text, bubbles and spotlight before export.
//!
//! Items added here are pinned to the step's source frame (`Item::pin`), so
//! they show only on that step and are written to `tour.json` as the layer.
//! The canvas, tools and undo are the editor's own. Guide cards and Tour settings
//! are tour data, not items; see `tour_cards`.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use eframe::egui::{self, Color32, ColorImage, FontId, Key, Modifiers, Rect, Response, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2, pos2, vec2};
use image::RgbaImage;
use mythicshot_editor::{Bounds, Item, Pt, Shape, pt};

use crate::editor_ui::Editor;
use crate::icons::{self, Icon};
use crate::tour::{self, HotspotStyle, StepPlan, TourDoc};
use crate::tour_cards::{CardUi, SettingsUi, card_has_text, parse_hex};
use crate::video::{Edit, crop_px};
use crate::video_editor::{VideoState, VideoView};

pub const AMBER: Color32 = Color32::from_rgb(245, 165, 36);
const MUTED: Color32 = Color32::from_rgb(163, 170, 184);
/// What the status bar says while the Tour view is open.
pub const STATUS: &str = "Text and spotlight stay editable after export";
/// Thumbnail width in the step strip.
const THUMB_W: u32 = 96;
/// Thumbnails read per frame, so a long tour does not hold up the window.
const THUMBS_PER_FRAME: usize = 4;

/// What the step plan depends on besides the recording and its input events, which never change.
#[derive(Clone, PartialEq)]
struct PlanKey {
    trim: (usize, usize),
    deleted: BTreeSet<usize>,
    crop: Option<Bounds>,
    removed: BTreeSet<usize>,
}

impl PlanKey {
    fn of(edit: &Edit) -> Self {
        Self { trim: edit.trim, deleted: edit.deleted.clone(), crop: edit.crop, removed: edit.tour_removed.clone() }
    }

    fn matches(&self, edit: &Edit) -> bool {
        self.trim == edit.trim && self.crop == edit.crop && self.deleted == edit.deleted && self.removed == edit.tour_removed
    }
}

/// A step plan, or why there is none.
type StepsOrError = Result<Vec<StepPlan>, String>;

/// Tells one editor window's card and panel widgets from another's, so they never share text or undo.
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// The Tour view's own state, kept with the recording.
pub struct TourState {
    /// This editor's number, for widget ids.
    pub(crate) instance: u64,
    /// The step plan, worked out once and kept until the trim, deleted frames, crop or removed steps change.
    plan: RefCell<Option<(PlanKey, StepsOrError)>>,
    /// Plain frame thumbnails by source frame; `None` when the frame could not be read.
    thumbs: HashMap<usize, Option<TextureHandle>>,
    /// The strength slider is being dragged, so the drag is one undo step.
    pub dim_dragging: bool,
    /// The shown step's guide card: selection, typing, the spot being picked, the last removal.
    pub(crate) card: CardUi,
    /// The Tour settings panel.
    pub(crate) settings: SettingsUi,
}

impl Default for TourState {
    fn default() -> Self {
        Self { instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed), plan: RefCell::new(None), thumbs: HashMap::new(), dim_dragging: false, card: CardUi::default(), settings: SettingsUi::default() }
    }
}

/// The tour's steps for `v`: worked out once, then reused until something they depend on changes.
pub(crate) fn planned_steps(v: &VideoState) -> Result<Vec<StepPlan>, String> {
    let mut plan = v.tour.plan.borrow_mut();
    match plan.as_ref() {
        Some((key, steps)) if key.matches(&v.edit) => steps.clone(),
        _ => {
            let steps = tour::plan_steps(&v.rec, &v.edit, &v.events);
            *plan = Some((PlanKey::of(&v.edit), steps.clone()));
            steps
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepMarks {
    pub text: bool,
    pub spotlight: bool,
    /// A guide card with a title or a body, as it would be exported.
    pub card: bool,
}

/// Which badges a step's thumbnail shows.
pub fn step_marks(items: &[Item], doc: &TourDoc, src: usize) -> StepMarks {
    let mut marks = StepMarks { card: doc.steps.get(&src).and_then(|s| s.card.as_ref()).is_some_and(card_has_text), ..Default::default() };
    for it in items.iter().filter(|it| it.pin == Some(src as u32)) {
        match it.shape {
            Shape::Text { .. } | Shape::Bubble { .. } => marks.text = true,
            Shape::Spotlight { .. } => marks.spotlight = true,
            _ => {}
        }
    }
    marks
}

/// "2", or "4 · end" for the final step without a hotspot.
pub fn step_label(index: usize, is_end: bool) -> String {
    if is_end { format!("{} · end", index + 1) } else { (index + 1).to_string() }
}

/// One copy of `it` for every source frame in `srcs` except its own, each pinned to its step.
pub fn copies_for_steps(it: &Item, srcs: &[usize]) -> Vec<Item> {
    let Some(own) = it.pin else { return Vec::new() };
    srcs.iter().filter(|s| **s != own as usize).map(|s| Item { frames: None, pin: Some(*s as u32), ..it.clone() }).collect()
}

/// Ids of the pinned spotlights on `src`, which a new spotlight on that step replaces.
pub fn spotlights_on(items: &[Item], src: usize) -> Vec<u64> {
    items.iter().filter(|it| it.pin == Some(src as u32) && matches!(it.shape, Shape::Spotlight { .. })).map(|it| it.id).collect()
}

/// The step to show when switching to Tour view: the last step at or before `current`, else the first.
pub fn step_for_frame(steps: &[StepPlan], current: usize) -> usize {
    steps.iter().rposition(|s| s.src <= current).unwrap_or(0)
}

/// The step on screen: `tour_step` while it still shows `current`, else the
/// step on that frame, else the one at or before it.
fn current_step(v: &VideoState, steps: &[StepPlan]) -> usize {
    if steps.get(v.tour_step).is_some_and(|s| s.src == v.current) {
        return v.tour_step;
    }
    steps.iter().position(|s| s.src == v.current).unwrap_or_else(|| step_for_frame(steps, v.current))
}

/// Where the shown step's click landed, in frame pixels, for the hotspot.
/// Only in the Tour view, and never for the end step, which has no click.
pub(crate) fn click_point(v: &VideoState) -> Option<Pt> {
    if v.view != VideoView::Tour {
        return None;
    }
    let steps = planned_steps(v).ok()?;
    let step = steps.get(current_step(v, &steps)).filter(|s| s.src == v.current)?;
    let (px, py) = step.click?;
    let (cx, cy, cw, ch) = crop_px(&v.rec, &v.edit);
    Some(pt(cx as f32 + px / 100.0 * cw as f32, cy as f32 + py / 100.0 * ch as f32))
}

/// Seconds per pulse of the hotspot ring. The preview page keeps the same number as `--pulse-period`
/// in `tour_preview.html`; a test checks they agree.
pub const PULSE_PERIOD: f64 = 1.9;
/// How often a pulsing hotspot is redrawn while the window has focus: about 30 times a second.
const PULSE_FRAME: Duration = Duration::from_millis(33);
/// How often it is redrawn while the window is in the background or covered: a few times a second.
const PULSE_FRAME_IDLE: Duration = Duration::from_millis(250);
/// The hotspot ring's radius on screen, as the players draw it (30 px across).
const HOTSPOT_R: f32 = 15.0;

/// The pulse ring at `t` seconds: its scale against the hotspot ring and its opacity (0 to 1). Over the
/// first 70% of each [`PULSE_PERIOD`] it grows to 2.2 times the ring while fading out, easing off as
/// it goes; for the rest of the period it is not seen. Only time drives it, so it can be tested.
pub fn pulse_ring(t: f64) -> (f32, f32) {
    let phase = (t.rem_euclid(PULSE_PERIOD) / PULSE_PERIOD) as f32;
    if phase >= 0.7 {
        return (2.2, 0.0);
    }
    let eased = 1.0 - (1.0 - phase / 0.7).powi(3);
    (1.0 + 1.2 * eased, 0.55 * (1.0 - eased))
}

/// How soon to redraw a pulsing hotspot, from what the window system says about the window: never
/// while minimised, quickly while focused, slowly while in the background or covered.
fn pulse_frame(minimized: Option<bool>, focused: Option<bool>, occluded: Option<bool>) -> Option<Duration> {
    match (minimized, focused, occluded) {
        (Some(true), _, _) => None,
        (_, _, Some(true)) | (_, Some(false), _) => Some(PULSE_FRAME_IDLE),
        _ => Some(PULSE_FRAME),
    }
}

/// The accent at `alpha` (0 to 1) of its strength.
fn faded(c: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The "Timeline | Tour" switch at the left of the bottom panel. Returns the view picked, if it changed.
pub(crate) fn view_switch(ui: &mut egui::Ui, view: VideoView) -> Option<VideoView> {
    let mut picked = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if ui.add(egui::Button::selectable(view == VideoView::Timeline, "Timeline")).on_hover_text("Trim, time and play the recording").clicked() {
            picked = Some(VideoView::Timeline);
        }
        if ui.add(egui::Button::selectable(view == VideoView::Tour, "Tour")).on_hover_text("Give each tour step text, bubbles and a spotlight").clicked() {
            picked = Some(VideoView::Tour);
        }
    });
    picked.filter(|p| *p != view)
}

/// The plain frame, 96 pixels wide.
fn thumbnail(img: &RgbaImage) -> RgbaImage {
    let h = (THUMB_W as f32 * img.height() as f32 / img.width().max(1) as f32).round().max(1.0) as u32;
    image::imageops::thumbnail(img, THUMB_W, h)
}

/// Read the thumbnails the strip still lacks, a few per frame.
fn load_thumbs(v: &mut VideoState, steps: &[StepPlan], ctx: &egui::Context) {
    let mut read = 0;
    for s in steps {
        if v.tour.thumbs.contains_key(&s.src) {
            continue;
        }
        if read == THUMBS_PER_FRAME {
            ctx.request_repaint();
            break;
        }
        let tex = v.reader.get(s.src).ok().map(|img| {
            let small = thumbnail(&img);
            let ci = ColorImage::from_rgba_unmultiplied([small.width() as usize, small.height() as usize], small.as_raw());
            ctx.load_texture(format!("tour-step-{}", s.src), ci, TextureOptions::LINEAR)
        });
        v.tour.thumbs.insert(s.src, tex);
        read += 1;
    }
}

/// One step in the strip: its thumbnail, badges and label; amber outline when shown.
fn step_thumb(ui: &mut egui::Ui, tex: Option<&TextureHandle>, size: Vec2, label: &str, marks: StepMarks, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size.x, size.y + 16.0), Sense::click());
    let img = Rect::from_min_size(rect.min, size);
    let p = ui.painter();
    p.rect_filled(img, 3.0, Color32::from_rgb(17, 20, 26));
    if let Some(t) = tex {
        p.image(t.id(), img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    let outline = if selected {
        Stroke::new(2.0, AMBER)
    } else if resp.hovered() {
        Stroke::new(1.0, Color32::from_rgb(140, 148, 164))
    } else {
        Stroke::new(1.0, Color32::from_rgb(52, 59, 72))
    };
    p.rect_stroke(img, 3.0, outline, StrokeKind::Outside);
    // Badges in the top-right corner: the card icon for a guide card, "T" for text or a bubble, the spotlight icon for a spotlight.
    let badge_bg = Color32::from_black_alpha(180);
    let mut right = img.right() - 3.0;
    if marks.card {
        let r = Rect::from_min_size(pos2(right - 16.0, img.top() + 3.0), Vec2::splat(16.0));
        p.rect_filled(r, 3.0, badge_bg);
        icons::paint(p, r.shrink(3.0), Icon::Card, Color32::WHITE);
        right -= 18.0;
    }
    if marks.spotlight {
        let r = Rect::from_min_size(pos2(right - 16.0, img.top() + 3.0), Vec2::splat(16.0));
        p.rect_filled(r, 3.0, badge_bg);
        icons::paint(p, r.shrink(3.0), Icon::Spotlight, Color32::WHITE);
        right -= 18.0;
    }
    if marks.text {
        let r = Rect::from_min_size(pos2(right - 16.0, img.top() + 3.0), Vec2::splat(16.0));
        p.rect_filled(r, 3.0, badge_bg);
        p.text(r.center(), egui::Align2::CENTER_CENTER, "T", FontId::proportional(11.0), Color32::WHITE);
    }
    p.text(pos2(img.center().x, img.bottom() + 8.0), egui::Align2::CENTER_CENTER, label, FontId::proportional(11.0), if selected { AMBER } else { MUTED });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// What the Tour panel asked for this frame.
enum TourAction {
    View(VideoView),
    Show(usize),
    CopyToAll,
    Remove,
    Restore,
    Settings,
}

/// `current` when it is the source frame of a planned step, else None
/// (no plan, or a frame that is not a step), when nothing can be pinned.
pub fn step_src(steps: &Result<Vec<StepPlan>, String>, current: usize) -> Option<usize> {
    steps.as_ref().ok()?.iter().any(|s| s.src == current).then_some(current)
}

impl Editor {
    /// The shown step's hotspot on the canvas, drawn the way the tour draws it: Pulse (a ring with a
    /// soft ring growing and fading from it), Dot (the ring, still) or None, in the tour's accent.
    /// Editor chrome on the screen only, so it never reaches a step image, a GIF or a video. The pulse
    /// is redrawn only while the Tour view shows it and the window is not minimised, and stands still
    /// when "Animate hotspot" is off.
    pub(crate) fn draw_tour_hotspot(&self, ctx: &egui::Context, painter: &egui::Painter, to_scr: &dyn Fn(Pt) -> egui::Pos2) {
        let Some(v) = self.video.as_ref() else { return };
        let Some(c) = click_point(v) else { return };
        let theme = &v.edit.tour_doc.theme;
        if theme.hotspot == HotspotStyle::None {
            return;
        }
        let accent = parse_hex(&theme.accent).or_else(|| parse_hex(tour::DEFAULT_ACCENT)).unwrap_or(AMBER);
        let at = to_scr(c);
        if theme.hotspot == HotspotStyle::Pulse && self.prefs.tour_animate_hotspot {
            let (scale, alpha) = pulse_ring(ctx.input(|i| i.time));
            if alpha > 0.0 {
                painter.circle_stroke(at, (HOTSPOT_R + 1.0) * scale, Stroke::new(2.0, faded(accent, alpha)));
            }
            if let Some(after) = ctx.input(|i| pulse_frame(i.viewport().minimized, i.viewport().focused, i.viewport().occluded)) {
                ctx.request_repaint_after(after);
            }
        }
        painter.circle(at, HOTSPOT_R, faded(accent, 0.18), Stroke::new(2.0, accent));
        painter.circle_filled(at, 4.0, accent);
    }

    /// The Tour view is open, whether or not there are steps.
    pub(crate) fn in_tour_view(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.view == VideoView::Tour)
    }

    /// The source frame of the step shown, while the Tour view is open and shows a step.
    pub(crate) fn tour_src(&self) -> Option<usize> {
        let v = self.video.as_ref().filter(|v| v.view == VideoView::Tour)?;
        step_src(&planned_steps(v), v.current)
    }

    /// The Tour view is open without a step to pin to: nothing new may be drawn.
    pub(crate) fn drawing_blocked(&self) -> bool {
        self.in_tour_view() && self.tour_src().is_none()
    }

    /// Add `shape` pinned to the step on `src`, with no frame range. A
    /// spotlight takes the place of the step's spotlight in the same undo step.
    pub(crate) fn add_pinned(&mut self, shape: Shape, src: usize) -> u64 {
        let style = self.style;
        let pin = Some(src as u32);
        let old = if matches!(shape, Shape::Spotlight { .. }) { spotlights_on(self.doc.items(), src) } else { Vec::new() };
        match old.split_first() {
            Some((&keep, rest)) => {
                self.doc.checkpoint();
                self.doc.update(keep, |it| {
                    it.shape = shape;
                    it.style = style;
                    it.frames = None;
                    it.pin = pin;
                });
                // Never more than one in practice: every way of adding a spotlight replaces.
                for id in rest {
                    self.doc.remove(*id);
                }
                keep
            }
            None => {
                let id = self.doc.add(shape, style);
                self.doc.update(id, |it| {
                    it.frames = None;
                    it.pin = pin;
                });
                id
            }
        }
    }

    /// Switch the bottom panel between Timeline and Tour. Going to Tour stops
    /// playback and shows the step at or before the current frame; coming back keeps the frame.
    pub(crate) fn set_view(&mut self, view: VideoView) {
        let Some(v) = self.video.as_mut() else { return };
        if v.view == view {
            return;
        }
        v.view = view;
        v.playing = None;
        if view != VideoView::Tour {
            // Nothing of a guide card stays selected, typed or half-picked off screen, and its tool goes with it.
            self.forget_card_ui();
            if self.tool_is_card() {
                self.use_select_tool();
            }
        }
        let Some(v) = self.video.as_mut() else { return };
        if view == VideoView::Tour
            && let Ok(steps) = planned_steps(v)
        {
            v.tour_step = step_for_frame(&steps, v.current);
            v.current = steps[v.tour_step].src;
        }
        self.drop_hidden_selection();
    }

    /// Clear the selection when the selected item is not on the canvas any more.
    fn drop_hidden_selection(&mut self) {
        if let Some(id) = self.selected
            && !self.display_items().iter().any(|it| it.id == id)
        {
            self.select(None);
        }
    }

    /// Show step `i`: its source frame becomes the current frame.
    fn show_step(&mut self, steps: &[StepPlan], i: usize) {
        let (Some(v), Some(s)) = (self.video.as_mut(), steps.get(i)) else { return };
        let changed = v.tour_step != i || v.current != s.src;
        v.tour_step = i;
        v.current = s.src;
        v.playing = None;
        if changed {
            self.forget_card_ui();
        }
        self.drop_hidden_selection();
    }

    /// Left and Right move between steps; Space does nothing in the Tour view.
    pub(crate) fn tour_keys(&mut self, ctx: &egui::Context) {
        ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space));
        let right = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowRight));
        let left = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowLeft));
        if !right && !left {
            return;
        }
        let Some(v) = self.video.as_ref() else { return };
        let Ok(steps) = planned_steps(v) else { return };
        let i = current_step(v, &steps);
        let next = if right { (i + 1).min(steps.len() - 1) } else { i.saturating_sub(1) };
        self.show_step(&steps, next);
    }

    /// "Copy to all steps": a copy of the selected pinned item on every other
    /// step, in one undo step. A spotlight copy takes the place of the spotlight already on a step.
    fn copy_to_all_steps(&mut self, srcs: &[usize]) {
        let Some(it) = self.selected.and_then(|id| self.doc.item(id)).cloned() else { return };
        let copies = copies_for_steps(&it, srcs);
        if copies.is_empty() {
            return;
        }
        let spotlight = matches!(it.shape, Shape::Spotlight { .. });
        self.doc.checkpoint();
        for c in copies {
            let src = c.pin.map(|p| p as usize).unwrap_or_default();
            let old = if spotlight { spotlights_on(self.doc.items(), src).first().copied() } else { None };
            match old {
                Some(id) => self.doc.update(id, |o| {
                    o.shape = c.shape;
                    o.style = c.style;
                }),
                None => {
                    let id = self.doc.add_initial(c);
                    // `add_initial` is for the starting document; this is a change to save.
                    self.doc.update(id, |_| {});
                }
            }
        }
    }

    /// The Tour view's bottom panel: the switch, "Step N of M", the chips and the step strip.
    pub(crate) fn tour_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(v) = self.video.as_mut() else { return };
        let steps = planned_steps(v);
        // Keep the shown step in line with the plan after a step is removed or restored.
        let mut moved = false;
        if let Ok(steps) = &steps {
            v.tour_step = current_step(v, steps);
            if v.current != steps[v.tour_step].src {
                v.current = steps[v.tour_step].src;
                moved = true;
                ctx.request_repaint();
            }
            load_thumbs(v, steps, &ctx);
        }
        // An item pinned to the step that dropped out is off the canvas now: never leave it selected.
        if moved {
            self.drop_hidden_selection();
            self.forget_card_ui();
        }
        let doc = &self.video.as_ref().expect("video").edit.tour_doc;
        let marks: Vec<StepMarks> = steps.as_ref().map(|s| s.iter().map(|st| step_marks(self.doc.items(), doc, st.src)).collect()).unwrap_or_default();
        let can_copy = self.selected.and_then(|id| self.doc.item(id)).is_some_and(|it| it.pin.is_some());
        let mut action = None;
        let frame = egui::Frame::new().fill(Color32::from_rgb(27, 31, 39)).inner_margin(egui::Margin::symmetric(8, 6));
        egui::Panel::bottom("editor-timeline").frame(frame).show(ui, |ui| {
            let v = self.video.as_ref().expect("video");
            let count = steps.as_ref().map(|s| s.len()).unwrap_or(0);
            let removed = v.edit.tour_removed.len();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if let Some(view) = view_switch(ui, v.view) {
                    action = Some(TourAction::View(view));
                }
                if count > 0 {
                    ui.label(RichText::new(format!("Step {} of {count}", v.tour_step + 1)).color(MUTED));
                }
                if ui.add(egui::Button::selectable(v.tour.settings.open, "Tour settings")).on_hover_text("Card style, accent colour, zoom, cover, buttons, end card and autoplay").clicked() {
                    action = Some(TourAction::Settings);
                }
                ui.separator();
                let copy = ui.add_enabled(can_copy, egui::Button::new("Copy to all steps"));
                if copy.on_hover_text("Put a copy of the selected text, bubble or spotlight on every step").on_disabled_hover_text("Select something on this step first").clicked() {
                    action = Some(TourAction::CopyToAll);
                }
                let remove = ui.add_enabled(count > 1, egui::Button::new("Remove step"));
                if remove.on_hover_text("Leave this step out of the tour").on_disabled_hover_text("A tour needs at least one step").clicked() {
                    action = Some(TourAction::Remove);
                }
                if removed > 0 && ui.button(format!("Restore {removed} removed")).on_hover_text("Bring back every removed step").clicked() {
                    action = Some(TourAction::Restore);
                }
            });
            ui.add_space(4.0);
            match &steps {
                Err(e) => {
                    ui.add(egui::Label::new(RichText::new(e).color(MUTED)).wrap());
                }
                Ok(steps) => {
                    let h = (THUMB_W as f32 * v.rec.height as f32 / v.rec.width.max(1) as f32).clamp(24.0, 96.0);
                    egui::ScrollArea::horizontal().id_salt("tour-steps").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            for (i, s) in steps.iter().enumerate() {
                                let tex = v.tour.thumbs.get(&s.src).and_then(|t| t.as_ref());
                                let label = step_label(i, s.click.is_none());
                                if step_thumb(ui, tex, vec2(THUMB_W as f32, h), &label, marks[i], i == v.tour_step).clicked() {
                                    action = Some(TourAction::Show(i));
                                }
                            }
                        });
                    });
                }
            }
        });
        let steps = steps.unwrap_or_default();
        match action {
            None => {}
            Some(TourAction::View(view)) => self.set_view(view),
            Some(TourAction::Show(i)) => {
                self.show_step(&steps, i);
                ctx.request_repaint();
            }
            Some(TourAction::CopyToAll) => self.copy_to_all_steps(&steps.iter().map(|s| s.src).collect::<Vec<_>>()),
            Some(TourAction::Remove) => {
                let Some(v) = self.video.as_mut() else { return };
                let i = v.tour_step.min(steps.len().saturating_sub(1));
                if steps.len() > 1 {
                    // The next step takes its place, or the previous one at the end.
                    let next = if i + 1 < steps.len() { i + 1 } else { i - 1 };
                    v.edit.tour_removed.insert(steps[i].src);
                    v.current = steps[next].src;
                    v.tour_step = if next > i { i } else { next };
                    self.drop_hidden_selection();
                    self.forget_card_ui();
                    ctx.request_repaint();
                }
            }
            Some(TourAction::Restore) => {
                if let Some(v) = self.video.as_mut() {
                    v.edit.tour_removed.clear();
                }
                ctx.request_repaint();
            }
            Some(TourAction::Settings) => {
                if let Some(v) = self.video.as_mut() {
                    v.tour.settings.open = !v.tour.settings.open;
                }
            }
        }
        self.tour_settings_panel(ui);
    }

    /// Debug test hook: the Tour view with a text, a bubble and a spotlight pinned to the first step,
    /// guide cards on the first two steps, a cover card and an always-on button.
    #[cfg(debug_assertions)]
    pub(crate) fn test_tour_view(&mut self) {
        use mythicshot_editor::Bounds;
        self.set_view(VideoView::Tour);
        if self.drawing_blocked() {
            return;
        }
        let (w, h) = (self.doc.width() as f32, self.doc.height() as f32);
        self.add_item(Shape::Text { at: pt(w * 0.06, h * 0.06), text: "Start here".into() });
        self.add_item(Shape::Bubble { rect: Bounds::new(pt(w * 0.6, h * 0.1), pt(w * 0.88, h * 0.24)), tail: pt(w * 0.62, h * 0.38), text: "Then click Save".into() });
        let dim = self.prefs.spotlight_dim;
        let id = self.add_item(Shape::Spotlight { rect: Bounds::new(pt(w * 0.25, h * 0.3), pt(w * 0.6, h * 0.7)), dim });
        self.select(Some(id));
        self.test_guide_cards();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tour::{StepExtras, StepPlan, TourDoc};
    use mythicshot_editor::{Bounds, Item, Shape, Style, pt};

    fn item(id: u64, shape: Shape, pin: Option<u32>) -> Item { Item { id, shape, style: Style::default(), frames: None, pin } }
    fn spot() -> Shape { Shape::Spotlight { rect: Bounds { x0: 0.0, y0: 0.0, x1: 10.0, y1: 10.0 }, dim: 60 } }
    fn text() -> Shape { Shape::Text { at: pt(1.0, 1.0), text: "a".into() } }

    #[test]
    fn badges_follow_the_items_pinned_to_the_step() {
        let items = [item(1, text(), Some(3)), item(2, spot(), Some(7)), item(3, text(), None)];
        let mut doc = TourDoc::default();
        assert_eq!(step_marks(&items, &doc, 3), StepMarks { text: true, spotlight: false, card: false });
        assert_eq!(step_marks(&items, &doc, 7), StepMarks { text: false, spotlight: true, card: false });
        assert_eq!(step_marks(&items, &doc, 9), StepMarks::default());
        // A guide card counts once it has a title or a body, as on export.
        let mut c = crate::tour_cards::new_card(true);
        doc.steps.insert(9, StepExtras { card: Some(c.clone()), ..Default::default() });
        assert!(!step_marks(&items, &doc, 9).card, "an empty card is not exported");
        c.body = "Click Save".into();
        doc.steps.insert(9, StepExtras { card: Some(c), ..Default::default() });
        assert_eq!(step_marks(&items, &doc, 9), StepMarks { text: false, spotlight: false, card: true });
        assert!(!step_marks(&items, &doc, 3).card, "the card belongs to its own step");
    }

    #[test]
    fn the_pulse_grows_and_fades_then_rests_once_per_period() {
        let close = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4;
        assert!(close(pulse_ring(0.0), (1.0, 0.55)), "starts at the ring, half seen");
        assert!(close(pulse_ring(PULSE_PERIOD * 0.7), (2.2, 0.0)), "gone at 70%");
        assert!(close(pulse_ring(PULSE_PERIOD * 0.9), (2.2, 0.0)), "rests unseen until the next pulse");
        // The same at every period, and before the clock's start.
        for t in [0.2, 0.5, 1.0, 1.5] {
            assert!(close(pulse_ring(t), pulse_ring(t + PULSE_PERIOD * 3.0)), "{t}");
            assert!(close(pulse_ring(t), pulse_ring(t - PULSE_PERIOD)), "{t}");
        }
        // It only ever grows and fades within a pulse.
        let mut last = pulse_ring(0.0);
        for i in 1..=70 {
            let now = pulse_ring(PULSE_PERIOD * 0.7 * i as f64 / 70.0 - 1e-6);
            assert!(now.0 >= last.0 - 1e-5 && now.1 <= last.1 + 1e-5, "{i}: {last:?} then {now:?}");
            last = now;
        }
        assert!((PULSE_PERIOD - 1.9).abs() < f64::EPSILON, "the preview page's period, which the owner liked");
    }

    #[test]
    fn the_pulse_redraws_quickly_only_while_the_window_is_in_front() {
        assert_eq!(pulse_frame(Some(false), Some(true), Some(false)), Some(PULSE_FRAME));
        assert_eq!(pulse_frame(None, None, None), Some(PULSE_FRAME), "nothing known: as if in front");
        assert_eq!(pulse_frame(Some(false), Some(false), None), Some(PULSE_FRAME_IDLE), "in the background");
        assert_eq!(pulse_frame(Some(false), Some(true), Some(true)), Some(PULSE_FRAME_IDLE), "covered");
        assert_eq!(pulse_frame(Some(true), Some(true), None), None, "minimised");
    }

    #[test]
    fn labels_number_from_one_and_mark_the_end_step() {
        assert_eq!(step_label(0, false), "1");
        assert_eq!(step_label(3, true), "4 · end");
    }

    #[test]
    fn copy_to_all_steps_skips_the_items_own_step() {
        let copies = copies_for_steps(&item(1, text(), Some(3)), &[1, 3, 7]);
        assert_eq!(copies.iter().map(|c| c.pin).collect::<Vec<_>>(), vec![Some(1), Some(7)]);
        assert!(copies.iter().all(|c| c.shape == text() && c.frames.is_none()));
        assert!(copies_for_steps(&item(1, text(), None), &[1, 3]).is_empty(), "only pinned items are copied");
    }

    #[test]
    fn a_new_spotlight_replaces_the_one_on_its_step_only() {
        let items = [item(1, spot(), Some(3)), item(2, spot(), Some(7)), item(3, spot(), None), item(4, text(), Some(3))];
        assert_eq!(spotlights_on(&items, 3), vec![1]);
    }

    #[test]
    fn tour_view_opens_on_the_step_at_or_before_the_current_frame() {
        let steps = [StepPlan { src: 2, click: Some((1.0, 1.0)), t_ms: 0 }, StepPlan { src: 6, click: Some((1.0, 1.0)), t_ms: 0 }, StepPlan { src: 9, click: None, t_ms: 0 }];
        assert_eq!(step_for_frame(&steps, 0), 0);
        assert_eq!(step_for_frame(&steps, 6), 1);
        assert_eq!(step_for_frame(&steps, 8), 1);
        assert_eq!(step_for_frame(&steps, 50), 2);
    }

    #[test]
    fn drawing_needs_a_step_on_the_current_frame() {
        let steps: Result<Vec<StepPlan>, String> = Ok(vec![StepPlan { src: 2, click: Some((1.0, 1.0)), t_ms: 0 }, StepPlan { src: 9, click: None, t_ms: 0 }]);
        assert_eq!(step_src(&steps, 2), Some(2));
        assert_eq!(step_src(&steps, 5), None, "a frame that is not a step");
        assert_eq!(step_src(&Err("no clicks".into()), 2), None, "no plan, no step");
    }
}
