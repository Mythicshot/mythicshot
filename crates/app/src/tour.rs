//! Tour export: turns a recording into a tour bundle, one image per click.
//!
//! A tour step is the frame from just before a left click plus where the
//! click landed. Step images are drawn by [`crate::video::decorate`] with the
//! overlays off, so annotations and crop match every other export.

mod core;
mod doc;
mod encode;
mod layer;

pub use doc::*;
pub use encode::*;
pub use layer::*;
pub use self::core::{Placed, Side, parse_bold, place_card};

use std::path::{Path, PathBuf};

use image::RgbaImage;
use mythicshot_editor::Item;
use mythicshot_editor::overlays::{ClickStyle, InputEvent, InputKind, KeyStyle, MouseButton, OverlayStyle};
use mythicshot_recording::Recording;

use crate::video::{Edit, crop_px, decorate, step_items};

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
const ALL_REMOVED: &str = "Every step has been removed from the tour.";

fn pct(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// One step per left press (merged within [`MERGE_MS`]), then a final step on the last kept frame.
/// Steps removed in the Tour view (`edit.tour_removed`) are dropped afterwards.
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
    steps.retain(|s| !edit.tour_removed.contains(&s.src));
    if steps.is_empty() {
        return Err(ALL_REMOVED.to_string());
    }
    Ok(steps)
}

pub const MANIFEST_VERSION: u32 = 3;

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
    /// Tour layer: the step's spotlight (null when none) and its notes.
    pub spotlight: Option<Area>,
    pub notes: Vec<Note>,
    /// Version 3: the step's guide card (null when none), how the step advances and its zoom
    /// (null follows the tour).
    pub card: Option<Card>,
    pub advance: Advance,
    pub zoom: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Manifest {
    pub version: u32,
    pub generator: String,
    pub title: String,
    pub steps: Vec<ManifestStep>,
    /// Darkening strength in percent, shared by every spotlight; present only when a step has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spotlight_dim: Option<u8>,
    /// Version 3 tour settings, always written, sanitised by [`sanitize`].
    pub theme: Theme,
    pub zoom: Zoom,
    pub cover: Option<Cover>,
    pub cta: Option<Link>,
    pub end: Option<EndCard>,
    pub autoplay: Autoplay,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TourSummary {
    pub dir: PathBuf,
    pub steps: usize,
    pub bytes: u64,
}

const PREVIEW_TEMPLATE: &str = include_str!("tour_preview.html");
const JSON_PLACEHOLDER: &str = "/*TOUR_JSON*/";
/// The shared rules every player uses, inlined into the preview's first script block.
const TOUR_CORE_JS: &str = include_str!("tour_core.js");
const CORE_PLACEHOLDER: &str = "/*TOUR_CORE*/";

/// The preview page for an already escaped compact manifest. The rules go in first, from the
/// template, so text in the manifest can never be taken for a placeholder.
fn preview_page(compact_json: &str) -> String {
    PREVIEW_TEMPLATE.replacen(CORE_PLACEHOLDER, TOUR_CORE_JS, 1).replacen(JSON_PLACEHOLDER, compact_json, 1)
}

/// Write a tour bundle into `dir`, which must not exist yet; its parent folder must.
/// `frame(i)` returns source frame `i`. `progress` gets 0.0-1.0 and returns false to cancel.
/// Only a folder this call created is ever removed: if anything fails after `create_dir`
/// succeeded, `dir` is removed again. An existing `dir` is never touched.
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
    if dir.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", dir.display()));
    }
    // The settings are sanitised and the logo loaded before the folder exists, so a bad logo writes nothing.
    // A logo travels only with a cover.
    let doc = sanitize(&edit.tour_doc);
    let logo = match (&doc.cover, &doc.logo_path) {
        (Some(_), Some(path)) => Some(load_logo(path)?),
        _ => None,
    };
    // Not create_dir_all: a missing parent is an error, and a folder that appears
    // between the check above and here makes this call fail instead of being reused.
    std::fs::create_dir(dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists { format!("{} already exists", dir.display()) } else { format!("Could not create {}: {e}", dir.display()) }
    })?;
    match write_into(rec, items, events, edit, title, dir, &plan, doc, logo, frame, progress) {
        Ok(summary) => Ok(summary),
        Err(e) => match std::fs::remove_dir_all(dir) {
            Ok(()) => Err(e),
            Err(_) => Err(format!("{e} (the partly written folder {} could not be removed)", dir.display())),
        },
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
    doc: TourDoc,
    logo: Option<(Vec<u8>, &'static str)>,
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
        // Layer items are data; everything else on the step is burned in. Pins and ranges are
        // cleared because `step_items` already chose what this step shows.
        let burned: Vec<Item> = step_items(items, step.src).into_iter().filter(|it| !is_layer(it)).map(|it| Item { pin: None, frames: None, ..it }).collect();
        let img = decorate(rec, step.src, frame(step.src)?, &burned, events, &tour_edit, true);
        let layer = step_layer(rec, edit, items, step.src);
        let img = fit_image(img);
        let (bytes, ext) = encode_step(&img)?;
        let name = format!("step-{:03}.{ext}", i + 1);
        std::fs::write(dir.join(&name), &bytes).map_err(|e| format!("Could not write {name}: {e}"))?;
        total += bytes.len() as u64;
        let extras = doc.steps.get(&step.src);
        steps.push(ManifestStep {
            image: name,
            width: img.width(),
            height: img.height(),
            bytes: bytes.len() as u64,
            click: step.click.map(|(x, y)| Point { x, y }),
            t_ms: step.t_ms,
            spotlight: layer.spotlight,
            notes: layer.notes,
            card: step_card(rec, edit, step.src, step.click),
            advance: extras.map(|x| x.advance).unwrap_or_default(),
            zoom: extras.and_then(|x| x.zoom),
        });
    }
    let srcs: Vec<usize> = plan.iter().map(|s| s.src).collect();
    let spotlight_dim = tour_dim(rec, edit, items, &srcs);
    let mut cover = doc.cover;
    if let (Some(c), Some((bytes, ext))) = (cover.as_mut(), &logo) {
        let name = format!("logo.{ext}");
        std::fs::write(dir.join(&name), bytes).map_err(|e| format!("Could not write {name}: {e}"))?;
        total += bytes.len() as u64;
        c.logo = Some(name);
    }
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        generator: format!("mythicshot {}", env!("CARGO_PKG_VERSION")),
        title: title.to_string(),
        steps,
        spotlight_dim,
        theme: doc.theme,
        zoom: doc.zoom,
        cover,
        cta: doc.cta,
        end: doc.end,
        autoplay: doc.autoplay,
    };

    let pretty = serde_json::to_string_pretty(&manifest).map_err(|e| format!("Could not write tour.json: {e}"))?;
    std::fs::write(dir.join("tour.json"), pretty).map_err(|e| format!("Could not write tour.json: {e}"))?;

    // `<` becomes a JSON escape so no title can close the page's script block.
    let compact = serde_json::to_string(&manifest).map_err(|e| format!("Could not write index.html: {e}"))?.replace('<', r"\u003c");
    let html = preview_page(&compact);
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
        assert_eq!(j["version"], 3);
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
        let mut r = rec();
        // The frame used for the step (frame 1, 500 ms) has a pointer inside the crop.
        r.frames[1].cursor = Some((75, 40));
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let mut e = Edit::new(&r, OverlayStyle { keys: KeyStyle::Chips, clicks: ClickStyle::Both, halo: true, pointer: true });
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        e.scale = 50; // must be ignored
        // A right press and a key just before the frame time, so a ripple and a key chip would be visible on it.
        let ev = [down(480, MouseButton::Right, 75, 40), InputEvent { t_ms: 490, kind: InputKind::KeyDown("K".into()) }, left(750, 75, 40)];
        write_bundle(&r, &[], &ev, &e, "t", &dir, flat, |_| true).unwrap();
        let img = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (50, 80));
        assert!(img.pixels().all(|p| p.0 == [200, 200, 200, 255]), "an overlay was drawn on the step image");
    }

    #[test]
    fn refuses_a_folder_that_already_exists() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let ev = [left(150, 50, 40)];

        let full = tmp.path().join("full");
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("keep.txt"), "x").unwrap();
        let err = write_bundle(&r, &[], &ev, &edit(&r), "t", &full, flat, |_| true).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read_dir(&full).unwrap().count(), 1);
        assert!(full.join("keep.txt").exists());

        let empty = tmp.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let err = write_bundle(&r, &[], &ev, &edit(&r), "t", &empty, flat, |_| true).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(empty.is_dir());
        assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);

        let parent = tmp.path().join("no-such-parent");
        assert!(write_bundle(&r, &[], &ev, &edit(&r), "t", &parent.join("t"), flat, |_| true).is_err());
        assert!(!parent.exists());
    }

    #[test]
    fn preview_controls_are_keyboard_reachable_buttons() {
        assert!(PREVIEW_TEMPLATE.contains(r#"<button id="ring""#) && PREVIEW_TEMPLATE.contains(r#"aria-label="Next step""#));
        assert!(PREVIEW_TEMPLATE.contains(r#"<button id="restart""#));
        assert!(PREVIEW_TEMPLATE.contains(":focus-visible"));
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

    use mythicshot_editor::{Item, Rgba, Shape, Style, pt};
    fn pinned(shape: Shape, src: u32) -> Item {
        Item { id: 0, shape, style: Style { stroke: Rgba([255, 0, 0, 255]), fill: None, width: 3.0, font_size: 20.0, shadow: false }, frames: None, pin: Some(src) }
    }
    fn text(x: f32, y: f32, s: &str, src: u32) -> Item { pinned(Shape::Text { at: pt(x, y), text: s.into() }, src) }
    fn spot(b: Bounds, src: u32) -> Item { pinned(Shape::Spotlight { rect: b, dim: 60 }, src) }
    const CLICKS: [(u64, i32, i32); 2] = [(750, 50, 40), (1750, 10, 20)]; // steps on frames 1, 3, then the end step on 4
    fn clicks() -> Vec<InputEvent> { CLICKS.iter().map(|&(t, x, y)| left(t, x, y)).collect() }

    #[test]
    fn pinned_text_and_spotlight_become_layer_data_as_percentages() {
        let r = rec();
        let items = [text(10.0, 20.0, "Unpaid invoices", 1), spot(Bounds { x0: 20.0, y0: 20.0, x1: 70.0, y1: 60.0 }, 1)];
        let l = step_layer(&r, &edit(&r), &items, 1);
        assert_eq!(l.spotlight, Some(Area { x: 20.0, y: 25.0, w: 50.0, h: 50.0 }));
        assert_eq!(l.notes, vec![Note::Text { x: 10.0, y: 25.0, text: "Unpaid invoices".into(), size: 25.0, color: "#ff0000".into() }]);
        assert_eq!(step_layer(&r, &edit(&r), &items, 3), StepLayer::default());
        assert_eq!(tour_dim(&r, &edit(&r), &items, &[1, 3, 4]), Some(60));
        assert_eq!(tour_dim(&r, &edit(&r), &items, &[3, 4]), None);
    }

    #[test]
    fn bubbles_carry_their_box_tail_and_fill() {
        let r = rec();
        let mut b = pinned(Shape::Bubble { rect: Bounds { x0: 50.0, y0: 40.0, x1: 90.0, y1: 60.0 }, tail: pt(25.0, 20.0), text: "Here".into() }, 1);
        b.style.fill = None; // bubbles default to white
        let l = step_layer(&r, &edit(&r), &[b], 1);
        assert_eq!(l.notes, vec![Note::Bubble { x: 50.0, y: 50.0, w: 40.0, h: 25.0, tail: Point { x: 25.0, y: 25.0 }, text: "Here".into(), size: 25.0, color: "#ff0000".into(), fill: "#ffffff".into() }]);
    }

    #[test]
    fn crop_clips_the_spotlight_and_drops_notes_outside_it() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        let items = [text(10.0, 20.0, "gone", 1), text(60.0, 20.0, "kept", 1), spot(Bounds { x0: 20.0, y0: 20.0, x1: 70.0, y1: 60.0 }, 1)];
        let l = step_layer(&r, &e, &items, 1);
        assert_eq!(l.spotlight, Some(Area { x: 0.0, y: 25.0, w: 40.0, h: 50.0 }));
        assert_eq!(l.notes.len(), 1);
        assert!(matches!(&l.notes[0], Note::Text { x, text, .. } if *x == 20.0 && text == "kept"));
        // A spotlight wholly outside the crop is dropped.
        let l = step_layer(&r, &e, &[spot(Bounds { x0: 0.0, y0: 0.0, x1: 40.0, y1: 40.0 }, 1)], 1);
        assert_eq!(l.spotlight, None);
    }

    #[test]
    fn layer_items_are_data_and_everything_else_is_burned_into_its_own_step() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let mut cover = pinned(Shape::Rect(Bounds { x0: 0.0, y0: 0.0, x1: 20.0, y1: 20.0 }), 1);
        cover.style.fill = Some(Rgba([255, 0, 0, 255]));
        let items = [text(40.0, 40.0, "note", 1), spot(Bounds { x0: 30.0, y0: 30.0, x1: 90.0, y1: 70.0 }, 1), cover];
        write_bundle(&r, &items, &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let one = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert_eq!(one.get_pixel(10, 10).0, [255, 0, 0, 255], "the pinned box is burned in");
        assert_eq!(one.get_pixel(95, 75).0, [200, 200, 200, 255], "the pinned spotlight is not");
        assert_eq!(one.get_pixel(45, 45).0, [200, 200, 200, 255], "the pinned text is not");
        let two = image::open(dir.join("step-002.png")).unwrap().to_rgba8();
        assert!(two.pixels().all(|p| p.0 == [200, 200, 200, 255]), "pinned items stay on their own step");
        let j = read_json(&dir);
        assert_eq!(j["version"], 3);
        assert_eq!(j["spotlight_dim"], 60);
        assert_eq!(j["steps"][0]["spotlight"]["w"], 60.0);
        assert_eq!(j["steps"][0]["notes"][0]["kind"], "text");
        assert!(j["steps"][1]["spotlight"].is_null());
        assert_eq!(j["steps"][1]["notes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn an_unpinned_spotlight_is_burned_in_and_a_plain_recording_has_an_empty_layer() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let mut s = spot(Bounds { x0: 30.0, y0: 30.0, x1: 90.0, y1: 70.0 }, 0);
        s.pin = None; // on every frame, as drawn in Timeline mode with its range stretched
        write_bundle(&r, &[s], &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let one = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert_eq!(one.get_pixel(5, 5).0, [80, 80, 80, 255]);
        let j = read_json(&dir);
        assert!(j.get("spotlight_dim").is_none());
        assert!(j["steps"].as_array().unwrap().iter().all(|s| s["spotlight"].is_null() && s["notes"].as_array().unwrap().is_empty()));
    }

    #[test]
    fn pinned_items_never_show_in_timeline_mode_or_file_exports() {
        let items = [text(10.0, 20.0, "tour only", 1)];
        assert!(crate::video::visible(&items, 1).is_empty());
        assert_eq!(crate::video::step_items(&items, 1).len(), 1);
        assert!(crate::video::step_items(&items, 3).is_empty());
    }

    #[test]
    fn removed_and_trimmed_steps_drop_out_with_their_layer_and_come_back() {
        let r = rec();
        let items = [text(10.0, 20.0, "Sticky", 1)];
        let mut e = edit(&r);
        e.tour_removed.insert(1);
        assert_eq!(shape(&plan_steps(&r, &e, &clicks()).unwrap()), vec![(3, Some((10.0, 25.0))), (4, None)]);
        e.tour_removed.clear();
        e.trim = (2, 4);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &items, &clicks(), &e, "t", &dir, flat, |_| true).unwrap();
        assert!(!std::fs::read_to_string(dir.join("tour.json")).unwrap().contains("Sticky"));
        e.trim = (0, 4);
        assert_eq!(step_layer(&r, &e, &items, 1).notes.len(), 1);
    }

    #[test]
    fn removing_every_step_is_an_error_and_writes_nothing() {
        let r = rec();
        let mut e = edit(&r);
        e.tour_removed.extend([1, 3, 4]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let err = write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).unwrap_err();
        assert_eq!(err, "Every step has been removed from the tour.");
        assert!(!dir.exists());
    }

    #[test]
    fn note_size_never_exceeds_100() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 0.0, y0: 0.0, x1: 100.0, y1: 10.0 });
        let l = step_layer(&r, &e, &[text(5.0, 2.0, "big", 1)], 1);
        assert!(matches!(&l.notes[0], Note::Text { size, .. } if *size == 100.0), "{:?}", l.notes);
    }

    #[test]
    fn rounding_never_pushes_a_box_past_the_edge() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 0.0, y0: 0.0, x1: 3.0, y1: 3.0 });
        // 0.00075 px is 0.03 percent of 3 px; the box to the edge is 99.975, which rounds up to 99.98.
        let b = Bounds { x0: 0.00075, y0: 0.00075, x1: 10.0, y1: 10.0 };
        let bubble = pinned(Shape::Bubble { rect: b, tail: pt(1.0, 1.0), text: "Edge".into() }, 1);
        let l = step_layer(&r, &e, &[bubble, spot(b, 1)], 1);
        let Note::Bubble { x, y, w, h, size, .. } = l.notes[0].clone() else { panic!("{:?}", l.notes) };
        assert!(x + w <= 100.0 && y + h <= 100.0, "{x} + {w}, {y} + {h}");
        assert!(size > 0.0 && size <= 100.0);
        let a = l.spotlight.unwrap();
        assert!(a.x + a.w <= 100.0 && a.y + a.h <= 100.0, "{a:?}");
    }

    #[test]
    fn tour_dim_counts_only_exported_spotlights_and_stays_in_range() {
        let r = rec();
        let dimmed = |b: Bounds, src: u32, dim: u8| pinned(Shape::Spotlight { rect: b, dim }, src);
        let inside = Bounds { x0: 60.0, y0: 10.0, x1: 90.0, y1: 40.0 };
        let left_half = Bounds { x0: 0.0, y0: 0.0, x1: 40.0, y1: 40.0 };
        assert_eq!(tour_dim(&r, &edit(&r), &[dimmed(inside, 1, 0)], &[1, 3, 4]), Some(10));
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        assert_eq!(tour_dim(&r, &e, &[dimmed(left_half, 1, 90), dimmed(inside, 3, 60)], &[1, 3, 4]), Some(60));
        assert_eq!(tour_dim(&r, &edit(&r), &[dimmed(inside, 1, 80), dimmed(left_half, 1, 40)], &[1, 3, 4]), Some(40));
    }

    #[test]
    fn empty_or_blank_text_is_not_a_note() {
        let r = rec();
        let blank = pinned(Shape::Bubble { rect: Bounds { x0: 50.0, y0: 40.0, x1: 90.0, y1: 60.0 }, tail: pt(25.0, 20.0), text: " \n\t".into() }, 1);
        let l = step_layer(&r, &edit(&r), &[text(10.0, 20.0, "", 1), text(10.0, 30.0, "   ", 1), blank], 1);
        assert!(l.notes.is_empty(), "{:?}", l.notes);
    }

    #[test]
    fn a_deleted_step_frame_drops_its_notes_and_undeleting_brings_them_back() {
        let r = rec();
        let items = [text(10.0, 20.0, "Sticky", 1)];
        let mut e = edit(&r);
        e.deleted.insert(1);
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        write_bundle(&r, &items, &clicks(), &e, "t", &gone, flat, |_| true).unwrap();
        assert!(!std::fs::read_to_string(gone.join("tour.json")).unwrap().contains("Sticky"));
        e.deleted.clear();
        let back = tmp.path().join("back");
        write_bundle(&r, &items, &clicks(), &e, "t", &back, flat, |_| true).unwrap();
        assert_eq!(read_json(&back)["steps"][0]["notes"][0]["text"], "Sticky");
    }

    #[test]
    fn preview_draws_the_layer_from_data_as_plain_text() {
        assert!(PREVIEW_TEMPLATE.contains("spotlight_dim") && PREVIEW_TEMPLATE.contains(".notes"));
        assert!(PREVIEW_TEMPLATE.contains("textContent"));
        for banned in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write"] {
            assert!(!PREVIEW_TEMPLATE.contains(banned), "{banned} must not be used");
        }
        assert!(PREVIEW_TEMPLATE.contains("prefers-reduced-motion"));
    }

    #[test]
    fn preview_notes_are_clipped_to_the_image_and_announced() {
        assert!(PREVIEW_TEMPLATE.contains(r#"<div id="layer" aria-live="polite">"#));
        assert!(PREVIEW_TEMPLATE.contains("#layer { position: absolute; inset: 0; z-index: 2; overflow: hidden;"));
    }

    #[test]
    fn note_text_cannot_break_out_of_the_preview_page() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        let items = [text(10.0, 20.0, "</script><img src=x onerror=alert(1)>", 1)];
        write_bundle(&r, &items, &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(!html.contains("</script><img"));
        assert_eq!(read_json(&dir)["steps"][0]["notes"][0]["text"], "</script><img src=x onerror=alert(1)>");
    }

    const HOSTILE: &str = "</script><img src=x onerror=alert(1)>";
    fn card(title: &str, body: &str) -> Card { Card { title: title.into(), body: body.into(), next_label: String::new(), anchor: Anchor::Hotspot, point: None, placement: Placement::Auto } }

    #[test]
    fn a_plain_recording_writes_version_3_with_only_defaults() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let j = read_json(&dir);
        assert_eq!(j["version"], 3);
        assert_eq!(j["theme"], serde_json::json!({"card_style":"dark","accent":"#5dcaa5","hotspot":"pulse","progress_bar":true,"backdrop":"auto","spotlight_soft":true}));
        assert_eq!(j["zoom"], serde_json::json!({"enabled":true,"percent":118}));
        assert_eq!(j["autoplay"], serde_json::json!({"enabled":false,"seconds":5}));
        assert!(j["cover"].is_null() && j["cta"].is_null() && j["end"].is_null());
        for s in j["steps"].as_array().unwrap() {
            assert!(s["card"].is_null() && s["zoom"].is_null());
            assert_eq!(s["advance"], "hotspot");
        }
        assert_eq!(j["steps"][0]["click"]["x"], 50.0); // unchanged from version 2
    }

    #[test]
    fn cards_export_per_step_with_their_anchor_resolved() {
        let r = rec();
        let mut e = edit(&r);
        e.tour_doc.steps.insert(1, StepExtras { card: Some(card("Unpaid invoices", "One click away.\n**Try it**")), advance: Advance::Anywhere, zoom: Some(false) });
        let mut end = card("Done", "");
        end.next_label = "Finish".into();
        e.tour_doc.steps.insert(4, StepExtras { card: Some(end), ..Default::default() }); // the end step has no click
        let mut picked = card("Here", "");
        picked.anchor = Anchor::Point;
        picked.point = Some(Point { x: 20.0, y: 40.0 });
        e.tour_doc.steps.insert(3, StepExtras { card: Some(picked), ..Default::default() });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).unwrap();
        let j = read_json(&dir);
        assert_eq!(j["steps"][0]["card"], serde_json::json!({"title":"Unpaid invoices","body":"One click away.\n**Try it**","next_label":"Next","anchor":"hotspot","point":null,"placement":"auto"}));
        assert_eq!(j["steps"][0]["advance"], "anywhere");
        assert_eq!(j["steps"][0]["zoom"], false);
        assert_eq!(j["steps"][1]["card"]["anchor"], "point");
        assert_eq!(j["steps"][1]["card"]["point"], serde_json::json!({"x":20.0,"y":50.0}));
        assert_eq!(j["steps"][2]["card"]["anchor"], "none");
        assert_eq!(j["steps"][2]["card"]["next_label"], "Finish");
        // No card ever reaches the pixels.
        let img = image::open(dir.join("step-001.png")).unwrap().to_rgba8();
        assert!(img.pixels().all(|p| p.0 == [200, 200, 200, 255]));
    }

    #[test]
    fn a_card_is_dropped_when_empty_and_its_point_when_cropped_away() {
        let r = rec();
        let mut e = edit(&r);
        e.crop = Some(Bounds { x0: 50.0, y0: 0.0, x1: 100.0, y1: 80.0 });
        e.tour_doc.steps.insert(1, StepExtras { card: Some(card("  ", "")), ..Default::default() });
        assert_eq!(step_card(&r, &e, 1, Some((50.0, 50.0))), None);
        let mut c = card("Here", "");
        c.anchor = Anchor::Point;
        c.point = Some(Point { x: 20.0, y: 40.0 }); // left of the crop
        e.tour_doc.steps.insert(1, StepExtras { card: Some(c), ..Default::default() });
        assert_eq!(step_card(&r, &e, 1, Some((50.0, 50.0))).unwrap().anchor, Anchor::None);
    }

    #[test]
    fn only_https_links_survive() {
        assert_eq!(safe_url(" https://tours.example.com/book "), Some("https://tours.example.com/book".into()));
        for bad in ["javascript:alert(1)", "http://tours.example.com", "https://", "https://a b.example", "//tours.example.com", "data:text/html,x", "HTTPS://\u{0}x"] {
            assert_eq!(safe_url(bad), None, "{bad}");
        }
        assert_eq!(safe_url(&format!("https://tours.example.com/{}", "a".repeat(500))), None);
        for bad in ["https://@", "https://:443", "https://user:pw@evil.example", "https://tours.example.com@evil.example/", "https://tours\u{202E}.example.com", "https://tours\u{200B}example.com", "https://tours.example.com/\u{202E}x", "https://tours..example.com", "https://.example.com", "https://tours.example.com:", "https://tours.example.com:123456", "https://tours.example.com:44a", "https://tours_example.com", "https://tours.example.com\\@evil.example"] {
            assert_eq!(safe_url(bad), None, "{bad:?}");
        }
        assert_eq!(safe_url("HTTPS://Tours.example.com:8443/a?b=c#d"), Some("https://Tours.example.com:8443/a?b=c#d".into()));
        assert_eq!(safe_url("https://localhost"), None);
    }

    #[test]
    fn text_is_cleaned_before_it_is_capped() {
        let r = rec();
        let mut e = edit(&r);
        let mut c = card("A\u{202E}B\r\nC\u{0}\u{FEFF}", "x\r\ny\rz\u{2066}\n**w**");
        c.next_label = "Go\non\u{200B}".into();
        e.tour_doc.steps.insert(1, StepExtras { card: Some(c), ..Default::default() });
        let got = step_card(&r, &e, 1, Some((50.0, 50.0))).unwrap();
        assert_eq!((got.title.as_str(), got.body.as_str(), got.next_label.as_str()), ("AB C", "x\ny\nz\n**w**", "Go on"));
        // Caps count code points after cleaning: invisible characters never use up the cap.
        let long = format!("{}{}", "\u{200B}".repeat(10), "a".repeat(90));
        e.tour_doc.steps.insert(1, StepExtras { card: Some(card(&long, "")), ..Default::default() });
        assert_eq!(step_card(&r, &e, 1, None).unwrap().title, "a".repeat(80));
        let mut d = TourDoc::default();
        d.cover = Some(Cover { title: "Line\none".into(), body: "Para\r\ntwo".into(), button: "Sta\u{202D}rt\n".into(), logo: None });
        d.cta = Some(Link { label: "Book\r\na call".into(), url: "https://tours.example.com".into() });
        d.end = Some(EndCard { text: "Thanks\r\nBye\u{0}".into(), primary: None, secondary: None, replay: true });
        let s = sanitize(&d);
        let cover = s.cover.unwrap();
        assert_eq!((cover.title.as_str(), cover.body.as_str(), cover.button.as_str()), ("Line one", "Para\ntwo", "Start"));
        assert_eq!(s.cta.unwrap().label, "Book a call");
        assert_eq!(s.end.unwrap().text, "Thanks\nBye");
    }

    #[test]
    fn sanitize_cuts_clamps_and_drops() {
        let mut d = TourDoc::default();
        d.theme.accent = "RED;background:url(//x)".into();
        d.theme.backdrop = "#AABBCC".into();
        d.zoom.percent = 400;
        d.autoplay.seconds = 0;
        d.cta = Some(Link { label: "Book a call".into(), url: "javascript:alert(1)".into() });
        d.cover = Some(Cover { title: " ".into(), body: "x".into(), button: String::new(), logo: None });
        d.end = Some(EndCard { text: "t".repeat(300), primary: Some(Link { label: "L".repeat(40), url: "https://tours.example.com".into() }), secondary: Some(Link { label: "x".into(), url: "http://tours.example.com".into() }), replay: true });
        let s = sanitize(&d);
        assert_eq!(s.theme.accent, "#5dcaa5");
        assert_eq!(s.theme.backdrop, "#aabbcc");
        assert_eq!((s.zoom.percent, s.autoplay.seconds), (140, 2));
        assert!(s.cta.is_none() && s.cover.is_none());
        let end = s.end.unwrap();
        assert_eq!(end.text.chars().count(), 200);
        assert_eq!(end.primary.unwrap().label.chars().count(), 24);
        assert!(end.secondary.is_none());
    }

    #[test]
    fn hostile_text_is_kept_as_data_in_every_new_field() {
        let r = rec();
        let mut e = edit(&r);
        let mut c = card(HOSTILE, HOSTILE);
        c.next_label = "<b>go</b>".into();
        e.tour_doc.steps.insert(1, StepExtras { card: Some(c), ..Default::default() });
        e.tour_doc.cover = Some(Cover { title: HOSTILE.into(), body: HOSTILE.into(), button: "<i>".into(), logo: None });
        e.tour_doc.cta = Some(Link { label: HOSTILE.chars().take(24).collect(), url: "https://tours.example.com".into() });
        let short: String = HOSTILE.chars().take(24).collect();
        let button = |url: &str| Some(Link { label: short.clone(), url: url.into() });
        e.tour_doc.end = Some(EndCard { text: HOSTILE.into(), primary: button("https://tours.example.com/a"), secondary: button("https://tours.example.com/b"), replay: true });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).unwrap();
        let j = read_json(&dir);
        assert_eq!(j["steps"][0]["card"]["title"], HOSTILE);
        assert_eq!(j["steps"][0]["card"]["body"], HOSTILE);
        assert_eq!(j["steps"][0]["card"]["next_label"], "<b>go</b>");
        assert_eq!(j["cover"]["title"], HOSTILE);
        assert_eq!(j["cover"]["body"], HOSTILE);
        assert_eq!(j["cover"]["button"], "<i>");
        assert_eq!(j["cta"]["label"], short.as_str());
        assert_eq!(j["end"]["text"], HOSTILE);
        assert_eq!(j["end"]["primary"]["label"], short.as_str());
        assert_eq!(j["end"]["secondary"]["label"], short.as_str());
        assert!(!std::fs::read_to_string(dir.join("index.html")).unwrap().contains("</script><img"));
    }

    #[test]
    fn the_logo_is_checked_shrunk_and_named() {
        let tmp = tempfile::tempdir().unwrap();
        let html = tmp.path().join("logo.png");
        std::fs::write(&html, b"<html>").unwrap();
        assert!(load_logo(&html).unwrap_err().contains("PNG or JPEG"));
        let big = tmp.path().join("big.png");
        RgbaImage::from_pixel(4000, 1000, image::Rgba([10, 20, 30, 255])).save(&big).unwrap();
        let (bytes, ext) = load_logo(&big).unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!((img.width(), img.height(), ext), (512, 128, "png"));
        assert!(bytes.len() <= LOGO_MAX_BYTES);
        // A bad logo stops the export before any folder exists.
        let r = rec();
        let mut e = edit(&r);
        e.tour_doc.cover = Some(Cover { title: "Hi".into(), body: String::new(), button: String::new(), logo: None });
        e.tour_doc.logo_path = Some(html);
        let dir = tmp.path().join("t");
        assert!(write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).is_err());
        assert!(!dir.exists());
        // A good one lands in the bundle and in the manifest.
        e.tour_doc.logo_path = Some(big);
        write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).unwrap();
        assert_eq!(read_json(&dir)["cover"]["logo"], "logo.png");
        assert!(dir.join("logo.png").exists());
    }

    #[test]
    fn a_logo_over_the_byte_cap_after_shrinking_is_refused_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let noisy = tmp.path().join("noisy.png");
        let mut s = 7u32;
        RgbaImage::from_fn(512, 512, |_, _| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            image::Rgba([(s >> 24) as u8, (s >> 16) as u8, (s >> 8) as u8, 255])
        })
        .save(&noisy)
        .unwrap();
        assert!(load_logo(&noisy).unwrap_err().contains("200 KB"));
        let r = rec();
        let mut e = edit(&r);
        e.tour_doc.cover = Some(Cover { title: "Hi".into(), body: String::new(), button: String::new(), logo: None });
        e.tour_doc.logo_path = Some(noisy);
        let dir = tmp.path().join("t");
        assert!(write_bundle(&r, &[], &clicks(), &e, "t", &dir, flat, |_| true).is_err());
        assert!(!dir.exists());
    }

    #[test]
    fn a_logo_over_8192_pixels_is_refused_before_decoding() {
        let tmp = tempfile::tempdir().unwrap();
        let wide = tmp.path().join("wide.png");
        RgbaImage::from_pixel(8193, 2, image::Rgba([1, 2, 3, 255])).save(&wide).unwrap();
        assert!(load_logo(&wide).unwrap_err().contains("8192"));
    }

    #[test]
    fn a_jpeg_logo_is_turned_upright_by_its_exif_orientation() {
        use image::ImageEncoder;
        let tmp = tempfile::tempdir().unwrap();
        let photo = tmp.path().join("photo.jpg");
        // A little-endian TIFF block with one entry: Orientation (0x0112) = 6, rotate 90 degrees clockwise.
        let exif: Vec<u8> = [&b"II*\0"[..], &8u32.to_le_bytes(), &1u16.to_le_bytes(), &0x0112u16.to_le_bytes(), &3u16.to_le_bytes(), &1u32.to_le_bytes(), &6u16.to_le_bytes(), &[0, 0], &0u32.to_le_bytes()].concat();
        let rgb = image::RgbImage::from_pixel(40, 20, image::Rgb([90, 120, 150]));
        let mut bytes = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90);
        enc.set_exif_metadata(exif).unwrap();
        enc.write_image(rgb.as_raw(), 40, 20, image::ExtendedColorType::Rgb8).unwrap();
        std::fs::write(&photo, &bytes).unwrap();
        let (out, ext) = load_logo(&photo).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height(), ext), (20, 40, "jpg"));
    }

    #[test]
    fn preview_inlines_the_shared_rules_and_uses_them() {
        let r = rec();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t");
        write_bundle(&r, &[], &clicks(), &edit(&r), "t", &dir, flat, |_| true).unwrap();
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(!html.contains("/*TOUR_CORE*/") && html.contains("placeCard") && html.contains("zoomOffset"));
        for needed in ["TourCore.safeUrl", "TourCore.parseBold", "prefers-reduced-motion", "noopener noreferrer", "cubic-bezier(.22,.8,.26,1)"] {
            assert!(PREVIEW_TEMPLATE.contains(needed), "{needed}");
        }
        for banned in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "localStorage", "sessionStorage", "document.cookie", "fetch(", "XMLHttpRequest"] {
            assert!(!PREVIEW_TEMPLATE.contains(banned), "{banned} must not be used");
        }
    }

    #[test]
    fn preview_still_reads_a_version_2_manifest() {
        // The page must not require any version 3 field.
        for field in ["TOUR.theme.", "TOUR.zoom.", "TOUR.autoplay.", "TOUR.cover.", "TOUR.end.", "TOUR.cta."] {
            assert!(!PREVIEW_TEMPLATE.contains(field), "{field} is read without a fallback");
        }
    }

    #[test]
    fn the_shared_rules_cannot_break_out_of_their_script_block() {
        // Inlined as-is, so the rules file must never close its script or hold the data placeholder.
        let lower = TOUR_CORE_JS.to_ascii_lowercase();
        assert!(!lower.contains("</script") && !lower.contains("<!--"));
        assert!(!TOUR_CORE_JS.contains(JSON_PLACEHOLDER) && !TOUR_CORE_JS.contains(CORE_PLACEHOLDER));
        // The rules come before the page script that uses them, and a title holding the
        // placeholder text stays data.
        let page = preview_page(r#"{"title":"/*TOUR_CORE*/","steps":[]}"#);
        let core_at = page.find("var TourCore").unwrap();
        let data_at = page.find("const TOUR = ").unwrap();
        assert!(core_at < data_at);
        assert!(page.contains(r#""title":"/*TOUR_CORE*/""#));
    }

    #[test]
    fn preview_links_open_safely_and_only_known_logo_names_load() {
        assert!(PREVIEW_TEMPLATE.contains(r#""_blank""#) && PREVIEW_TEMPLATE.contains("no-referrer"));
        assert!(PREVIEW_TEMPLATE.contains(r#""logo.png""#) && PREVIEW_TEMPLATE.contains(r#""logo.jpg""#));
        // Autoplay has a visible, keyboard-reachable pause control.
        assert!(PREVIEW_TEMPLATE.contains(r#"<button id="autoplay""#));
        assert!(PREVIEW_TEMPLATE.contains("Pause autoplay"));
        // Kept from version 2.
        assert!(PREVIEW_TEMPLATE.contains("Local preview"));
    }

    #[test]
    fn preview_allows_no_network_and_only_its_own_step_images() {
        let csp = PREVIEW_TEMPLATE.split("Content-Security-Policy\" content=\"").nth(1).expect("a CSP meta tag").split('"').next().unwrap();
        assert!(PREVIEW_TEMPLATE.find("Content-Security-Policy").unwrap() < PREVIEW_TEMPLATE.find("<script").unwrap());
        for part in ["default-src 'none'", "script-src 'unsafe-inline'", "style-src 'unsafe-inline'", "img-src 'self'", "base-uri 'none'", "form-action 'none'"] {
            assert!(csp.contains(part), "{part}");
        }
        assert!(!csp.contains("http") && !csp.contains('*') && !csp.contains("data:") && !csp.contains("connect-src"), "{csp}");
        // Step images are taken only by the bundle's own names, in the page and in the preload.
        assert!(PREVIEW_TEMPLATE.contains(r"/^step-[0-9]{3,}\.(png|jpg)$/"));
        assert!(!PREVIEW_TEMPLATE.contains("s.image;") && !PREVIEW_TEMPLATE.contains("= ahead.image"));
    }

    #[test]
    fn preview_handles_broken_tours_and_keeps_controls_reachable() {
        // No steps, a missing picture, and a card whose controls are inert until it is visible.
        assert!(PREVIEW_TEMPLATE.contains("This tour has no steps."));
        assert!(PREVIEW_TEMPLATE.contains(r#"addEventListener("error""#) && PREVIEW_TEMPLATE.contains(r#"<div id="missing" hidden>"#));
        assert!(PREVIEW_TEMPLATE.contains("card.inert = true") && PREVIEW_TEMPLATE.contains("card.inert = false"));
        // Leaving the end card backwards shows the last step, and focus follows the control used.
        assert!(PREVIEW_TEMPLATE.contains("if (d < 0 && done) { done = false; render(); return; }"));
        assert!(PREVIEW_TEMPLATE.contains(r#"go(-1, false, "back")"#) && PREVIEW_TEMPLATE.contains(r#"go(1, false, "next")"#));
        // A hidden tab holds autoplay.
        assert!(PREVIEW_TEMPLATE.contains(r#"addEventListener("visibilitychange""#) && PREVIEW_TEMPLATE.contains("document.hidden"));
    }
}
