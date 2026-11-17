//! Tour export: turns a recording into a tour bundle, one image per click.
//!
//! A tour step is the frame from just before a left click plus where the
//! click landed. Step images are drawn by [`crate::video::decorate`] with the
//! overlays off, so annotations and crop match every other export.

// Nothing calls into this module until the editor's "Export tour…" button
// lands; remove this line then.
#![allow(dead_code)]

mod encode;

pub use encode::*;

use mythicshot_editor::overlays::{InputEvent, InputKind, MouseButton};
use mythicshot_recording::Recording;

use crate::video::{Edit, crop_px};

/// Presses closer together than this are one step.
pub const MERGE_MS: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepPlan {
    /// Source frame index in the recording.
    pub src: usize,
    /// Hotspot centre as a percentage of the cropped image, or None for the final step.
    pub click: Option<(f32, f32)>,
    /// Recording-clock time of the press, or of the frame for the final step.
    pub t_ms: u64,
}

fn pct(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// One step per left press (merged within [`MERGE_MS`]), then a final step on the last kept frame.
pub fn plan_steps(rec: &Recording, edit: &Edit, events: &[InputEvent]) -> Result<Vec<StepPlan>, String> {
    let mut presses: Vec<(u64, i32, i32)> = events
        .iter()
        .filter_map(|e| match e.kind {
            InputKind::MouseDown(MouseButton::Left, x, y) => Some((e.t_ms, x, y)),
            _ => None,
        })
        .collect();
    presses.sort_by_key(|p| p.0);

    let (cx, cy, cw, ch) = crop_px(rec, edit);
    let mut steps: Vec<StepPlan> = Vec::new();
    let mut prev: Option<u64> = None;
    for (t, x, y) in presses {
        let merged = prev.is_some_and(|p| t - p <= MERGE_MS);
        prev = Some(t);
        if merged {
            continue;
        }
        let src = rec.frames.iter().rposition(|f| f.t_ms <= t).unwrap_or(0);
        if !edit.kept(src) {
            continue;
        }
        let inside_frame = x >= 0 && y >= 0 && (x as u32) < rec.width && (y as u32) < rec.height;
        let (ux, uy) = (x as u32, y as u32);
        if !inside_frame || ux < cx || uy < cy || ux >= cx + cw || uy >= cy + ch {
            continue;
        }
        let px = pct((ux - cx) as f32 / cw as f32 * 100.0);
        let py = pct((uy - cy) as f32 / ch as f32 * 100.0);
        steps.push(StepPlan { src, click: Some((px, py)), t_ms: t });
    }
    if steps.is_empty() {
        return Err("This recording has no left clicks inside the kept frames, so there are no tour steps.".to_string());
    }
    if let Some(last) = (0..rec.frames.len()).rev().find(|&i| edit.kept(i))
        && steps.last().is_some_and(|s| s.src != last)
    {
        steps.push(StepPlan { src: last, click: None, t_ms: rec.frames[last].t_ms });
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
use super::*;
use mythicshot_editor::overlays::{InputEvent, InputKind, MouseButton, OverlayStyle};
use mythicshot_editor::Bounds;

// Five frames 500 ms apart (wider than MERGE_MS), 100×80, ending at 2500 ms.
fn rec() -> Recording { Recording::timing_only(100, 80, &[0, 500, 1000, 1500, 2000], 2500) }
fn edit(r: &Recording) -> Edit { Edit::new(r, OverlayStyle::default()) }
fn down(t_ms: u64, b: MouseButton, x: i32, y: i32) -> InputEvent { InputEvent { t_ms, kind: InputKind::MouseDown(b, x, y) } }
fn left(t_ms: u64, x: i32, y: i32) -> InputEvent { down(t_ms, MouseButton::Left, x, y) }
fn shape(s: &[StepPlan]) -> Vec<(usize, Option<(f32, f32)>)> { s.iter().map(|p| (p.src, p.click)).collect() }

#[test]
fn one_step_per_click_then_a_final_step() {
    let r = rec();
    let s = plan_steps(&r, &edit(&r), &[left(750, 50, 40), left(1750, 10, 20)]).unwrap();
    assert_eq!(shape(&s), vec![(1, Some((50.0, 50.0))), (3, Some((10.0, 25.0))), (4, None)]);
    assert_eq!(s.iter().map(|p| p.t_ms).collect::<Vec<_>>(), vec![750, 1750, 2000]);
}

#[test]
fn presses_within_400_ms_of_the_previous_press_are_one_step() {
    let r = rec();
    // 750 → 1000 → 1300: each is within 400 ms of the one before, so a triple-click is one step.
    let s = plan_steps(&r, &edit(&r), &[left(750, 50, 40), left(1000, 50, 40), left(1300, 50, 40)]).unwrap();
    assert_eq!(shape(&s), vec![(1, Some((50.0, 50.0))), (4, None)]);
}

#[test]
fn clicks_on_trimmed_or_deleted_frames_are_dropped() {
    let r = rec();
    let mut e = edit(&r);
    e.trim = (2, 4);
    e.deleted.insert(3);
    // 750 → frame 1 (trimmed), 1250 → frame 2 (kept), 1750 → frame 3 (deleted).
    let s = plan_steps(&r, &e, &[left(750, 50, 40), left(1250, 50, 40), left(1750, 50, 40)]).unwrap();
    assert_eq!(shape(&s), vec![(2, Some((50.0, 50.0))), (4, None)]);
}

#[test]
fn crop_remaps_positions_and_drops_clicks_outside_it() {
    let r = rec();
    let mut e = edit(&r);
    e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
    let s = plan_steps(&r, &e, &[left(750, 75, 40), left(1750, 10, 20)]).unwrap();
    assert_eq!(shape(&s), vec![(1, Some((50.0, 50.0))), (4, None)]);
}

#[test]
fn clicks_outside_the_recorded_area_are_dropped_not_clamped() {
    let r = rec();
    let ev = [left(750, -5, 10), left(1250, 100, 10), left(1750, 10, 80), left(2250, 99, 79)];
    let s = plan_steps(&r, &edit(&r), &ev).unwrap();
    assert_eq!(shape(&s), vec![(4, Some((99.0, 98.75)))]);
}

#[test]
fn only_left_presses_make_steps_and_events_need_not_be_sorted() {
    let r = rec();
    let ev = [left(1750, 10, 20), down(1250, MouseButton::Right, 5, 5), InputEvent { t_ms: 1260, kind: InputKind::KeyDown("S".into()) }, left(750, 50, 40)];
    let s = plan_steps(&r, &edit(&r), &ev).unwrap();
    assert_eq!(shape(&s), vec![(1, Some((50.0, 50.0))), (3, Some((10.0, 25.0))), (4, None)]);
}

#[test]
fn no_final_step_when_the_last_click_is_on_the_last_kept_frame() {
    let r = rec();
    let s = plan_steps(&r, &edit(&r), &[left(2250, 50, 40)]).unwrap();
    assert_eq!(shape(&s), vec![(4, Some((50.0, 50.0)))]);
}

#[test]
fn a_click_before_the_first_frame_uses_frame_zero() {
    let r = Recording::timing_only(100, 80, &[50, 100], 200);
    let s = plan_steps(&r, &edit(&r), &[left(10, 50, 40)]).unwrap();
    assert_eq!(shape(&s), vec![(0, Some((50.0, 50.0))), (1, None)]);
}

#[test]
fn no_usable_clicks_is_an_error() {
    let r = rec();
    let err = plan_steps(&r, &edit(&r), &[down(750, MouseButton::Right, 5, 5)]).unwrap_err();
    assert!(err.contains("no left clicks"), "{err}");
}
}
