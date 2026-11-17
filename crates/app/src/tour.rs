//! Tour export: turns a recording into a tour bundle, one image per click.
//!
//! A tour step is the frame from just before a left click plus where the
//! click landed. Step images are drawn by [`crate::video::decorate`] with the
//! overlays off, so annotations and crop match every other export.

mod encode;

pub use encode::*;

use std::path::{Path, PathBuf};

use image::RgbaImage;
use mythicshot_editor::Item;
use mythicshot_editor::overlays::{ClickStyle, InputEvent, InputKind, KeyStyle, MouseButton, OverlayStyle};
use mythicshot_recording::Recording;

use crate::video::{Edit, crop_px, decorate};

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

const NO_CLICKS: &str = "This recording has no left clicks inside the kept frames, so there are no tour steps.";

fn pct(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// One step per left press (merged within [`MERGE_MS`]), then a final step on the last kept frame.
pub fn plan_steps(rec: &Recording, edit: &Edit, events: &[InputEvent]) -> Result<Vec<StepPlan>, String> {
    if rec.frames.is_empty() {
        return Err(NO_CLICKS.to_string());
    }
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
        return Err(NO_CLICKS.to_string());
    }
    if let Some(last) = (0..rec.frames.len()).rev().find(|&i| edit.kept(i))
        && steps.last().is_some_and(|s| s.src != last)
    {
        steps.push(StepPlan { src: last, click: None, t_ms: rec.frames[last].t_ms });
    }
    Ok(steps)
}

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ManifestStep {
    pub image: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub click: Option<Point>,
    pub t_ms: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Manifest {
    pub version: u32,
    pub generator: String,
    pub title: String,
    pub steps: Vec<ManifestStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TourSummary {
    pub dir: PathBuf,
    pub steps: usize,
    pub bytes: u64,
}

const PREVIEW_TEMPLATE: &str = include_str!("tour_preview.html");
const JSON_PLACEHOLDER: &str = "/*TOUR_JSON*/";

/// Write a tour bundle into `dir`, which must not exist or must be empty.
/// `frame(i)` returns source frame `i`. `progress` gets 0.0-1.0 and returns false to cancel.
/// On any error after `dir` was created, `dir` is removed again.
#[allow(clippy::too_many_arguments)]
pub fn write_bundle(
    rec: &Recording,
    items: &[Item],
    events: &[InputEvent],
    edit: &Edit,
    title: &str,
    dir: &Path,
    frame: impl FnMut(usize) -> Result<RgbaImage, String>,
    progress: impl FnMut(f32) -> bool,
) -> Result<TourSummary, String> {
    let plan = plan_steps(rec, edit, events)?;
    if dir.exists() {
        let has_entries = std::fs::read_dir(dir).map_err(|e| format!("Could not read {}: {e}", dir.display()))?.next().is_some();
        if has_entries {
            return Err(format!("{} is not empty", dir.display()));
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    match write_into(rec, items, events, edit, title, dir, &plan, frame, progress) {
        Ok(summary) => Ok(summary),
        Err(e) => {
            let _ = std::fs::remove_dir_all(dir);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_into(
    rec: &Recording,
    items: &[Item],
    events: &[InputEvent],
    edit: &Edit,
    title: &str,
    dir: &Path,
    plan: &[StepPlan],
    mut frame: impl FnMut(usize) -> Result<RgbaImage, String>,
    mut progress: impl FnMut(f32) -> bool,
) -> Result<TourSummary, String> {
    // Same crop and annotations as every export, but no scale and no pointer, ripple, halo or keys.
    let mut tour_edit = edit.clone();
    tour_edit.scale = 100;
    tour_edit.overlays = OverlayStyle { keys: KeyStyle::Off, clicks: ClickStyle::Off, halo: false, pointer: false };

    let n = plan.len();
    let mut steps = Vec::with_capacity(n);
    let mut total = 0u64;
    for (i, step) in plan.iter().enumerate() {
        if !progress(i as f32 / n as f32) {
            return Err("cancelled".to_string());
        }
        let img = decorate(rec, step.src, frame(step.src)?, items, events, &tour_edit, true);
        let img = fit_image(img);
        let (bytes, ext) = encode_step(&img)?;
        let name = format!("step-{:03}.{ext}", i + 1);
        std::fs::write(dir.join(&name), &bytes).map_err(|e| format!("Could not write {name}: {e}"))?;
        total += bytes.len() as u64;
        steps.push(ManifestStep {
            image: name,
            width: img.width(),
            height: img.height(),
            bytes: bytes.len() as u64,
            click: step.click.map(|(x, y)| Point { x, y }),
            t_ms: step.t_ms,
        });
    }
    let manifest = Manifest { version: MANIFEST_VERSION, generator: format!("mythicshot {}", env!("CARGO_PKG_VERSION")), title: title.to_string(), steps };

    let pretty = serde_json::to_string_pretty(&manifest).map_err(|e| format!("Could not write tour.json: {e}"))?;
    std::fs::write(dir.join("tour.json"), pretty).map_err(|e| format!("Could not write tour.json: {e}"))?;

    // `<` becomes a JSON escape so no title can close the page's script block.
    let compact = serde_json::to_string(&manifest).map_err(|e| format!("Could not write index.html: {e}"))?.replace('<', r"\u003c");
    let html = PREVIEW_TEMPLATE.replacen(JSON_PLACEHOLDER, &compact, 1);
    std::fs::write(dir.join("index.html"), html).map_err(|e| format!("Could not write index.html: {e}"))?;

    progress(1.0);
    Ok(TourSummary { dir: dir.to_path_buf(), steps: n, bytes: total })
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

    #[test]
    fn crop_edges_are_exact() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        let s = plan_steps(&r, &e, &[left(750, 49, 40), left(1250, 50, 40), left(1750, 100, 40)]).unwrap();
        assert_eq!(shape(&s), vec![(2, Some((0.0, 50.0))), (4, None)]);
    }

    #[test]
    fn a_recording_with_no_frames_is_an_error() {
        let r = Recording::timing_only(100, 80, &[], 0);
        let err = plan_steps(&r, &edit(&r), &[left(0, 50, 40)]).unwrap_err();
        assert!(err.contains("no left clicks"), "{err}");
    }

    fn flat(_: usize) -> Result<RgbaImage, String> { Ok(RgbaImage::from_pixel(100, 80, image::Rgba([200, 200, 200, 255]))) }
    fn read_json(dir: &Path) -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(dir.join("tour.json")).unwrap()).unwrap() }

    #[test]
    fn bundle_has_images_manifest_and_preview() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("my-tour");
        let sum = write_bundle(&r, &[], &[left(750, 50, 40), left(1750, 10, 20)], &edit(&r), "My tour", &dir, flat, |_| true).unwrap();
        assert_eq!(sum.steps, 3);
        let j = read_json(&dir);
        assert_eq!(j["version"], 1);
        assert_eq!(j["title"], "My tour");
        assert!(j["generator"].as_str().unwrap().starts_with("mythicshot "));
        assert_eq!(j["steps"].as_array().unwrap().len(), 3);
        assert_eq!(j["steps"][0]["image"], "step-001.png");
        assert_eq!(j["steps"][0]["width"], 100);
        assert_eq!(j["steps"][0]["height"], 80);
        assert_eq!(j["steps"][0]["click"]["x"], 50.0);
        assert_eq!(j["steps"][1]["click"]["y"], 25.0);
        assert!(j["steps"][2]["click"].is_null());
        for s in j["steps"].as_array().unwrap() {
            let f = dir.join(s["image"].as_str().unwrap());
            assert_eq!(std::fs::metadata(&f).unwrap().len(), s["bytes"].as_u64().unwrap());
        }
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(html.contains("step-001.png") && !html.contains("/*TOUR_JSON*/"));
    }

    #[test]
    fn step_images_have_crop_applied_and_no_overlays() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let mut e = edit(&r); // default style draws the pointer and click ripple
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        e.scale = 50; // must be ignored
        write_bundle(&r, &[], &[left(750, 75, 40)], &e, "t", &dir, flat, |_| true).unwrap();
        let img = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (50, 80));
        assert!(img.pixels().all(|p| p.0 == [200, 200, 200, 255]), "an overlay was drawn on the step image");
    }

    #[test]
    fn refuses_a_folder_that_already_has_files() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("keep.txt"), "x").unwrap();
        let err = write_bundle(&r, &[], &[left(150, 50, 40)], &edit(&r), "t", tmp.path(), flat, |_| true).unwrap_err();
        assert!(err.contains("not empty"), "{err}");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[test]
    fn no_clicks_leaves_no_folder_behind() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        assert!(write_bundle(&r, &[], &[], &edit(&r), "t", &dir, flat, |_| true).is_err());
        assert!(!dir.exists());
    }

    #[test]
    fn cancel_or_a_frame_error_removes_the_partial_bundle() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let ev = [left(750, 50, 40), left(1750, 10, 20)];
        let dir = tmp.path().join("cancelled");
        let err = write_bundle(&r, &[], &ev, &edit(&r), "t", &dir, flat, |_| false).unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dir.exists());
        let dir = tmp.path().join("broken");
        let mut n = 0;
        let failing = |_: usize| { n += 1; if n == 2 { Err("disk read failed".to_string()) } else { flat(0) } };
        assert_eq!(write_bundle(&r, &[], &ev, &edit(&r), "t", &dir, failing, |_| true).unwrap_err(), "disk read failed");
        assert!(!dir.exists());
    }

    #[test]
    fn keys_and_script_breakouts_never_reach_the_bundle() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let ev = [left(150, 50, 40), InputEvent { t_ms: 160, kind: InputKind::KeyDown("Hunter2Key".into()) }];
        write_bundle(&r, &[], &ev, &edit(&r), "</script><b>x", &dir, flat, |_| true).unwrap();
        let json = std::fs::read_to_string(dir.join("tour.json")).unwrap();
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(!json.contains("Hunter2Key") && !html.contains("Hunter2Key"));
        assert!(!html.contains("</script><b>x"));
        assert_eq!(read_json(&dir)["title"], "</script><b>x");
    }
}
