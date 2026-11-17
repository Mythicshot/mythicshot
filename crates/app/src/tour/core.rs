//! The geometry and text rules every tour player must agree with: card placement,
//! zoom and bold text. Mirrors `tour_core.js`; both are tested against
//! `tests/fixtures/tour-core-cases.json`. Links, colours and text clean-up live in `doc.rs`
//! and are checked against the same fixture here.
#![cfg_attr(not(test), expect(dead_code, reason = "used by the guide card overlay, which is not built yet"))]

use mythicshot_editor::Bounds;

use super::Placement;

const MARGIN: f32 = 8.0;
const GAP: f32 = 12.0;
const BOTTOM_INSET: f32 = 16.0;
/// A fit within this many pixels counts, so float32 and float64 callers agree on ties.
const SLACK: f32 = 0.001;
/// Half the box kept clear around an aimed spot when nothing else fits: the players' 44 px anchor box.
const AIM_HALF: f32 = 22.0;

/// Where a card ended up, in image pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub side: Side,
}

/// The side of the avoided box the card sits on. `None`: there was nothing to avoid,
/// bottom centre. `Docked`: no side fits, the card goes under the image at full width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Bottom,
    Top,
    Right,
    Left,
    None,
    Docked,
}

/// Same as `Math.min(Math.max(v, lo), hi)` for finite input: never panics when `lo > hi`. Callers
/// check for non-finite input first, since `f32::max` and `Math.max` treat NaN differently.
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    v.max(lo).min(hi)
}

/// Places a `card` on an `img` (both width, height) so it never covers `avoid` and never leaves the image.
/// The asked side is tried first, then bottom, top, right, left. A box holding a non-finite value docks the card.
///
/// `aim` is a spot the card points at, such as a spot picked inside or beside a spotlight: the card is
/// centred on the aim along the side it takes, and when the aim lies outside the box the side facing it
/// is tried right after the asked side. The card first keeps clear of the box and the aim together;
/// when no side fits that, of the box alone (still pointing at the aim); then of a box around the aim
/// alone; only then is it docked. An aim that is not finite is ignored.
pub fn place_card(img: (f32, f32), card: (f32, f32), avoid: Option<Bounds>, placement: Placement, aim: Option<(f32, f32)>) -> Placed {
    let ((iw, ih), (cw, ch)) = (img, card);
    let Some(b) = avoid else {
        return Placed { x: (iw - cw) / 2.0, y: ih - ch - BOTTOM_INSET, side: Side::None };
    };
    let docked = Placed { x: 0.0, y: ih, side: Side::Docked };
    if ![b.x0, b.y0, b.x1, b.y1].iter().all(|v| v.is_finite()) {
        return docked;
    }
    let b = b.norm();
    let mut order = vec![Side::Bottom, Side::Top, Side::Right, Side::Left];
    let aim = aim.filter(|(x, y)| x.is_finite() && y.is_finite());
    if let Some((ax, ay)) = aim {
        // The side of the box the aim lies beyond, if any: the one it is furthest past.
        let past = [(Side::Bottom, ay - b.y1), (Side::Top, b.y0 - ay), (Side::Right, ax - b.x1), (Side::Left, b.x0 - ax)];
        let mut facing = None;
        let mut most = 0.0;
        for (side, d) in past {
            if d > most {
                (most, facing) = (d, Some(side));
            }
        }
        if let Some(f) = facing {
            order.retain(|s| *s != f);
            order.insert(0, f);
        }
    }
    let asked = match placement {
        Placement::Auto => None,
        Placement::Top => Some(Side::Top),
        Placement::Bottom => Some(Side::Bottom),
        Placement::Left => Some(Side::Left),
        Placement::Right => Some(Side::Right),
    };
    if let Some(a) = asked {
        order.retain(|s| *s != a);
        order.insert(0, a);
    }
    // The first side of `order` on which the card keeps clear of `k`, centred on `(mx, my)` along it.
    let fit = |k: Bounds, (mx, my): (f32, f32)| {
        order.iter().find_map(|&side| {
            let (x, y) = match side {
                Side::Bottom | Side::Top => {
                    let x = clamp(mx - cw / 2.0, MARGIN, iw - cw - MARGIN);
                    (x, if side == Side::Bottom { k.y1 + GAP } else { k.y0 - GAP - ch })
                }
                _ => {
                    let y = clamp(my - ch / 2.0, MARGIN, ih - ch - MARGIN);
                    (if side == Side::Right { k.x1 + GAP } else { k.x0 - GAP - cw }, y)
                }
            };
            (x >= MARGIN - SLACK && y >= MARGIN - SLACK && x + cw <= iw - MARGIN + SLACK && y + ch <= ih - MARGIN + SLACK).then_some(Placed { x, y, side })
        })
    };
    let Some((ax, ay)) = aim else {
        return fit(b, ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0)).unwrap_or(docked);
    };
    let both = Bounds { x0: b.x0.min(ax), y0: b.y0.min(ay), x1: b.x1.max(ax), y1: b.y1.max(ay) };
    let around = Bounds { x0: ax - AIM_HALF, y0: ay - AIM_HALF, x1: ax + AIM_HALF, y1: ay + AIM_HALF };
    fit(both, (ax, ay)).or_else(|| fit(b, (ax, ay))).or_else(|| fit(around, (ax, ay))).unwrap_or(docked)
}

/// The `(tx, ty)` translation for a `view` (width, height) zoomed by `k` around `focus` that never shows
/// anything outside the image. Any non-finite input means no zoom: `(0, 0)`, and the caller draws at scale 1.
pub fn zoom_offset(view: (f32, f32), focus: (f32, f32), k: f32) -> (f32, f32) {
    if ![view.0, view.1, focus.0, focus.1, k].iter().all(|v| v.is_finite()) {
        return (0.0, 0.0);
    }
    let axis = |len: f32, f: f32| clamp(len / 2.0 - f * k, len - len * k, 0.0);
    (axis(view.0, focus.0), axis(view.1, focus.1))
}

/// `(text, bold)` pieces: text between a pair of `**` is bold, an unpaired `**` stays as typed, empty
/// pieces are dropped. CRLF and CR become LF.
pub fn parse_bold(text: &str) -> Vec<(String, bool)> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut parts: Vec<String> = text.split("**").map(str::to_string).collect();
    if parts.len().is_multiple_of(2) {
        let last = parts.pop().unwrap_or_default();
        if let Some(prev) = parts.last_mut() {
            prev.push_str("**");
            prev.push_str(&last);
        }
    }
    parts.into_iter().enumerate().filter(|(_, p)| !p.is_empty()).map(|(i, p)| (p, i % 2 == 1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::doc::{clean, hex_colour};
    use crate::tour::safe_url;
    use serde_json::Value;

    const CASES: &str = include_str!("../../tests/fixtures/tour-core-cases.json");

    /// A number, or one of the strings the fixture uses for non-finite numbers.
    fn f(v: &Value) -> f32 {
        match v.as_str() {
            Some("NaN") => f32::NAN,
            Some("Infinity") => f32::INFINITY,
            Some("-Infinity") => f32::NEG_INFINITY,
            _ => v.as_f64().unwrap() as f32,
        }
    }

    fn pair(v: &Value) -> (f32, f32) {
        (f(&v[0]), f(&v[1]))
    }

    fn side_name(s: Side) -> &'static str {
        match s {
            Side::Bottom => "bottom",
            Side::Top => "top",
            Side::Right => "right",
            Side::Left => "left",
            Side::None => "none",
            Side::Docked => "docked",
        }
    }

    fn placement(s: &str) -> Placement {
        match s {
            "auto" => Placement::Auto,
            "top" => Placement::Top,
            "bottom" => Placement::Bottom,
            "left" => Placement::Left,
            "right" => Placement::Right,
            other => panic!("unknown placement {other}"),
        }
    }

    fn close(a: f32, b: f32, what: &str) {
        assert!((a - b).abs() <= 0.01, "{what}: {a} vs {b}");
    }

    #[test]
    fn matches_the_shared_cases() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        for c in cases["place"].as_array().unwrap() {
            let name = c["name"].as_str().unwrap();
            let (img, card) = (pair(&c["image"]), pair(&c["card"]));
            let avoid = c["avoid"].as_array().map(|a| Bounds { x0: f(&a[0]), y0: f(&a[1]), x1: f(&a[2]), y1: f(&a[3]) });
            let aim = c.get("aim").and_then(|a| a.as_array()).map(|a| (f(&a[0]), f(&a[1])));
            let p = place_card(img, card, avoid, placement(c["placement"].as_str().unwrap()), aim);
            close(p.x, f(&c["expect"]["x"]), name);
            close(p.y, f(&c["expect"]["y"]), name);
            assert_eq!(side_name(p.side), c["expect"]["side"].as_str().unwrap(), "{name}");
        }
        for c in cases["zoom"].as_array().unwrap() {
            let name = c["name"].as_str().unwrap();
            let (tx, ty) = zoom_offset(pair(&c["view"]), pair(&c["focus"]), f(&c["k"]));
            close(tx, f(&c["expect"]["tx"]), name);
            close(ty, f(&c["expect"]["ty"]), name);
        }
        for c in cases["bold"].as_array().unwrap() {
            let want: Vec<(String, bool)> = c["expect"].as_array().unwrap().iter().map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_bool().unwrap())).collect();
            assert_eq!(parse_bold(c["text"].as_str().unwrap()), want, "{}", c["name"]);
        }
        for c in cases["url"].as_array().unwrap() {
            let input = c["input"].as_str().unwrap();
            assert_eq!(safe_url(input).as_deref(), c["expect"].as_str(), "{input:?}");
        }
        for c in cases["text"].as_array().unwrap() {
            let got = clean(c["input"].as_str().unwrap(), c["max"].as_u64().unwrap() as usize, c["multiline"].as_bool().unwrap());
            assert_eq!(got, c["expect"].as_str().unwrap(), "{}", c["name"]);
        }
        for c in cases["color"].as_array().unwrap() {
            let input = c["input"].as_str().unwrap();
            assert_eq!(hex_colour(input).as_deref(), c["expect"].as_str(), "{input:?}");
        }
    }
}
