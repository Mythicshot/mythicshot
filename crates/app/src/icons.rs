//! Toolbar icons drawn with egui's painter: no image files, sharp at any scale,
//! and coloured by the theme. Each icon is drawn in a unit square mapped onto
//! the button's icon rect.

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Select,
    Rect,
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
    Card,
    Crop,
    Undo,
    Redo,
    Save,
    SaveAs,
    Copy,
    /// A tick: something worked ("Copied").
    Check,
    Send,
    Folder,
    Effects,
    Fit,
    Actual,
    Print,
    Email,
    Editor,
    Close,
    Delete,
    Front,
    EditText,
    Shadow,
    Pause,
    Play,
    Stop,
    Restart,
    StepBack,
    StepForward,
    Export,
    /// The main window's capture tiles.
    Region,
    Window,
    Screen,
    Record,
    /// A small padlock: who can open something is restricted.
    Lock,
    /// A triangle with an exclamation mark: the icon of every caution box.
    Warning,
    /// Add something: Add profile.
    Plus,
    /// A list opens under the control: Switch profile.
    ChevronDown,
    /// The main window's left menu, one per page.
    Home,
    Tours,
    Workspace,
    Preferences,
}

impl Icon {
    /// Every icon, for the tests that draw them all.
    #[cfg(test)]
    pub const ALL: [Icon; 56] = [
        Icon::Select, Icon::Rect, Icon::Ellipse, Icon::Line, Icon::Arrow, Icon::Freehand, Icon::Circle, Icon::Group, Icon::Cone, Icon::Slice, Icon::Text, Icon::Bubble, Icon::Step, Icon::Highlight, Icon::Obfuscate, Icon::Spotlight, Icon::Card, Icon::Crop, Icon::Undo, Icon::Redo, Icon::Save, Icon::SaveAs, Icon::Copy, Icon::Check, Icon::Send, Icon::Folder, Icon::Effects, Icon::Fit, Icon::Actual, Icon::Print, Icon::Email, Icon::Editor, Icon::Close, Icon::Delete, Icon::Front, Icon::EditText, Icon::Shadow, Icon::Pause, Icon::Play, Icon::Stop, Icon::Restart, Icon::StepBack, Icon::StepForward, Icon::Export, Icon::Region, Icon::Window, Icon::Screen, Icon::Record, Icon::Lock, Icon::Warning, Icon::Plus, Icon::ChevronDown, Icon::Home, Icon::Tours, Icon::Workspace, Icon::Preferences,
    ];
}


/// A square icon button. `selected` fills it with the primary colour, the icon in on-primary.
pub fn button(ui: &mut Ui, icon: Icon, size: f32, selected: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let enabled = ui.is_enabled();
    let fill = if selected {
        theme::PRIMARY
    } else if resp.hovered() && enabled {
        theme::RAISED
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, f32::from(theme::control_radius(size)), fill);
    // Disabled, the icon stays in text: egui already fades everything in a disabled Ui (to
    // theme::DISABLED_ALPHA, over the opaque window), and fading theme::DISABLED as well
    // would leave it at 2.8:1.
    let color = if selected { theme::ON_PRIMARY } else { theme::TEXT };
    paint(ui.painter(), rect.shrink(size * 0.22), icon, color);
    resp.on_hover_text(tip)
}

/// An icon followed by a label, for menu rows: a row of the app's own menus
/// ([`theme::menu_row`]), the whole row the click target and lit under the pointer. The icon's
/// middle is 18 points from the row's left, the label 38.
pub fn menu_row(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    theme::menu_row(ui, true, false, label, |ui| {
        let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_string(), FontId::proportional(14.0), theme::TEXT));
        let lead = 38.0 - theme::MENU_ROW_PAD_X;
        let (rect, _) = ui.allocate_exact_size(vec2(lead + galley.size().x, 30.0), Sense::hover());
        let icon_rect = Rect::from_center_size(pos2(rect.left() + 18.0 - theme::MENU_ROW_PAD_X, rect.center().y), Vec2::splat(16.0));
        paint(ui.painter(), icon_rect, icon, theme::TEXT);
        ui.painter().galley(pos2(rect.left() + lead, rect.center().y - galley.size().y / 2.0), galley, theme::TEXT);
    })
}

pub fn paint(p: &egui::Painter, r: Rect, icon: Icon, c: Color32) {
    let at = |x: f32, y: f32| -> Pos2 { pos2(r.left() + x * r.width(), r.top() + y * r.height()) };
    let s = Stroke::new((r.width() / 11.0).max(1.4), c);
    let thin = Stroke::new((r.width() / 16.0).max(1.0), c);
    let line = |pts: &[(f32, f32)]| Shape::line(pts.iter().map(|(x, y)| at(*x, *y)).collect(), s);
    let poly = |pts: &[(f32, f32)]| Shape::convex_polygon(pts.iter().map(|(x, y)| at(*x, *y)).collect(), c, Stroke::NONE);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(at(x0, y0), at(x1, y1));
    match icon {
        Icon::Select => {
            p.add(Shape::convex_polygon(vec![at(0.2, 0.05), at(0.2, 0.8), at(0.4, 0.62), at(0.55, 0.95), at(0.68, 0.88), at(0.53, 0.58), at(0.8, 0.55)], Color32::TRANSPARENT, s));
        }
        Icon::Rect => {
            p.rect_stroke(rect(0.05, 0.18, 0.95, 0.82), 1.5, s, StrokeKind::Middle);
        }
        Icon::Circle => {
            p.circle_stroke(r.center(), r.width() * 0.42, s);
        }
        Icon::Group => {
            let pts = vec![at(0.06, 0.12), at(0.94, 0.12), at(0.94, 0.88), at(0.06, 0.88), at(0.06, 0.12)];
            p.extend(Shape::dashed_line(&pts, thin, r.width() * 0.14, r.width() * 0.09));
            p.circle_filled(at(0.36, 0.42), r.width() * 0.09, c);
            p.circle_filled(at(0.64, 0.42), r.width() * 0.09, c);
            p.add(line(&[(0.24, 0.72), (0.36, 0.6), (0.48, 0.72)]));
            p.add(line(&[(0.52, 0.72), (0.64, 0.6), (0.76, 0.72)]));
        }
        Icon::Cone => {
            p.add(Shape::closed_line(vec![at(0.08, 0.5), at(0.92, 0.12), at(0.92, 0.88)], s));
        }
        Icon::Slice => {
            let apex = at(0.12, 0.88);
            let mut pts = vec![apex];
            for i in 0..=8 {
                let a = (-80.0f32 + 70.0 * i as f32 / 8.0).to_radians();
                pts.push(pos2(apex.x + r.width() * 0.86 * a.cos(), apex.y + r.height() * 0.86 * a.sin()));
            }
            p.add(Shape::closed_line(pts, s));
        }
        Icon::Ellipse => {
            p.add(Shape::ellipse_stroke(r.center(), vec2(r.width() * 0.46, r.height() * 0.34), s));
        }
        Icon::Line => {
            p.add(line(&[(0.1, 0.9), (0.9, 0.1)]));
        }
        Icon::Arrow => {
            p.add(line(&[(0.1, 0.9), (0.72, 0.28)]));
            p.add(poly(&[(0.95, 0.05), (0.5, 0.2), (0.8, 0.5)]));
        }
        Icon::Freehand => {
            let pts: Vec<Pos2> = (0..=20).map(|i| {
                let t = i as f32 / 20.0;
                at(0.05 + t * 0.9, 0.5 + (t * std::f32::consts::TAU * 1.3).sin() * 0.3)
            }).collect();
            p.add(Shape::line(pts, s));
        }
        Icon::Text => {
            p.add(line(&[(0.12, 0.2), (0.12, 0.08), (0.88, 0.08), (0.88, 0.2)]));
            p.add(line(&[(0.5, 0.08), (0.5, 0.92)]));
            p.add(line(&[(0.32, 0.92), (0.68, 0.92)]));
        }
        Icon::Bubble => {
            p.rect_stroke(rect(0.05, 0.08, 0.95, 0.68), 4.0, s, StrokeKind::Middle);
            p.add(line(&[(0.3, 0.68), (0.22, 0.95), (0.5, 0.68)]));
        }
        Icon::Step => {
            p.circle_filled(r.center(), r.width() * 0.48, c);
            p.text(r.center(), egui::Align2::CENTER_CENTER, "1", FontId::proportional(r.height() * 0.75), theme::SURFACE);
        }
        Icon::Highlight => {
            p.rect_filled(rect(0.0, 0.62, 1.0, 0.95), 1.0, Color32::from_rgba_unmultiplied(255, 220, 0, 170));
            p.add(Shape::convex_polygon(vec![at(0.35, 0.58), at(0.6, 0.05), at(0.85, 0.18), at(0.6, 0.66)], Color32::TRANSPARENT, s));
        }
        Icon::Obfuscate => {
            let n = 4;
            for i in 0..n {
                for j in 0..n {
                    if (i + j) % 2 == 0 {
                        let (x, y) = (i as f32 / n as f32, j as f32 / n as f32);
                        p.rect_filled(rect(x, y, x + 1.0 / n as f32, y + 1.0 / n as f32), 0.0, c);
                    }
                }
            }
            p.rect_stroke(r, 0.0, thin, StrokeKind::Middle);
        }
        Icon::Spotlight => {
            // A dimmed frame around a bright box.
            let dark = c.gamma_multiply(0.35);
            let (x0, y0, x1, y1) = (0.3, 0.32, 0.74, 0.68);
            p.rect_filled(rect(0.0, 0.1, 1.0, y0), 0.0, dark);
            p.rect_filled(rect(0.0, y1, 1.0, 0.9), 0.0, dark);
            p.rect_filled(rect(0.0, y0, x0, y1), 0.0, dark);
            p.rect_filled(rect(x1, y0, 1.0, y1), 0.0, dark);
            p.rect_stroke(rect(x0, y0, x1, y1), 1.0, s, StrokeKind::Middle);
        }
        Icon::Card => {
            // A guide card: a rounded panel with a title line, a body line and a button.
            p.rect_stroke(rect(0.04, 0.14, 0.96, 0.86), 2.5, thin, StrokeKind::Middle);
            p.add(line(&[(0.18, 0.34), (0.62, 0.34)]));
            p.add(Shape::line_segment([at(0.18, 0.52), at(0.8, 0.52)], thin));
            p.rect_filled(rect(0.56, 0.64, 0.84, 0.76), 1.5, c);
        }
        Icon::Crop => {
            p.add(line(&[(0.25, 0.0), (0.25, 0.75), (1.0, 0.75)]));
            p.add(line(&[(0.0, 0.25), (0.75, 0.25), (0.75, 1.0)]));
        }
        Icon::Undo | Icon::Redo => {
            let flip = icon == Icon::Redo;
            let fx = |x: f32| if flip { 1.0 - x } else { x };
            let pts: Vec<Pos2> = (0..=16).map(|i| {
                let a = std::f32::consts::PI * (1.0 - i as f32 / 16.0);
                at(fx(0.55 - 0.4 * a.cos()), 0.62 - 0.4 * a.sin())
            }).collect();
            p.add(Shape::line(pts, s));
            p.add(poly(&[(fx(0.0), 0.6), (fx(0.3), 0.6), (fx(0.15), 0.85)]));
        }
        Icon::Save | Icon::SaveAs => {
            p.rect_stroke(rect(0.08, 0.08, 0.92, 0.92), 2.0, s, StrokeKind::Middle);
            p.rect_filled(rect(0.28, 0.08, 0.72, 0.36), 0.0, c);
            p.rect_stroke(rect(0.25, 0.56, 0.75, 0.92), 0.0, thin, StrokeKind::Middle);
            if icon == Icon::SaveAs {
                p.add(Shape::line(vec![at(0.62, 1.0), at(1.05, 0.55)], Stroke::new(s.width * 1.3, theme::ACCENT)));
            }
        }
        Icon::Copy => {
            p.rect_stroke(rect(0.3, 0.3, 0.95, 0.95), 1.5, s, StrokeKind::Middle);
            p.add(line(&[(0.15, 0.7), (0.05, 0.7), (0.05, 0.05), (0.7, 0.05), (0.7, 0.15)]));
        }
        Icon::Check => {
            p.add(line(&[(0.08, 0.52), (0.38, 0.82), (0.92, 0.2)]));
        }
        Icon::Send => {
            p.add(Shape::convex_polygon(vec![at(0.0, 0.45), at(1.0, 0.05), at(0.62, 0.95), at(0.45, 0.6)], Color32::TRANSPARENT, s));
            p.add(line(&[(0.45, 0.6), (1.0, 0.05)]));
        }
        Icon::Folder => {
            p.add(line(&[(0.05, 0.85), (0.05, 0.15), (0.38, 0.15), (0.48, 0.28), (0.95, 0.28), (0.95, 0.85), (0.05, 0.85)]));
        }
        Icon::Effects => {
            let star = |cx: f32, cy: f32, k: f32| {
                vec![at(cx, cy - k), at(cx + k * 0.25, cy - k * 0.25), at(cx + k, cy), at(cx + k * 0.25, cy + k * 0.25), at(cx, cy + k), at(cx - k * 0.25, cy + k * 0.25), at(cx - k, cy), at(cx - k * 0.25, cy - k * 0.25)]
            };
            p.add(Shape::convex_polygon(star(0.42, 0.55, 0.42), c, Stroke::NONE));
            p.add(Shape::convex_polygon(star(0.82, 0.18, 0.18), c, Stroke::NONE));
        }
        Icon::Fit => {
            for (a, b, d) in [((0.0, 0.3), (0.0, 0.0), (0.3, 0.0)), ((0.7, 0.0), (1.0, 0.0), (1.0, 0.3)), ((1.0, 0.7), (1.0, 1.0), (0.7, 1.0)), ((0.3, 1.0), (0.0, 1.0), (0.0, 0.7))] {
                p.add(line(&[a, b, d]));
            }
        }
        Icon::Actual => {
            p.text(r.center(), egui::Align2::CENTER_CENTER, "1:1", FontId::proportional(r.height() * 0.8), c);
        }
        Icon::Print => {
            p.rect_stroke(rect(0.25, 0.0, 0.75, 0.35), 0.0, thin, StrokeKind::Middle);
            p.rect_stroke(rect(0.0, 0.35, 1.0, 0.75), 2.0, s, StrokeKind::Middle);
            p.rect_filled(rect(0.25, 0.62, 0.75, 1.0), 0.0, c);
        }
        Icon::Email => {
            p.rect_stroke(rect(0.0, 0.15, 1.0, 0.85), 1.5, s, StrokeKind::Middle);
            p.add(line(&[(0.0, 0.15), (0.5, 0.55), (1.0, 0.15)]));
        }
        Icon::Editor | Icon::EditText => {
            p.add(Shape::convex_polygon(vec![at(0.1, 0.9), at(0.15, 0.68), at(0.7, 0.12), at(0.88, 0.3), at(0.33, 0.85)], Color32::TRANSPARENT, s));
            if icon == Icon::EditText {
                p.add(line(&[(0.55, 0.95), (1.0, 0.95)]));
            }
        }
        Icon::Close => {
            p.add(line(&[(0.12, 0.12), (0.88, 0.88)]));
            p.add(line(&[(0.88, 0.12), (0.12, 0.88)]));
        }
        Icon::Delete => {
            p.add(line(&[(0.05, 0.2), (0.95, 0.2)]));
            p.add(line(&[(0.35, 0.2), (0.35, 0.05), (0.65, 0.05), (0.65, 0.2)]));
            p.add(line(&[(0.15, 0.2), (0.22, 0.95), (0.78, 0.95), (0.85, 0.2)]));
        }
        Icon::Front => {
            p.rect_stroke(rect(0.0, 0.0, 0.62, 0.62), 1.0, thin, StrokeKind::Middle);
            p.rect_filled(rect(0.38, 0.38, 1.0, 1.0), 1.0, c);
        }
        Icon::Pause => {
            p.rect_filled(rect(0.15, 0.05, 0.4, 0.95), 1.0, c);
            p.rect_filled(rect(0.6, 0.05, 0.85, 0.95), 1.0, c);
        }
        Icon::Play => {
            p.add(poly(&[(0.15, 0.05), (0.92, 0.5), (0.15, 0.95)]));
        }
        Icon::Stop => {
            p.rect_filled(rect(0.1, 0.1, 0.9, 0.9), 2.0, c);
        }
        Icon::Restart => {
            let pts: Vec<Pos2> = (0..=20).map(|i| {
                let a = std::f32::consts::PI * (0.35 + 1.5 * i as f32 / 20.0);
                at(0.5 + 0.42 * a.cos(), 0.5 + 0.42 * a.sin())
            }).collect();
            p.add(Shape::line(pts, s));
            p.add(poly(&[(0.95, 0.05), (0.95, 0.42), (0.6, 0.3)]));
        }
        Icon::StepBack => {
            p.rect_filled(rect(0.08, 0.1, 0.24, 0.9), 1.0, c);
            p.add(poly(&[(0.92, 0.1), (0.92, 0.9), (0.3, 0.5)]));
        }
        Icon::StepForward => {
            p.rect_filled(rect(0.76, 0.1, 0.92, 0.9), 1.0, c);
            p.add(poly(&[(0.08, 0.1), (0.08, 0.9), (0.7, 0.5)]));
        }
        Icon::Export => {
            p.add(line(&[(0.05, 0.55), (0.05, 0.95), (0.95, 0.95), (0.95, 0.55)]));
            p.add(line(&[(0.5, 0.7), (0.5, 0.05)]));
            p.add(line(&[(0.25, 0.3), (0.5, 0.05), (0.75, 0.3)]));
        }
        Icon::Region => {
            // A selection: a dashed rectangle with a solid corner being dragged out.
            let pts = vec![at(0.05, 0.12), at(0.95, 0.12), at(0.95, 0.88), at(0.05, 0.88), at(0.05, 0.12)];
            p.extend(Shape::dashed_line(&pts, thin, r.width() * 0.13, r.width() * 0.09));
            p.add(line(&[(0.62, 0.88), (0.95, 0.88), (0.95, 0.55)]));
            p.circle_filled(at(0.95, 0.88), r.width() * 0.08, c);
        }
        Icon::Window => {
            p.rect_stroke(rect(0.05, 0.12, 0.95, 0.88), 2.0, s, StrokeKind::Middle);
            p.add(line(&[(0.05, 0.34), (0.95, 0.34)]));
            p.circle_filled(at(0.17, 0.23), r.width() * 0.04, c);
            p.circle_filled(at(0.29, 0.23), r.width() * 0.04, c);
        }
        Icon::Screen => {
            p.rect_stroke(rect(0.04, 0.1, 0.96, 0.7), 2.0, s, StrokeKind::Middle);
            p.add(line(&[(0.5, 0.7), (0.5, 0.9)]));
            p.add(line(&[(0.28, 0.9), (0.72, 0.9)]));
        }
        Icon::Lock => {
            p.rect_filled(rect(0.18, 0.45, 0.82, 0.95), 1.5, c);
            p.add(line(&[(0.32, 0.45), (0.32, 0.28), (0.4, 0.12), (0.6, 0.12), (0.68, 0.28), (0.68, 0.45)]));
        }
        Icon::Record => {
            p.circle_stroke(r.center(), r.width() * 0.44, s);
            p.circle_filled(r.center(), r.width() * 0.22, c);
        }
        Icon::Warning => {
            p.add(Shape::closed_line(vec![at(0.5, 0.06), at(0.96, 0.9), at(0.04, 0.9)], s));
            p.add(line(&[(0.5, 0.36), (0.5, 0.6)]));
            p.circle_filled(at(0.5, 0.75), s.width * 0.7, c);
        }
        Icon::Shadow => {
            p.rect_filled(rect(0.25, 0.25, 1.0, 1.0), 2.0, theme::ICON_SHADOW);
            p.rect_stroke(rect(0.0, 0.0, 0.75, 0.75), 2.0, s, StrokeKind::Middle);
        }
        Icon::Plus => {
            p.add(line(&[(0.5, 0.12), (0.5, 0.88)]));
            p.add(line(&[(0.12, 0.5), (0.88, 0.5)]));
        }
        Icon::ChevronDown => {
            p.add(line(&[(0.18, 0.36), (0.5, 0.68), (0.82, 0.36)]));
        }
        Icon::Home => {
            // A house: the roof, the walls and the door.
            p.add(line(&[(0.06, 0.5), (0.5, 0.1), (0.94, 0.5)]));
            p.add(line(&[(0.2, 0.4), (0.2, 0.92), (0.8, 0.92), (0.8, 0.4)]));
            p.add(line(&[(0.41, 0.92), (0.41, 0.64), (0.59, 0.64), (0.59, 0.92)]));
        }
        Icon::Tours => {
            // A screen with the play mark of a tour on it.
            p.rect_stroke(rect(0.04, 0.14, 0.96, 0.86), 2.5, s, StrokeKind::Middle);
            p.add(poly(&[(0.4, 0.32), (0.7, 0.5), (0.4, 0.68)]));
        }
        Icon::Workspace => {
            // A globe: the site the tours are published to.
            p.circle_stroke(r.center(), r.width() * 0.44, s);
            p.add(Shape::ellipse_stroke(r.center(), vec2(r.width() * 0.2, r.height() * 0.44), thin));
            p.add(Shape::line_segment([at(0.06, 0.5), at(0.94, 0.5)], thin));
        }
        Icon::Preferences => {
            // A gear: a ring with eight teeth.
            p.circle_stroke(r.center(), r.width() * 0.24, s);
            for i in 0..8 {
                let a = std::f32::consts::TAU * i as f32 / 8.0;
                let (x, y) = (a.cos(), a.sin());
                p.add(Shape::line_segment([at(0.5 + 0.3 * x, 0.5 + 0.3 * y), at(0.5 + 0.46 * x, 0.5 + 0.46 * y)], Stroke::new(s.width * 1.4, c)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon draws something, inside its square and in the colour asked for, at 100% and
    /// 200%; the ones the main window's pages use among them.
    #[test]
    fn every_icon_draws_inside_its_square_in_its_colour() {
        for ppp in [1.0, 2.0] {
            for icon in Icon::ALL {
                let side = 18.0;
                let size = vec2(side + 8.0, side + 8.0);
                let image = crate::snapshot::picture(&theme::test_context(), size, ppp, 1, |ui| {
                    ui.painter().rect_filled(Rect::from_min_size(Pos2::ZERO, size), 0.0, theme::SURFACE);
                    paint(ui.painter(), Rect::from_min_size(pos2(4.0, 4.0), Vec2::splat(side)), icon, theme::TEXT);
                });
                let inked = image.enumerate_pixels().filter(|(_, _, p)| theme::contrast(Color32::from_rgb(p.0[0], p.0[1], p.0[2]), theme::SURFACE) >= 3.0).collect::<Vec<_>>();
                assert!(inked.len() as f32 >= 8.0 * ppp * ppp, "{icon:?} at {ppp} draws almost nothing");
                // The page icons stay inside their square (a pixel of slack for the line's
                // width); some older toolbar icons reach a little past theirs on purpose.
                if !matches!(icon, Icon::Plus | Icon::ChevronDown | Icon::Home | Icon::Tours | Icon::Workspace | Icon::Preferences) {
                    continue;
                }
                let slack = 2.0 * ppp;
                for (x, y, _) in &inked {
                    let (x, y) = (*x as f32, *y as f32);
                    assert!(x >= 4.0 * ppp - slack && x <= (4.0 + side) * ppp + slack && y >= 4.0 * ppp - slack && y <= (4.0 + side) * ppp + slack, "{icon:?} at {ppp} draws outside its square at {x},{y}");
                }
            }
        }
    }
}
