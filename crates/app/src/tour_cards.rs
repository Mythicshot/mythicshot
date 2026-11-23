//! Guide cards on the Tour view's canvas and the Properties panel.
//!
//! A guide card is tour data (`TourDoc::steps`), not an annotation: it is drawn as an
//! egui overlay above the canvas, where the shared placement rule puts it, so it can
//! never reach a step image, a GIF or a video. Its counter and Back are filled in, never typed.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};
use mythicshot_editor::{Bounds, MAX_DIM, MIN_DIM, Pt, Shape, pt};

use crate::theme::{self, Strong as _};
use crate::editor_ui::Editor;
use crate::icons::{self, Icon};
use crate::settings::SavedTheme;
use crate::tour::{
    self, Advance, Anchor, BUTTON_LABEL_MAX, CARD_BODY_MAX, CARD_TITLE_MAX, COVER_BODY_MAX, COVER_BUTTON_MAX, COVER_TITLE_MAX, Card, CardStyle, Cover, DEFAULT_ACCENT, END_TEXT_MAX, EndCard, HotspotStyle, LINK_MAX, Link, NEXT_LABEL_MAX, Placed, Placement, Point, Side, TourDoc, ZOOM_MAX,
    ZOOM_MIN, AUTOPLAY_MAX, AUTOPLAY_MIN, parse_bold, place_card, safe_url,
};
use crate::tour_view::{PULSE_HINT, PulseAction, click_point, planned_steps};
use crate::video::crop_px;

/// The card's width on screen, the same as the players draw it.
const CARD_W: f32 = 260.0;
const CARD_PAD: f32 = 14.0;
const CARD_RADIUS: u8 = 10;
/// The box a card keeps clear of around its anchor when the step has no spotlight.
const ANCHOR_BOX: f32 = 44.0;
const DEFAULT_GREEN: Color32 = Color32::from_rgb(0x5d, 0xca, 0xa5);
const INK_DARK: Color32 = Color32::from_rgb(0x10, 0x14, 0x1a);
const INK_LIGHT: Color32 = Color32::from_rgb(0xf4, 0xf6, 0xf9);
/// Five calm accents; the first is the default. None of them is red.
const ACCENTS: [(&str, &str); 5] = [("Calm green", "#5dcaa5"), ("Sky", "#6aa8f0"), ("Lavender", "#a593f2"), ("Honey", "#e8b04b"), ("Slate", "#8fa3b8")];
/// A backdrop colour to start from when "Pick a colour" is chosen.
const BACKDROP_START: &str = "#20252e";
const LINK_WARNING: &str = "Links must start with https://";
const HEX_WARNING: &str = "Use a colour like #5dcaa5";

/// A new card for a step: points at the hotspot when the step has a click, at nothing otherwise.
pub fn new_card(has_click: bool) -> Card {
    Card { title: String::new(), body: String::new(), next_label: String::new(), anchor: if has_click { Anchor::Hotspot } else { Anchor::None }, point: None, placement: Placement::Auto }
}

/// The card's rectangle on the canvas in frame pixels, from the same rule the players use.
/// The box to keep clear is the spotlight; without one, a 44-pixel square around the anchor.
/// `picked` is a spot chosen with "A spot I pick": the card points at it while still keeping clear of
/// the spotlight, as every player places it.
pub fn card_rect(frame: (f32, f32), card_size: (f32, f32), spotlight: Option<Bounds>, anchor_at: Option<Pt>, picked: Option<Pt>, placement: Placement) -> Placed {
    let half = ANCHOR_BOX / 2.0;
    let avoid = spotlight.or_else(|| anchor_at.map(|p| Bounds { x0: p.x - half, y0: p.y - half, x1: p.x + half, y1: p.y + half }));
    place_card(frame, card_size, avoid, placement, picked.map(|p| (p.x, p.y)))
}

/// Card colours for a style: (background, text, muted text). The compact bar is a light pill, so it
/// takes Light's colours; `Auto` is drawn as dark in the editor.
pub fn card_colors(style: CardStyle) -> (Color32, Color32, Color32) {
    match style {
        CardStyle::Light | CardStyle::Compact => (Color32::from_rgb(248, 249, 251), Color32::from_rgb(28, 32, 40), Color32::from_rgb(98, 106, 120)),
        CardStyle::Dark | CardStyle::Auto => (Color32::from_rgb(30, 35, 44), Color32::from_rgb(236, 239, 244), Color32::from_rgb(152, 160, 174)),
    }
}

/// Corner radius for a pill of height `h`: half its height, within what a corner radius holds.
pub fn pill_radius(h: f32) -> u8 {
    (h / 2.0).round().clamp(0.0, 255.0) as u8
}

/// Relative luminance of an sRGB colour.
fn luminance(c: Color32) -> f32 {
    let ch = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
}

/// Black or near-white, whichever reads better on the accent.
pub fn on_accent(accent: Color32) -> Color32 {
    let contrast = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
    let l = luminance(accent);
    if contrast(l, luminance(INK_DARK)) >= contrast(l, luminance(INK_LIGHT)) { INK_DARK } else { INK_LIGHT }
}

/// Text cut at `cap` characters.
pub fn capped(s: &str, cap: usize) -> String {
    s.chars().take(cap).collect()
}

/// `#rrggbb` as a colour, or None.
pub(crate) fn parse_hex(s: &str) -> Option<Color32> {
    let d = s.trim().strip_prefix('#')?;
    if d.len() != 6 || !d.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(d, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn hex_of(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn style_key(s: CardStyle) -> &'static str {
    match s {
        CardStyle::Dark => "dark",
        CardStyle::Light => "light",
        CardStyle::Compact => "compact",
        CardStyle::Auto => "auto",
    }
}

fn hotspot_key(s: HotspotStyle) -> &'static str {
    match s {
        HotspotStyle::Pulse => "pulse",
        HotspotStyle::Dot => "dot",
        HotspotStyle::None => "none",
    }
}

/// The theme and zoom of `doc` as the settings file keeps them.
pub fn saved_theme(doc: &TourDoc) -> SavedTheme {
    let t = &doc.theme;
    SavedTheme { card_style: style_key(t.card_style).into(), accent: t.accent.clone(), hotspot: hotspot_key(t.hotspot).into(), progress_bar: t.progress_bar, backdrop: t.backdrop.clone(), spotlight_soft: t.spotlight_soft, zoom_enabled: doc.zoom.enabled, zoom_percent: doc.zoom.percent }
}

/// Start a tour from the remembered theme and zoom; anything unreadable keeps the default.
pub fn apply_saved_theme(doc: &mut TourDoc, s: &SavedTheme) {
    let t = &mut doc.theme;
    t.card_style = [CardStyle::Dark, CardStyle::Light, CardStyle::Compact, CardStyle::Auto].into_iter().find(|c| style_key(*c) == s.card_style).unwrap_or_default();
    t.accent = parse_hex(&s.accent).map(hex_of).unwrap_or_else(|| DEFAULT_ACCENT.to_string());
    t.hotspot = [HotspotStyle::Pulse, HotspotStyle::Dot, HotspotStyle::None].into_iter().find(|h| hotspot_key(*h) == s.hotspot).unwrap_or_default();
    t.progress_bar = s.progress_bar;
    t.backdrop = if s.backdrop == "auto" { "auto".into() } else { parse_hex(&s.backdrop).map(hex_of).unwrap_or_else(|| "auto".into()) };
    t.spotlight_soft = s.spotlight_soft;
    doc.zoom.enabled = s.zoom_enabled;
    doc.zoom.percent = s.zoom_percent.clamp(ZOOM_MIN, ZOOM_MAX);
}

/// True when the card would be exported: it has a title or a body.
pub fn card_has_text(c: &Card) -> bool {
    !c.title.trim().is_empty() || !c.body.trim().is_empty()
}

/// Which of a card's texts is being typed in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    Title,
    Body,
    Next,
}

/// The guide card's editing state, kept with the recording.
#[derive(Default)]
pub struct CardUi {
    /// The shown step's card is selected: dashed outline and the "Points at" row.
    selected: bool,
    editing: Option<Field>,
    /// Give the field being edited the keyboard next frame.
    focus: bool,
    /// The next click on the canvas sets the card's spot: the step being picked on and what the card
    /// pointed at before, which a cancelled pick restores.
    picking: Option<(usize, Anchor)>,
    /// The card's height on screen last frame, for placing it this frame.
    height: f32,
    /// Where the card was drawn last frame, in frame pixels, so a click on it selects it.
    drawn: Option<Bounds>,
    /// The same on screen, so a press off the card can end typing on it.
    drawn_screen: Option<Rect>,
    /// The last card removed and its step, for "Card removed · Undo".
    removed: Option<(usize, Card)>,
}

/// The Properties panel's state: open or not, and the text of fields that must be valid before they reach the tour.
#[derive(Default)]
pub struct SettingsUi {
    pub open: bool,
    /// The drafts below were filled from the tour.
    loaded: bool,
    accent: String,
    cta_url: String,
    primary_url: String,
    secondary_url: String,
    logo_error: Option<String>,
    /// A chosen logo being checked off the UI thread.
    logo_job: Option<Receiver<Result<PathBuf, String>>>,
    /// What a switched-off section held, so switching it on again brings it back.
    stash_cover: Option<Cover>,
    stash_cta: Option<Link>,
    stash_end: Option<EndCard>,
}

/// Give step `src` a new card when it has none. A removed card on that step is forgotten then,
/// so "Undo" can never overwrite the new one. True when a card was added.
fn add_card(doc: &mut TourDoc, ui: &mut CardUi, src: usize, has_click: bool) -> bool {
    let extras = doc.steps.entry(src).or_default();
    if extras.card.is_some() {
        return false;
    }
    extras.card = Some(new_card(has_click));
    if ui.removed.as_ref().is_some_and(|(s, _)| *s == src) {
        ui.removed = None;
    }
    true
}

/// Take step `src`'s card away and remember it for "Undo", in place of any card removed before.
fn take_card(doc: &mut TourDoc, ui: &mut CardUi, src: usize) -> bool {
    let Some(card) = doc.steps.get_mut(&src).and_then(|s| s.card.take()) else { return false };
    *ui = CardUi { removed: Some((src, card)), height: ui.height, ..Default::default() };
    true
}

/// "Undo": the removed card comes back on step `shown`, only if it is that card's step and the step has no card now.
fn undo_removal(doc: &mut TourDoc, ui: &mut CardUi, shown: usize) -> bool {
    let free = doc.steps.get(&shown).is_none_or(|s| s.card.is_none());
    if !free || ui.removed.as_ref().is_none_or(|(s, _)| *s != shown) {
        return false;
    }
    let Some((_, card)) = ui.removed.take() else { return false };
    doc.steps.entry(shown).or_default().card = Some(card);
    ui.selected = true;
    true
}

/// "Undo" is offered only while it would bring back a card without replacing one.
fn can_undo_removal(doc: &TourDoc, ui: &CardUi, shown: usize) -> bool {
    ui.removed.as_ref().is_some_and(|(s, _)| *s == shown) && doc.steps.get(&shown).is_none_or(|s| s.card.is_none())
}

/// "A spot I pick" on step `src`: the next click on the picture sets the spot. An earlier spot is used
/// straight away; what the card pointed at before is kept so a cancelled pick can restore it.
fn start_pick(doc: &mut TourDoc, ui: &mut CardUi, src: usize) {
    let Some(c) = doc.steps.get_mut(&src).and_then(|s| s.card.as_mut()) else { return };
    let before = match ui.picking {
        Some((s, before)) if s == src => before,
        _ => c.anchor,
    };
    if c.point.is_some() {
        c.anchor = Anchor::Point;
    }
    ui.picking = Some((src, before));
}

/// The click that ends a pick: the card points at `p` on the step being picked on.
fn finish_pick(doc: &mut TourDoc, ui: &mut CardUi, p: Pt) -> bool {
    let Some((src, _)) = ui.picking.take() else { return false };
    if let Some(c) = doc.steps.get_mut(&src).and_then(|s| s.card.as_mut()) {
        c.anchor = Anchor::Point;
        c.point = Some(Point { x: p.x, y: p.y });
    }
    true
}

/// A pick that ended without a click on the picture (Esc, focus lost, another step, tool or choice):
/// the card points at what it did before, so "A spot I pick" is only ever chosen with a spot.
fn cancel_pick(doc: &mut TourDoc, ui: &mut CardUi) {
    let Some((src, before)) = ui.picking.take() else { return };
    if let Some(c) = doc.steps.get_mut(&src).and_then(|s| s.card.as_mut()) {
        c.anchor = if before == Anchor::Point && c.point.is_none() { Anchor::None } else { before };
    }
}

/// True when a press this frame (`press`) ends typing on the card: any press that is not on the card.
fn ends_typing(press: Option<Pos2>, on_card: bool) -> bool {
    press.is_some() && !on_card
}

/// The choice the "Points at" row shows: "A spot I pick" while picking or with a spot, else the card's anchor.
fn shown_anchor(card: &Card, picking: bool) -> Anchor {
    match card.anchor {
        _ if picking => Anchor::Point,
        Anchor::Point if card.point.is_none() => Anchor::None,
        a => a,
    }
}

/// The theme to remember after the panel ran: only a change the panel made, never the shared one compared again.
fn changed_theme(before: &SavedTheme, after: SavedTheme) -> Option<SavedTheme> {
    (*before != after).then_some(after)
}

/// Check a chosen logo off the UI thread: decoding and shrinking a large picture takes a moment.
fn start_logo_check(path: PathBuf) -> Receiver<Result<PathBuf, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(tour::load_logo(&path).map(|_| path));
    });
    rx
}

/// A link field's text written to the tour: the link when it is safe, else nothing.
fn link_from_draft(draft: &str) -> String {
    safe_url(draft).unwrap_or_default()
}

/// The inline message beside a link field, if its text is not a link that can be exported.
fn link_warning(draft: &str) -> Option<&'static str> {
    (!draft.trim().is_empty() && safe_url(draft).is_none()).then_some(LINK_WARNING)
}

/// How one of a card's texts is typed and shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lines {
    /// Typed on one line, shown wrapped (title, button).
    Wrap,
    /// Typed and shown as several lines (body).
    Paragraph,
    /// Typed on one line and shown on one line, cut with an ellipsis (the compact bar's text).
    One,
}

/// Text pieces with `**bold**` marked, laid out at `width`. With `bold_only`, plain pieces are
/// transparent, so drawing it a hair to the right over the full text makes the bold pieces heavier.
/// With [`Lines::One`] line breaks become spaces and the text is cut to one row with an ellipsis.
fn text_job(pieces: &[(String, bool)], font: &FontId, color: Color32, width: f32, bold_only: bool, lines: Lines) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    if lines == Lines::One {
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
    }
    for (text, bold) in pieces {
        let c = if bold_only && !bold { Color32::TRANSPARENT } else { color };
        let text = if lines == Lines::One { text.replace('\n', " ") } else { text.clone() };
        job.append(&text, 0.0, TextFormat { font_id: font.clone(), color: c, ..Default::default() });
    }
    job
}

/// The id of one of a card's text boxes: its own per editor window, step and text, so no two
/// share what is typed or its undo history.
fn field_id(instance: u64, src: usize, field: Field) -> egui::Id {
    egui::Id::unique(("tour-card-text", instance, src, field))
}

/// What a card's texts share while they are drawn.
struct FieldCtx {
    /// The editor window and the step's source frame, for the text boxes' ids.
    instance: u64,
    src: usize,
    editing: Option<Field>,
    focus: bool,
    clicked: Option<Field>,
}

/// One of the card's texts: drawn (with bold) until clicked, then typed in place, capped at `cap`.
/// `all_bold` draws the whole text heavier (title, button); otherwise `**bold**` is honoured.
#[allow(clippy::too_many_arguments)]
fn card_field(ui: &mut egui::Ui, fc: &mut FieldCtx, which: Field, text: &mut String, cap: usize, font: FontId, color: Color32, hint: (&str, Color32), width: f32, lines: Lines, all_bold: bool) {
    if fc.editing == Some(which) {
        let edit = if lines == Lines::Paragraph { egui::TextEdit::multiline(text).desired_rows(1) } else { egui::TextEdit::singleline(text) };
        let resp = ui.add(edit.id(field_id(fc.instance, fc.src, which)).frame(egui::Frame::NONE).margin(Margin::ZERO).font(font).text_color(color).hint_text(RichText::new(hint.0).color(hint.1)).char_limit(cap).desired_width(width));
        if fc.focus {
            resp.request_focus();
            fc.focus = false;
        } else if !resp.has_focus() {
            fc.editing = None;
        }
        if text.chars().count() > cap {
            *text = capped(text, cap);
        }
        return;
    }
    let (pieces, c) = if text.trim().is_empty() { (vec![(hint.0.to_string(), false)], hint.1) } else if all_bold { (vec![(text.clone(), true)], color) } else { (parse_bold(text), color) };
    let galley = ui.painter().layout_job(text_job(&pieces, &font, c, width, false, lines));
    let size = vec2(if lines == Lines::Paragraph { width } else { galley.size().x.min(width) }, galley.size().y);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    ui.painter().galley(rect.min, galley, c);
    if pieces.iter().any(|(_, b)| *b) {
        let bold = ui.painter().layout_job(text_job(&pieces, &font, c, width, true, lines));
        ui.painter().galley(rect.min + vec2(0.55, 0.0), bold, c);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    if resp.clicked() {
        fc.clicked = Some(which);
    }
}

/// The Properties panel's heading. Approved wording.
pub(crate) const PROPERTIES: &str = crate::editor_ui::PROPERTIES;
/// The close button's hover text.
pub(crate) const CLOSE_PROPERTIES: &str = "Close Properties";
/// The panel's two sections, in this order: the shown step's settings, then the tour's. Approved wording.
pub(crate) const THIS_STEP: &str = "This step";
pub(crate) const TOUR: &str = "Tour";

/// One of the Properties panel's two sections ([`THIS_STEP`], [`TOUR`]): a line, then its title,
/// a size up from the headings inside it.
fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    ui.label(RichText::new(title).bold().size(theme::STRONG_SIZE).color(theme::TEXT));
    ui.add_space(2.0);
}

/// A heading inside a section of the Properties panel.
fn heading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    ui.label(RichText::new(title).bold().color(theme::TEXT));
    ui.add_space(2.0);
}

/// A muted label in the first column of a settings grid.
fn grid_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::MUTED));
}

/// A one-line text field capped at `cap`.
fn line_field(ui: &mut egui::Ui, text: &mut String, cap: usize, hint: &str) -> egui::Response {
    let resp = ui.add(egui::TextEdit::singleline(text).hint_text(hint).char_limit(cap).desired_width(f32::INFINITY));
    if text.chars().count() > cap {
        *text = capped(text, cap);
    }
    resp
}

/// A few lines of text capped at `cap`.
fn text_field(ui: &mut egui::Ui, text: &mut String, cap: usize, hint: &str) {
    ui.add(egui::TextEdit::multiline(text).hint_text(hint).char_limit(cap).desired_rows(2).desired_width(f32::INFINITY));
    if text.chars().count() > cap {
        *text = capped(text, cap);
    }
}

/// A warning under the field it belongs to: the warning icon and caution-text.
fn warning(ui: &mut egui::Ui, text: &str) {
    theme::caution_line(ui, text, theme::SMALL_SIZE, theme::Fit::Wrap);
}

/// The label and link of a button, the link checked as it is typed. Returns nothing; writes the link only when safe.
fn link_rows(ui: &mut egui::Ui, label_name: &str, link: &mut Link, draft: &mut String, label_hint: &str) {
    grid_label(ui, label_name);
    line_field(ui, &mut link.label, BUTTON_LABEL_MAX, label_hint);
    ui.end_row();
    grid_label(ui, "Link");
    ui.vertical(|ui| {
        if line_field(ui, draft, LINK_MAX, "https://").changed() {
            link.url = link_from_draft(draft);
        }
        if let Some(w) = link_warning(draft) {
            warning(ui, w);
        }
    });
    ui.end_row();
}

/// The "Pulse" row of the shown step: "Where I clicked" or "A spot I pick", in the style of the
/// card's "Points at" row; while the picture takes the pulse, the hint in the accent colour; and
/// once the pulse was moved, "Put it back where I clicked". A step without a click (the end step)
/// has the row disabled. `moved`: the step's pulse is not where it was clicked; `placing`: the
/// picture takes it now.
fn pulse_row(ui: &mut egui::Ui, has_click: bool, moved: bool, placing: bool) -> Option<PulseAction> {
    let mut action = None;
    let picked = has_click && (moved || placing);
    ui.horizontal(|ui| {
        ui.label("Pulse");
        ui.add_enabled_ui(has_click, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let recorded = ui.add(egui::Button::selectable(!picked, "Where I clicked"));
            if recorded.on_hover_text("Where the recording was clicked").on_disabled_hover_text("This step has no click").clicked() {
                action = Some(PulseAction::Recorded);
            }
            let spot = ui.add(egui::Button::selectable(picked, "A spot I pick"));
            if spot.on_hover_text(PULSE_HINT).on_disabled_hover_text("This step has no click").clicked() {
                action = Some(PulseAction::Pick);
            }
        });
    });
    if has_click && placing {
        ui.label(RichText::new(PULSE_HINT).color(theme::ACCENT));
    }
    if has_click && moved && ui.button("Put it back where I clicked").clicked() {
        action = Some(PulseAction::PutBack);
    }
    action
}

impl Editor {
    /// The shown step's source frame, its index, the step count and whether it has a click.
    fn card_step(&self) -> Option<(usize, usize, usize, bool)> {
        let src = self.tour_src()?;
        let steps = planned_steps(self.video.as_ref()?).ok()?;
        let i = steps.iter().position(|s| s.src == src)?;
        Some((src, i, steps.len(), steps[i].click.is_some()))
    }

    /// The shown step's card, if it has one.
    fn shown_card(&self) -> Option<&Card> {
        let src = self.tour_src()?;
        self.video.as_ref()?.edit.tour_doc.steps.get(&src)?.card.as_ref()
    }

    /// The shown step's card is selected.
    pub(crate) fn card_selected(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.tour.card.selected) && self.shown_card().is_some()
    }

    /// Select or unselect the shown step's card; selecting it unselects any annotation.
    pub(crate) fn select_card(&mut self, on: bool) {
        if on {
            self.select(None);
        }
        if let Some(v) = self.video.as_mut() {
            let c = &mut v.tour.card;
            c.selected = on;
            if !on {
                (c.editing, c.focus) = (None, false);
                cancel_pick(&mut v.edit.tour_doc, c);
            }
        }
    }

    /// Stop picking the card's spot, if a pick is under way, and point the card where it did before.
    pub(crate) fn cancel_card_pick(&mut self) {
        if let Some(v) = self.video.as_mut() {
            cancel_pick(&mut v.edit.tour_doc, &mut v.tour.card);
        }
    }

    /// Forget the card's selection, editing and undo when the shown step changes.
    pub(crate) fn forget_card_ui(&mut self) {
        if let Some(v) = self.video.as_mut() {
            let c = &mut v.tour.card;
            cancel_pick(&mut v.edit.tour_doc, c);
            (c.selected, c.editing, c.focus, c.removed, c.drawn) = (false, None, false, None, None);
        }
    }

    /// The text box of the card being typed in, while one is.
    pub(crate) fn card_typing_id(&self) -> Option<egui::Id> {
        let v = self.video.as_ref()?;
        let src = self.tour_src()?;
        v.tour.card.editing.filter(|_| v.tour.card.selected).map(|f| field_id(v.tour.instance, src, f))
    }

    /// A press anywhere but on the card ends typing on it in the press's own frame, so the card's text
    /// box hands the keyboard back before the button is released: the click (on a toolbar button,
    /// "A spot I pick" or Export) then lands on a settled screen and one click is enough. Called after
    /// the canvas, so a key typed in the same frame has already reached the card.
    /// `press` is where the pointer went down this frame, in screen points.
    pub(crate) fn end_card_typing_on_press(&mut self, ctx: &egui::Context, press: Option<Pos2>) {
        let Some(id) = self.card_typing_id() else { return };
        let Some(v) = self.video.as_mut() else { return };
        let on_card = v.tour.card.drawn_screen.is_some_and(|r| press.is_some_and(|p| r.contains(p)));
        if ends_typing(press, on_card) {
            ctx.memory_mut(|m| m.surrender_focus(id));
            (v.tour.card.editing, v.tour.card.focus) = (None, false);
            ctx.request_repaint();
        }
    }

    /// Esc: stop typing on the card, else unselect it. True when the card took the key.
    pub(crate) fn card_escape(&mut self) -> bool {
        if !self.in_tour_view() || self.shown_card().is_none() {
            return false;
        }
        let Some(v) = self.video.as_mut() else { return false };
        let c = &mut v.tour.card;
        if c.editing.is_some() || c.picking.is_some() {
            (c.editing, c.focus) = (None, false);
            cancel_pick(&mut v.edit.tour_doc, c);
            return true;
        }
        if c.selected {
            c.selected = false;
            return true;
        }
        false
    }

    /// A click on the canvas at `p` (frame pixels) in the Tour view. Sets a picked spot, selects the
    /// card when the click is on it, or with the Guide card tool adds the step's card or selects it.
    /// True when the click was the card's.
    pub(crate) fn card_click(&mut self, p: Pt) -> bool {
        let Some((src, _, _, has_click)) = self.card_step() else { return false };
        let card_tool = self.tool_is_card();
        let Some(v) = self.video.as_mut() else { return false };
        let ui = &mut v.tour.card;
        let doc = &mut v.edit.tour_doc;
        if finish_pick(doc, ui, p) {
            return true;
        }
        let on_card = ui.drawn.is_some_and(|b| p.x >= b.x0 && p.x <= b.x1 && p.y >= b.y0 && p.y <= b.y1);
        if !on_card && !card_tool {
            (ui.selected, ui.editing) = (false, None);
            return false;
        }
        if add_card(doc, ui, src, has_click) {
            // The new card's title is ready for typing.
            (ui.editing, ui.focus) = (Some(Field::Title), true);
        }
        self.select_card(true);
        true
    }

    /// "Remove card": the card goes, and "Card removed · Undo" brings it back until another is removed or the step changes.
    pub(crate) fn remove_card(&mut self) {
        let Some(src) = self.tour_src() else { return };
        let Some(v) = self.video.as_mut() else { return };
        take_card(&mut v.edit.tour_doc, &mut v.tour.card, src);
    }

    /// "Card removed · Undo" in the status bar while the removed card's step is shown.
    pub(crate) fn card_undo_status(&mut self, ui: &mut egui::Ui) {
        let Some(src) = self.tour_src() else { return };
        let Some(v) = self.video.as_mut() else { return };
        if !can_undo_removal(&v.edit.tour_doc, &v.tour.card, src) {
            return;
        }
        ui.label(RichText::new("Card removed ·").color(theme::MUTED));
        if ui.link("Undo").on_hover_text("Bring the guide card back").clicked() {
            undo_removal(&mut v.edit.tour_doc, &mut v.tour.card, src);
        }
        ui.separator();
    }

    /// The properties row while the Guide card tool is active or the card is selected: "Points at" and "Remove card".
    pub(crate) fn card_properties(&mut self, ui: &mut egui::Ui) {
        let Some((_, _, _, has_click)) = self.card_step() else {
            ui.label(RichText::new("Show a step to add its guide card").color(theme::MUTED));
            return;
        };
        let picking = self.video.as_ref().is_some_and(|v| v.tour.card.picking.is_some());
        let Some(anchor) = self.shown_card().map(|c| shown_anchor(c, picking)) else {
            ui.label(RichText::new("Click the picture to add this step's guide card").color(theme::MUTED));
            return;
        };
        if !self.card_selected() {
            ui.label(RichText::new("Click the card to edit it").color(theme::MUTED));
            return;
        }
        ui.label("Points at");
        let mut pick = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let hot = ui.add_enabled(has_click, egui::Button::selectable(anchor == Anchor::Hotspot, "Hotspot"));
            if hot.on_hover_text("Sit beside the place this step was clicked").on_disabled_hover_text("This step has no click").clicked() {
                pick = Some(Anchor::Hotspot);
            }
            if ui.add(egui::Button::selectable(anchor == Anchor::Point, "A spot I pick")).on_hover_text("Click the picture to choose the spot").clicked() {
                pick = Some(Anchor::Point);
            }
            if ui.add(egui::Button::selectable(anchor == Anchor::None, "Nothing")).on_hover_text("Sit at the bottom of the picture").clicked() {
                pick = Some(Anchor::None);
            }
        });
        if picking {
            ui.label(RichText::new("Click the picture to set the spot").color(theme::ACCENT));
        }
        ui.separator();
        if ui.button("Remove card").on_hover_text("Take this step's guide card away; Undo in the status bar brings it back").clicked() {
            self.remove_card();
            return;
        }
        let Some(a) = pick else { return };
        let Some(src) = self.tour_src() else { return };
        let Some(v) = self.video.as_mut() else { return };
        let (doc, card_ui) = (&mut v.edit.tour_doc, &mut v.tour.card);
        if a == Anchor::Point {
            // An earlier spot is used straight away and the next click can move it; without one, the next click sets it.
            start_pick(doc, card_ui, src);
        } else {
            card_ui.picking = None;
            if let Some(c) = doc.steps.get_mut(&src).and_then(|s| s.card.as_mut()) {
                c.anchor = a;
            }
        }
    }

    /// The shown step's guide card over the canvas, in the tour's style, where the shared rule puts it.
    pub(crate) fn draw_guide_card(&mut self, ui: &egui::Ui, painter: &egui::Painter, z: f32, to_scr: &dyn Fn(Pt) -> Pos2) {
        let Some((src, index, count, _)) = self.card_step() else {
            if let Some(v) = self.video.as_mut() {
                v.tour.card.drawn = None;
            }
            return;
        };
        let spot = self.doc.items().iter().filter(|it| it.pin == Some(src as u32)).find_map(|it| match it.shape {
            Shape::Spotlight { rect, .. } => Some(rect.norm()),
            _ => None,
        });
        let Some(v) = self.video.as_mut() else { return };
        (v.tour.card.drawn, v.tour.card.drawn_screen) = (None, None);
        let Some(mut card) = v.edit.tour_doc.steps.get(&src).and_then(|s| s.card.clone()) else { return };
        let theme = v.edit.tour_doc.theme.clone();
        let accent = parse_hex(&theme.accent).unwrap_or(DEFAULT_GREEN);
        let (cx, cy, cw, ch) = crop_px(&v.rec, &v.edit);
        let (cx, cy, cw, ch) = (cx as f32, cy as f32, cw as f32, ch as f32);
        let inside = |p: &Pt| p.x >= cx && p.x < cx + cw && p.y >= cy && p.y < cy + ch;
        let anchor = match card.anchor {
            Anchor::Hotspot => click_point(v),
            Anchor::Point => card.point.as_ref().map(|p| pt(p.x, p.y)).filter(inside),
            Anchor::None => None,
        };
        // The spot picked for the card: a small accent dot. Editor chrome, never exported.
        if card.anchor == Anchor::Point
            && let Some(p) = anchor
        {
            painter.circle(to_scr(p), 5.0, accent, Stroke::new(1.5, theme::ON_PICTURE));
        }
        let compact = theme.card_style == CardStyle::Compact;
        let (bg, ink, muted) = card_colors(theme.card_style);
        let state = &mut v.tour.card;
        let h = if state.height > 0.0 { state.height } else if compact { 44.0 } else { 120.0 };
        let crop = Rect::from_min_max(to_scr(pt(cx, cy)), to_scr(pt(cx + cw, cy + ch)));
        let (pos, width) = if compact {
            (pos2(crop.left() + 8.0, crop.bottom() - h - 8.0), (crop.width() - 16.0).max(CARD_W))
        } else {
            let local = |b: Bounds| Bounds { x0: b.x0 - cx, y0: b.y0 - cy, x1: b.x1 - cx, y1: b.y1 - cy };
            let anchor_local = anchor.map(|a| pt(a.x - cx, a.y - cy));
            let picked = anchor_local.filter(|_| card.anchor == Anchor::Point);
            let placed = card_rect((cw, ch), (CARD_W / z, h / z), spot.map(local), anchor_local, picked, card.placement);
            if placed.side == Side::Docked { (pos2(crop.left(), crop.bottom() + 8.0), crop.width().max(CARD_W)) } else { (to_scr(pt(cx + placed.x, cy + placed.y)), CARD_W) }
        };
        let selected = state.selected;
        let instance = v.tour.instance;
        let mut fc = FieldCtx { instance, src, editing: if selected { state.editing } else { None }, focus: state.focus, clicked: None };
        let before = card.clone();
        let next_fill = accent;
        let next_ink = on_accent(accent);
        let clip = ui.clip_rect();
        let shown = egui::Area::new(egui::Id::unique(("tour-guide-card", instance))).fixed_pos(pos).order(egui::Order::Middle).constrain_to(clip).show(ui.ctx(), |ui| {
            let frame = egui::Frame::new()
                .fill(bg)
                // The compact bar is a pill: fully rounded ends, whatever its height.
                .corner_radius(CornerRadius::same(if compact { pill_radius(h) } else { CARD_RADIUS }))
                .inner_margin(if compact { Margin::symmetric(12, 8) } else { Margin::same(CARD_PAD as i8) })
                .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: theme::CARD_SHADOW });
            let inner = width - if compact { 24.0 } else { 2.0 * CARD_PAD };
            let r = frame.show(ui, |ui| {
                ui.set_width(inner);
                ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
                let counter = format!("{} of {count}", index + 1);
                let next = |ui: &mut egui::Ui, fc: &mut FieldCtx, label: &mut String| {
                    egui::Frame::new().fill(next_fill).corner_radius(CornerRadius::same(if compact { 12 } else { 6 })).inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
                        card_field(ui, fc, Field::Next, label, NEXT_LABEL_MAX, FontId::proportional(13.0), next_ink, ("Next", next_ink), 90.0, Lines::Wrap, true);
                    });
                };
                if compact {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&counter).color(muted).size(12.0));
                        let body_w = (ui.available_width() - 110.0).max(60.0);
                        card_field(ui, &mut fc, Field::Body, &mut card.body, CARD_BODY_MAX, FontId::proportional(13.0), ink, ("Say what to do here", muted), body_w, Lines::One, false);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| next(ui, &mut fc, &mut card.next_label));
                    });
                } else {
                    card_field(ui, &mut fc, Field::Title, &mut card.title, CARD_TITLE_MAX, FontId::proportional(15.0), ink, ("Add a title", muted), inner, Lines::Wrap, true);
                    card_field(ui, &mut fc, Field::Body, &mut card.body, CARD_BODY_MAX, FontId::proportional(13.0), ink, ("Say what to do here", muted), inner, Lines::Paragraph, false);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        // Back and the counter are filled in by the player, never typed.
                        ui.label(RichText::new(format!("‹  {counter}")).color(muted).size(12.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| next(ui, &mut fc, &mut card.next_label));
                    });
                }
            });
            if selected {
                let r = r.response.rect.expand(4.0);
                let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
                theme::selection_dashes(&ui.painter().with_clip_rect(clip), &pts, 1.5);
            }
        });
        let rect = shown.response.rect;
        if (rect.height() - h).abs() > 0.5 {
            state.height = rect.height();
            ui.ctx().request_repaint();
        }
        let origin = to_scr(pt(0.0, 0.0));
        state.drawn = Some(Bounds { x0: (rect.left() - origin.x) / z, y0: (rect.top() - origin.y) / z, x1: (rect.right() - origin.x) / z, y1: (rect.bottom() - origin.y) / z });
        state.drawn_screen = Some(rect);
        state.focus = fc.focus;
        if selected {
            state.editing = fc.editing;
        }
        let clicked = fc.clicked;
        if card != before
            && let Some(c) = v.edit.tour_doc.steps.get_mut(&src).and_then(|s| s.card.as_mut())
        {
            *c = card;
        }
        if let Some(f) = clicked {
            // A click on a text selects the card and types into that text. The properties row above
            // was drawn before this click was seen, so draw again now that it shows the card's row.
            self.select_card(true);
            if let Some(v) = self.video.as_mut() {
                (v.tour.card.editing, v.tour.card.focus) = (Some(f), true);
            }
            ui.ctx().request_repaint();
        }
    }

    /// The Properties panel, docked at the right of the canvas: first the shown step's settings
    /// ("This step"), then the tour's ("Tour"), then the editor's own ("In this editor").
    pub(crate) fn tour_settings_panel(&mut self, ui: &mut egui::Ui) {
        let step = self.card_step();
        let placing = self.placing_pulse();
        let dim = self.prefs.spotlight_dim.clamp(MIN_DIM, MAX_DIM);
        let Some(v) = self.video.as_mut() else { return };
        // A logo being checked lands even while the panel is closed.
        let s = &mut v.tour.settings;
        let mut loading = false;
        if let Some(rx) = &s.logo_job {
            match rx.try_recv() {
                Ok(Ok(path)) => {
                    v.edit.tour_doc.logo_path = Some(path);
                    (s.logo_error, s.logo_job) = (None, None);
                }
                Ok(Err(e)) => (s.logo_error, s.logo_job) = (Some(e), None),
                Err(TryRecvError::Disconnected) => (s.logo_error, s.logo_job) = (Some("Could not check the cover logo.".into()), None),
                Err(TryRecvError::Empty) => {
                    loading = true;
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        if !v.tour.settings.open {
            // The "Pulse" row is gone with the panel: the picture stops taking the pulse.
            v.tour.pulse.placing = None;
            return;
        }
        let theme_before = saved_theme(&v.edit.tour_doc);
        let animate_before = self.prefs.tour_animate_hotspot;
        let mut animate = animate_before;
        let instance = v.tour.instance;
        let mut new_dim: Option<(u8, bool)> = None;
        let mut pulse: Option<(usize, PulseAction)> = None;
        let mut close = false;
        let frame = egui::Frame::new().fill(theme::SURFACE).inner_margin(Margin::symmetric(12, 10));
        egui::Panel::right(("tour-settings", instance)).resizable(false).exact_size(304.0).frame(frame).show(ui, |ui| {
            let (doc, s) = (&mut v.edit.tour_doc, &mut v.tour.settings);
            if !s.loaded {
                s.loaded = true;
                s.accent = doc.theme.accent.clone();
                s.cta_url = doc.cta.as_ref().map(|l| l.url.clone()).unwrap_or_default();
                s.primary_url = doc.end.as_ref().and_then(|e| e.primary.as_ref()).map(|l| l.url.clone()).unwrap_or_default();
                s.secondary_url = doc.end.as_ref().and_then(|e| e.secondary.as_ref()).map(|l| l.url.clone()).unwrap_or_default();
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(PROPERTIES).bold().size(theme::STRONG_SIZE).color(theme::TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Close, 22.0, false, CLOSE_PROPERTIES).clicked() {
                        close = true;
                    }
                });
            });
            egui::ScrollArea::vertical().id_salt("tour-settings-scroll").auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;

                section(ui, THIS_STEP);
                match step {
                    None => {
                        ui.label(RichText::new("Show a step to set these").color(theme::MUTED));
                    }
                    Some((src, i, _, has_click)) => {
                        ui.label(RichText::new(format!("Step {}", i + 1)).color(theme::MUTED).size(12.0));
                        let x = doc.steps.entry(src).or_default();
                        pulse = pulse_row(ui, has_click, x.pulse.is_some(), placing == Some(src)).map(|a| (src, a));
                        ui.label("Zoom to the spotlight");
                        ui.horizontal(|ui| {
                            for (z, label) in [(None, "Follow the tour"), (Some(true), "On"), (Some(false), "Off")] {
                                if ui.add(egui::Button::selectable(x.zoom == z, label)).clicked() {
                                    x.zoom = z;
                                }
                            }
                        });
                        let mut anywhere = x.advance == Advance::Anywhere;
                        if ui.checkbox(&mut anywhere, "Continue on a click anywhere").on_hover_text("A click anywhere on the picture moves on, not only on the hotspot").changed() {
                            x.advance = if anywhere { Advance::Anywhere } else { Advance::Hotspot };
                        }
                    }
                }

                section(ui, TOUR);
                ui.label(RichText::new("How the tour looks and plays. None of it is drawn into the step images.").color(theme::MUTED).size(12.0));
                let t = &mut doc.theme;

                heading(ui, "Card style");
                ui.horizontal_wrapped(|ui| {
                    for (style, label, tip) in [
                        (CardStyle::Dark, "Dark", "A dark card beside the spot"),
                        (CardStyle::Light, "Light", "A light card beside the spot"),
                        (CardStyle::Compact, "Compact bar", "A light pill along the bottom of the picture: the count, the text and the button on one line"),
                        (CardStyle::Auto, "Match viewer", "Dark or light, following the viewer's system setting"),
                    ] {
                        if ui.add(egui::Button::selectable(t.card_style == style, label)).on_hover_text(tip).clicked() {
                            t.card_style = style;
                        }
                    }
                });

                heading(ui, "Accent colour");
                ui.horizontal(|ui| {
                    for (name, hex) in ACCENTS {
                        let c = parse_hex(hex).unwrap_or(DEFAULT_GREEN);
                        let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                        ui.painter().circle_filled(r.center(), 9.0, c);
                        if t.accent.eq_ignore_ascii_case(hex) {
                            ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.5, theme::TEXT));
                        } else if resp.hovered() {
                            ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.0, theme::MUTED));
                        }
                        // Reached with Tab: the focus ring, outside the others.
                        if resp.has_focus() {
                            ui.painter().circle_stroke(r.center(), 12.5, Stroke::new(theme::FOCUS_WIDTH, theme::FOCUS));
                        }
                        if resp.on_hover_text(name).clicked() {
                            t.accent = hex.to_string();
                            s.accent = hex.to_string();
                        }
                    }
                    ui.add_space(4.0);
                    let r = ui.add(egui::TextEdit::singleline(&mut s.accent).char_limit(7).desired_width(70.0).hint_text("#rrggbb"));
                    if r.on_hover_text("Any colour as #rrggbb").changed()
                        && let Some(c) = parse_hex(&s.accent)
                    {
                        t.accent = hex_of(c);
                    }
                });
                if parse_hex(&s.accent).is_none() {
                    warning(ui, HEX_WARNING);
                }

                heading(ui, "Zoom");
                ui.checkbox(&mut doc.zoom.enabled, "Zoom toward each step's spot");
                ui.add_enabled_ui(doc.zoom.enabled, |ui| {
                    ui.horizontal(|ui| {
                        grid_label(ui, "Amount");
                        ui.add(egui::Slider::new(&mut doc.zoom.percent, ZOOM_MIN..=ZOOM_MAX).suffix("%"));
                    });
                });

                let t = &mut doc.theme;
                heading(ui, "Spotlight");
                ui.checkbox(&mut t.spotlight_soft, "Soft edge").on_hover_text("Round the bright box's corners and feather its edge");
                ui.horizontal(|ui| {
                    grid_label(ui, "Darken");
                    let mut d = dim;
                    let r = ui.add(egui::Slider::new(&mut d, MIN_DIM..=MAX_DIM).suffix("%")).on_hover_text("How dark everything outside a spotlight gets. One strength for every spotlight.");
                    if r.changed() {
                        // One undo step per drag.
                        new_dim = Some((d, r.drag_started() || !r.dragged()));
                    }
                });

                heading(ui, "Hotspot");
                ui.horizontal(|ui| {
                    for (h, label, tip) in [(HotspotStyle::Pulse, "Pulse", "A soft pulsing ring where to click"), (HotspotStyle::Dot, "Dot", "A still dot where to click"), (HotspotStyle::None, "None", "No mark; the card does the pointing")] {
                        if ui.add(egui::Button::selectable(t.hotspot == h, label)).on_hover_text(tip).clicked() {
                            t.hotspot = h;
                        }
                    }
                });

                heading(ui, "Backdrop");
                ui.horizontal(|ui| {
                    let auto = t.backdrop == "auto";
                    if ui.add(egui::Button::selectable(auto, "Auto")).on_hover_text("A calm tint from the accent colour").clicked() {
                        t.backdrop = "auto".into();
                    }
                    if ui.add(egui::Button::selectable(!auto, "Pick a colour")).clicked() && auto {
                        t.backdrop = BACKDROP_START.into();
                    }
                    if let Some(mut c) = (!auto).then(|| parse_hex(&t.backdrop)).flatten()
                        && egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).on_hover_text("Backdrop colour").changed()
                    {
                        t.backdrop = hex_of(c);
                    }
                });

                heading(ui, "Progress bar");
                ui.checkbox(&mut t.progress_bar, "Show a thin progress bar in the accent colour");

                heading(ui, "Cover card");
                let mut cover_on = doc.cover.is_some();
                if ui.checkbox(&mut cover_on, "Open the tour on a cover card").changed() {
                    if cover_on {
                        doc.cover = Some(s.stash_cover.take().unwrap_or(Cover { title: String::new(), body: String::new(), button: String::new(), logo: None }));
                    } else {
                        s.stash_cover = doc.cover.take();
                    }
                }
                if let Some(c) = doc.cover.as_mut() {
                    egui::Grid::new("tour-cover").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                        grid_label(ui, "Title");
                        line_field(ui, &mut c.title, COVER_TITLE_MAX, "Needed for the cover to show");
                        ui.end_row();
                        grid_label(ui, "Body");
                        text_field(ui, &mut c.body, COVER_BODY_MAX, "A line about what the tour shows");
                        ui.end_row();
                        grid_label(ui, "Button");
                        line_field(ui, &mut c.button, COVER_BUTTON_MAX, "Start");
                        ui.end_row();
                        grid_label(ui, "Logo");
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!loading, egui::Button::new("Add logo…")).on_hover_text("A PNG or JPEG; it is shrunk to 512 pixels").clicked()
                                    && let Some(path) = rfd::FileDialog::new().add_filter("PNG or JPEG", &["png", "jpg", "jpeg"]).pick_file()
                                {
                                    // Checked first, so a file that is not a usable logo never reaches the tour.
                                    s.logo_job = Some(start_logo_check(path));
                                    s.logo_error = None;
                                }
                                if doc.logo_path.is_some() && !loading && ui.button("Remove logo").clicked() {
                                    doc.logo_path = None;
                                    s.logo_error = None;
                                }
                            });
                            if loading {
                                ui.label(RichText::new("Loading logo…").color(theme::MUTED).size(12.0));
                            }
                            if let Some(name) = doc.logo_path.as_ref().and_then(|p| p.file_name()) {
                                ui.label(RichText::new(name.to_string_lossy()).color(theme::MUTED).size(12.0));
                            }
                            if let Some(e) = &s.logo_error {
                                theme::caution_line(ui, e, theme::SMALL_SIZE, theme::Fit::Wrap);
                            }
                        });
                        ui.end_row();
                    });
                }

                heading(ui, "Always-on button");
                let mut cta_on = doc.cta.is_some();
                if ui.checkbox(&mut cta_on, "Show a button on every step").changed() {
                    if cta_on {
                        doc.cta = Some(s.stash_cta.take().unwrap_or(Link { label: String::new(), url: link_from_draft(&s.cta_url) }));
                    } else {
                        s.stash_cta = doc.cta.take();
                    }
                }
                if let Some(l) = doc.cta.as_mut() {
                    egui::Grid::new("tour-cta").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| link_rows(ui, "Label", l, &mut s.cta_url, "Book a demo"));
                }

                heading(ui, "End card");
                let mut end_on = doc.end.is_some();
                if ui.checkbox(&mut end_on, "Finish on an end card").changed() {
                    if end_on {
                        doc.end = Some(s.stash_end.take().unwrap_or(EndCard { text: String::new(), primary: None, secondary: None, replay: true }));
                    } else {
                        s.stash_end = doc.end.take();
                    }
                }
                if let Some(e) = doc.end.as_mut() {
                    text_field(ui, &mut e.text, END_TEXT_MAX, "What to do next");
                    let primary = e.primary.get_or_insert_with(|| Link { label: String::new(), url: String::new() });
                    egui::Grid::new("tour-end-1").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| link_rows(ui, "Main button", primary, &mut s.primary_url, "Get started"));
                    let secondary = e.secondary.get_or_insert_with(|| Link { label: String::new(), url: String::new() });
                    egui::Grid::new("tour-end-2").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| link_rows(ui, "Second button", secondary, &mut s.secondary_url, "Learn more"));
                    ui.checkbox(&mut e.replay, "Show Replay");
                }

                heading(ui, "Autoplay");
                ui.checkbox(&mut doc.autoplay.enabled, "Move on by itself");
                ui.add_enabled_ui(doc.autoplay.enabled, |ui| {
                    ui.horizontal(|ui| {
                        grid_label(ui, "Seconds per step");
                        ui.add(egui::Slider::new(&mut doc.autoplay.seconds, AUTOPLAY_MIN..=AUTOPLAY_MAX).suffix(" s"));
                    });
                });

                heading(ui, "In this editor");
                ui.checkbox(&mut animate, "Animate hotspot").on_hover_text("Pulse the hotspot on the picture here, as the tour will. Turn off for a still ring.");
                ui.add_space(12.0);
            });
        });
        // The theme and zoom become the next recording's, but only when this panel changed them:
        // another open recording's theme is never overwritten by this one's.
        if let Some(saved) = changed_theme(&theme_before, saved_theme(&v.edit.tour_doc)) {
            self.prefs.tour_theme = saved;
            self.prefs_changed = true;
        }
        if let Some((d, checkpoint)) = new_dim {
            self.apply_spotlight_dim(d, checkpoint);
        }
        if let Some((src, action)) = pulse {
            self.pulse_action(src, action);
        }
        if animate != animate_before {
            self.prefs.tour_animate_hotspot = animate;
            self.prefs_changed = true;
        }
        // Closed with its own button: as with the toolbar's button, it stays closed next time.
        if close {
            self.toggle_properties();
        }
    }

    /// Debug test hook: guide cards on the first two steps, a cover card and an always-on button.
    #[cfg(debug_assertions)]
    pub(crate) fn test_guide_cards(&mut self) {
        let Some(v) = self.video.as_mut() else { return };
        let Ok(steps) = planned_steps(v) else { return };
        let texts = [("Start here", "Open the **Invoices** page."), ("Then save", "Click Save to keep your changes.")];
        for (s, (title, body)) in steps.iter().zip(texts) {
            let mut c = new_card(s.click.is_some());
            (c.title, c.body) = (title.into(), body.into());
            v.edit.tour_doc.steps.entry(s.src).or_default().card = Some(c);
        }
        let doc = &mut v.edit.tour_doc;
        doc.cover = Some(Cover { title: "A quick tour".into(), body: "Two minutes, a few clicks.".into(), button: String::new(), logo: None });
        doc.cta = Some(Link { label: "Book a demo".into(), url: "https://tours.example.com/book".into() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tour::{Anchor, Placement, Side};
    use eframe::egui::Color32;
    use mythicshot_editor::{Bounds, pt};

    #[test]
    fn a_new_card_points_at_the_hotspot_only_when_there_is_one() {
        assert_eq!(new_card(true).anchor, Anchor::Hotspot);
        assert_eq!(new_card(false).anchor, Anchor::None);
        assert!(new_card(true).title.is_empty() && new_card(true).next_label.is_empty());
    }

    #[test]
    fn the_card_sits_where_the_shared_rule_puts_it() {
        let spot = Bounds { x0: 300.0, y0: 100.0, x1: 500.0, y1: 160.0 };
        let p = card_rect((800.0, 500.0), (260.0, 120.0), Some(spot), None, None, Placement::Auto);
        assert_eq!((p.x, p.y, p.side), (270.0, 172.0, Side::Bottom));
        // No spotlight: a 44-pixel box around the anchor.
        let p = card_rect((800.0, 500.0), (260.0, 120.0), None, Some(pt(400.0, 100.0)), None, Placement::Auto);
        assert_eq!((p.x, p.y, p.side), (270.0, 134.0, Side::Bottom));
        // A picked spot with no spotlight places the card exactly as before.
        let p = card_rect((800.0, 500.0), (260.0, 120.0), None, Some(pt(400.0, 100.0)), Some(pt(400.0, 100.0)), Placement::Auto);
        assert_eq!((p.x, p.y, p.side), (270.0, 134.0, Side::Bottom));
        // Neither: bottom centre.
        let p = card_rect((800.0, 500.0), (260.0, 120.0), None, None, None, Placement::Auto);
        assert_eq!((p.x, p.y, p.side), (270.0, 364.0, Side::None));
    }

    /// Fixture-style cases for a picked spot on a step with a spotlight: the card points at the spot and
    /// still keeps clear of the spotlight, as `tour_core.js` places it (the "aim" cases in the fixture).
    #[test]
    fn a_picked_spot_moves_the_card_even_with_a_spotlight() {
        let (frame, card) = ((800.0, 500.0), (260.0, 120.0));
        let clear = |p: Placed, b: Bounds| p.x + card.0 <= b.x0 || p.x >= b.x1 || p.y + card.1 <= b.y0 || p.y >= b.y1;
        // Inside the spotlight: the card slides along the bottom to sit under the spot.
        let spot = Bounds { x0: 300.0, y0: 100.0, x1: 500.0, y1: 160.0 };
        let at = pt(320.0, 130.0);
        let p = card_rect(frame, card, Some(spot), Some(at), Some(at), Placement::Auto);
        assert_eq!((p.x, p.y, p.side), (190.0, 172.0, Side::Bottom));
        assert!(clear(p, spot));
        // The hotspot (not a picked spot) keeps the card centred on the spotlight, as before.
        let p = card_rect(frame, card, Some(spot), Some(at), None, Placement::Auto);
        assert_eq!((p.x, p.y), (270.0, 172.0));
        // Above the spotlight: the card goes above the spot.
        let spot = Bounds { x0: 300.0, y0: 300.0, x1: 500.0, y1: 400.0 };
        let at = pt(450.0, 200.0);
        let p = card_rect(frame, card, Some(spot), Some(at), Some(at), Placement::Auto);
        assert_eq!((p.x, p.y, p.side), (320.0, 68.0, Side::Top));
        assert!(clear(p, spot) && p.y + card.1 < at.y, "clear of the spotlight and of the spot");
        // Right of the spotlight: the card goes right of the spot.
        let spot = Bounds { x0: 100.0, y0: 150.0, x1: 300.0, y1: 350.0 };
        let at = pt(450.0, 250.0);
        let p = card_rect(frame, card, Some(spot), Some(at), Some(at), Placement::Auto);
        assert_eq!((p.x, p.y, p.side), (462.0, 190.0, Side::Right));
        assert!(clear(p, spot));
    }

    #[test]
    fn a_cancelled_pick_restores_what_the_card_pointed_at() {
        let (mut doc, mut ui) = (TourDoc::default(), CardUi::default());
        assert!(add_card(&mut doc, &mut ui, 3, true));
        let anchor = |doc: &TourDoc| doc.steps[&3].card.as_ref().unwrap().anchor;
        // No spot yet: picking shows "A spot I pick" but the card still points at the hotspot.
        start_pick(&mut doc, &mut ui, 3);
        assert_eq!(anchor(&doc), Anchor::Hotspot);
        assert_eq!(shown_anchor(doc.steps[&3].card.as_ref().unwrap(), ui.picking.is_some()), Anchor::Point);
        cancel_pick(&mut doc, &mut ui);
        assert!(ui.picking.is_none());
        assert_eq!(anchor(&doc), Anchor::Hotspot);
        assert_eq!(shown_anchor(doc.steps[&3].card.as_ref().unwrap(), false), Anchor::Hotspot, "the row goes back too");
        // A click on the picture ends the pick with a spot.
        start_pick(&mut doc, &mut ui, 3);
        assert!(finish_pick(&mut doc, &mut ui, pt(10.0, 20.0)));
        let c = doc.steps[&3].card.clone().unwrap();
        assert_eq!((c.anchor, c.point), (Anchor::Point, Some(Point { x: 10.0, y: 20.0 })));
        assert!(!finish_pick(&mut doc, &mut ui, pt(1.0, 1.0)), "only one click per pick");
        // With an earlier spot, choosing Nothing and then picking again uses the old spot at once;
        // cancelling goes back to Nothing, not to the spot.
        doc.steps.get_mut(&3).unwrap().card.as_mut().unwrap().anchor = Anchor::None;
        start_pick(&mut doc, &mut ui, 3);
        assert_eq!(anchor(&doc), Anchor::Point);
        start_pick(&mut doc, &mut ui, 3); // a second press keeps the first "before"
        cancel_pick(&mut doc, &mut ui);
        assert_eq!(anchor(&doc), Anchor::None);
        // "A spot I pick" is never left on without a spot, even from a hand-made state.
        let c = doc.steps.get_mut(&3).unwrap().card.as_mut().unwrap();
        (c.anchor, c.point) = (Anchor::Point, None);
        assert_eq!(shown_anchor(c, false), Anchor::None);
        ui.picking = Some((3, Anchor::Point));
        cancel_pick(&mut doc, &mut ui);
        assert_eq!(anchor(&doc), Anchor::None);
    }

    #[test]
    fn the_compact_bar_text_is_shown_on_one_line_with_an_ellipsis() {
        let pieces = vec![("Open the\ninvoices".to_string(), false), (" page".to_string(), true)];
        let one = text_job(&pieces, &FontId::proportional(13.0), Color32::WHITE, 120.0, false, Lines::One);
        assert_eq!((one.wrap.max_rows, one.wrap.overflow_character, one.wrap.break_anywhere), (1, Some('…'), true));
        assert!(!one.text.contains('\n'), "line breaks become spaces on one line");
        for lines in [Lines::Wrap, Lines::Paragraph] {
            let job = text_job(&pieces, &FontId::proportional(13.0), Color32::WHITE, 120.0, false, lines);
            assert_eq!(job.wrap.max_rows, usize::MAX, "{lines:?} still wraps");
        }
    }

    #[test]
    fn a_press_off_the_card_ends_typing_on_it() {
        assert!(ends_typing(Some(pos2(5.0, 5.0)), false), "a press on the toolbar");
        assert!(!ends_typing(Some(pos2(5.0, 5.0)), true), "a press on the card keeps typing");
        assert!(!ends_typing(None, false), "no press, no change");
    }

    #[test]
    fn the_compact_bar_is_a_light_pill() {
        assert_eq!(card_colors(CardStyle::Compact), card_colors(CardStyle::Light));
        assert_ne!(card_colors(CardStyle::Compact), card_colors(CardStyle::Dark));
        assert_eq!(card_colors(CardStyle::Auto), card_colors(CardStyle::Dark), "Match viewer is drawn dark in the editor, as before");
        assert_eq!(pill_radius(44.0), 22);
        assert_eq!(pill_radius(1000.0), 255);
    }

    #[test]
    fn text_on_the_accent_stays_readable() {
        assert_eq!(on_accent(Color32::from_rgb(0x5d, 0xca, 0xa5)), Color32::from_rgb(0x10, 0x14, 0x1a));
        assert_eq!(on_accent(Color32::from_rgb(0x1b, 0x3a, 0x6b)), Color32::from_rgb(0xf4, 0xf6, 0xf9));
    }

    #[test]
    fn typing_stops_at_the_cap_on_a_character_boundary() {
        assert_eq!(capped("héllo wörld", 5), "héllo");
        assert_eq!(capped("abc", 80), "abc");
    }

    #[test]
    fn the_remembered_theme_starts_the_next_tour_and_bad_values_fall_back() {
        let mut doc = TourDoc::default();
        doc.theme.card_style = CardStyle::Compact;
        doc.theme.accent = "#6aa8f0".into();
        doc.zoom.percent = 130;
        let saved = saved_theme(&doc);
        let mut next = TourDoc::default();
        apply_saved_theme(&mut next, &saved);
        assert_eq!((next.theme.card_style, next.theme.accent.as_str(), next.zoom.percent), (CardStyle::Compact, "#6aa8f0", 130));
        assert!(next.cover.is_none() && next.cta.is_none() && next.end.is_none() && !next.autoplay.enabled);
        let bad = SavedTheme { card_style: "neon".into(), accent: "red".into(), backdrop: "url(x)".into(), zoom_percent: 900, ..SavedTheme::default() };
        apply_saved_theme(&mut next, &bad);
        assert_eq!((next.theme.card_style, next.theme.accent.as_str(), next.theme.backdrop.as_str(), next.zoom.percent), (CardStyle::Dark, DEFAULT_ACCENT, "auto", 140));
        assert_eq!(saved_theme(&TourDoc::default()), SavedTheme::default());
    }

    #[test]
    fn bad_links_and_colours_never_reach_the_tour() {
        assert_eq!(link_from_draft("http://tours.example.com"), "");
        assert_eq!(link_from_draft("javascript:alert(1)"), "");
        assert_eq!(link_from_draft(" https://tours.example.com/book "), "https://tours.example.com/book");
        assert_eq!(link_warning("http://tours.example.com"), Some(LINK_WARNING));
        assert_eq!(link_warning("https://tours.example.com"), None);
        assert_eq!(link_warning(""), None, "an empty field is not an error");
        assert_eq!(parse_hex("#5DCAA5").map(hex_of).as_deref(), Some("#5dcaa5"));
        for bad in ["red", "#5dcaa", "#5dcaa5;x", "5dcaa5"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
        // Every preset is a valid colour and none of them is red.
        for (_, hex) in ACCENTS {
            let c = parse_hex(hex).unwrap();
            assert!(!(c.r() > 180 && c.g() < 110 && c.b() < 110), "{hex} is red");
        }
        assert_eq!(ACCENTS[0].1, DEFAULT_ACCENT);
    }

    #[test]
    fn only_a_card_with_text_counts() {
        let mut c = new_card(true);
        assert!(!card_has_text(&c));
        c.title = "  ".into();
        assert!(!card_has_text(&c));
        c.body = "Click Save".into();
        assert!(card_has_text(&c));
    }

    /// Every step's text boxes, in every editor window, have their own typing and undo history.
    #[test]
    fn each_text_box_has_its_own_id_per_window_step_and_field() {
        let id = field_id(1, 3, Field::Title);
        assert_eq!(id, field_id(1, 3, Field::Title), "the same box keeps its id from frame to frame");
        for other in [field_id(1, 4, Field::Title), field_id(1, 3, Field::Body), field_id(1, 3, Field::Next), field_id(2, 3, Field::Title)] {
            assert_ne!(id, other);
        }
    }

    #[test]
    fn undo_never_replaces_a_card_added_after_the_removal() {
        let (mut doc, mut ui) = (TourDoc::default(), CardUi::default());
        assert!(add_card(&mut doc, &mut ui, 3, true));
        doc.steps.get_mut(&3).unwrap().card.as_mut().unwrap().title = "Old".into();
        assert!(take_card(&mut doc, &mut ui, 3));
        assert!(can_undo_removal(&doc, &ui, 3) && !can_undo_removal(&doc, &ui, 4), "offered on its own step only");
        // A new card on that step: Undo is no longer offered and does nothing.
        assert!(add_card(&mut doc, &mut ui, 3, true));
        assert!(ui.removed.is_none(), "the removed card is forgotten once the step has a new one");
        doc.steps.get_mut(&3).unwrap().card.as_mut().unwrap().title = "New".into();
        assert!(!can_undo_removal(&doc, &ui, 3));
        assert!(!undo_removal(&mut doc, &mut ui, 3));
        assert_eq!(doc.steps[&3].card.as_ref().unwrap().title, "New");
        // Without a new card, Undo brings the old one back once.
        assert!(take_card(&mut doc, &mut ui, 3));
        assert!(undo_removal(&mut doc, &mut ui, 3));
        assert_eq!(doc.steps[&3].card.as_ref().unwrap().title, "New");
        assert!(!can_undo_removal(&doc, &ui, 3));
        // A card added on another step leaves the remembered one alone.
        assert!(take_card(&mut doc, &mut ui, 3));
        assert!(add_card(&mut doc, &mut ui, 5, false));
        assert!(can_undo_removal(&doc, &ui, 3));
    }

    #[test]
    fn the_theme_is_remembered_only_when_the_panel_changed_it() {
        let before = SavedTheme::default();
        assert_eq!(changed_theme(&before, SavedTheme::default()), None, "an open panel that changed nothing writes nothing");
        let after = SavedTheme { accent: "#6aa8f0".into(), ..SavedTheme::default() };
        assert_eq!(changed_theme(&before, after.clone()), Some(after));
    }

    #[test]
    fn a_logo_is_checked_off_the_ui_thread() {
        let tmp = tempfile::tempdir().unwrap();
        let html = tmp.path().join("logo.png");
        std::fs::write(&html, b"<html>").unwrap();
        assert!(start_logo_check(html).recv().unwrap().unwrap_err().contains("PNG or JPEG"));
        let good = tmp.path().join("good.png");
        image::RgbaImage::from_pixel(40, 20, image::Rgba([10, 20, 30, 255])).save(&good).unwrap();
        assert_eq!(start_logo_check(good.clone()).recv().unwrap(), Ok(good));
    }
}
