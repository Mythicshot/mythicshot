//! Key and click overlays for recordings, drawn at export (and in the editor's
//! preview) from a log of input events. Nothing is burned in while recording,
//! so the style can change afterwards.

use serde::{Deserialize, Serialize};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::render::rounded_rect;
use crate::text::{draw_lines, line_height_px};
use crate::{Rgba, measure_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputKind {
    /// Key name as shown, for example `Ctrl` or `S`.
    KeyDown(String),
    KeyUp(String),
    /// Button and position relative to the recorded area.
    MouseDown(MouseButton, i32, i32),
    MouseUp(MouseButton, i32, i32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputEvent {
    /// On the recording clock, like frame times.
    pub t_ms: u64,
    pub kind: InputKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStyle {
    #[default]
    Off,
    Chips,
    Keyboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickStyle {
    Off,
    #[default]
    Ripple,
    Mouse,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayStyle {
    pub keys: KeyStyle,
    pub clicks: ClickStyle,
    pub halo: bool,
    pub pointer: bool,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        Self { keys: KeyStyle::Off, clicks: ClickStyle::Ripple, halo: false, pointer: true }
    }
}

const MODIFIERS: [&str; 4] = ["Ctrl", "Alt", "Shift", "Win"];
const CHIP_WINDOW_MS: u64 = 1500;
const CHIP_FADE_MS: u64 = 300;
const RIPPLE_MS: u64 = 450;
const FLASH_MS: u64 = 150;
const ORANGE: Rgba = Rgba([255, 140, 26, 255]);
const PANEL: Rgba = Rgba([17, 20, 26, 220]);
const KEY_BG: Rgba = Rgba([42, 48, 59, 255]);
const KEY_ON: Rgba = Rgba([47, 111, 235, 255]);

fn is_modifier(k: &str) -> bool {
    MODIFIERS.contains(&k)
}

/// Draw every overlay for the moment `t_ms` onto a frame.
pub fn draw_overlays(pm: &mut Pixmap, t_ms: u64, pointer: Option<(i32, i32)>, events: &[InputEvent], style: &OverlayStyle) {
    if style.halo {
        if let Some((x, y)) = pointer {
            fill_circle(pm, x as f32, y as f32, 22.0, Rgba([255, 214, 10, 90]));
        }
    }
    if matches!(style.clicks, ClickStyle::Ripple | ClickStyle::Both) {
        ripples(pm, t_ms, events);
    }
    if style.pointer {
        if let Some((x, y)) = pointer {
            draw_pointer(pm, x, y);
        }
    }
    // Without any logged keys (key logging was off) there is nothing to show.
    let any_keys = events.iter().any(|e| matches!(e.kind, InputKind::KeyDown(_)));
    match style.keys {
        KeyStyle::Chips if any_keys => key_chips(pm, t_ms, events),
        KeyStyle::Keyboard if any_keys => keyboard(pm, t_ms, events),
        _ => {}
    }
    if matches!(style.clicks, ClickStyle::Mouse | ClickStyle::Both) {
        mouse_widget(pm, t_ms, events);
    }
}

/// Keys held at `t_ms`, plus keys pressed within the last flash period.
fn keys_down(t_ms: u64, events: &[InputEvent]) -> Vec<String> {
    let mut held: Vec<String> = Vec::new();
    for e in events.iter().filter(|e| e.t_ms <= t_ms) {
        match &e.kind {
            InputKind::KeyDown(k) if !held.contains(k) => held.push(k.clone()),
            InputKind::KeyUp(k) if t_ms - e.t_ms > FLASH_MS => held.retain(|h| h != k),
            _ => {}
        }
    }
    held
}

fn buttons_down(t_ms: u64, events: &[InputEvent]) -> Vec<MouseButton> {
    let mut held = Vec::new();
    for e in events.iter().filter(|e| e.t_ms <= t_ms) {
        match &e.kind {
            InputKind::MouseDown(b, ..) if !held.contains(b) => held.push(*b),
            InputKind::MouseUp(b, ..) if t_ms - e.t_ms > FLASH_MS => held.retain(|h| h != b),
            _ => {}
        }
    }
    held
}

/// The most recent shortcut within the chip window: modifiers held when the
/// key went down, then the key, and how many times it was pressed in a row.
fn last_combo(t_ms: u64, events: &[InputEvent]) -> Option<(Vec<String>, u32, u64)> {
    let mut held: Vec<String> = Vec::new();
    let mut last: Option<(Vec<String>, u64)> = None;
    let mut count = 0u32;
    for e in events.iter().filter(|e| e.t_ms <= t_ms) {
        match &e.kind {
            InputKind::KeyDown(k) => {
                if is_modifier(k) {
                    if !held.contains(k) {
                        held.push(k.clone());
                    }
                    // A modifier on its own still shows until another key joins it.
                    if last.as_ref().is_none_or(|(_, at)| e.t_ms - at > CHIP_WINDOW_MS) {
                        last = Some((held.clone(), e.t_ms));
                        count = 1;
                    }
                } else {
                    let mut combo: Vec<String> = MODIFIERS.iter().filter(|m| held.iter().any(|h| h == *m)).map(|m| m.to_string()).collect();
                    combo.push(k.clone());
                    match &mut last {
                        Some((prev, at)) if *prev == combo && e.t_ms - *at <= CHIP_WINDOW_MS => {
                            count += 1;
                            *at = e.t_ms;
                        }
                        _ => {
                            last = Some((combo, e.t_ms));
                            count = 1;
                        }
                    }
                }
            }
            InputKind::KeyUp(k) => held.retain(|h| h != k),
            _ => {}
        }
    }
    last.filter(|(_, at)| t_ms - at <= CHIP_WINDOW_MS).map(|(c, at)| (c, count, at))
}

fn key_chips(pm: &mut Pixmap, t_ms: u64, events: &[InputEvent]) {
    let Some((combo, count, at)) = last_combo(t_ms, events) else { return };
    let age = t_ms - at;
    let fade = if age > CHIP_WINDOW_MS - CHIP_FADE_MS { (CHIP_WINDOW_MS - age) as f32 / CHIP_FADE_MS as f32 } else { 1.0 };
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let size = (h * 0.045).clamp(13.0, 30.0);
    let pad = size * 0.5;
    let gap = size * 0.45;
    let lh = line_height_px(size);
    let plus_w = measure_text("+", size).0;
    let count_label = if count > 1 { format!("×{count}") } else { String::new() };
    let widths: Vec<f32> = combo.iter().map(|k| measure_text(k, size).0 + pad * 2.0).collect();
    let mut total = widths.iter().sum::<f32>() + (combo.len().saturating_sub(1)) as f32 * (plus_w + gap * 2.0);
    if !count_label.is_empty() {
        total += gap * 2.0 + measure_text(&count_label, size * 0.8).0;
    }
    let chip_h = lh + pad;
    let x0 = (w - total) / 2.0;
    let y0 = h - h * 0.06 - chip_h;
    let alpha = |c: Rgba| Rgba([c.0[0], c.0[1], c.0[2], (c.0[3] as f32 * fade) as u8]);
    fill_rounded(pm, x0 - pad, y0 - pad * 0.6, total + pad * 2.0, chip_h + pad * 1.2, size * 0.5, alpha(PANEL));
    let mut x = x0;
    for (i, (k, cw)) in combo.iter().zip(&widths).enumerate() {
        let last_key = i + 1 == combo.len();
        fill_rounded(pm, x, y0, *cw, chip_h, size * 0.3, alpha(if last_key { KEY_ON } else { KEY_BG }));
        draw_lines(pm, &[k.clone()], x + pad, y0 + pad / 2.0, size, alpha(Rgba::WHITE));
        x += cw;
        if !last_key {
            draw_lines(pm, &["+".to_string()], x + gap, y0 + pad / 2.0, size, alpha(Rgba([163, 170, 184, 255])));
            x += plus_w + gap * 2.0;
        }
    }
    if !count_label.is_empty() {
        draw_lines(pm, &[count_label], x + gap * 2.0, y0 + pad / 2.0 + size * 0.15, size * 0.8, alpha(Rgba([163, 170, 184, 255])));
    }
}

const KEYBOARD_ROWS: [&[(&str, f32)]; 5] = [
    &[("Esc", 1.4), ("1", 1.0), ("2", 1.0), ("3", 1.0), ("4", 1.0), ("5", 1.0), ("6", 1.0), ("7", 1.0), ("8", 1.0), ("9", 1.0), ("0", 1.0), ("Bksp", 1.8)],
    &[("Tab", 1.4), ("Q", 1.0), ("W", 1.0), ("E", 1.0), ("R", 1.0), ("T", 1.0), ("Y", 1.0), ("U", 1.0), ("I", 1.0), ("O", 1.0), ("P", 1.0), ("Del", 1.8)],
    &[("Caps", 1.7), ("A", 1.0), ("S", 1.0), ("D", 1.0), ("F", 1.0), ("G", 1.0), ("H", 1.0), ("J", 1.0), ("K", 1.0), ("L", 1.0), ("Enter", 2.5)],
    &[("Shift", 2.2), ("Z", 1.0), ("X", 1.0), ("C", 1.0), ("V", 1.0), ("B", 1.0), ("N", 1.0), ("M", 1.0), ("Up", 1.0), ("Shift", 2.0)],
    &[("Ctrl", 1.5), ("Win", 1.3), ("Alt", 1.3), ("Space", 4.6), ("Alt", 1.3), ("Left", 1.0), ("Down", 1.0), ("Right", 1.0)],
];

fn keyboard(pm: &mut Pixmap, t_ms: u64, events: &[InputEvent]) {
    let down = keys_down(t_ms, events);
    let h = pm.height() as f32;
    let k = (h * 0.035).clamp(12.0, 26.0);
    let gap = (k * 0.15).max(2.0);
    let row_w = |row: &[(&str, f32)]| row.iter().map(|(_, u)| u * k + gap).sum::<f32>() - gap;
    let width = KEYBOARD_ROWS.iter().map(|r| row_w(r)).fold(0.0, f32::max);
    let height = KEYBOARD_ROWS.len() as f32 * (k + gap) - gap;
    let pad = k * 0.4;
    let (x0, y0) = (h * 0.03 + pad, h - h * 0.03 - height - pad);
    fill_rounded(pm, x0 - pad, y0 - pad, width + pad * 2.0, height + pad * 2.0, k * 0.4, PANEL);
    let font = (k * 0.42).max(7.0);
    let lh = line_height_px(font);
    for (r, row) in KEYBOARD_ROWS.iter().enumerate() {
        let mut x = x0;
        let y = y0 + r as f32 * (k + gap);
        for (label, units) in row.iter() {
            let kw = units * k;
            let on = down.iter().any(|d| d == label);
            fill_rounded(pm, x, y, kw, k, k * 0.18, if on { KEY_ON } else { KEY_BG });
            let tw = measure_text(label, font).0;
            draw_lines(pm, &[label.to_string()], x + (kw - tw) / 2.0, y + (k - lh) / 2.0, font, if on { Rgba::WHITE } else { Rgba([163, 170, 184, 255]) });
            x += kw + gap;
        }
    }
}

fn ripples(pm: &mut Pixmap, t_ms: u64, events: &[InputEvent]) {
    for e in events.iter().filter(|e| e.t_ms <= t_ms) {
        if let InputKind::MouseDown(_, x, y) = e.kind {
            let age = t_ms - e.t_ms;
            if age <= RIPPLE_MS {
                let f = age as f32 / RIPPLE_MS as f32;
                let r = 8.0 + 26.0 * f;
                let a = ((1.0 - f) * 255.0) as u8;
                fill_circle(pm, x as f32, y as f32, r, Rgba([255, 140, 26, a / 5]));
                stroke_circle(pm, x as f32, y as f32, r, 3.0, Rgba([255, 140, 26, a]));
            }
        }
    }
}

fn mouse_widget(pm: &mut Pixmap, t_ms: u64, events: &[InputEvent]) {
    let held = buttons_down(t_ms, events);
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let m = (h * 0.11).clamp(40.0, 90.0);
    let (mw, mh) = (m * 0.62, m);
    let pad = m * 0.18;
    let label = if held.contains(&MouseButton::Left) {
        "Left click"
    } else if held.contains(&MouseButton::Right) {
        "Right click"
    } else if held.contains(&MouseButton::Middle) {
        "Middle click"
    } else {
        ""
    };
    let font = (m * 0.16).max(9.0);
    let lh = line_height_px(font);
    let panel_h = mh + pad * 2.0 + lh + pad * 0.5;
    let x0 = w - h * 0.03 - mw - pad * 2.0;
    let y0 = h - h * 0.03 - panel_h;
    fill_rounded(pm, x0, y0, mw + pad * 2.0, panel_h, m * 0.15, PANEL);
    let (bx, by) = (x0 + pad, y0 + pad);
    let body = Rgba([42, 48, 59, 255]);
    fill_rounded(pm, bx, by, mw, mh, mw * 0.45, body);
    let split = by + mh * 0.42;
    // Button halves, lit when pressed: the top-left or top-right quarter of
    // the rounded body, cut at the split line.
    let r = mw * 0.45;
    let k = 0.4477; // control-point inset for a quarter circle
    for (left, on) in [(true, held.contains(&MouseButton::Left)), (false, held.contains(&MouseButton::Right))] {
        if !on {
            continue;
        }
        let (edge, mid) = if left { (bx, bx + mw / 2.0) } else { (bx + mw, bx + mw / 2.0) };
        let dir = if left { 1.0 } else { -1.0 };
        let mut pb = PathBuilder::new();
        pb.move_to(mid, split);
        pb.line_to(edge, split);
        pb.line_to(edge, by + r);
        pb.cubic_to(edge, by + r * k, edge + dir * r * k, by, edge + dir * r, by);
        pb.line_to(mid, by);
        pb.close();
        if let Some(path) = pb.finish() {
            pm.fill_path(&path, &paint(ORANGE), FillRule::Winding, Transform::identity(), None);
        }
    }
    let line = Rgba([90, 99, 117, 255]);
    stroke_line(pm, bx + mw / 2.0, by, bx + mw / 2.0, split, 1.5, line);
    stroke_line(pm, bx, split, bx + mw, split, 1.5, line);
    let wheel_on = held.contains(&MouseButton::Middle);
    fill_rounded(pm, bx + mw / 2.0 - mw * 0.08, by + mh * 0.12, mw * 0.16, mh * 0.2, mw * 0.08, if wheel_on { ORANGE } else { Rgba([17, 20, 26, 255]) });
    if !label.is_empty() {
        let tw = measure_text(label, font).0;
        draw_lines(pm, &[label.to_string()], x0 + (mw + pad * 2.0 - tw) / 2.0, by + mh + pad * 0.5, font, Rgba([255, 176, 102, 255]));
    }
}

/// The standard arrow pointer, `#` outline and `o` fill.
const ARROW: [&str; 19] = [
    "#..........",
    "##.........",
    "#o#........",
    "#oo#.......",
    "#ooo#......",
    "#oooo#.....",
    "#ooooo#....",
    "#oooooo#...",
    "#ooooooo#..",
    "#oooooooo#.",
    "#ooooo#####",
    "#oo#oo#....",
    "#o#.#oo#...",
    "##..#oo#...",
    "#....#oo#..",
    ".....#oo#..",
    "......#oo#.",
    "......#oo#.",
    ".......##..",
];

fn draw_pointer(pm: &mut Pixmap, x: i32, y: i32) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let px = pm.pixels_mut();
    for (dy, row) in ARROW.iter().enumerate() {
        for (dx, ch) in row.chars().enumerate() {
            let c = match ch {
                '#' => tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 255),
                'o' => tiny_skia::PremultipliedColorU8::from_rgba(255, 255, 255, 255),
                _ => continue,
            };
            let (tx, ty) = (x + dx as i32, y + dy as i32);
            if tx >= 0 && ty >= 0 && tx < w && ty < h {
                if let Some(c) = c {
                    px[(ty * w + tx) as usize] = c;
                }
            }
        }
    }
}

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.0[0], c.0[1], c.0[2], c.0[3]);
    p.anti_alias = true;
    p
}

fn fill_rounded(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
    if let Some(path) = Rect::from_xywh(x, y, w.max(1.0), h.max(1.0)).and_then(|rect| rounded_rect(rect, r)) {
        pm.fill_path(&path, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

fn fill_circle(pm: &mut Pixmap, x: f32, y: f32, r: f32, c: Rgba) {
    if let Some(path) = PathBuilder::from_circle(x, y, r) {
        pm.fill_path(&path, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke_circle(pm: &mut Pixmap, x: f32, y: f32, r: f32, width: f32, c: Rgba) {
    if let Some(path) = PathBuilder::from_circle(x, y, r) {
        pm.stroke_path(&path, &paint(c), &Stroke { width, ..Default::default() }, Transform::identity(), None);
    }
}

fn stroke_line(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, &paint(c), &Stroke { width, ..Default::default() }, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t_ms: u64, kind: InputKind) -> InputEvent {
        InputEvent { t_ms, kind }
    }

    fn key(t: u64, k: &str, down: bool) -> InputEvent {
        ev(t, if down { InputKind::KeyDown(k.into()) } else { InputKind::KeyUp(k.into()) })
    }

    #[test]
    fn combo_with_modifiers_and_repeat_count() {
        let events = vec![key(100, "Ctrl", true), key(120, "Shift", true), key(200, "S", true), key(250, "S", false), key(400, "S", true), key(450, "S", false), key(500, "Shift", false), key(520, "Ctrl", false)];
        let (combo, count, _) = last_combo(600, &events).unwrap();
        assert_eq!(combo, vec!["Ctrl", "Shift", "S"]);
        assert_eq!(count, 2);
        assert!(last_combo(400 + CHIP_WINDOW_MS + 1, &events).is_none());
    }

    #[test]
    fn held_keys_and_buttons() {
        let events = vec![key(0, "A", true), key(500, "A", false), ev(600, InputKind::MouseDown(MouseButton::Left, 5, 5)), ev(900, InputKind::MouseUp(MouseButton::Left, 5, 5))];
        assert_eq!(keys_down(300, &events), vec!["A".to_string()]);
        // A key stays lit briefly after release, so quick taps still show.
        assert_eq!(keys_down(600, &events), vec!["A".to_string()]);
        assert!(keys_down(700, &events).is_empty());
        assert_eq!(buttons_down(800, &events), vec![MouseButton::Left]);
        assert!(buttons_down(1100, &events).is_empty());
    }
}
