//! The app's look, in one place: the brand's colour roles, type, shape and space, applied once to
//! egui at start-up ([`apply`]). Every colour the app paints comes from here, except what belongs
//! to the user or to a tour's author (annotation colours, a tour's accent and card styles), which
//! the guard test at the bottom allows by name.
//!
//! The app is dark on purpose: [`apply`] fixes egui to its dark theme instead of following
//! Windows, and every role is the brand's DARK token value (named beside each). The roles are
//! fields of a [`Palette`], so a light app theme can be added later as a second palette fed
//! through [`visuals`]; only [`DARK`] ships, and the role constants below read from it.
//!
//! Windows can be see-through (the clear colour is transparent, for the live draw window), so
//! every window paints an opaque [`GROUND`] or [`SURFACE`], dialogs and menus an opaque
//! [`RAISED`] with a 1px [`BORDER_STRONG`] edge, and nothing here has a shadow.

// The module lists every token of the brand (the whole spacing scale, every role), not only the
// ones some screen draws today.
#![allow(dead_code)]

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Shadow, Stroke, Style, TextStyle, Visuals};

/// A colour from its `0xRRGGBB` value, as the brand's tokens write it.
const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Every colour role of the interface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Window backgrounds.
    pub ground: Color32,
    /// Cards, panels, bars and fields.
    pub surface: Color32,
    /// Dialogs, menus, popups and toasts.
    pub raised: Color32,
    /// Hairlines that only separate.
    pub border: Color32,
    /// The edge of a field, a secondary button, a dialog.
    pub border_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    /// Text and icons of a control that cannot be used now, where the app draws the disabled
    /// state itself, opaque. Never inside a disabled Ui: egui fades that already (see
    /// [`DISABLED_ALPHA`]), so there the text colour is used and egui's fade makes it disabled.
    pub disabled: Color32,
    /// The one main button of a view, selected tabs and toggles.
    pub primary: Color32,
    pub primary_hover: Color32,
    /// Text and icons on primary, primary hover and danger fill.
    pub on_primary: Color32,
    /// Purple as text: quiet buttons, selected labels.
    pub primary_text: Color32,
    /// Decorative gold: small marks and rules, never a filled box.
    pub gold: Color32,
    /// Gold as text: links and small highlights.
    pub accent: Color32,
    /// The keyboard focus ring.
    pub focus: Color32,
    /// Behind selected rows and selected text.
    pub selection: Color32,
    pub success_tint: Color32,
    pub success_text: Color32,
    pub success_border: Color32,
    pub caution_tint: Color32,
    pub caution_text: Color32,
    pub caution_border: Color32,
    pub danger_tint: Color32,
    pub danger_text: Color32,
    pub danger_border: Color32,
    /// The confirming button of a delete.
    pub danger_fill: Color32,
}

/// The brand's dark theme: the app's only palette.
pub const DARK: Palette = Palette {
    ground: hex(0x16121c),         // ground
    surface: hex(0x1e1928),        // surface
    raised: hex(0x241e30),         // surface-raised
    border: hex(0x3a3250),         // border
    border_strong: hex(0x7c7399),  // border-strong
    text: hex(0xede8f5),           // text
    muted: hex(0xb2a8c6),          // text-muted
    disabled: hex(0x948bb0),       // no token: between border-strong and text-muted, at least 4.5:1 on every ground
    primary: hex(0x7352ea),        // primary
    primary_hover: hex(0x7a59ee),  // primary-hover
    on_primary: hex(0xffffff),     // on-primary
    primary_text: hex(0xb7a3ff),   // primary-text
    gold: hex(0xf5b841),           // gold
    accent: hex(0xf5b841),         // accent-text
    focus: hex(0xf5b841),          // focus
    selection: hex(0x3a2e66),      // selection
    success_tint: hex(0x12302d),   // success-tint
    success_text: hex(0x7fdccf),   // success-text
    success_border: hex(0x2ba596), // success-border
    caution_tint: hex(0x2e1a0c),   // caution-tint
    caution_text: hex(0xffb380),   // caution-text
    caution_border: hex(0xc8590c), // caution-border
    danger_tint: hex(0x3a1719),    // danger-tint
    danger_text: hex(0xff9c9c),    // danger-text
    danger_border: hex(0xe05252),  // danger-border
    danger_fill: hex(0xb42323),    // danger-fill
};

pub const GROUND: Color32 = DARK.ground;
pub const SURFACE: Color32 = DARK.surface;
pub const RAISED: Color32 = DARK.raised;
pub const BORDER: Color32 = DARK.border;
pub const BORDER_STRONG: Color32 = DARK.border_strong;
pub const TEXT: Color32 = DARK.text;
pub const MUTED: Color32 = DARK.muted;
pub const DISABLED: Color32 = DARK.disabled;
pub const PRIMARY: Color32 = DARK.primary;
pub const PRIMARY_HOVER: Color32 = DARK.primary_hover;
pub const ON_PRIMARY: Color32 = DARK.on_primary;
pub const PRIMARY_TEXT: Color32 = DARK.primary_text;
pub const GOLD: Color32 = DARK.gold;
pub const ACCENT: Color32 = DARK.accent;
pub const FOCUS: Color32 = DARK.focus;
pub const SELECTION: Color32 = DARK.selection;
pub const SUCCESS_TINT: Color32 = DARK.success_tint;
pub const SUCCESS_TEXT: Color32 = DARK.success_text;
pub const SUCCESS_BORDER: Color32 = DARK.success_border;
pub const CAUTION_TINT: Color32 = DARK.caution_tint;
pub const CAUTION_TEXT: Color32 = DARK.caution_text;
pub const CAUTION_BORDER: Color32 = DARK.caution_border;
pub const DANGER_TINT: Color32 = DARK.danger_tint;
pub const DANGER_TEXT: Color32 = DARK.danger_text;
pub const DANGER_BORDER: Color32 = DARK.danger_border;
pub const DANGER_FILL: Color32 = DARK.danger_fill;

// Functional colours that are not part of the interface's palette: they sit over the user's own
// pictures (a screenshot under the region selector, an image in the editor), where only a plain
// darkening or a plain white edge reads on any content.

/// Over the desktop outside the region being chosen.
pub const SCRIM: Color32 = Color32::from_black_alpha(130);
/// Over the picture outside a crop.
pub const SCRIM_STRONG: Color32 = Color32::from_black_alpha(150);
/// Behind the recording countdown's number, over the desktop.
pub const SCRIM_COUNTDOWN: Color32 = Color32::from_black_alpha(170);
/// Behind the small badges on a step thumbnail.
pub const SCRIM_BADGE: Color32 = Color32::from_black_alpha(180);
/// The faint darkening that shows live drawing is on, over the desktop.
pub const SCRIM_FAINT: Color32 = Color32::from_black_alpha(28);
/// The soft shadow under a tour card preview, over the step picture.
pub const CARD_SHADOW: Color32 = Color32::from_black_alpha(80);
/// The shadow the Shadow icon depicts.
pub const ICON_SHADOW: Color32 = Color32::from_black_alpha(200);
/// What marks a selection on the user's picture is two-tone, so it reads on any picture: a gold
/// line (which reads on dark pictures) inside a dark band of ground (which reads on light ones).
/// The gold line of the outline, the handles' ring, the edge of a crop and a selected card's dashes.
pub const SELECTION_EDGE: Color32 = DARK.accent;
/// The dark band on each side of [`SELECTION_EDGE`], and the outer edge of a handle.
pub const SELECTION_HALO: Color32 = DARK.ground;
/// How wide the dark band is on each side of the gold line.
pub const SELECTION_HALO_WIDTH: f32 = 1.0;
/// The glass band around the region being chosen in the region selector and around the area
/// being recorded: from the region outwards, a bright edge, a translucent white band and a dark
/// edge, each a whole number of pixels wide. The bright edge reads on dark pictures, the dark
/// edge on light ones. Neutral on purpose: the same in screenshot and record mode, recording or
/// paused. The colours are premultiplied, as egui keeps them and as a layered window takes them.
/// The bright edge, on the side facing the region: white at 95%.
pub const GLASS_EDGE: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 242);
/// The band between the two edges: white at 30%.
pub const GLASS_FILL: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 77);
/// The dark edge, outermost: black at 60%.
pub const GLASS_RIM: Color32 = Color32::from_black_alpha(153);
/// The width of the bright edge in pixels.
pub const GLASS_EDGE_PX: u32 = 1;
/// The width of the translucent band in pixels.
pub const GLASS_FILL_PX: u32 = 6;
/// The width of the dark edge in pixels.
pub const GLASS_RIM_PX: u32 = 1;
/// The glass band's layers from the region outwards: each one's width in pixels and colour.
pub const GLASS_LAYERS: [(u32, Color32); 3] = [(GLASS_EDGE_PX, GLASS_EDGE), (GLASS_FILL_PX, GLASS_FILL), (GLASS_RIM_PX, GLASS_RIM)];
/// The whole band's width in pixels.
pub const GLASS_BAND_PX: u32 = GLASS_EDGE_PX + GLASS_FILL_PX + GLASS_RIM_PX;
/// The light fill of a selection handle.
pub const HANDLE_FILL: Color32 = Color32::WHITE;
/// Lines, handles and outlines drawn over the user's picture, and the number of the countdown.
pub const ON_PICTURE: Color32 = Color32::WHITE;
/// The pixel grid lines of the magnifier, over the desktop.
pub const PICTURE_GUIDE: Color32 = Color32::from_rgba_unmultiplied_const(0xf5, 0xb8, 0x41, 120);
/// Behind a picture while it loads, or around one that does not fill its box.
pub const PICTURE_BACKDROP: Color32 = Color32::BLACK;
/// A texture drawn as it is (egui multiplies a picture by this).
pub const IMAGE_TINT: Color32 = Color32::WHITE;
/// The usage meter: its track, what is used, and what is used past the limit.
pub const METER_TRACK: Color32 = DARK.border;
pub const METER_USED: Color32 = DARK.primary;
pub const METER_OVER: Color32 = DARK.caution_border;
/// A neutral status chip's fill (a draft, an idle connection): the border colour, so it shows on a card.
pub const CHIP: Color32 = DARK.border;
/// The recording indicator and the frames cut from a recording: a functional red from the danger role.
pub const RECORD: Color32 = DARK.danger_border;
/// The recording dot while paused: the brand's gold, near the amber it replaces.
pub const RECORD_PAUSED: Color32 = DARK.gold;

// Type: Manrope for the interface (the brand's dense desktop scale), Hack for monospace.

/// app-heading: window and dialog headings.
pub const HEADING_SIZE: f32 = 18.0;
/// app-strong: emphasised lines.
pub const STRONG_SIZE: f32 = 14.0;
/// app-body: default text.
pub const BODY_SIZE: f32 = 13.0;
/// app-small: secondary lines, toolbar values, status text.
pub const SMALL_SIZE: f32 = 12.0;
/// code: links shown for copying.
pub const MONO_SIZE: f32 = 13.0;
/// app-body and app-small weight: medium, because the app draws small text lighter than a browser does.
pub const BODY_WEIGHT: f32 = 500.0;
/// app-heading and app-strong weight.
pub const STRONG_WEIGHT: f32 = 700.0;

static MANROPE: &[u8] = include_bytes!("../../../assets/fonts/manrope/Manrope[wght].ttf");

/// The font family headings are set in: the same Manrope bytes (borrowed, not copied) at
/// app-heading's bold weight, with the interface family's fallbacks.
pub fn heading_family() -> FontFamily {
    FontFamily::Name("Heading".into())
}

// Shape and space.

// Corners. A button is never a pill: a control's corner radius is at most a quarter of its
// height. egui's own controls and fields are dense (about 21 points high), so they take
// radius-sm; radius-md is for controls at least 36 points high (the 44-point buttons of the
// share, publish and sign-in windows, the link row, the close question's buttons, big icon
// buttons) and for caution and notice boxes; [`control_radius`] picks between them. Chips and
// badges are fully rounded, so a badge never looks like a button.

/// radius-sm: egui's dense controls and fields, menu rows, small icon buttons.
pub const RADIUS_SM: u8 = 4;
/// radius-md: controls 36 points high or more, caution and notice boxes.
pub const RADIUS_MD: u8 = 8;
/// Chips and badges: fully rounded (egui caps a radius at half the height).
pub const RADIUS_FULL: u8 = u8::MAX;
/// radius-lg: cards, dialogs, popovers.
pub const RADIUS_LG: u8 = 12;
pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;
pub const SPACE_6: f32 = 24.0;
pub const SPACE_8: f32 = 32.0;
pub const SPACE_12: f32 = 48.0;
/// The focus ring's width.
pub const FOCUS_WIDTH: f32 = 2.0;

/// The corner radius of a control `height` points high: radius-md from 36 points up, radius-sm below.
pub fn control_radius(height: f32) -> u8 {
    if height >= 36.0 { RADIUS_MD } else { RADIUS_SM }
}

// ---- The roomy sizes ----
//
// The main window's pages and the dialogs opened from them (the profiles list, the publish
// dialog, the share window, the tour rows) are laid out on one sizing system, defined here and
// nowhere else: a 4-point grid, 36-point controls, 44-point rows inside raised cards. A page
// takes it with [`page`] (or [`roomy`] for a dialog's own Ui) and draws its groups with
// [`section_heading`], [`card`] and [`CardRows::row`]. The editor windows keep egui's dense
// controls. Every size is in points, so it scales with pixels-per-point like the rest of egui.

/// The grid every size below sits on.
pub const GRID: f32 = 4.0;
/// Buttons, text fields and dropdowns.
pub const CONTROL_HEIGHT: f32 = 36.0;
/// The space left and right of a button's label, and inside a field.
pub const CONTROL_PAD_X: f32 = 14.0;
/// The space above and below a field's text: centres a line of [`CONTROL_TEXT_SIZE`] in [`CONTROL_HEIGHT`].
pub const CONTROL_PAD_Y: f32 = 8.0;
/// The text of controls and of the page.
pub const CONTROL_TEXT_SIZE: f32 = 14.0;
/// A control's corners: radius-md, a quarter of [`CONTROL_HEIGHT`] at most.
pub const CONTROL_RADIUS: u8 = RADIUS_MD;
/// The gap between two controls side by side, and between stacked lines.
pub const ITEM_GAP: f32 = 8.0;
/// The least height of a row in a card.
pub const ROW_HEIGHT: f32 = 44.0;
/// The width of a row's label column.
pub const LABEL_WIDTH: f32 = 120.0;
/// A card's corners.
pub const CARD_RADIUS: u8 = RADIUS_LG;
/// The space inside a card, left and right of its rows.
pub const CARD_PAD_X: f32 = 16.0;
/// The space inside a card above its first row and below its last.
pub const CARD_PAD_Y: f32 = 4.0;
/// The gap between two cards, and after the last one before what follows.
pub const CARD_GAP: f32 = 14.0;
/// The small heading above a card.
pub const SECTION_HEADING_SIZE: f32 = SMALL_SIZE;
/// The gap between a section heading and its card.
pub const SECTION_HEADING_GAP: f32 = 6.0;
/// The space around a page, inside the window.
pub const PAGE_PAD: f32 = 20.0;
/// The left navigation's items.
pub const NAV_ITEM_HEIGHT: f32 = 36.0;
/// The gap between a navigation item's icon and its label.
pub const NAV_ICON_GAP: f32 = 10.0;
/// The space before a navigation item's label (or its icon).
pub const NAV_PAD_X: f32 = 12.0;
/// The space left and right inside a status pill or chip.
pub const PILL_PAD_X: f32 = 10.0;
/// The gap between a chip's icon and its text.
pub const CHIP_ICON_GAP: f32 = 6.0;
/// The round badge with a profile's initials.
pub const BADGE_SIZE: f32 = 40.0;
/// A tab of a page's tab row (Preferences: General, Output, Hotkeys).
pub const TAB_WIDTH: f32 = 96.0;
/// A dropdown that holds a short choice ("30 fps"), and one that holds a few words.
pub const COMBO_WIDTH: f32 = 96.0;
pub const COMBO_WIDTH_WIDE: f32 = 136.0;
/// The button that shows a shortcut's keys.
pub const KEYS_WIDTH: f32 = 170.0;
/// The space inside a dialog (a question over a page), all round.
pub const DIALOG_PAD: f32 = SPACE_6;
/// A dialog's width.
pub const DIALOG_WIDTH: f32 = 440.0;

/// `style` on the roomy sizes: 36-point controls with 14-point text, radius-md corners.
pub fn roomy_style(style: &mut Style) {
    let s = &mut style.spacing;
    s.interact_size.y = CONTROL_HEIGHT;
    s.button_padding = egui::vec2(CONTROL_PAD_X, CONTROL_PAD_Y);
    s.item_spacing = egui::vec2(ITEM_GAP, ITEM_GAP);
    for (text, size) in [(TextStyle::Body, CONTROL_TEXT_SIZE), (TextStyle::Button, CONTROL_TEXT_SIZE)] {
        style.text_styles.insert(text, FontId::proportional(size));
    }
    let w = &mut style.visuals.widgets;
    for v in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        v.corner_radius = CornerRadius::same(CONTROL_RADIUS);
    }
}

/// [`CARD_GAP`] between the block above and the next one, whatever gap the layout adds itself.
pub fn block_gap(ui: &mut egui::Ui) {
    let spacing = ui.spacing().item_spacing.y;
    ui.add_space((CARD_GAP - spacing).max(0.0));
}

/// Put `ui` (and what is drawn in it from now on) on the roomy sizes.
pub fn roomy(ui: &mut egui::Ui) {
    roomy_style(ui.style_mut());
}

/// A page of the main window: [`PAGE_PAD`] all round, on the roomy sizes.
pub fn page<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .inner_margin(Margin::same(PAGE_PAD as i8))
        .show(ui, |ui| {
            roomy(ui);
            add(ui)
        })
        .inner
}

/// A page's title at the left and, at the right, what `right` draws (laid out from the right),
/// on one line [`CONTROL_HEIGHT`] high.
pub fn page_title(ui: &mut egui::Ui, title: &str, right: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(width, CONTROL_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(egui::vec2(width, CONTROL_HEIGHT));
        ui.add(egui::Label::new(RichText::new(title).heading()).truncate());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
    block_gap(ui);
}

/// The small heading above a card, in the secondary text colour.
pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(strong(RichText::new(text).size(SECTION_HEADING_SIZE).color(MUTED))).truncate());
    ui.add_space(SECTION_HEADING_GAP - ITEM_GAP);
}

/// A card's frame: the surface one step above the page, a hairline edge, [`CARD_RADIUS`].
pub fn card_box() -> egui::Frame {
    egui::Frame::new().fill(SURFACE).stroke(Stroke::new(1.0, BORDER)).corner_radius(CARD_RADIUS).inner_margin(Margin::symmetric(CARD_PAD_X as i8, CARD_PAD_Y as i8))
}

/// A card across the width, with the rows `add` draws through [`CardRows`], and [`CARD_GAP`]
/// after it.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut CardRows<'_>) -> R) -> R {
    let r = card_box()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Rows meet at their hairlines; inside a row, the page's own spacing.
            let spacing = ui.spacing().item_spacing;
            ui.spacing_mut().item_spacing.y = 0.0;
            add(&mut CardRows { ui, count: 0, spacing })
        })
        .inner;
    block_gap(ui);
    r
}

/// The rows of a [`card`]: each at least [`ROW_HEIGHT`] high, with a hairline between two rows.
pub struct CardRows<'a> {
    ui: &'a mut egui::Ui,
    count: usize,
    /// The spacing between items inside a row: the page's.
    spacing: egui::Vec2,
}

impl CardRows<'_> {
    /// How many rows were drawn.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Ask for the card's window to be drawn again after `after`.
    pub fn repaint_after(&self, after: std::time::Duration) {
        self.ui.ctx().request_repaint_after(after);
    }

    /// The hairline above every row but the first.
    fn hairline(&mut self) {
        if self.count > 0 {
            let x = self.ui.max_rect().x_range();
            let y = self.ui.cursor().top();
            self.ui.painter().hline(x, y, Stroke::new(1.0, BORDER));
        }
        self.count += 1;
    }

    /// A row: `label` in the label column, in the secondary colour; what `value` draws beside it;
    /// and what `actions` draws at the right end, laid out from the right. The value gets the
    /// width that is left, so it wraps or is cut short with "…" there and never runs under the
    /// actions.
    pub fn row(&mut self, label: &str, value: impl FnOnce(&mut egui::Ui), actions: impl FnOnce(&mut egui::Ui)) {
        self.row_tinted(label, None, value, actions);
    }

    /// [`Self::row`] on `tint` across the card's width, when there is one: a row in a state of
    /// its own (a shortcut waiting for its keys, one that could not be set).
    pub fn row_tinted(&mut self, label: &str, tint: Option<egui::Color32>, value: impl FnOnce(&mut egui::Ui), actions: impl FnOnce(&mut egui::Ui)) {
        self.hairline();
        let ui = &mut *self.ui;
        let spacing = self.spacing;
        let behind = ui.painter().add(egui::Shape::Noop);
        let width = ui.available_width();
        let row = ui.allocate_ui_with_layout(egui::vec2(width, ROW_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_size(egui::vec2(width, ROW_HEIGHT));
            ui.spacing_mut().item_spacing = egui::vec2(spacing.x, spacing.y.min(ITEM_GAP / 2.0));
            // A row without a label gives the whole width to its value.
            if !label.is_empty() {
                let label_width = LABEL_WIDTH.min(width / 3.0);
                ui.allocate_ui_with_layout(egui::vec2(label_width, ROW_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_size(egui::vec2(label_width, ROW_HEIGHT));
                    ui.set_max_width(label_width);
                    ui.add(egui::Label::new(RichText::new(label).color(MUTED)).truncate());
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                actions(ui);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    // A one-line value sits in the middle of the row: the space under it counts
                    // the gap the layout leaves after every item.
                    let pad = ((ROW_HEIGHT - ui.text_style_height(&TextStyle::Body)) / 2.0).max(0.0);
                    ui.add_space(pad);
                    value(ui);
                    ui.add_space((pad - ui.spacing().item_spacing.y).max(0.0));
                });
            });
        });
        if let Some(tint) = tint {
            let r = row.response.rect.expand2(egui::vec2(CARD_PAD_X / 2.0, -0.5));
            ui.painter().set(behind, egui::Shape::rect_filled(r, RADIUS_SM, tint));
        }
    }

    /// A row with no label: what `add` draws, across the card's width.
    pub fn full(&mut self, add: impl FnOnce(&mut egui::Ui)) {
        self.hairline();
        let spacing = self.spacing;
        let ui = &mut *self.ui;
        let width = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(width, ROW_HEIGHT), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_size(egui::vec2(width, ROW_HEIGHT));
            ui.spacing_mut().item_spacing = spacing;
            ui.add_space(ITEM_GAP);
            add(ui);
            ui.add_space(ITEM_GAP / 2.0);
        });
    }
}

/// A row's value: `text` in the primary text colour, cut short with "…" when it does not fit.
pub fn value(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(egui::Label::new(RichText::new(text.into()).color(TEXT)).truncate())
}

/// A secondary line: small, in the secondary colour, wrapped.
pub fn caption(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(egui::Label::new(RichText::new(text.into()).size(SMALL_SIZE).color(MUTED)).wrap())
}

/// What a status pill says by its colour (never by colour alone: success carries a check,
/// warning the warning icon).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Success,
    Warning,
    Neutral,
}

/// A status pill: fully rounded, in the tone's tint, with its icon before the text. As wide and
/// as high as what it holds, wherever it is laid out.
pub fn pill(ui: &mut egui::Ui, text: &str, tone: Tone) -> egui::Response {
    let (fill, fg, icon) = match tone {
        Tone::Success => (SUCCESS_TINT, SUCCESS_TEXT, Some((crate::icons::Icon::Check, SUCCESS_TEXT))),
        Tone::Warning => (CAUTION_TINT, CAUTION_TEXT, Some((crate::icons::Icon::Warning, CAUTION_BORDER))),
        Tone::Neutral => (CHIP, MUTED, None),
    };
    chip(ui, strong(RichText::new(text).size(SMALL_SIZE).color(fg)), fill, icon)
}

/// A chip: `text` on `fill`, fully rounded, with `icon` in its colour before the text when there
/// is one, as wide and as high as what it holds wherever it is laid out.
pub fn chip(ui: &mut egui::Ui, text: RichText, fill: egui::Color32, icon: Option<(crate::icons::Icon, egui::Color32)>) -> egui::Response {
    let label = text.text().to_string();
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Small);
    let side = galley.size().y;
    let gap = CHIP_ICON_GAP;
    let icon_w = if icon.is_some() { side + gap } else { 0.0 };
    let pad = egui::vec2(PILL_PAD_X, SPACE_1);
    let size = egui::vec2(icon_w + galley.size().x, side) + 2.0 * pad;
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, RADIUS_FULL, fill);
    let inner = rect.shrink2(pad);
    if let Some((icon, colour)) = icon {
        let r = egui::Rect::from_min_size(inner.min, egui::vec2(side, side));
        crate::icons::paint(p, r.shrink(side * 0.15), icon, colour);
    }
    // The text keeps the colour it was given; TEXT only stands in for a text given none.
    p.galley(egui::pos2(inner.left() + icon_w, inner.top()), galley, TEXT);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    resp
}

/// Up to two initials of `name`, upper case: of its first two words, or its first two letters.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let letters: String = match words.as_slice() {
        [] => String::new(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    letters.to_uppercase()
}

/// A round badge [`BADGE_SIZE`] across with `name`'s initials, on the purple selection tint.
pub fn initials_badge(ui: &mut egui::Ui, name: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(BADGE_SIZE, BADGE_SIZE), egui::Sense::hover());
    let p = ui.painter();
    p.circle_filled(rect.center(), BADGE_SIZE / 2.0, SELECTION);
    p.text(rect.center(), egui::Align2::CENTER_CENTER, initials(name), FontId::new(CONTROL_TEXT_SIZE, heading_family()), PRIMARY_TEXT);
    resp
}

/// An item of the left navigation, [`NAV_ITEM_HEIGHT`] high across the width: the label from
/// the left, on the purple selection fill with the label in accent-text when `selected` (the
/// treatment of egui's selectable buttons), on surface-raised while hovered. The app ships no
/// icon font with glyphs for the pages, so the items carry no icon.
pub fn nav_item(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), NAV_ITEM_HEIGHT), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), selected, label));
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CONTROL_RADIUS, SELECTION);
    } else if resp.hovered() {
        p.rect_filled(rect, CONTROL_RADIUS, RAISED);
    }
    if resp.has_focus() {
        p.rect_stroke(rect, CONTROL_RADIUS, Stroke::new(FOCUS_WIDTH, FOCUS), egui::StrokeKind::Inside);
    }
    let galley = ui.fonts_mut(|f| f.layout(label.to_string(), FontId::proportional(CONTROL_TEXT_SIZE), if selected { ACCENT } else { TEXT }, rect.width() - 2.0 * NAV_PAD_X));
    p.galley(egui::pos2(rect.left() + NAV_PAD_X, rect.center().y - galley.size().y / 2.0), galley, TEXT);
    resp
}

/// A danger outline button (Sign out): no fill, a danger-border edge, danger-text label.
pub fn danger_outline_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).color(DANGER_TEXT)).fill(egui::Color32::TRANSPARENT).stroke(Stroke::new(1.0, DANGER_BORDER))
}

/// Fix the app to the dark theme, with the brand's look and fonts. Called once, at start-up.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style());
}

/// Tell a window with a native title bar to draw it dark, whatever Windows uses: the app is
/// always dark, and a light title bar over it would not be. Sent once to each such window when
/// it is made (the main window and every editor window; the other windows have no title bar).
pub fn dark_title_bar(ctx: &egui::Context, id: egui::ViewportId) {
    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
}

/// A context set up the way the app sets up its own, for tests that draw frames.
#[cfg(test)]
pub fn test_context() -> egui::Context {
    let ctx = egui::Context::default();
    apply(&ctx);
    ctx
}

/// egui's built-in fonts with Manrope at the head of the interface family, at medium weight
/// (the font is variable; [`strong`] asks for bold), Manrope at bold weight for headings
/// ([`heading_family`]), and Hack for monospace. The icon and emoji fonts stay as fallbacks.
pub fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let at = |weight: f32| egui::FontTweak { coords: egui::epaint::text::VariationCoords::new([("wght", weight)]), ..Default::default() };
    // `from_static` borrows the one copy of the font that is built into the program.
    fonts.font_data.insert("Manrope".into(), FontData::from_static(MANROPE).tweak(at(BODY_WEIGHT)).into());
    fonts.font_data.insert("Manrope-Heading".into(), FontData::from_static(MANROPE).tweak(at(STRONG_WEIGHT)).into());
    // egui's built-in interface face, Ubuntu-Light, stays right after Manrope as a fallback: it
    // has letters Manrope lacks (much of Cyrillic, Greek Extended), so a title or a file name in
    // them shows its letters, not boxes.
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.retain(|name| name != "Manrope" && name != "Ubuntu-Light");
    proportional.splice(0..0, ["Manrope".to_string(), "Ubuntu-Light".to_string()]);
    let proportional = proportional.clone();
    // Monospace text falls back to the interface family for what Hack lacks.
    let mut monospace = vec!["Hack".to_string()];
    monospace.extend(proportional.iter().cloned());
    fonts.families.insert(FontFamily::Monospace, monospace);
    let mut heading = vec!["Manrope-Heading".to_string()];
    heading.extend(proportional);
    fonts.families.insert(heading_family(), heading);
    fonts
}

/// Emphasis: app-strong's weight, and egui's strong text colour.
pub fn strong(text: RichText) -> RichText {
    text.strong().variation("wght", STRONG_WEIGHT)
}

/// `RichText::strong` in the brand's bold weight; see [`strong`].
pub trait Strong {
    fn bold(self) -> RichText;
}

impl Strong for RichText {
    fn bold(self) -> RichText {
        strong(self)
    }
}

/// egui's look from a palette.
pub fn visuals(p: &Palette) -> Visuals {
    let mut v = Visuals::dark();
    // egui's controls are dense: radius-sm (see the rule above `RADIUS_SM`).
    let md = CornerRadius::same(RADIUS_SM);
    let w = &mut v.widgets;
    // Labels, separators and frames that take no input.
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = md;
    // Controls at rest: a border-strong edge and a text label. egui fills a button with
    // `weak_bg_fill` (surface), and a checkbox's box, a slider's handle and the like with
    // `bg_fill` (surface-raised); a text field takes `text_edit_bg_color` (surface, below).
    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = md;
    w.inactive.expansion = 0.0;
    w.hovered.bg_fill = p.raised;
    w.hovered.weak_bg_fill = p.raised;
    w.hovered.bg_stroke = Stroke::new(1.0, p.muted);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = md;
    w.hovered.expansion = 0.0;
    // Pressed, and focused from the keyboard: the gold focus ring.
    w.active.bg_fill = p.selection;
    w.active.weak_bg_fill = p.selection;
    w.active.bg_stroke = Stroke::new(FOCUS_WIDTH, p.focus);
    w.active.fg_stroke = Stroke::new(2.0, p.text);
    w.active.corner_radius = md;
    w.active.expansion = 0.0;
    // An open dropdown.
    w.open.bg_fill = p.raised;
    w.open.weak_bg_fill = p.raised;
    w.open.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = md;
    w.open.expansion = 0.0;
    // Selected rows and selected text: the selection fill, gold text; a focused field is outlined in gold.
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(FOCUS_WIDTH, p.focus);
    v.hyperlink_color = p.accent;
    v.faint_bg_color = p.surface;
    v.extreme_bg_color = p.ground;
    v.text_edit_bg_color = Some(p.surface);
    v.code_bg_color = p.surface;
    v.warn_fg_color = p.caution_text;
    v.error_fg_color = p.danger_text;
    v.window_corner_radius = CornerRadius::same(RADIUS_LG);
    v.window_shadow = Shadow::NONE;
    v.window_fill = p.raised;
    v.window_stroke = Stroke::new(1.0, p.border_strong);
    v.menu_corner_radius = CornerRadius::same(RADIUS_LG);
    v.panel_fill = p.ground;
    v.popup_shadow = Shadow::NONE;
    v.text_cursor.stroke = Stroke::new(2.0, p.focus);
    v.weak_text_color = Some(p.muted);
    // egui fades a disabled widget to this alpha over the opaque window behind it; the window
    // itself is never faded. Text at 0.6 stays above 4.5:1 on every ground (see the contrast test).
    v.disabled_alpha = DISABLED_ALPHA;
    v
}

/// How strongly egui fades a disabled widget.
pub const DISABLED_ALPHA: f32 = 0.6;

/// egui's style with the brand's type sizes and looks.
pub fn style() -> Style {
    let mut s = Style { visuals: visuals(&DARK), ..Style::default() };
    s.text_styles = [
        (TextStyle::Small, FontId::proportional(SMALL_SIZE)),
        (TextStyle::Body, FontId::proportional(BODY_SIZE)),
        (TextStyle::Button, FontId::proportional(BODY_SIZE)),
        (TextStyle::Heading, FontId::new(HEADING_SIZE, heading_family())),
        (TextStyle::Monospace, FontId::monospace(MONO_SIZE)),
    ]
    .into();
    s
}

/// A popup or toast: surface-raised, a 1px border-strong edge, radius-lg.
pub fn popup_frame() -> egui::Frame {
    egui::Frame::new().fill(RAISED).stroke(Stroke::new(1.0, BORDER_STRONG)).corner_radius(RADIUS_LG)
}

/// Paint the whole window in ground before its panels: where two panels meet, egui softens both
/// edges, and without this the seam between them lets a little of the desktop through.
pub fn window_background(ui: &egui::Ui) {
    ui.painter().rect_filled(ui.ctx().content_rect(), 0.0, GROUND);
}

/// A small window of its own (the destination picker, a toast): the whole window painted opaque
/// in ground, and on it a surface-raised card with a 1px border-strong edge and radius-lg that
/// fills it, so the rounded corners show ground and never the desktop. `margin` is the space
/// between the card's edge and what `add` draws.
pub fn window_card<R>(ui: &mut egui::Ui, margin: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(GROUND))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.painter().rect(rect, RADIUS_LG, RAISED, Stroke::new(1.0, BORDER_STRONG), egui::StrokeKind::Inside);
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(margin + 1.0)), add).inner
        })
        .inner
}

/// The outline of what is selected on the user's picture (and the edge of a crop), just outside
/// `rect`: a gold line `width` points wide with a dark band on each side (see [`SELECTION_HALO`]).
pub fn selection_outline(painter: &egui::Painter, rect: egui::Rect, radius: f32, width: f32) {
    let halo = SELECTION_HALO_WIDTH;
    painter.rect_stroke(rect, radius, Stroke::new(width + 2.0 * halo, SELECTION_HALO), egui::StrokeKind::Outside);
    painter.rect_stroke(rect.expand(halo), radius + halo, Stroke::new(width, SELECTION_EDGE), egui::StrokeKind::Outside);
}

/// The glass band's rectangles just outside `rect` (in points), each layer of [`GLASS_LAYERS`]
/// as many whole physical pixels wide as the band around a recording, at `pixels_per_point`.
/// `rect` is snapped to whole pixels first. Each layer is four rectangles that meet without
/// overlapping (top and bottom take the corners), so no pixel is covered twice.
pub fn glass_band_rects(rect: egui::Rect, pixels_per_point: f32) -> Vec<(egui::Rect, Color32)> {
    let ppp = pixels_per_point;
    let snap = |v: f32| (v * ppp).round();
    let (l, t, r, b) = (snap(rect.min.x), snap(rect.min.y), snap(rect.max.x), snap(rect.max.y));
    let pt = |x0: f32, y0: f32, x1: f32, y1: f32| egui::Rect::from_min_max(egui::pos2(x0 / ppp, y0 / ppp), egui::pos2(x1 / ppp, y1 / ppp));
    let mut out = Vec::with_capacity(4 * GLASS_LAYERS.len());
    let mut from = 0.0;
    for (px, colour) in GLASS_LAYERS {
        let to = from + px as f32;
        out.push((pt(l - to, t - to, r + to, t - from), colour));
        out.push((pt(l - to, b + from, r + to, b + to), colour));
        out.push((pt(l - to, t - from, l - from, b + from), colour));
        out.push((pt(r + from, t - from, r + to, b + from), colour));
        from = to;
    }
    out
}

/// The glass band just outside `rect` (see [`glass_band_rects`]), filled on whole physical
/// pixels so its 1 px edges are crisp and match the band around a recording. The region
/// selector marks the region with it in screenshot and in record mode alike.
pub fn glass_band(painter: &egui::Painter, rect: egui::Rect, pixels_per_point: f32) {
    for (r, colour) in glass_band_rects(rect, pixels_per_point) {
        painter.rect_filled(r, 0.0, colour);
    }
}

/// A handle of what is selected on the user's picture, `side` points square around `center`: a
/// light fill, a gold ring and a dark outer edge.
pub fn selection_handle(painter: &egui::Painter, center: egui::Pos2, side: f32) {
    let r = egui::Rect::from_center_size(center, egui::vec2(side, side));
    painter.rect_filled(r, 1.0, HANDLE_FILL);
    painter.rect_stroke(r, 1.0, Stroke::new(1.0, SELECTION_EDGE), egui::StrokeKind::Outside);
    painter.rect_stroke(r.expand(1.0), 2.0, Stroke::new(SELECTION_HALO_WIDTH, SELECTION_HALO), egui::StrokeKind::Outside);
}

/// A dashed outline along the closed path `pts` over the user's picture: gold dashes `width`
/// points wide over an unbroken dark line, so the gaps read as dark and the dashes as gold.
pub fn selection_dashes(painter: &egui::Painter, pts: &[egui::Pos2], width: f32) {
    painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(width + 2.0 * SELECTION_HALO_WIDTH, SELECTION_HALO)));
    painter.extend(egui::Shape::dashed_line(pts, Stroke::new(width, SELECTION_EDGE), 6.0, 4.0));
}

/// The box around a caution: caution-tint fill, a 1px caution-border edge, radius-md. The edge
/// takes one point of the 10 the boxes had around their text, so nothing moves.
pub fn caution_frame() -> egui::Frame {
    egui::Frame::new().fill(CAUTION_TINT).stroke(Stroke::new(1.0, CAUTION_BORDER)).corner_radius(RADIUS_MD).inner_margin(Margin::same(9))
}

/// The box around an error or a delete question: danger-tint fill, a 1px danger-border edge, radius-md.
pub fn danger_frame() -> egui::Frame {
    egui::Frame::new().fill(DANGER_TINT).stroke(Stroke::new(1.0, DANGER_BORDER)).corner_radius(RADIUS_MD).inner_margin(Margin::same(9))
}

/// A card on a page: surface, radius-lg.
pub fn card_frame() -> egui::Frame {
    egui::Frame::new().fill(SURFACE).corner_radius(RADIUS_LG)
}

/// A caution box: [`caution_frame`] with the warning icon in caution-border beside `add`,
/// which sets its text in caution-text.
pub fn caution_box<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    caution_frame()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let side = ui.text_style_height(&TextStyle::Body);
                let (icon, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                crate::icons::paint(ui.painter(), icon.shrink(1.0), crate::icons::Icon::Warning, CAUTION_BORDER);
                ui.vertical(add).inner
            })
            .inner
        })
        .inner
}

/// How an inline caution's text fits its line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Onto more lines.
    Wrap,
    /// Cut short with "…".
    Truncate,
}

/// A caution on a line of its own, without a box: the small warning icon in caution-border, then
/// `text` in caution-text at `size`. A caution always carries its icon, never colour alone. The
/// icon comes first in the reading order, also in a row laid out from the right. The text's response.
pub fn caution_line(ui: &mut egui::Ui, text: impl Into<String>, size: f32, fit: Fit) -> egui::Response {
    let text = text.into();
    let from_right = ui.layout().prefer_right_to_left();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = SPACE_1;
        let icon = |ui: &mut egui::Ui| {
            let side = ui.fonts_mut(|f| f.row_height(&FontId::proportional(size)));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
            crate::icons::paint(ui.painter(), rect.shrink(side * 0.1), crate::icons::Icon::Warning, CAUTION_BORDER);
        };
        let label = |ui: &mut egui::Ui| {
            let label = egui::Label::new(RichText::new(text).color(CAUTION_TEXT).size(size));
            ui.add(if fit == Fit::Wrap { label.wrap() } else { label.truncate() })
        };
        if from_right {
            let response = label(ui);
            icon(ui);
            response
        } else {
            icon(ui);
            label(ui)
        }
    })
    .inner
}

/// A primary button: primary fill, on-primary label in the strong weight.
pub fn primary_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(strong(RichText::new(text).color(ON_PRIMARY))).fill(PRIMARY)
}

/// The confirming button of a delete: danger fill, on-primary label.
pub fn danger_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).color(ON_PRIMARY)).fill(DANGER_FILL)
}

/// WCAG 2 contrast ratio between two opaque colours.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    fn luminance(c: Color32) -> f32 {
        let ch = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `fg` at `alpha` over the opaque `bg`, blended the way egui blends (in gamma space).
pub fn over(fg: Color32, alpha: f32, bg: Color32) -> Color32 {
    let mix = |f: u8, b: u8| (f32::from(f) * alpha + f32::from(b) * (1.0 - alpha)).round() as u8;
    Color32::from_rgb(mix(fg.r(), bg.r()), mix(fg.g(), bg.g()), mix(fg.b(), bg.b()))
}

/// Checks the frame tests of every window share: what a frame painted under each point.
#[cfg(test)]
pub mod checks {
    use super::*;
    use egui::{Rect, Shape};

    /// The filled rectangles of a frame in paint order: where each shows (cut to its clip), its
    /// fill and its corner radius.
    pub fn fills(out: &egui::FullOutput) -> Vec<(Rect, Color32, u8)> {
        fn open(clip: Rect, s: &Shape, into: &mut Vec<(Rect, Color32, u8)>) {
            match s {
                Shape::Vec(v) => v.iter().for_each(|s| open(clip, s, into)),
                Shape::Rect(r) if r.fill.a() > 0 => into.push((r.rect.intersect(clip), r.fill, r.corner_radius.nw)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        for c in &out.shapes {
            open(c.clip_rect, &c.shape, &mut all);
        }
        all
    }

    /// A window's background: at every point of a 16-point grid over `window`, the first fill
    /// painted there is fully opaque, so nothing behind the window shows through. A ground
    /// painted under the whole window first ([`window_background`]) only covers the seams
    /// between panels, so the check is made again without it: the window's own panels, cards
    /// and bars cover every point themselves, opaque. And every card or dialog (radius-lg) on it
    /// is opaque too.
    pub fn assert_window_opaque(out: &egui::FullOutput, window: Rect, what: &str) {
        let fills = fills(out);
        let under_all = fills.first().filter(|(r, ..)| r.contains_rect(window)).is_some();
        let panels = &fills[usize::from(under_all)..];
        for (layer, fills) in [("the window", &fills[..]), ("its panels", panels)] {
            let mut y = window.top() + 2.0;
            while y < window.bottom() {
                let mut x = window.left() + 2.0;
                while x < window.right() {
                    let p = egui::pos2(x, y);
                    let first = fills.iter().find(|(r, ..)| r.contains(p));
                    assert!(first.is_some_and(|(_, c, _)| c.a() == 255), "{what}, {layer}: at {p:?} the first fill is {first:?}");
                    x += 16.0;
                }
                y += 16.0;
            }
        }
        for (r, c, radius) in &fills {
            if *radius >= RADIUS_LG && r.area() >= 4000.0 {
                assert_eq!(c.a(), 255, "{what}: a see-through card or dialog at {r:?}");
            }
        }
    }

    /// No label sticks out of what holds it: every text that is not cut short with "…" lies
    /// wholly inside the smallest button, field, chip or card (a filled or outlined rectangle
    /// under 150 000 square points) that holds its middle. Returns how many texts were checked.
    pub fn assert_texts_fit(out: &egui::FullOutput, what: &str) -> usize {
        fn open<'a>(clip: Rect, s: &'a Shape, boxes: &mut Vec<Rect>, texts: &mut Vec<(Rect, Rect, &'a egui::epaint::TextShape)>) {
            match s {
                Shape::Vec(v) => v.iter().for_each(|s| open(clip, s, boxes, texts)),
                // What a box shows of itself: a box scrolled out of sight holds nothing.
                Shape::Rect(r) if (r.fill.a() > 0 || r.stroke.width > 0.0) && r.rect.area() < 150_000.0 && r.rect.intersect(clip).is_positive() => boxes.push(r.rect),
                Shape::Text(t) if !t.galley.text().trim().is_empty() && !t.galley.elided => texts.push((t.visual_bounding_rect(), clip, t)),
                _ => {}
            }
        }
        let (mut boxes, mut texts) = (Vec::new(), Vec::new());
        for c in &out.shapes {
            open(c.clip_rect, &c.shape, &mut boxes, &mut texts);
        }
        let mut checked = 0;
        for (rect, clip, t) in texts {
            // A text the page has scrolled out of sight is not a text that sticks out.
            if !clip.contains(rect.center()) {
                continue;
            }
            let holder = boxes.iter().filter(|b| b.contains(rect.center())).min_by(|a, b| a.area().total_cmp(&b.area()));
            if let Some(holder) = holder {
                assert!(holder.expand(1.0).contains_rect(rect), "{what}: {:?} at {rect:?} sticks out of {holder:?}", t.galley.text());
                checked += 1;
            }
        }
        checked
    }

    /// No two texts in sight run over each other: the letters of every pair of texts drawn
    /// where they can be seen (each cut to its clip) share no more than a point's sliver.
    pub fn assert_no_overlaps(out: &egui::FullOutput, what: &str) {
        fn open(clip: Rect, s: &Shape, into: &mut Vec<(Rect, String)>) {
            match s {
                Shape::Vec(v) => v.iter().for_each(|s| open(clip, s, into)),
                Shape::Text(t) if !t.galley.text().trim().is_empty() => {
                    let r = t.visual_bounding_rect().intersect(clip);
                    if r.is_positive() {
                        into.push((r, t.galley.text().to_string()));
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for c in &out.shapes {
            open(c.clip_rect, &c.shape, &mut texts);
        }
        for (i, (a, ta)) in texts.iter().enumerate() {
            for (b, tb) in &texts[i + 1..] {
                let both = a.intersect(*b);
                assert!(!(both.width() > 1.0 && both.height() > 1.0), "{what}: {ta:?} at {a:?} runs over {tb:?} at {b:?}");
            }
        }
    }

    /// The glass band around `hole` (in points, on whole pixels at `ppp`): twelve filled
    /// rectangles in the band's colours, four per layer, on whole physical pixels, together
    /// covering exactly each layer's ring of pixels around the hole, no pixel twice.
    pub fn assert_glass_band(out: &egui::FullOutput, hole: Rect, ppp: f32, what: &str) {
        let band: Vec<(Rect, Color32)> = fills(out).into_iter().filter(|(_, c, _)| GLASS_LAYERS.iter().any(|(_, g)| g == c)).map(|(r, c, _)| (r, c)).collect();
        assert_eq!(band.len(), 12, "{what}: {band:?}");
        let px = |r: Rect| [r.min.x * ppp, r.min.y * ppp, r.max.x * ppp, r.max.y * ppp];
        for (r, _) in &band {
            for v in px(*r) {
                assert!((v - v.round()).abs() < 1e-3, "{what}: {r:?} is not on whole pixels at {ppp}");
            }
        }
        let [l, t, r, b] = px(hole).map(|v| v.round() as i32);
        let mut from = 0i32;
        for (w, colour) in GLASS_LAYERS {
            let to = from + w as i32;
            let mut seen = std::collections::BTreeSet::new();
            for (rect, _) in band.iter().filter(|(_, c)| *c == colour) {
                let [x0, y0, x1, y1] = px(*rect).map(|v| v.round() as i32);
                for y in y0..y1 {
                    for x in x0..x1 {
                        assert!(seen.insert((x, y)), "{what}: {colour:?} covers {x},{y} twice");
                    }
                }
            }
            let within = |x: i32, y: i32, d: i32| x >= l - d && x < r + d && y >= t - d && y < b + d;
            let ring: std::collections::BTreeSet<(i32, i32)> = (t - to..b + to).flat_map(|y| (l - to..r + to).map(move |x| (x, y))).filter(|&(x, y)| !within(x, y, from)).collect();
            assert!(seen == ring, "{what}: the {colour:?} layer is not the ring {from}..{to} px around {l},{t} {r},{b}");
            from = to;
        }
    }

    /// A dialog's frame: the biggest fill with a dialog's corners is surface-raised and opaque.
    pub fn assert_dialog_opaque(out: &egui::FullOutput, what: &str) {
        let frame = fills(out).into_iter().filter(|(_, _, radius)| *radius == RADIUS_LG).max_by(|a, b| a.0.area().total_cmp(&b.0.area()));
        let (rect, fill, _) = frame.unwrap_or_else(|| panic!("{what}: no dialog frame was painted"));
        assert!(rect.area() >= 40_000.0, "{what}: the frame {rect:?}");
        assert_eq!(fill, RAISED, "{what}: the frame is surface-raised and opaque");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One frame of `ctx`, its textures dropped.
    fn run(ctx: &egui::Context, input: egui::RawInput, f: impl FnMut(&mut egui::Ui)) {
        ctx.run_ui(input, f).textures_delta.clear();
    }

    const GROUNDS: [(&str, Color32); 3] = [("ground", GROUND), ("surface", SURFACE), ("surface-raised", RAISED)];

    /// Every pair of colours the theme puts together, and the ratio it must reach: 4.5 for text,
    /// 3 for borders, the focus ring, icons and fills that carry meaning.
    fn pairs() -> Vec<(String, Color32, Color32, f32)> {
        let mut all = Vec::new();
        for (ground, g) in GROUNDS {
            for (name, fg, min) in [
                ("text", TEXT, 4.5),
                ("text-muted", MUTED, 4.5),
                ("disabled text", DISABLED, 4.5),
                ("text faded as disabled", over(TEXT, DISABLED_ALPHA, g), 4.5),
                ("primary-text", PRIMARY_TEXT, 4.5),
                ("accent-text (links)", ACCENT, 4.5),
                ("success-text", SUCCESS_TEXT, 4.5),
                ("caution-text", CAUTION_TEXT, 4.5),
                ("danger-text", DANGER_TEXT, 4.5),
                ("border-strong", BORDER_STRONG, 3.0),
                ("focus", FOCUS, 3.0),
                ("primary (fill edge)", PRIMARY, 3.0),
            ] {
                all.push((format!("{name} on {ground}"), fg, g, min));
            }
        }
        for (name, fg, bg, min) in [
            ("on-primary on primary", ON_PRIMARY, PRIMARY, 4.5),
            ("on-primary on primary-hover", ON_PRIMARY, PRIMARY_HOVER, 4.5),
            ("on-primary on danger-fill", ON_PRIMARY, DANGER_FILL, 4.5),
            ("text-muted on a neutral chip", MUTED, CHIP, 4.5),
            ("meter used against its track", METER_USED, METER_TRACK, 1.5),
            ("meter used on ground", METER_USED, GROUND, 3.0),
            ("meter over the limit on ground", METER_OVER, GROUND, 3.0),
            ("text on selection", TEXT, SELECTION, 4.5),
            ("accent-text on selection (selected label)", ACCENT, SELECTION, 4.5),
            ("primary-text on selection (initials badge)", PRIMARY_TEXT, SELECTION, 4.5),
            ("success-text on success-tint", SUCCESS_TEXT, SUCCESS_TINT, 4.5),
            ("caution-text on caution-tint", CAUTION_TEXT, CAUTION_TINT, 4.5),
            ("danger-text on danger-tint", DANGER_TEXT, DANGER_TINT, 4.5),
            ("success-border on success-tint", SUCCESS_BORDER, SUCCESS_TINT, 3.0),
            ("caution-border (edge, icon) on caution-tint", CAUTION_BORDER, CAUTION_TINT, 3.0),
            ("danger-border on danger-tint", DANGER_BORDER, DANGER_TINT, 3.0),
            ("caution-border on surface-raised", CAUTION_BORDER, RAISED, 3.0),
            ("focus around a primary button, on ground", FOCUS, GROUND, 3.0),
            ("gold against caution-border (told apart)", GOLD, CAUTION_BORDER, 2.0),
            ("selection halo on a white picture", SELECTION_HALO, Color32::WHITE, 3.0),
            ("selection gold on a black picture", SELECTION_EDGE, Color32::BLACK, 3.0),
            ("selection gold against its halo", SELECTION_EDGE, SELECTION_HALO, 3.0),
            ("handle fill against the selection halo", HANDLE_FILL, SELECTION_HALO, 3.0),
            ("glass band's bright edge on a near-black picture", blend(GLASS_EDGE, Color32::from_rgb(12, 12, 14)), Color32::from_rgb(12, 12, 14), 3.0),
            ("glass band's dark edge on a white picture", blend(GLASS_RIM, Color32::WHITE), Color32::WHITE, 3.0),
            ("glass band's bright edge against its dark edge, on a mid-grey picture", blend(GLASS_EDGE, Color32::from_gray(128)), blend(GLASS_RIM, Color32::from_gray(128)), 3.0),
        ] {
            all.push((name.to_string(), fg, bg, min));
        }
        all
    }

    #[test]
    fn every_pair_the_theme_produces_reaches_its_contrast() {
        let mut table = String::from("| Pair | Ratio | Needs |\n|---|---|---|\n");
        let mut failed = Vec::new();
        for (name, fg, bg, min) in pairs() {
            let ratio = contrast(fg, bg);
            table += &format!("| {name} | {ratio:.2} | {min} |\n");
            if ratio < min {
                failed.push(format!("{name}: {ratio:.2} < {min}"));
            }
        }
        println!("{table}");
        assert!(failed.is_empty(), "{failed:?}\n{table}");
    }

    #[test]
    fn the_ratios_match_the_brand_books_measurements() {
        // Pinned from the brand book's table (dark column), so a changed token is noticed.
        for (fg, bg, want) in [
            (TEXT, GROUND, 15.35),
            (TEXT, SURFACE, 14.26),
            (TEXT, RAISED, 13.40),
            (MUTED, RAISED, 7.14),
            (ON_PRIMARY, PRIMARY, 5.09),
            (ON_PRIMARY, PRIMARY_HOVER, 4.68),
            (ON_PRIMARY, DANGER_FILL, 6.55),
            (ACCENT, RAISED, 9.06),
            (TEXT, SELECTION, 9.91),
            (BORDER_STRONG, RAISED, 3.66),
            (CAUTION_TEXT, CAUTION_TINT, 9.46),
            (CAUTION_BORDER, CAUTION_TINT, 3.85),
            (SUCCESS_TEXT, SUCCESS_TINT, 8.77),
            (DANGER_TEXT, DANGER_TINT, 7.97),
        ] {
            assert!((contrast(fg, bg) - want).abs() < 0.01, "{fg:?} on {bg:?}: {} != {want}", contrast(fg, bg));
        }
    }

    /// Whether some pixel of `image` on the line from `a` to `b` (in points, at `ppp`) has at
    /// least 3:1 contrast against `bg`.
    pub(crate) fn reads_against(image: &image::RgbaImage, ppp: f32, a: egui::Pos2, b: egui::Pos2, bg: Color32) -> bool {
        let steps = ((b - a).length() * ppp * 2.0).ceil().max(1.0) as usize;
        (0..=steps).any(|i| {
            let p = a + (b - a) * (i as f32 / steps as f32);
            let (x, y) = ((p.x * ppp) as u32, (p.y * ppp) as u32);
            x < image.width() && y < image.height() && {
                let [r, g, bl, _] = image.get_pixel(x, y).0;
                contrast(Color32::from_rgb(r, g, bl), bg) >= 3.0
            }
        })
    }

    /// The pictures a selection is tested on: a white page and a near-black one.
    pub(crate) const PICTURES: [(&str, Color32); 2] = [("white", Color32::WHITE), ("near-black", Color32::from_rgb(12, 12, 14))];

    /// What marks a selection over the user's picture (the outline, a handle, the dashed outline
    /// of a tour card) reads on a white picture and on a near-black one: across each of them there
    /// is a pixel with at least 3:1 contrast against the picture.
    #[test]
    fn what_marks_a_selection_reads_on_a_white_and_on_a_near_black_picture() {
        let ppp = 1.25;
        let size = egui::vec2(120.0, 100.0);
        let rect = egui::Rect::from_min_size(egui::pos2(30.0, 30.0), egui::vec2(60.0, 40.0));
        let mut failed = Vec::new();
        for (name, bg) in PICTURES {
            let draw = |mark: &dyn Fn(&egui::Painter)| {
                crate::snapshot::picture(&test_context(), size, ppp, 1, |ui| {
                    ui.painter().rect_filled(egui::Rect::from_min_size(egui::Pos2::ZERO, size), 0.0, bg);
                    mark(ui.painter());
                })
            };
            // Cross-sections at a quarter of each side, from the edge outwards.
            let across_by = |image: &image::RgbaImage, what: &str, failed: &mut Vec<String>, reach: f32| {
                let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
                let (qx, qy) = (l + rect.width() / 4.0, t + rect.height() / 4.0);
                for (side, a, z) in [("top", egui::pos2(qx, t), egui::pos2(qx, t - reach)), ("bottom", egui::pos2(qx, b), egui::pos2(qx, b + reach)), ("left", egui::pos2(l, qy), egui::pos2(l - reach, qy)), ("right", egui::pos2(r, qy), egui::pos2(r + reach, qy))] {
                    if !reads_against(image, ppp, a, z, bg) {
                        failed.push(format!("{what} on {name}, {side} side"));
                    }
                }
            };
            let across = |image: &image::RgbaImage, what: &str, failed: &mut Vec<String>| across_by(image, what, failed, 6.0);
            across(&draw(&|p| selection_outline(p, rect, 2.0, 1.0)), "the selection outline", &mut failed);
            // The glass band of the region selector, across its whole width: the bright edge
            // reads on the dark picture, the dark edge on the white one.
            across_by(&draw(&|p| glass_band(p, rect, ppp)), "the glass band", &mut failed, GLASS_BAND_PX as f32 / ppp + 0.5);
            // And over the selector's scrim, as it is drawn there.
            let image = draw(&|p| {
                p.rect_filled(egui::Rect::from_min_size(egui::Pos2::ZERO, size), 0.0, SCRIM);
                glass_band(p, rect, ppp);
            });
            across_by(&image, "the glass band over the scrim", &mut failed, GLASS_BAND_PX as f32 / ppp + 0.5);
            across(&draw(&|p| selection_outline(p, rect, 0.0, 2.0)), "the crop edge", &mut failed);
            // A dashed line has gaps, so along each side, over more than one dash and gap (10
            // points), some pixel reads; across it, the dark line under the dashes reads on white.
            let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
            let image = draw(&|p| selection_dashes(p, &pts, 1.5));
            for (side, a, z) in [("top", rect.left_top() + egui::vec2(15.0, 0.0), rect.left_top() + egui::vec2(27.0, 0.0)), ("right", rect.right_top() + egui::vec2(0.0, 10.0), rect.right_top() + egui::vec2(0.0, 22.0)), ("bottom", rect.right_bottom() - egui::vec2(15.0, 0.0), rect.right_bottom() - egui::vec2(27.0, 0.0)), ("left", rect.left_bottom() - egui::vec2(0.0, 10.0), rect.left_bottom() - egui::vec2(0.0, 22.0))] {
                if !reads_against(&image, ppp, a, z, bg) {
                    failed.push(format!("the dashed card outline on {name}, along the {side} side"));
                }
            }
            if bg == Color32::WHITE {
                across(&image, "the dashed card outline", &mut failed);
            }
            // A handle: across it, from outside one side to its middle.
            let c = rect.center();
            let image = draw(&|p| selection_handle(p, c, 8.0));
            if !reads_against(&image, ppp, c - egui::vec2(8.0, 0.0), c, bg) {
                failed.push(format!("a handle on {name}"));
            }
        }
        assert!(failed.is_empty(), "lost on the picture: {failed:?}");
    }

    /// A premultiplied colour over an opaque one, as egui and a layered window blend it.
    fn blend(fg: Color32, bg: Color32) -> Color32 {
        let a = f32::from(fg.a()) / 255.0;
        let mix = |f: u8, b: u8| (f32::from(f) + f32::from(b) * (1.0 - a)).round().min(255.0) as u8;
        Color32::from_rgb(mix(fg.r(), bg.r()), mix(fg.g(), bg.g()), mix(fg.b(), bg.b()))
    }

    /// The glass band is what the owner approved: from the region outwards 1 px of white at 95%,
    /// 6 px of white at 30% and 1 px of black at 60%, premultiplied, and neutral: none of its
    /// colours is the recording red or the paused gold.
    #[test]
    fn the_glass_band_is_a_bright_edge_a_translucent_band_and_a_dark_edge() {
        assert_eq!(GLASS_LAYERS.map(|(px, _)| px), [1, 6, 1]);
        assert_eq!(GLASS_BAND_PX, 8);
        assert_eq!(GLASS_EDGE.to_array(), [242, 242, 242, 242]);
        assert_eq!(GLASS_FILL.to_array(), [77, 77, 77, 77]);
        assert_eq!(GLASS_RIM.to_array(), [0, 0, 0, 153]);
        for (_, c) in GLASS_LAYERS {
            assert!(c.r() <= c.a() && c.g() <= c.a() && c.b() <= c.a(), "{c:?} is premultiplied");
            assert!(c.r() == c.g() && c.g() == c.b(), "{c:?} is a neutral grey");
            assert!(c != RECORD && c != RECORD_PAUSED);
        }
    }

    /// The selector's band, drawn at 100%, 125% and 200%: each layer filled on whole physical
    /// pixels, its four rectangles covering exactly the ring of pixels it takes around the region,
    /// none twice, nothing inside the region. Also around a region whose edges fall between pixels.
    #[test]
    fn the_glass_band_in_the_selector_lies_just_outside_the_region_in_whole_pixels() {
        for ppp in [1.0, 1.25, 2.0] {
            // A region on whole pixels at this scale: 40,32 to 140,92 pixels, in points.
            let rect = egui::Rect::from_min_max(egui::pos2(40.0 / ppp, 32.0 / ppp), egui::pos2(140.0 / ppp, 92.0 / ppp));
            let ctx = test_context();
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| glass_band(ui.painter(), rect, ppp));
            out.textures_delta.clear();
            checks::assert_glass_band(&out, rect, ppp, &format!("{ppp}"));
            // A region off the pixel grid: the band goes round the pixels it snaps to.
            let off = rect.translate(egui::vec2(0.3 / ppp, -0.2 / ppp));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| glass_band(ui.painter(), off, ppp));
            out.textures_delta.clear();
            checks::assert_glass_band(&out, rect, ppp, &format!("{ppp}, off the grid"));
        }
    }

    /// A button is never a pill: egui's buttons, fields and dropdowns have a corner radius of at
    /// most a quarter of their height, and so do the tall buttons of the share window, at
    /// radius-md. (A checkbox's 14-point box keeps radius-sm: a rounded square, not a pill.)
    #[test]
    fn a_button_is_never_a_pill() {
        let ctx = test_context();
        let mut text = String::from("Name");
        let mut out = None;
        for _ in 0..2 {
            let o = ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = ui.button("Save");
                ui.text_edit_singleline(&mut text);
                egui::ComboBox::from_id_salt("pick").selected_text("PNG").show_ui(ui, |_| {});
                ui.allocate_ui(egui::vec2(520.0, 60.0), |ui| crate::share_ui::action_row(ui, &[crate::share_ui::ActionButton { label: "Done", accent: false }]));
            });
            out = Some(o);
            out.as_mut().unwrap().textures_delta.clear();
        }
        let out = out.unwrap();
        let controls: Vec<_> = checks::fills(&out).into_iter().filter(|(r, ..)| r.height() >= 10.0 && r.width() >= 10.0).collect();
        assert!(controls.len() >= 4, "{controls:?}");
        for (r, c, radius) in &controls {
            assert!(f32::from(*radius) <= r.height() / 4.0, "a control {r:?} filled {c:?} has radius {radius}");
        }
        assert!(controls.iter().any(|(r, _, radius)| (r.height() - crate::share_ui::BUTTON_HEIGHT).abs() < 0.5 && *radius == RADIUS_MD), "the tall button keeps radius-md: {controls:?}");
        assert!(controls.iter().any(|(r, _, radius)| r.height() < 30.0 && *radius == RADIUS_SM), "a dense control takes radius-sm: {controls:?}");
    }

    /// A disabled icon button (undo with nothing to undo, a tool that cannot be used) is faded
    /// once, by egui: its icon keeps at least 4.5:1 against the bar it sits on.
    #[test]
    fn a_disabled_icon_is_faded_once() {
        let ppp = 2.0;
        let size = egui::vec2(60.0, 60.0);
        let image = crate::snapshot::picture(&test_context(), size, ppp, 2, |ui| {
            ui.painter().rect_filled(egui::Rect::from_min_size(egui::Pos2::ZERO, size), 0.0, SURFACE);
            ui.add_enabled_ui(false, |ui| crate::icons::button(ui, crate::icons::Icon::Undo, 36.0, false, "Undo"));
        });
        let best = image.pixels().map(|p| contrast(Color32::from_rgb(p.0[0], p.0[1], p.0[2]), SURFACE)).fold(0.0, f32::max);
        assert!(best >= 4.5, "the disabled icon reaches only {best:.2}:1 on surface");
    }

    /// An inline caution carries the warning icon before its text, in a row laid out from the
    /// left and in one laid out from the right.
    #[test]
    fn an_inline_caution_carries_its_icon_before_the_text() {
        let ctx = test_context();
        for layout in [egui::Layout::left_to_right(egui::Align::Center), egui::Layout::right_to_left(egui::Align::Center)] {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.with_layout(layout, |ui| caution_line(ui, "The address is not valid.", SMALL_SIZE, Fit::Wrap));
            });
            out.textures_delta.clear();
            let (mut icon, mut text) = (None::<egui::Rect>, None::<egui::Rect>);
            fn walk(s: &egui::Shape, icon: &mut Option<egui::Rect>, text: &mut Option<egui::Rect>) {
                match s {
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, icon, text)),
                    egui::Shape::Text(t) if t.galley.job.sections.iter().all(|sec| sec.format.color == CAUTION_TEXT) => *text = Some(t.visual_bounding_rect()),
                    egui::Shape::Text(_) => {}
                    s if format!("{s:?}").contains(&format!("{CAUTION_BORDER:?}")) => *icon = Some(icon.map_or(s.visual_bounding_rect(), |r| r.union(s.visual_bounding_rect()))),
                    _ => {}
                }
            }
            out.shapes.iter().for_each(|c| walk(&c.shape, &mut icon, &mut text));
            let (icon, text) = (icon.expect("the warning icon is drawn"), text.expect("the text is drawn"));
            assert!(icon.right() <= text.left() + 0.5, "{layout:?}: the icon {icon:?} comes before the text {text:?}");
        }
    }

    /// Every caution in caution-text outside this module is either in a caution box (which
    /// carries the icon) or a [`caution_line`]: none is colour alone.
    #[test]
    fn no_caution_is_colour_alone() {
        let mut bare = Vec::new();
        for (file, lines) in production_sources() {
            if file == "theme.rs" {
                continue;
            }
            for (i, (n, line)) in lines.iter().enumerate() {
                if line.split("//").next().unwrap_or("").contains("theme::CAUTION_TEXT") && !lines[i.saturating_sub(8)..i].iter().any(|(_, l)| l.contains("caution_box(")) {
                    bare.push(format!("{file}:{n}: {}", line.trim()));
                }
            }
        }
        assert!(bare.is_empty(), "cautions without their icon:
{}", bare.join("
"));
    }

    /// The roomy sizes sit on the 4-point grid (but the approved 14-point card gap and padding),
    /// and a page on them draws 36-point buttons, fields and dropdowns with radius-md corners and
    /// 14-point text, at 100% and at 200%.
    #[test]
    fn the_roomy_sizes_make_36_point_controls_and_44_point_rows() {
        for v in [CONTROL_HEIGHT, ROW_HEIGHT, LABEL_WIDTH, CARD_PAD_X, PAGE_PAD, NAV_ITEM_HEIGHT, BADGE_SIZE, DIALOG_PAD, ITEM_GAP, CONTROL_PAD_Y, CARD_PAD_Y] {
            assert_eq!(v % GRID, 0.0, "{v} is off the grid");
        }
        assert_eq!((CONTROL_HEIGHT, CONTROL_PAD_X, CONTROL_TEXT_SIZE, CONTROL_RADIUS, ROW_HEIGHT, CARD_RADIUS, CARD_PAD_X, CARD_GAP, PAGE_PAD, NAV_ITEM_HEIGHT, NAV_ICON_GAP), (36.0, 14.0, 14.0, 8, 44.0, 12, 16.0, 14.0, 20.0, 36.0, 10.0));
        assert!(f32::from(CONTROL_RADIUS) <= CONTROL_HEIGHT / 4.0, "never a pill");
        for ppp in [1.0, 2.0] {
            let ctx = test_context();
            ctx.set_pixels_per_point(ppp);
            let mut text = String::from("Name");
            let mut out = None;
            for _ in 0..2 {
                let mut o = ctx.run_ui(egui::RawInput::default(), |ui| {
                    page(ui, |ui| {
                        let _ = ui.button("Save");
                        ui.text_edit_singleline(&mut text);
                        egui::ComboBox::from_id_salt("pick").selected_text("PNG").show_ui(ui, |_| {});
                        card(ui, |rows| {
                            rows.row("Site", |ui| drop(value(ui, "tours.example.com")), |ui| drop(ui.button("Open site")));
                            rows.row("Account", |ui| drop(value(ui, "alex")), |_| {});
                            rows.full(|ui| drop(caption(ui, "A line.")));
                            assert_eq!(rows.count(), 3);
                        });
                    });
                });
                o.textures_delta.clear();
                out = Some(o);
            }
            let out = out.unwrap();
            let controls: Vec<_> = checks::fills(&out).into_iter().filter(|(r, _, radius)| *radius != CARD_RADIUS && r.width() >= 30.0 && r.height() >= 20.0 && r.height() < 60.0).collect();
            assert!(controls.len() >= 4, "{controls:?}");
            for (r, c, radius) in &controls {
                assert!((r.height() - CONTROL_HEIGHT).abs() < 0.6 && *radius == CONTROL_RADIUS, "at {ppp}: a control {r:?} filled {c:?} with radius {radius}");
            }
            // The card: surface, a hairline edge, radius-lg; its rows 44 points apart.
            let card_fill = checks::fills(&out).into_iter().find(|(_, c, r)| *c == SURFACE && *r == CARD_RADIUS).expect("the card");
            assert!(card_fill.0.height() >= 3.0 * ROW_HEIGHT, "{card_fill:?}");
            let (site, account) = (crate::share_ui::tests::text_rect(&out, "Site").unwrap(), crate::share_ui::tests::text_rect(&out, "Account").unwrap());
            assert!((account.center().y - site.center().y - ROW_HEIGHT).abs() < 0.6, "{site:?} {account:?}");
            let value_at = crate::share_ui::tests::text_rect(&out, "tours.example.com").unwrap();
            assert!((value_at.left() - site.left() - LABEL_WIDTH - ITEM_GAP).abs() < 0.6, "the value column: {value_at:?}");
            checks::assert_texts_fit(&out, &format!("the card at {ppp}"));
            checks::assert_no_overlaps(&out, &format!("the card at {ppp}"));
        }
    }

    #[test]
    fn initials_are_of_the_first_two_words_or_letters() {
        assert_eq!(initials("Sandbox test"), "ST");
        assert_eq!(initials("customer1"), "CU");
        assert_eq!(initials("Alex · mythicshot.com"), "AM");
        assert_eq!(initials("  "), "");
        assert_eq!(initials("élan vital"), "ÉV");
    }

    /// The ground under the whole window alone does not pass for a window's panels.
    #[test]
    #[should_panic(expected = "its panels")]
    fn the_ground_under_a_window_alone_does_not_make_it_opaque() {
        let ctx = test_context();
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 100.0));
        let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(window), ..Default::default() }, |ui| {
            window_background(ui);
            ui.painter().rect_filled(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0)), 0.0, SURFACE);
        });
        out.textures_delta.clear();
        checks::assert_window_opaque(&out, window, "half a window");
    }

    #[test]
    fn the_app_is_dark_whatever_windows_uses() {
        let ctx = test_context();
        run(&ctx, egui::RawInput { system_theme: Some(egui::Theme::Light), ..Default::default() }, |_| {});
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        assert_eq!(ctx.global_style().visuals.panel_fill, GROUND);
        assert_eq!(ctx.global_style().visuals.window_fill, RAISED);
        assert!(ctx.global_style().visuals.window_shadow == Shadow::NONE && ctx.global_style().visuals.popup_shadow == Shadow::NONE);
    }

    #[test]
    fn every_background_the_theme_paints_is_opaque() {
        let v = visuals(&DARK);
        for (name, c) in [("panel", v.panel_fill), ("window", v.window_fill), ("faint", v.faint_bg_color), ("extreme", v.extreme_bg_color), ("code", v.code_bg_color), ("text edit", v.text_edit_bg_color())] {
            assert_eq!(c.a(), 255, "{name}");
        }
        let w = &v.widgets;
        for (name, wv) in [("noninteractive", w.noninteractive), ("inactive", w.inactive), ("hovered", w.hovered), ("active", w.active), ("open", w.open)] {
            assert!(wv.bg_fill.a() == 255 && wv.weak_bg_fill.a() == 255, "{name}");
        }
    }

    #[test]
    fn interface_text_is_manrope_at_medium_weight_and_monospace_is_hack() {
        let fonts = fonts();
        assert_eq!(fonts.families[&FontFamily::Proportional][0], "Manrope");
        assert_eq!(fonts.families[&FontFamily::Monospace][0], "Hack");
        // Ubuntu-Light is a fallback right after Manrope, never ahead of it.
        assert_eq!(fonts.families[&FontFamily::Proportional][1], "Ubuntu-Light");
        assert_eq!(fonts.families[&heading_family()][..2], ["Manrope-Heading", "Manrope"]);
        let axes = fonts.font_data["Manrope"].variation_axes();
        let wght = axes.iter().find(|a| a.tag.to_string() == "wght").expect("Manrope is variable in weight");
        assert!(wght.range.min <= BODY_WEIGHT && STRONG_WEIGHT <= wght.range.max, "{wght:?}");
    }

    /// Headings are app-heading's bold: set in the heading family, Manrope at weight 700, they
    /// come out wider than the same words in the body family at the same size. The heading face
    /// borrows the same bytes as the body face: the font is in the program once.
    #[test]
    fn headings_are_bold_manrope_from_the_same_bytes() {
        let fonts = fonts();
        let (body, heading) = (&fonts.font_data["Manrope"], &fonts.font_data["Manrope-Heading"]);
        let bytes = |d: &FontData| (*d.font).as_ref().as_ptr();
        assert!(bytes(body) == MANROPE.as_ptr() && bytes(heading) == MANROPE.as_ptr(), "both borrow the built-in bytes");
        assert_eq!(style().text_styles[&TextStyle::Heading], FontId::new(HEADING_SIZE, heading_family()));
        let ctx = test_context();
        run(&ctx, egui::RawInput::default(), |_| {});
        let wide = |family: FontFamily| ctx.fonts_mut(|f| f.layout_no_wrap("Publish tour".into(), FontId::new(HEADING_SIZE, family), TEXT).size().x);
        let (medium, bold) = (wide(FontFamily::Proportional), wide(heading_family()));
        assert!(bold > medium * 1.02, "body {medium}, heading {bold}");
        // `ui.heading` and the share window's heading both take the heading family.
        let mut families = Vec::new();
        run(&ctx, egui::RawInput::default(), |ui| {
            for text in [egui::WidgetText::from(RichText::new("Hotkeys").heading()), egui::WidgetText::from(RichText::new("Publish to").heading().size(crate::share_ui::HEADING_SIZE))] {
                let galley = text.into_galley(ui, None, f32::INFINITY, TextStyle::Body);
                families.push(galley.job.sections[0].format.font_id.family.clone());
            }
        });
        assert!(families.iter().all(|f| *f == heading_family()), "{families:?}");
    }

    /// A title or a file name in letters Manrope lacks (extended Cyrillic, Greek Extended) shows
    /// them through the Ubuntu-Light fallback, in the body and in a heading, not as boxes.
    #[test]
    fn letters_manrope_lacks_come_from_the_fallback() {
        let ctx = test_context();
        run(&ctx, egui::RawInput::default(), |_| {});
        let title = "Ѣѳѵ Ґґ Ӂӂ Ӑӑ Ӧӧ — Ἀθῆναι ᾄδω ὕδωρ";
        for family in [FontFamily::Proportional, heading_family(), FontFamily::Monospace] {
            let lacking: Vec<char> = title.chars().filter(|c| !c.is_whitespace()).filter(|c| !ctx.fonts_mut(|f| f.has_glyph(&FontId::new(13.0, family.clone()), *c))).collect();
            assert!(lacking.is_empty(), "{family:?} shows boxes for {lacking:?}");
        }
        // And Manrope alone does lack some of them, so the fallback is what draws them.
        let only = egui::Context::default();
        let mut defs = FontDefinitions::default();
        defs.font_data.insert("Manrope".into(), FontData::from_static(MANROPE).into());
        defs.families.insert(FontFamily::Proportional, vec!["Manrope".into()]);
        only.set_fonts(defs);
        run(&only, egui::RawInput::default(), |_| {});
        assert!(title.chars().any(|c| !c.is_whitespace() && !only.fonts_mut(|f| f.has_glyph(&FontId::proportional(13.0), c))));
    }

    /// Width of `text` in the interface font at `size`, at the weight `bold` asks for.
    fn width(ctx: &egui::Context, text: &str, size: f32, bold: bool) -> f32 {
        let mut w = 0.0;
        run(ctx, egui::RawInput::default(), |ui| {
            let t = RichText::new(text).size(size);
            let t = if bold { strong(t) } else { t };
            let galley = egui::WidgetText::from(t).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Body);
            w = galley.size().x;
        });
        w
    }

    #[test]
    fn the_variable_font_draws_bold_wider_than_medium() {
        // egui renders a variable font at the asked weight: bold Manrope is wider than medium.
        let ctx = test_context();
        run(&ctx, egui::RawInput::default(), |_| {});
        let (medium, bold) = (width(&ctx, "Publish tour", 14.0, false), width(&ctx, "Publish tour", 14.0, true));
        assert!(bold > medium * 1.02, "medium {medium}, bold {bold}");
    }

    #[test]
    fn manrope_has_every_character_the_interface_writes() {
        let ctx = egui::Context::default();
        let mut only = FontDefinitions::default();
        only.font_data.insert("Manrope".into(), FontData::from_static(MANROPE).into());
        only.families.insert(FontFamily::Proportional, vec!["Manrope".into()]);
        ctx.set_fonts(only);
        run(&ctx, egui::RawInput::default(), |_| {});
        // Every character outside ASCII that the app's own strings use (test data aside).
        let lacking: Vec<char> = ['·', '…', '×', '›', '‹', '–', '—', '≈', '²', 'é', 'ü', 'ö', 'ä', 'ß', 'Ü'].into_iter().filter(|c| !ctx.fonts_mut(|f| f.has_glyph(&FontId::proportional(13.0), *c))).collect();
        assert!(lacking.is_empty(), "Manrope lacks {lacking:?}");
    }

    /// Production code of the app crate: every source file, with test modules and test files left out.
    fn production_sources() -> Vec<(String, Vec<(usize, String)>)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    if p.file_name().is_some_and(|n| n != "tests") {
                        walk(&p, out);
                    }
                } else if p.extension().is_some_and(|x| x == "rs") && p.file_name().is_some_and(|n| n != "tests.rs") {
                    out.push(p);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        files.sort();
        // Modules built only for tests (`#[cfg(test)] mod name;` in main.rs).
        let main = std::fs::read_to_string(root.join("main.rs")).unwrap();
        let lines: Vec<&str> = main.lines().collect();
        let test_only: Vec<String> = lines.windows(2).filter(|w| w[0].trim() == "#[cfg(test)]").filter_map(|w| w[1].trim().strip_prefix("mod ")?.strip_suffix(';').map(|m| format!("{m}.rs"))).collect();
        files.retain(|p| !test_only.iter().any(|t| p.strip_prefix(&root).is_ok_and(|r| r.to_string_lossy() == *t)));
        files
            .into_iter()
            .map(|p| {
                let text = std::fs::read_to_string(&p).unwrap();
                let mut lines = Vec::new();
                let mut after_cfg_test = false;
                for (i, line) in text.lines().enumerate() {
                    let t = line.trim_start();
                    if after_cfg_test && !t.starts_with("#[") {
                        // `#[cfg(test)]` on a module: the rest of the file is the test module.
                        if t.split_whitespace().take(2).any(|w| w == "mod") {
                            break;
                        }
                        after_cfg_test = false;
                    }
                    if t.starts_with("#[cfg(test)]") {
                        after_cfg_test = true;
                    }
                    lines.push((i + 1, line.to_string()));
                }
                (p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), lines)
            })
            .collect()
    }

    /// Colour literals in production code outside this module: numeric `Color32` constructors,
    /// egui's named colours other than transparent and the placeholder, and `#rrggbb` strings.
    fn colour_literals() -> Vec<(String, usize, String)> {
        let numeric = ["from_rgb(", "from_rgba_unmultiplied(", "from_rgba_premultiplied(", "from_rgba_unmultiplied_const(", "from_gray(", "from_black_alpha(", "from_white_alpha("];
        let named = ["WHITE", "BLACK", "GRAY", "LIGHT_GRAY", "DARK_GRAY", "RED", "LIGHT_RED", "DARK_RED", "GREEN", "LIGHT_GREEN", "DARK_GREEN", "BLUE", "LIGHT_BLUE", "DARK_BLUE", "YELLOW", "LIGHT_YELLOW", "KHAKI", "GOLD", "BROWN", "ORANGE", "MAGENTA", "PURPLE", "CYAN"];
        let mut found = Vec::new();
        for (file, lines) in production_sources() {
            if file == "theme.rs" {
                continue;
            }
            for (n, line) in lines {
                let code = line.split("//").next().unwrap_or("");
                let mut hit = false;
                for f in numeric {
                    for (at, _) in code.match_indices(&format!("Color32::{f}")) {
                        let args = code[at + "Color32::".len() + f.len()..].trim_start();
                        // A literal starts with a digit; a conversion of a colour the user chose starts with a name.
                        hit |= args.starts_with(|c: char| c.is_ascii_digit());
                    }
                }
                for c in named {
                    hit |= code.match_indices(&format!("Color32::{c}")).any(|(at, _)| !code[at + 9 + c.len()..].starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '_'));
                }
                let bytes = code.as_bytes();
                for (at, _) in code.match_indices("\"#") {
                    let hexits = bytes[at + 2..].iter().take_while(|b| b.is_ascii_hexdigit()).count();
                    hit |= matches!(hexits, 3 | 6 | 8) && bytes.get(at + 2 + hexits) == Some(&b'"');
                }
                if hit {
                    found.push((file.clone(), n, line.trim().to_string()));
                }
            }
        }
        found
    }

    /// Colour literals that stay where they are, and why.
    const ALLOWED: &[(&str, &str, &str)] = &[
        // A tour's look belongs to its author: the default accent, the accent choices, the
        // backdrop a custom colour starts from, and the card colours the preview shares with the
        // exported player. Existing tours and exports must stay byte-identical.
        ("tour/doc.rs", "DEFAULT_ACCENT: &str = \"#5dcaa5\"", "tour author default"),
        ("tour_cards.rs", "const DEFAULT_GREEN: Color32", "tour author default accent"),
        ("tour_cards.rs", "const INK_DARK: Color32", "ink on a tour's accent, as the player draws it"),
        ("tour_cards.rs", "const INK_LIGHT: Color32", "ink on a tour's accent, as the player draws it"),
        ("tour_cards.rs", "const ACCENTS:", "a tour's accent choices"),
        ("tour_cards.rs", "const BACKDROP_START: &str", "a tour's backdrop default"),
        ("tour_cards.rs", "CardStyle::Light | CardStyle::Compact =>", "a tour's light card, as the player draws it"),
        ("tour_cards.rs", "CardStyle::Dark | CardStyle::Auto =>", "a tour's dark card, as the player draws it"),
        // The highlighter tool's icon shows the colour it draws with: the annotation palette's.
        ("icons.rs", "Color32::from_rgba_unmultiplied(255, 220, 0, 170)", "annotation palette: the highlighter"),
    ];

    #[test]
    fn no_colour_is_written_outside_the_theme_but_the_allowed_ones() {
        let found = colour_literals();
        let stray: Vec<_> = found.iter().filter(|(file, _, line)| !ALLOWED.iter().any(|(f, snippet, _)| f == file && line.contains(snippet))).collect();
        assert!(stray.is_empty(), "{} colour literals outside theme.rs:\n{}", stray.len(), stray.iter().map(|(f, n, l)| format!("{f}:{n}: {l}")).collect::<Vec<_>>().join("\n"));
        // Every allowance is still needed.
        for (file, snippet, why) in ALLOWED {
            assert!(found.iter().any(|(f, _, l)| f == file && l.contains(snippet)), "an allowance nothing uses any more: {file} {snippet} ({why})");
        }
    }
}
