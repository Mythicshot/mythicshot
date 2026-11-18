//! Toolbar icons drawn with egui's painter: no image files, sharp at any scale,
//! and coloured by the theme. Each icon is drawn in a unit square mapped onto
//! the button's icon rect.

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

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
    Card,
    Crop,
    Undo,
    Redo,
    Save,
    SaveAs,
    Copy,
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
}

const FG: Color32 = Color32::from_rgb(214, 218, 226);
const ACCENT: Color32 = Color32::from_rgb(47, 111, 235);
const HOVER: Color32 = Color32::from_rgb(44, 50, 62);

/// A square icon button. `selected` fills it with the accent colour.
pub fn button(ui: &mut Ui, icon: Icon, size: f32, selected: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let enabled = ui.is_enabled();
    let fill = if selected {
        ACCENT
    } else if resp.hovered() && enabled {
        HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 5.0, fill);
    let color = if !enabled { Color32::from_gray(90) } else if selected { Color32::WHITE } else { FG };
    paint(ui.painter(), rect.shrink(size * 0.22), icon, color);
    resp.on_hover_text(tip)
}

/// An icon followed by a label, for menu rows.
pub fn menu_row(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, HOVER);
    }
    let icon_rect = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), Vec2::splat(16.0));
    paint(ui.painter(), icon_rect, icon, FG);
    ui.painter().text(pos2(rect.left() + 38.0, rect.center().y), egui::Align2::LEFT_CENTER, label, FontId::proportional(14.0), FG);
    resp
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
        Icon::Donut => {
            p.circle_stroke(r.center(), r.width() * 0.44, s);
            p.circle_stroke(r.center(), r.width() * 0.2, thin);
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
        Icon::Cross => {
            let d = 0.13;
            let u = [(0.0, d), (d, 0.0), (0.5, 0.5 - d), (1.0 - d, 0.0), (1.0, d), (0.5 + d, 0.5), (1.0, 1.0 - d), (1.0 - d, 1.0), (0.5, 0.5 + d), (d, 1.0), (0.0, 1.0 - d), (0.5 - d, 0.5)];
            p.add(Shape::closed_line(u.iter().map(|(x, y)| at(0.08 + x * 0.84, 0.08 + y * 0.84)).collect(), thin));
        }
        Icon::Asterisk => {
            for (x0, y0, x1, y1) in [(0.08, 0.5, 0.92, 0.5), (0.5, 0.08, 0.5, 0.92), (0.2, 0.2, 0.8, 0.8), (0.2, 0.8, 0.8, 0.2)] {
                p.add(line(&[(x0, y0), (x1, y1)]));
            }
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
            p.text(r.center(), egui::Align2::CENTER_CENTER, "1", FontId::proportional(r.height() * 0.75), Color32::from_rgb(27, 31, 39));
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
                p.add(Shape::line(vec![at(0.62, 1.0), at(1.05, 0.55)], Stroke::new(s.width * 1.3, ACCENT)));
            }
        }
        Icon::Copy => {
            p.rect_stroke(rect(0.3, 0.3, 0.95, 0.95), 1.5, s, StrokeKind::Middle);
            p.add(line(&[(0.15, 0.7), (0.05, 0.7), (0.05, 0.05), (0.7, 0.05), (0.7, 0.15)]));
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
        Icon::Record => {
            p.circle_stroke(r.center(), r.width() * 0.44, s);
            p.circle_filled(r.center(), r.width() * 0.22, c);
        }
        Icon::Shadow => {
            p.rect_filled(rect(0.25, 0.25, 1.0, 1.0), 2.0, Color32::from_black_alpha(200));
            p.rect_stroke(rect(0.0, 0.0, 0.75, 0.75), 2.0, s, StrokeKind::Middle);
        }
    }
}
