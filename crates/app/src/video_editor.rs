//! The editor's video mode: a recording on the canvas one frame at a time,
//! with a timeline, playback and an export panel (screen 4 of the mockup).
//!
//! Annotations are the editor's own items with a frame range; the preview and
//! every export draw through `video::decorate`, so they match.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use mythicshot_destinations::{self as dest, Context};
use mythicshot_editor::overlays::{ClickStyle, InputEvent, KeyStyle, OverlayStyle};
use mythicshot_recording::export::{Format, ffmpeg_available};
use mythicshot_recording::{FrameReader, Recording};

use crate::editor_ui::Editor;
use crate::icons::{self, Icon};
use crate::publish::{Published, Target};
use crate::publish_ui::{self, Picker, PickerOutcome, Run};
use crate::recorder_ui::{EncodeJob, ExportSpec};
use crate::site::{self, SiteSummary};
use crate::site_ui::{SiteDialog, SiteOutcome};
use crate::settings_ui::{click_style_label, key_style_label};
use crate::tour::{self, StepPlan, TourSummary};
use crate::video::{self, Edit};

const AMBER: Color32 = Color32::from_rgb(245, 165, 36);
const MUTED: Color32 = Color32::from_rgb(163, 170, 184);
/// Width of the grips at each end of an annotation's range bar.
const GRIP: f32 = 12.0;
/// Height of one row of annotation bars.
const LANE: f32 = 20.0;
/// How long a new annotation shows.
const DEFAULT_SPAN_MS: u64 = 1000;

#[derive(Clone, Copy, PartialEq)]
enum TimelineDrag {
    Playhead,
    In,
    Out,
    /// An annotation's range: which end (or the whole bar), its range when the
    /// drag began, and for a move, the frame offset of the pointer in the bar.
    ItemStart(u64, (u32, u32)),
    ItemEnd(u64, (u32, u32)),
    ItemMove(u64, (u32, u32), i64),
}

/// One annotation's bar on the timeline.
struct Bar {
    id: u64,
    range: (u32, u32),
    lane: usize,
    label: String,
    color: Color32,
}

/// Give each annotation a row: the first row where it overlaps nothing.
/// Items pinned to a tour step belong to the Tour view and get no bar.
fn lay_out_bars(items: &[mythicshot_editor::Item]) -> (Vec<Bar>, usize) {
    let mut ranged: Vec<&mythicshot_editor::Item> = items.iter().filter(|it| it.frames.is_some() && it.pin.is_none()).collect();
    ranged.sort_by_key(|it| (it.frames.map(|f| f.0), it.id));
    let mut lane_ends: Vec<u32> = Vec::new();
    let mut bars = Vec::new();
    for it in ranged {
        let (a, b) = it.frames.expect("ranged");
        let lane = match lane_ends.iter().position(|end| *end < a) {
            Some(l) => l,
            None => {
                lane_ends.push(0);
                lane_ends.len() - 1
            }
        };
        lane_ends[lane] = b;
        let [r, g, bl, _] = it.style.stroke.0;
        bars.push(Bar { id: it.id, range: (a, b), lane, label: item_label(it), color: Color32::from_rgb(r, g, bl) });
    }
    let lanes = lane_ends.len();
    (bars, lanes)
}

fn item_label(it: &mythicshot_editor::Item) -> String {
    use mythicshot_editor::Shape;
    let short = |t: &str| {
        let t = t.lines().next().unwrap_or("").trim();
        if t.chars().count() > 18 { format!("{}…", t.chars().take(18).collect::<String>()) } else { t.to_string() }
    };
    match &it.shape {
        Shape::Rect(_) => "Rectangle".into(),
        Shape::Ellipse(_) => "Ellipse".into(),
        Shape::Line(..) => "Line".into(),
        Shape::Arrow(..) => "Arrow".into(),
        Shape::Freehand(_) => "Freehand".into(),
        Shape::Text { text, .. } => format!("Text: {}", short(text)),
        Shape::Bubble { text, .. } => format!("Bubble: {}", short(text)),
        Shape::Step { number, .. } => format!("Step {number}"),
        Shape::Highlight(_) => "Highlight".into(),
        Shape::Obfuscate { .. } => "Obfuscate".into(),
        Shape::Image { image, .. } => format!("Image {}×{}", image.width(), image.height()),
        Shape::Circle(_) => "Circle".into(),
        Shape::Group(_) => "Group".into(),
        Shape::Cone(..) => "Cone".into(),
        Shape::Donut(_) => "Donut".into(),
        Shape::Slice(..) => "Pizza slice".into(),
        Shape::Cross(_) => "Cross".into(),
        Shape::Asterisk(_) => "Asterisk".into(),
        Shape::Spotlight { .. } => "Spotlight".into(),
    }
}

/// Where "Export tour" writes: a folder with the page and its pictures beside it, or one HTML file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TourOutput {
    Folder,
    File,
}

/// The export running in the editor: a file (GIF, APNG, MP4, WebM), a tour bundle or single
/// file, a tour added to a site, or a tour being published, with the flag that cancels it.
enum ExportJob {
    File(EncodeJob),
    /// A tour export, with the run that cancels it and that quitting waits for.
    Tour(EncodeJob<TourSummary>, TourOutput, Arc<Run>),
    /// A tour added to a site, cancelled like a tour export, with the site title to remember once it worked.
    Site(EncodeJob<SiteSummary>, Arc<Run>, String),
    Publish(EncodeJob<Published>, Arc<Run>),
}

/// What a finished export made.
enum Exported {
    File(PathBuf, u64),
    Tour(TourSummary, TourOutput),
    Site(SiteSummary, String),
    Published(Published),
}

impl ExportJob {
    /// A tour export or a publish: either one hands "Export and close" over to the file export.
    fn is_tour(&self) -> bool {
        matches!(self, ExportJob::Tour(..) | ExportJob::Site(..) | ExportJob::Publish(..))
    }

    fn is_site(&self) -> bool {
        matches!(self, ExportJob::Site(..))
    }

    fn progress(&self) -> f32 {
        match self {
            ExportJob::File(j) => j.progress(),
            ExportJob::Tour(j, ..) => j.progress(),
            ExportJob::Site(j, ..) => j.progress(),
            ExportJob::Publish(j, _) => j.progress(),
        }
    }

    /// The outcome once the export has finished.
    fn take_result(&self) -> Option<Result<Exported, String>> {
        match self {
            ExportJob::File(j) => j.take_result().map(|r| r.map(|(path, size)| Exported::File(path, size))),
            ExportJob::Tour(j, out, _) => j.take_result().map(|r| r.map(|t| Exported::Tour(t, *out))),
            ExportJob::Site(j, _, title) => j.take_result().map(|r| r.map(|s| Exported::Site(s, title.clone()))),
            ExportJob::Publish(j, _) => j.take_result().map(|r| r.map(Exported::Published)),
        }
    }

    /// For a publish: whether cancel was asked for (None for any other export).
    fn cancelling(&self) -> Option<bool> {
        match self {
            ExportJob::Publish(_, run) => Some(run.is_cancelled()),
            _ => None,
        }
    }

    /// For a tour export or an add to a site: whether cancel was asked for (None for any other export).
    fn tour_cancelling(&self) -> Option<bool> {
        match self {
            ExportJob::Tour(_, _, run) | ExportJob::Site(_, run, _) => Some(run.is_cancelled()),
            _ => None,
        }
    }
}

/// The notification while an export runs. `cancelling` is a publish's cancel state, `tour_cancelling`
/// whether a tour export was asked to stop.
fn progress_message(is_tour: bool, cancelling: Option<bool>, tour_cancelling: bool, progress: f32) -> String {
    match cancelling {
        Some(true) => "Cancelling…".to_string(),
        None if tour_cancelling => "Cancelling…".to_string(),
        Some(false) => format!("Publishing tour… {:.0}%", progress * 100.0),
        None if is_tour => format!("Exporting tour… {:.0}%", progress * 100.0),
        None => format!("Exporting… {:.0}%", progress * 100.0),
    }
}

/// The notification while a tour is being added to a site.
fn site_progress_message(cancelling: bool, progress: f32) -> String {
    if cancelling { "Cancelling…".to_string() } else { format!("Adding to site… {:.0}%", progress * 100.0) }
}

/// The status message once a tour is in its site; the status bar adds "· Show in folder · Open site".
fn site_message(s: &SiteSummary) -> String {
    format!("{} site: {}", if s.replaced { "Replaced in" } else { "Added to" }, step_count(s.tour.steps))
}

/// The sentences after a site add's message: pictures saved at lower quality, and an old copy left behind.
fn site_more(s: &SiteSummary) -> Option<String> {
    let parts: Vec<String> = s.tour.squeezed_note().into_iter().chain(s.note.clone()).collect();
    if parts.is_empty() { None } else { Some(parts.join(" ")) }
}

/// The site folder to remember after an add, or None to keep the remembered one: the test hook's folder
/// stays in memory only.
fn remembered_folder(dir: &Path, hook: Option<&str>) -> Option<String> {
    let folder = dir.to_string_lossy().into_owned();
    if hook.is_some_and(|h| Path::new(h.trim()) == dir) { None } else { Some(folder) }
}

/// The time a site's list records for a tour: UTC to the second.
fn utc_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Share of a publish's progress spent writing the bundle; the upload takes the rest.
const PUBLISH_WRITE_SHARE: f32 = 0.3;

/// Run a publish's work so that it always ends: a panic becomes a failed publish, and
/// the run is marked finished either way.
fn guarded<T>(run: &Arc<Run>, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    guarded_as(run, "Publishing failed because of an internal error.", work)
}

/// [`guarded`] with the message a panic becomes.
fn guarded_as<T>(run: &Arc<Run>, failed: &str, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_| Err(failed.to_string()));
    run.finish();
    result
}

/// A folder for a new tour bundle inside `parent`: `<stem>-tour`, or with
/// `-2`, `-3` and so on when that name is taken.
fn tour_dir(parent: &Path, stem: &str) -> PathBuf {
    let mut dir = parent.join(format!("{stem}-tour"));
    let mut n = 2;
    while dir.exists() {
        dir = parent.join(format!("{stem}-tour-{n}"));
        n += 1;
    }
    dir
}

/// The name a normal export would get, without its folders or extension.
/// Unlike `dest::new_path_with_extension`, this never touches the disk.
fn tour_stem(ctx: &Context, now: chrono::DateTime<chrono::Local>) -> String {
    let rel = dest::expand_pattern(&ctx.pattern, &ctx.title, now);
    let name = rel.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    // Only a real export extension is dropped: a dot in a window title is part of the name.
    let name = match name.rsplit_once('.') {
        Some((stem, ext)) if ["gif", "png", "apng", "mp4", "webm", "jpg", "jpeg"].contains(&ext.to_ascii_lowercase().as_str()) => stem.trim(),
        _ => name,
    };
    if name.is_empty() { "recording".to_string() } else { name.to_string() }
}

/// What the editor does once an export has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterExport {
    Nothing,
    /// The user asked to export and close while a tour was running: the file export starts now.
    StartFileExport,
    Close,
}

/// "Export and close" is only ever satisfied by a file export. A tour that
/// finishes, well or badly, hands over to the file export and never closes the editor.
fn after_export(was_tour: bool, succeeded: bool, close_after_export: bool) -> AfterExport {
    match (close_after_export, was_tour, succeeded) {
        (false, _, _) => AfterExport::Nothing,
        (true, true, _) => AfterExport::StartFileExport,
        (true, false, true) => AfterExport::Close,
        (true, false, false) => AfterExport::Nothing,
    }
}

/// The save dialog's suggested name for a single-file tour: the folder export's name as a file.
fn tour_file_name(stem: &str) -> String {
    format!("{stem}-tour.html")
}

/// A file size for the status bar: KB under a megabyte, else MB with one decimal.
fn size_label(bytes: u64) -> String {
    if bytes < 1_048_576 { format!("{} KB", (bytes + 512) / 1024) } else { format!("{:.1} MB", bytes as f64 / 1_048_576.0) }
}

/// The status message after a single-file tour export; the status bar adds "· Show in folder".
fn tour_file_message(steps: usize, bytes: u64) -> String {
    format!("Tour exported as one file: {} · {}", step_count(steps), size_label(bytes))
}

/// The sentences a tour export's status adds after its message: a single file over
/// [`tour::LARGE_FILE_BYTES`] (`file_bytes` is None for a folder) and pictures saved at lower quality.
fn tour_more(file_bytes: Option<u64>, squeezed: Option<String>) -> Option<String> {
    let large = file_bytes.filter(|&b| b > tour::LARGE_FILE_BYTES).map(|_| "Large file: it may be slow to open and too big to send.".to_string());
    let parts: Vec<String> = large.into_iter().chain(squeezed).collect();
    if parts.is_empty() { None } else { Some(parts.join(" ")) }
}

/// One item of the "Export tour" menu: what it does, with a quieter line saying what you get.
fn menu_item(ui: &mut egui::Ui, label: &str, hint: &str) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    RichText::new(label).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Min);
    RichText::new(format!("\n{hint}")).color(MUTED).size(12.0).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Min);
    ui.add(egui::Button::new(job))
}

fn step_count(n: usize) -> String {
    if n == 1 { "1 step".to_string() } else { format!("{n} steps") }
}

/// The export panel's line under "Tour".
fn tour_steps_label(steps: &[StepPlan]) -> String {
    let last_frame = if steps.last().is_some_and(|s| s.click.is_none()) { ", plus the last frame" } else { "" };
    format!("{} · one per click{last_frame}", step_count(steps.len()))
}

/// The bottom panel's two modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoView {
    Timeline,
    Tour,
}

pub struct VideoState {
    pub rec: Arc<Recording>,
    pub(crate) reader: FrameReader,
    pub view: VideoView,
    /// Index into the planned tour steps of the step shown in the Tour view.
    pub tour_step: usize,
    /// Step thumbnails and the strength slider's drag.
    pub(crate) tour: crate::tour_view::TourState,
    pub events: Vec<InputEvent>,
    pub edit: Edit,
    pub current: usize,
    loaded: Option<usize>,
    /// Playing since this instant, from this output time.
    pub(crate) playing: Option<(Instant, u64)>,
    format: Format,
    quality: u8,
    loop_forever: bool,
    fps: u32,
    ffmpeg: bool,
    job: Option<ExportJob>,
    /// The "Publish tour" dialog, while it is open.
    picker: Option<Picker>,
    /// The "Add to site" dialog, while it is open.
    site_dialog: Option<SiteDialog>,
    pub exported: bool,
    pub close_after_export: bool,
    drag: Option<TimelineDrag>,
    /// Overlay style last reported to the app, to notice changes.
    last_overlays: OverlayStyle,
}

impl Drop for VideoState {
    /// The recording's temp files go with the editor; an export still running
    /// keeps them until it finishes. A publish is cancelled: nobody would see its result.
    fn drop(&mut self) {
        if let Some(ExportJob::Publish(_, run)) = &self.job {
            run.cancel();
        }
        let rec = self.rec.clone();
        match self.job.take() {
            None => rec.delete_files(),
            Some(job) => {
                std::thread::spawn(move || {
                    while job.take_result().is_none() {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    rec.delete_files();
                });
            }
        }
    }
}

impl Editor {
    #[allow(clippy::too_many_arguments)]
    pub fn new_video(
        rec: Arc<Recording>,
        events: Vec<InputEvent>,
        items: Vec<mythicshot_editor::Item>,
        title: String,
        keys: std::collections::BTreeMap<crate::hotkeys::EditorAction, String>,
        dest: Context,
        overlays: OverlayStyle,
        prefs: crate::settings::EditorPrefs,
    ) -> Result<Editor, String> {
        let mut reader = FrameReader::new(rec.clone())?;
        let first = reader.get(0)?;
        let format = match prefs.export_format.as_str() {
            "apng" => Format::Apng,
            "mp4" => Format::Mp4,
            "webm" => Format::Webm,
            _ => Format::Gif,
        };
        let ffmpeg = ffmpeg_available();
        let format = if format.needs_ffmpeg() && !ffmpeg { Format::Gif } else { format };
        let (quality, loop_forever, fps, scale) = (prefs.export_quality, prefs.export_loop, prefs.export_fps, prefs.export_scale);
        let mut editor = Editor::new(first, title, keys, dest, prefs);
        for it in items {
            editor.doc.add_initial(it);
        }
        let mut edit = Edit::new(&rec, overlays);
        edit.scale = scale;
        // The last tour's theme and zoom; cover, buttons, end card and autoplay start empty.
        crate::tour_cards::apply_saved_theme(&mut edit.tour_doc, &editor.prefs.tour_theme);
        editor.video = Some(VideoState {
            rec,
            reader,
            view: VideoView::Timeline,
            tour_step: 0,
            tour: Default::default(),
            events,
            edit,
            current: 0,
            loaded: Some(0),
            playing: None,
            format,
            quality,
            loop_forever,
            fps,
            ffmpeg,
            last_overlays: overlays,
            job: None,
            picker: None,
            site_dialog: None,
            exported: false,
            close_after_export: false,
            drag: None,
        });
        // Test hook: start with one selected annotation, to check the range bar.
        #[cfg(debug_assertions)]
        if std::env::var_os("MYTHICSHOT_TEST_ANNOTATE").is_some() {
            let shape = mythicshot_editor::Shape::Rect(mythicshot_editor::Bounds::new(mythicshot_editor::pt(100.0, 100.0), mythicshot_editor::pt(600.0, 400.0)));
            let id = editor.add_item(shape);
            editor.selected = Some(id);
            let n = editor.video.as_ref().map(|v| v.rec.frames.len() as u32).unwrap_or(1);
            let more = [
                (mythicshot_editor::Shape::Arrow(mythicshot_editor::pt(700.0, 500.0), mythicshot_editor::pt(900.0, 300.0)), (0, n / 2)),
                (mythicshot_editor::Shape::Text { at: mythicshot_editor::pt(200.0, 600.0), text: "Click Save first".into() }, (n / 3, n - 1)),
            ];
            for (shape, range) in more {
                let id = editor.add_item(shape);
                editor.doc.update(id, |it| it.frames = Some(range));
            }
        }
        // Test hook: open in the Tour view with a text, a bubble and a spotlight on the first step.
        #[cfg(debug_assertions)]
        if std::env::var_os("MYTHICSHOT_TEST_TOUR_VIEW").is_some() {
            editor.test_tour_view();
        }
        Ok(editor)
    }

    /// Frame range for a new annotation: about one second from the current
    /// frame, never past the trim end; stretch it on the timeline.
    pub(crate) fn default_range(&self) -> Option<(u32, u32)> {
        let v = self.video.as_ref()?;
        let t0 = v.rec.frames[v.current].t_ms;
        let end = (v.current..=v.edit.trim.1.max(v.current)).take_while(|i| v.rec.frames[*i].t_ms < t0 + DEFAULT_SPAN_MS).last().unwrap_or(v.current);
        Some((v.current as u32, end as u32))
    }

    /// Put the current frame under the annotations if it changed.
    pub(crate) fn sync_frame(&mut self) {
        let Some(v) = self.video.as_mut() else { return };
        if v.loaded == Some(v.current) {
            return;
        }
        let current = v.current;
        match v.reader.get(current) {
            Ok(img) => {
                v.loaded = Some(current);
                self.doc.replace_base(img);
            }
            Err(e) => self.notify(format!("Could not read frame {}: {e}", current + 1)),
        }
    }

    /// Undo and redo restore whichever frame was showing; show the current one again.
    pub(crate) fn reload_frame(&mut self) {
        if let Some(v) = self.video.as_mut() {
            v.loaded = None;
        }
        self.sync_frame();
    }

    /// Playback and export progress, every frame.
    pub(crate) fn video_tick(&mut self, ctx: &egui::Context) {
        // Test hook: export straight away in the given format (gif, apng, mp4, webm).
        #[cfg(debug_assertions)]
        if let Some(f) = std::env::var("MYTHICSHOT_TEST_EXPORT").ok() {
            if let Some(v) = self.video.as_mut() {
                if !v.exported && v.job.is_none() && !v.close_after_export {
                    v.format = match f.as_str() {
                        "apng" => Format::Apng,
                        "mp4" => Format::Mp4,
                        "webm" => Format::Webm,
                        _ => Format::Gif,
                    };
                    v.close_after_export = true;
                    self.start_export(None);
                }
            }
        }
        // Test hook: export the tour once as a single file to the given path, with no dialog.
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os("MYTHICSHOT_TEST_TOUR_FILE") {
            static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if self.video.as_ref().is_some_and(|v| v.job.is_none()) && !STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                self.start_tour_file(PathBuf::from(path));
            }
        }
        let Some(v) = self.video.as_mut() else { return };
        // Overlay style changes become the default for the next recording.
        if v.edit.overlays != v.last_overlays {
            v.last_overlays = v.edit.overlays;
            self.overlay_default = Some(v.edit.overlays);
        }
        let Some(v) = self.video.as_mut() else { return };
        if let Some((since, from)) = v.playing {
            let (frames, end) = video::plan(&v.rec, &v.edit);
            if frames.is_empty() {
                v.playing = None;
            } else {
                let mut t = from + since.elapsed().as_millis() as u64;
                if t >= end {
                    // Loop within the kept frames.
                    v.playing = Some((Instant::now(), 0));
                    t = 0;
                }
                if let Some(f) = frames.iter().rev().find(|f| f.t_ms <= t) {
                    v.current = f.src;
                }
                ctx.request_repaint();
            }
        }
        // The "Publish tour" dialog. "Upload draft" either starts the upload or keeps the
        // dialog open with the reason it cannot.
        match v.picker.as_mut().and_then(|p| p.show(ctx)) {
            None => {}
            Some(PickerOutcome::Cancel) => v.picker = None,
            Some(PickerOutcome::Upload { title, target }) => {
                let has_steps = tour::plan_steps(&v.rec, &v.edit, &v.events).is_ok();
                match publish_ui::publish_blocked(v.job.is_some(), has_steps, &publish_ui::server(), &publish_ui::token_status()) {
                    Some(blocked) => {
                        if let Some(p) = v.picker.as_mut() {
                            p.refuse(blocked.refusal());
                        }
                    }
                    None => {
                        v.picker = None;
                        self.start_publish(title, target);
                    }
                }
            }
        }
        // The "Add to site" dialog. "Add to site" either starts the job or keeps the dialog open with the reason.
        let Some(v) = self.video.as_mut() else { return };
        match v.site_dialog.as_mut().and_then(|d| d.show(ctx)) {
            None => {}
            Some(SiteOutcome::Cancel) => v.site_dialog = None,
            Some(SiteOutcome::Add { folder, title, slug }) => {
                let refusal = if v.job.is_some() {
                    Some("Wait for the running export to finish, then add the tour.".to_string())
                } else {
                    tour::plan_steps(&v.rec, &v.edit, &v.events).err()
                };
                match refusal {
                    Some(why) => {
                        if let Some(d) = v.site_dialog.as_mut() {
                            d.refuse(why);
                        }
                    }
                    None => {
                        v.site_dialog = None;
                        self.start_site(folder, title, slug);
                    }
                }
            }
        }
        let Some(v) = self.video.as_mut() else { return };
        let finished = v.job.as_ref().and_then(|j| j.take_result());
        let progress = v.job.as_ref().map(|j| j.progress());
        let is_tour = v.job.as_ref().is_some_and(|j| j.is_tour());
        let is_site = v.job.as_ref().is_some_and(|j| j.is_site());
        let cancelling = v.job.as_ref().and_then(|j| j.cancelling());
        let tour_cancelling = v.job.as_ref().and_then(|j| j.tour_cancelling()) == Some(true);
        if let Some(r) = finished {
            v.job = None;
            let next = after_export(is_tour, r.is_ok(), v.close_after_export);
            match r {
                Ok(done) => {
                    v.exported = true;
                    self.doc.mark_saved();
                    match done {
                        Exported::File(path, size) => {
                            let msg = format!("Exported {} ({:.1} MB)", path.display(), size as f64 / 1_048_576.0);
                            eprintln!("{msg}");
                            self.notify(msg);
                        }
                        Exported::Tour(tour, TourOutput::Folder) => {
                            let mut msg = format!("Tour exported: {}", step_count(tour.steps));
                            if let Some(more) = tour_more(None, tour.squeezed_note()) {
                                msg = format!("{msg}. {more}");
                            }
                            eprintln!("{msg} ({:.1} MB) in {}", tour.bytes as f64 / 1_048_576.0, tour.path.display());
                            self.notify(msg);
                            crate::platform::open_path(&tour.path.join("index.html"));
                        }
                        // A single file is for sending on: it is not opened, and the status bar offers to show it,
                        // followed by any warning.
                        Exported::Tour(tour, TourOutput::File) => {
                            let msg = tour_file_message(tour.steps, tour.bytes);
                            let more = tour_more(Some(tour.bytes), tour.squeezed_note());
                            eprintln!("{msg} {} at {}", more.as_deref().unwrap_or(""), tour.path.display());
                            self.notify(msg);
                            self.status_reveal = Some(tour.path);
                            self.status_more = more;
                        }
                        // The site's index opens in the browser, and the status bar offers the folder and the site again.
                        Exported::Site(s, title) => {
                            let msg = site_message(&s);
                            eprintln!("{msg} at {} in {}", site::address(&s.slug), s.dir.display());
                            self.notify(msg);
                            let index = s.dir.join("index.html");
                            crate::platform::open_path(&index);
                            self.status_reveal = Some(index.clone());
                            self.status_open = Some(index);
                            self.status_more = site_more(&s);
                            #[cfg(debug_assertions)]
                            let hook = std::env::var("MYTHICSHOT_TEST_SITE").ok();
                            #[cfg(not(debug_assertions))]
                            let hook: Option<String> = None;
                            let folder = remembered_folder(&s.dir, hook.as_deref()).unwrap_or_else(|| self.prefs.site_folder.clone());
                            if self.prefs.site_folder != folder || self.prefs.site_title != title {
                                self.prefs.site_folder = folder;
                                self.prefs.site_title = title;
                                self.prefs_changed = true;
                            }
                        }
                        Exported::Published(p) => {
                            let msg = format!("Tour uploaded as a draft: {}", step_count(p.steps));
                            eprintln!("{msg}");
                            self.notify(msg);
                        }
                    }
                }
                Err(e) if cancelling.is_some() => {
                    let msg = if e == crate::publish::CANCELLED { "Publishing cancelled".to_string() } else { e };
                    eprintln!("Publish tour: {msg}");
                    self.notify(msg);
                }
                Err(e) if is_site => {
                    let msg = if e == "cancelled" { "Adding to site cancelled".to_string() } else { e };
                    eprintln!("Add to site: {msg}");
                    self.notify(msg);
                }
                Err(e) if is_tour => {
                    let msg = if e == "cancelled" { "Tour export cancelled".to_string() } else { e };
                    eprintln!("Tour export: {msg}");
                    self.notify(msg);
                }
                Err(e) => {
                    eprintln!("Export failed: {e}");
                    self.notify(format!("Export failed: {e}"));
                }
            }
            match next {
                AfterExport::Nothing => {}
                AfterExport::StartFileExport => self.start_export(None),
                AfterExport::Close => self.closed = true,
            }
        } else if let Some(p) = progress {
            let msg = if is_site { site_progress_message(tour_cancelling, p) } else { progress_message(is_tour, cancelling, tour_cancelling, p) };
            self.notify(msg);
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        self.sync_frame();
    }

    pub(crate) fn exporting(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.job.is_some())
    }

    /// Export with the panel's settings, to `path` or a new file named by the pattern.
    pub(crate) fn start_export(&mut self, path: Option<PathBuf>) {
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        let path = match path {
            Some(p) => p,
            None => match dest::new_path_with_extension(&self.dest, v.format.extension()) {
                Ok(p) => p,
                Err(e) => return self.notify(e),
            },
        };
        let spec = ExportSpec { format: v.format, quality: v.quality, loop_forever: v.loop_forever, video_fps: v.fps };
        let job = EncodeJob::start(v.rec.clone(), self.doc.items().to_vec(), v.events.clone(), v.edit.clone(), spec, path, false);
        if let Some(v) = self.video.as_mut() {
            v.job = Some(ExportJob::File(job));
        }
        self.notify("Exporting… 0%");
    }

    /// Ask for a folder, then export a tour bundle into a new subfolder of it.
    fn export_tour(&mut self) {
        if let Some(parent) = rfd::FileDialog::new().set_directory(&self.dest.output_dir).pick_folder() {
            self.start_tour(&parent);
        }
    }

    /// Export a tour bundle into a new folder inside `parent`, named like a normal export.
    fn start_tour(&mut self, parent: &Path) {
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        let dir = tour_dir(parent, &tour_stem(&self.dest, chrono::Local::now()));
        self.spawn_tour(TourOutput::Folder, move |rec, items, events, edit, title, frame, progress| tour::write_bundle(rec, items, events, edit, title, &dir, frame, progress));
    }

    /// Run a tour export in the background with a cancel flag that "Cancel export" and quitting set.
    /// A cancelled export removes what it wrote: the bundle's folder, or the single file's temporary file.
    #[allow(clippy::type_complexity)]
    fn spawn_tour(
        &mut self,
        out: TourOutput,
        write: impl FnOnce(&mythicshot_recording::Recording, &[mythicshot_editor::Item], &[InputEvent], &Edit, &str, &mut dyn FnMut(usize) -> Result<mythicshot_editor::RgbaImage, String>, &mut dyn FnMut(f32) -> bool) -> Result<TourSummary, String> + Send + 'static,
    ) {
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        let (rec, items, events, edit, title) = (v.rec.clone(), self.doc.items().to_vec(), v.events.clone(), v.edit.clone(), self.title().to_string());
        let run = Run::begin();
        let worker = run.clone();
        let job = EncodeJob::spawn(move |progress| {
            guarded_as(&worker, "The tour export failed because of an internal error.", || {
                // The editor's reader stays on the UI thread; this one belongs to the export.
                let mut reader = FrameReader::new(rec.clone())?;
                write(&rec, &items, &events, &edit, &title, &mut |i| reader.get(i), &mut |f| {
                    progress(f);
                    !worker.is_cancelled()
                })
            })
        });
        if let Some(v) = self.video.as_mut() {
            v.job = Some(ExportJob::Tour(job, out, run));
        }
        self.notify("Exporting tour… 0%");
    }

    /// Ask a running tour export or add to a site to stop; it stops before its next picture, and an add
    /// leaves the site as it was.
    pub(crate) fn cancel_tour_export(&mut self) {
        let Some(v) = self.video.as_ref() else { return };
        if let Some(ExportJob::Tour(_, _, run) | ExportJob::Site(_, run, _)) = &v.job {
            run.cancel();
            self.notify("Cancelling…");
        }
    }

    /// Ask where to save, then export the tour as one HTML file. The save dialog asks before
    /// replacing a file that exists.
    fn export_tour_file(&mut self) {
        let name = tour_file_name(&tour_stem(&self.dest, chrono::Local::now()));
        if let Some(path) = rfd::FileDialog::new().set_directory(&self.dest.output_dir).set_file_name(&name).add_filter("Web page", &["html"]).save_file() {
            self.start_tour_file(path);
        }
    }

    /// Export the tour as one self-contained HTML file at `path`, replacing it only once finished.
    fn start_tour_file(&mut self, path: PathBuf) {
        self.spawn_tour(TourOutput::File, move |rec, items, events, edit, title, frame, progress| tour::write_single_file(rec, items, events, edit, title, &path, frame, progress));
    }

    /// The "Add to site" dialog is open: the editor takes no shortcuts or canvas input.
    pub(crate) fn site_dialog_open(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.site_dialog.is_some())
    }

    /// Open the "Add to site" dialog with the remembered folder and title, unless an export is running.
    fn open_site_dialog(&mut self) {
        let title = self.title().to_string();
        let (folder, site_title) = (self.prefs.site_folder.clone(), self.prefs.site_title.clone());
        let Some(v) = self.video.as_mut() else { return };
        if v.job.is_some() || v.picker.is_some() || v.site_dialog.is_some() {
            return;
        }
        v.site_dialog = Some(SiteDialog::open(&folder, &site_title, &title));
    }

    /// Add the tour to the site in `folder` at `slug`, in the background: the tour export writes it as a
    /// site tour, with its limits and clean-up, and it is swapped into the site once complete.
    /// "Cancel export" and quitting stop it, and the site is left as it was.
    fn start_site(&mut self, folder: PathBuf, site_title: String, slug: String) {
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        let (rec, items, events, edit, title) = (v.rec.clone(), self.doc.items().to_vec(), v.events.clone(), v.edit.clone(), self.title().to_string());
        let run = Run::begin();
        let worker = run.clone();
        let (now, remembered) = (utc_now(), site_title.clone());
        let job = EncodeJob::spawn(move |progress| {
            guarded_as(&worker, "Adding to site failed because of an internal error.", || {
                // The editor's reader stays on the UI thread; this one belongs to the job.
                let mut reader = FrameReader::new(rec.clone())?;
                site::add_tour(&folder, &site_title, &slug, &title, &now, |dir, assets| {
                    tour::write_tour(&rec, &items, &events, &edit, &title, dir, |i| reader.get(i), |f| {
                        progress(f);
                        !worker.is_cancelled()
                    }, tour::Page::Site(assets))
                })
            })
        });
        if let Some(v) = self.video.as_mut() {
            v.job = Some(ExportJob::Site(job, run, remembered));
        }
        self.notify("Adding to site… 0%");
    }

    /// The "Publish tour" dialog is open: the editor takes no shortcuts or canvas input.
    pub(crate) fn publish_picker_open(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.picker.is_some())
    }

    fn open_publish_picker(&mut self) {
        let title = self.title().to_string();
        let Some(v) = self.video.as_mut() else { return };
        if v.job.is_some() || v.picker.is_some() || v.site_dialog.is_some() {
            return;
        }
        v.picker = Some(Picker::open(publish_ui::server(), title));
    }

    /// Write the tour bundle into a fresh temporary folder and upload it as a draft.
    /// The folder is removed afterwards whether it worked or not.
    fn start_publish(&mut self, title: String, target: Target) {
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        let server = publish_ui::server();
        let (rec, items, events, edit) = (v.rec.clone(), self.doc.items().to_vec(), v.events.clone(), v.edit.clone());
        let run = Run::begin();
        let worker = run.clone();
        let job = EncodeJob::spawn(move |progress| {
            guarded(&worker, || {
                let tmp = tempfile::Builder::new().prefix(publish_ui::TEMP_PREFIX).tempdir().map_err(|e| format!("Could not create a temporary folder: {e}"))?;
                worker.set_dir(Some(tmp.path().to_path_buf()));
                let result = (|| {
                    let dir = tmp.path().join("tour");
                    // The editor's reader stays on the UI thread; this one belongs to the publish.
                    let mut reader = FrameReader::new(rec.clone())?;
                    tour::write_bundle(&rec, &items, &events, &edit, &title, &dir, |i| reader.get(i), |f| {
                        progress(f * PUBLISH_WRITE_SHARE);
                        !worker.is_cancelled()
                    })?;
                    if worker.is_cancelled() {
                        return Err(crate::publish::CANCELLED.to_string());
                    }
                    let token = publish_ui::token_for_job()?;
                    crate::publish::upload_bundle(&server, &token, &dir, &target, |f| {
                        progress(PUBLISH_WRITE_SHARE + f * (1.0 - PUBLISH_WRITE_SHARE));
                        !worker.is_cancelled()
                    })
                })();
                let shown = tmp.path().display().to_string();
                if let Err(e) = tmp.close() {
                    eprintln!("Publish tour: could not remove the temporary folder {shown}: {e}");
                }
                result
            })
        });
        if let Some(v) = self.video.as_mut() {
            v.job = Some(ExportJob::Publish(job, run));
        }
        self.notify("Publishing tour… 0%");
    }

    /// Ask a running publish to stop. The request in flight finishes first, so
    /// "Cancelling…" shows until it does.
    pub(crate) fn cancel_publish(&mut self) {
        let Some(v) = self.video.as_ref() else { return };
        if let Some(ExportJob::Publish(_, run)) = &v.job {
            run.cancel();
            self.notify("Cancelling…");
        }
    }

    fn export_as(&mut self) {
        let Some(v) = self.video.as_ref() else { return };
        let ext = v.format.extension();
        let name = dest::new_path_with_extension(&self.dest, ext).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
        if let Some(p) = rfd::FileDialog::new().set_directory(&self.dest.output_dir).set_file_name(&name).add_filter(v.format.label(), &[ext]).save_file() {
            self.start_export(Some(p));
        }
    }

    /// Copy the current frame as it would export (annotations, overlays, crop).
    pub(crate) fn copy_frame(&mut self) {
        let Some(v) = self.video.as_ref() else { return };
        let img = video::decorate(&v.rec, v.current, self.doc.base().clone(), self.doc.items(), &v.events, &v.edit, true);
        match dest::copy_to_clipboard(&img) {
            Ok(()) => self.notify("Copied this frame to the clipboard"),
            Err(e) => self.notify(e),
        }
    }

    /// The export panel, opened from the toolbar.
    pub(crate) fn export_menu(&mut self, ui: &mut egui::Ui) {
        let exporting = self.exporting();
        let Some(v) = self.video.as_mut() else { return };
        ui.set_min_width(260.0);
        ui.label(RichText::new("Format").strong());
        ui.horizontal(|ui| {
            for f in Format::ALL {
                let ok = !f.needs_ffmpeg() || v.ffmpeg;
                let r = ui.add_enabled(ok, egui::Button::selectable(v.format == f, f.label()));
                let r = if ok { r } else { r.on_disabled_hover_text("Needs ffmpeg installed and on the PATH") };
                if r.clicked() {
                    v.format = f;
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Scale");
            for s in [100, 75, 50, 25] {
                if ui.add(egui::Button::selectable(v.edit.scale == s, format!("{s}%"))).clicked() {
                    v.edit.scale = s;
                }
            }
        });
        match v.format {
            Format::Gif => {
                ui.add(egui::Slider::new(&mut v.quality, 50..=100).text("quality"));
            }
            Format::Mp4 | Format::Webm => {
                ui.add(egui::Slider::new(&mut v.fps, 10..=60).text("fps"));
            }
            Format::Apng => {}
        }
        if matches!(v.format, Format::Gif | Format::Apng) {
            ui.checkbox(&mut v.loop_forever, "Loop forever");
        }
        let (w, h) = video::output_size(&v.rec, &v.edit);
        let (frames, end) = video::plan(&v.rec, &v.edit);
        ui.label(RichText::new(format!("{w} × {h} · {} frames · {:.1} s", frames.len(), end as f64 / 1000.0)).color(MUTED).size(12.0));
        let (tour_line, tour_ok) = match tour::plan_steps(&v.rec, &v.edit, &v.events) {
            Ok(steps) => (tour_steps_label(&steps), true),
            Err(e) => (e, false),
        };
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(!exporting, egui::Button::new(RichText::new("Export").strong()).fill(Color32::from_rgb(47, 111, 235))).clicked() {
                self.start_export(None);
                ui.close();
            }
            if ui.add_enabled(!exporting, egui::Button::new("Export as…")).clicked() {
                self.export_as();
                ui.close();
            }
        });
        ui.separator();
        ui.label(RichText::new("Tour").strong());
        ui.add(egui::Label::new(RichText::new(tour_line).color(MUTED).size(12.0)).wrap());
        let publishing = self.video.as_ref().and_then(|v| v.job.as_ref()).and_then(|j| j.cancelling());
        let tour_running = self.video.as_ref().and_then(|v| v.job.as_ref()).and_then(|j| j.tour_cancelling());
        let blocked = publish_ui::publish_blocked(exporting, tour_ok, &publish_ui::server(), &publish_ui::token_status());
        let mut chosen = None;
        let mut add_to_site = false;
        ui.horizontal(|ui| {
            // A menu with both choices every time; neither is remembered. It opens with Enter, Space
            // or the right arrow, and the arrows move between its items.
            ui.add_enabled_ui(tour_ok && !exporting, |ui| {
                ui.menu_button("Export tour", |ui| {
                    if menu_item(ui, "As a folder…", "A web page with its pictures beside it").clicked() {
                        chosen = Some(TourOutput::Folder);
                        ui.close();
                    }
                    if menu_item(ui, "As a single file…", "One .html file to send to someone").clicked() {
                        chosen = Some(TourOutput::File);
                        ui.close();
                    }
                });
            });
            if ui.add_enabled(tour_ok && !exporting, egui::Button::new("Add to site…")).on_hover_text("Write this tour into a folder of plain web files").clicked() {
                add_to_site = true;
                ui.close();
            }
            let publish = egui::Button::new(RichText::new("Publish tour…").strong()).fill(Color32::from_rgb(47, 111, 235));
            if ui.add_enabled(blocked.is_none(), publish).on_hover_text("Upload this tour to your server as a private draft").clicked() {
                self.open_publish_picker();
                ui.close();
            }
        });
        if add_to_site {
            self.open_site_dialog();
        }
        if let Some(out) = chosen {
            ui.close();
            match out {
                TourOutput::Folder => self.export_tour(),
                TourOutput::File => self.export_tour_file(),
            }
        }
        if let Some(reason) = blocked.and_then(|b| b.reason()) {
            ui.add(egui::Label::new(RichText::new(reason).color(MUTED).size(12.0)).wrap());
        }
        match publishing {
            Some(false) => {
                if ui.button("Cancel publishing").clicked() {
                    self.cancel_publish();
                }
            }
            Some(true) => {
                ui.add_enabled(false, egui::Button::new("Cancelling…"));
            }
            None => {}
        }
        match tour_running {
            Some(false) => {
                if ui.button("Cancel export").clicked() {
                    self.cancel_tour_export();
                }
            }
            Some(true) => {
                ui.add_enabled(false, egui::Button::new("Cancelling…"));
            }
            None => {}
        }
        self.remember_export();
    }

    /// Save the export panel's choices for the next recording.
    fn remember_export(&mut self) {
        let Some(v) = self.video.as_ref() else { return };
        let format = v.format.extension().replace("png", "apng");
        let p = &mut self.prefs;
        let changed = p.export_format != format || p.export_scale != v.edit.scale || p.export_quality != v.quality || p.export_loop != v.loop_forever || p.export_fps != v.fps;
        if changed {
            p.export_format = format;
            p.export_scale = v.edit.scale;
            p.export_quality = v.quality;
            p.export_loop = v.loop_forever;
            p.export_fps = v.fps;
            self.prefs_changed = true;
        }
    }

    /// Controls and the timeline strip, above the status bar.
    pub(crate) fn timeline(&mut self, ui: &mut egui::Ui) {
        if self.video.is_none() {
            return;
        }
        if self.in_tour_view() {
            return self.tour_panel(ui);
        }
        let mut switched = None;
        let selected = self.selected;
        let (bars, lanes) = lay_out_bars(self.doc.items());
        let frame = egui::Frame::new().fill(Color32::from_rgb(27, 31, 39)).inner_margin(egui::Margin::symmetric(8, 6));
        let mut new_range: Option<(u64, (u32, u32))> = None;
        let mut range_drag_started = false;
        let mut select: Option<u64> = None;
        egui::Panel::bottom("editor-timeline").frame(frame).show(ui, |ui| {
            let v = self.video.as_mut().expect("video");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                switched = crate::tour_view::view_switch(ui, v.view);
                ui.separator();
                let (icon, tip) = if v.playing.is_some() { (Icon::Pause, "Pause (Space)") } else { (Icon::Play, "Play (Space)") };
                if icons::button(ui, icon, 26.0, false, tip).clicked() {
                    toggle_play(v);
                }
                if icons::button(ui, Icon::StepBack, 26.0, false, "Previous frame").clicked() {
                    v.playing = None;
                    v.current = v.current.saturating_sub(1);
                }
                if icons::button(ui, Icon::StepForward, 26.0, false, "Next frame").clicked() {
                    v.playing = None;
                    v.current = (v.current + 1).min(v.rec.frames.len() - 1);
                }
                let t = v.rec.frames[v.current].t_ms as f64 / 1000.0;
                ui.label(RichText::new(format!("Frame {} / {} · {t:05.2} s", v.current + 1, v.rec.frames.len())).monospace().color(MUTED));
                ui.separator();
                if ui.button("Start here").on_hover_text("Trim: keep from this frame").clicked() {
                    v.edit.trim.0 = v.current.min(v.edit.trim.1);
                }
                if ui.button("End here").on_hover_text("Trim: keep up to this frame").clicked() {
                    v.edit.trim.1 = v.current.max(v.edit.trim.0);
                }
                let deleted = v.edit.deleted.contains(&v.current);
                if ui.button(if deleted { "Restore frame" } else { "Delete frame" }).clicked() {
                    if deleted {
                        v.edit.deleted.remove(&v.current);
                    } else {
                        v.edit.deleted.insert(v.current);
                    }
                }
                ui.separator();
                egui::ComboBox::from_id_salt("speed").width(64.0).selected_text(format!("{}×", v.edit.speed)).show_ui(ui, |ui| {
                    for s in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
                        ui.selectable_value(&mut v.edit.speed, s, format!("{s}×"));
                    }
                });
                ui.toggle_value(&mut v.edit.reverse, "Reverse");
                ui.separator();
                overlay_controls(ui, &mut v.edit.overlays);
                let no_keys = !v.events.iter().any(|e| matches!(e.kind, mythicshot_editor::overlays::InputKind::KeyDown(_)));
                if no_keys && v.edit.overlays.keys != KeyStyle::Off {
                    ui.label(RichText::new("No key presses were recorded").color(AMBER).size(12.0))
                        .on_hover_text("Key logging is off by default. Turn on Keys on the recording bar (or Record key presses in Settings › Output) before recording.");
                }
                if v.edit.crop.is_some() && ui.button("Clear crop").clicked() {
                    v.edit.crop = None;
                }
            });
            ui.add_space(4.0);

            // The strip: recording time left to right, then one row per lane of annotation bars.
            let rows = lanes.max(1) as f32;
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0 + rows * LANE), Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            let strip = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.min.y + 30.0));
            let band_top = strip.max.y + 4.0;
            let end = v.rec.end_ms.max(1) as f32;
            let n = v.rec.frames.len();
            let x_of = |t: u64| strip.left() + strip.width() * (t as f32 / end);
            let frame_end = |i: usize| v.rec.frames.get(i + 1).map(|f| f.t_ms).unwrap_or(v.rec.end_ms);
            let frame_at = |x: f32| {
                let t = (((x - strip.left()) / strip.width()).clamp(0.0, 1.0) * end) as u64;
                v.rec.frames.iter().rposition(|f| f.t_ms <= t).unwrap_or(0)
            };
            let bar_rect = |bar: &Bar| {
                let y = band_top + bar.lane as f32 * LANE;
                Rect::from_min_max(pos2(x_of(v.rec.frames[bar.range.0 as usize].t_ms), y), pos2(x_of(frame_end(bar.range.1 as usize)).max(x_of(v.rec.frames[bar.range.0 as usize].t_ms) + GRIP * 2.0 + 4.0), y + LANE - 3.0))
            };
            painter.rect_filled(strip, CornerRadius::same(4), Color32::from_rgb(17, 20, 26));
            let kept = Rect::from_min_max(pos2(x_of(v.rec.frames[v.edit.trim.0].t_ms), strip.top()), pos2(x_of(frame_end(v.edit.trim.1)), strip.bottom()));
            painter.rect_filled(kept, 0.0, Color32::from_rgb(52, 59, 72));
            for (i, f) in v.rec.frames.iter().enumerate() {
                let x = x_of(f.t_ms);
                let color = if v.edit.deleted.contains(&i) { Color32::from_rgb(229, 72, 77) } else { Color32::from_rgb(90, 99, 117) };
                painter.vline(x, strip.top() + 20.0..=strip.bottom() - 2.0, Stroke::new(1.0, color));
            }
            for x in [kept.left(), kept.right()] {
                let g = Rect::from_center_size(pos2(x, strip.center().y), vec2(6.0, strip.height() + 4.0));
                painter.rect_filled(g, 2.0, AMBER);
                if resp.hover_pos().is_some_and(|p| g.expand(4.0).contains(p)) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
            }

            // Annotation bars, in their own colour, with grips at both ends.
            if bars.is_empty() {
                painter.text(pos2(rect.left() + 4.0, band_top + LANE / 2.0), egui::Align2::LEFT_CENTER, "Annotations you add show here; drag their ends to set when they appear", egui::FontId::proportional(11.0), MUTED);
            }
            let hover = resp.hover_pos();
            for bar in &bars {
                let r = bar_rect(bar);
                let is_sel = selected == Some(bar.id);
                painter.rect_filled(r, 3.0, bar.color.gamma_multiply(if is_sel { 0.75 } else { 0.4 }));
                painter.rect_stroke(r, 3.0, Stroke::new(if is_sel { 2.0 } else { 1.0 }, if is_sel { Color32::WHITE } else { bar.color }), StrokeKind::Inside);
                let label_clip = Rect::from_min_max(pos2(r.left() + GRIP + 3.0, r.top()), pos2(r.right() - GRIP - 3.0, r.bottom()));
                ui.painter_at(label_clip.intersect(rect)).text(pos2(label_clip.left(), r.center().y), egui::Align2::LEFT_CENTER, &bar.label, egui::FontId::proportional(11.0), Color32::WHITE);
                for x in [r.left() + GRIP / 2.0, r.right() - GRIP / 2.0] {
                    let g = Rect::from_center_size(pos2(x, r.center().y), vec2(GRIP, r.height()));
                    let hot = hover.is_some_and(|p| g.expand(2.0).contains(p));
                    painter.rect_filled(g, 3.0, if hot { Color32::WHITE } else { Color32::from_rgb(200, 210, 230) });
                    for dx in [-2.0, 2.0] {
                        painter.vline(g.center().x + dx, g.center().y - 4.0..=g.center().y + 4.0, Stroke::new(1.5, Color32::from_rgb(27, 31, 39)));
                    }
                    if hot {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                }
                if hover.is_some_and(|p| r.contains(p)) && !hover.is_some_and(|p| p.x < r.left() + GRIP + 2.0 || p.x > r.right() - GRIP - 2.0) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
            }
            let head = x_of(v.rec.frames[v.current].t_ms);
            painter.vline(head, rect.y_range(), Stroke::new(2.0, Color32::WHITE));

            if resp.drag_started() || resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let near = |x: f32| (p.x - x).abs() <= 6.0;
                    let on_bar = bars.iter().rev().find(|b| bar_rect(b).expand(1.0).contains(p));
                    v.drag = Some(if strip.contains(p) && near(kept.left()) {
                        TimelineDrag::In
                    } else if strip.contains(p) && near(kept.right()) {
                        TimelineDrag::Out
                    } else if let Some(bar) = on_bar {
                        let r = bar_rect(bar);
                        select = Some(bar.id);
                        range_drag_started = resp.drag_started();
                        if p.x < r.left() + GRIP + 2.0 {
                            TimelineDrag::ItemStart(bar.id, bar.range)
                        } else if p.x > r.right() - GRIP - 2.0 {
                            TimelineDrag::ItemEnd(bar.id, bar.range)
                        } else {
                            TimelineDrag::ItemMove(bar.id, bar.range, frame_at(p.x) as i64 - bar.range.0 as i64)
                        }
                    } else {
                        TimelineDrag::Playhead
                    });
                    v.playing = None;
                }
            }
            if let (Some(drag), Some(p)) = (v.drag, resp.interact_pointer_pos()) {
                let f = frame_at(p.x);
                match drag {
                    TimelineDrag::Playhead => v.current = f,
                    TimelineDrag::In => v.edit.trim.0 = f.min(v.edit.trim.1),
                    TimelineDrag::Out => v.edit.trim.1 = f.max(v.edit.trim.0),
                    TimelineDrag::ItemStart(id, (_, b)) => new_range = Some((id, ((f as u32).min(b), b))),
                    TimelineDrag::ItemEnd(id, (a, _)) => new_range = Some((id, (a, (f as u32).max(a)))),
                    TimelineDrag::ItemMove(id, (a, b), grab) => {
                        let len = (b - a) as i64;
                        let start = (f as i64 - grab).clamp(0, (n as i64 - 1 - len).max(0));
                        new_range = Some((id, (start as u32, (start + len) as u32)));
                    }
                }
                if matches!(drag, TimelineDrag::In | TimelineDrag::Out) {
                    v.current = f;
                }
            }
            if resp.drag_stopped() || resp.clicked() {
                v.drag = None;
            }
        });
        if let Some(id) = select {
            self.select(Some(id));
        }
        if range_drag_started {
            self.doc.checkpoint();
        }
        if let Some((id, r)) = new_range {
            self.doc.update(id, |it| it.frames = Some(r));
        }
        if let Some(view) = switched {
            self.set_view(view);
        }
    }

    /// Space plays and pauses in video mode; the arrows step frames, or tour steps in the Tour view.
    pub(crate) fn video_keys(&mut self, ctx: &egui::Context) {
        if self.text_editing() || ctx.egui_wants_keyboard_input() {
            return;
        }
        if self.in_tour_view() {
            return self.tour_keys(ctx);
        }
        if let Some(v) = self.video.as_mut() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)) {
                toggle_play(v);
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)) {
                v.playing = None;
                v.current = (v.current + 1).min(v.rec.frames.len() - 1);
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)) {
                v.playing = None;
                v.current = v.current.saturating_sub(1);
            }
        }
    }
}

fn toggle_play(v: &mut VideoState) {
    if v.playing.take().is_some() {
        return;
    }
    let (frames, _) = video::plan(&v.rec, &v.edit);
    let from = frames.iter().find(|f| f.src == v.current).map(|f| f.t_ms).unwrap_or(0);
    v.playing = Some((Instant::now(), from));
}

fn overlay_controls(ui: &mut egui::Ui, o: &mut OverlayStyle) {
    egui::ComboBox::from_id_salt("tl-keys").width(118.0).selected_text(format!("Keys: {}", key_style_label(o.keys))).show_ui(ui, |ui| {
        for k in [KeyStyle::Off, KeyStyle::Chips, KeyStyle::Keyboard] {
            ui.selectable_value(&mut o.keys, k, key_style_label(k));
        }
    });
    egui::ComboBox::from_id_salt("tl-clicks").width(118.0).selected_text(format!("Clicks: {}", click_style_label(o.clicks))).show_ui(ui, |ui| {
        for c in [ClickStyle::Off, ClickStyle::Ripple, ClickStyle::Mouse, ClickStyle::Both] {
            ui.selectable_value(&mut o.clicks, c, click_style_label(c));
        }
    });
    ui.toggle_value(&mut o.pointer, "Pointer");
    ui.toggle_value(&mut o.halo, "Halo");
}

#[cfg(test)]
mod tests {
    use super::*;
    use mythicshot_editor::{Bounds, Document, RgbaImage, Shape, Style, pt};

    #[test]
    fn overlapping_annotations_get_their_own_rows() {
        let mut d = Document::new(RgbaImage::new(10, 10));
        let r = || Shape::Rect(Bounds::new(pt(0.0, 0.0), pt(5.0, 5.0)));
        for range in [(0, 10), (5, 20), (11, 30), (25, 40)] {
            let id = d.add(r(), Style::default());
            d.update(id, |it| it.frames = Some(range));
        }
        let (bars, lanes) = lay_out_bars(d.items());
        let rows: Vec<(u32, usize)> = bars.iter().map(|b| (b.range.0, b.lane)).collect();
        // (0,10) and (11,30) share row 0; (5,20) and (25,40) share row 1.
        assert_eq!(rows, vec![(0, 0), (5, 1), (11, 0), (25, 1)]);
        assert_eq!(lanes, 2);
    }

    #[test]
    fn pinned_items_get_no_annotation_bar() {
        let mut d = Document::new(RgbaImage::new(10, 10));
        let r = || Shape::Rect(Bounds::new(pt(0.0, 0.0), pt(5.0, 5.0)));
        let pinned = d.add(r(), Style::default());
        d.update(pinned, |it| {
            it.frames = Some((0, 4));
            it.pin = Some(2);
        });
        let plain = d.add(r(), Style::default());
        d.update(plain, |it| it.frames = Some((0, 4)));
        let (bars, lanes) = lay_out_bars(d.items());
        assert_eq!(bars.iter().map(|b| b.id).collect::<Vec<_>>(), vec![plain]);
        assert_eq!(lanes, 1);
    }

    #[test]
    fn tour_line_counts_steps_and_names_the_last_frame_only_when_it_is_a_step() {
        let step = |click| StepPlan { src: 0, click, t_ms: 0 };
        assert_eq!(tour_steps_label(&[step(Some((1.0, 1.0))), step(Some((2.0, 2.0))), step(None)]), "3 steps · one per click, plus the last frame");
        assert_eq!(tour_steps_label(&[step(Some((1.0, 1.0))), step(Some((2.0, 2.0)))]), "2 steps · one per click");
        assert_eq!(tour_steps_label(&[step(Some((1.0, 1.0)))]), "1 step · one per click");
    }

    #[test]
    fn only_a_file_export_satisfies_close_after_export() {
        use AfterExport::*;
        // (finished job was a tour, it succeeded, close_after_export) -> what next
        assert_eq!(after_export(false, true, true), Close);
        assert_eq!(after_export(false, false, true), Nothing);
        assert_eq!(after_export(false, true, false), Nothing);
        assert_eq!(after_export(false, false, false), Nothing);
        // A tour never closes the editor: the file export the user asked for starts instead.
        assert_eq!(after_export(true, true, true), StartFileExport);
        assert_eq!(after_export(true, false, true), StartFileExport);
        assert_eq!(after_export(true, true, false), Nothing);
        assert_eq!(after_export(true, false, false), Nothing);
    }

    #[test]
    fn a_panicking_publish_ends_as_a_failure() {
        let run = Run::begin();
        let r: Result<(), String> = guarded(&run, || panic!("boom"));
        assert_eq!(r, Err("Publishing failed because of an internal error.".to_string()));
        let run = Run::begin();
        assert_eq!(guarded(&run, || Ok(3)), Ok(3));
        // A job built on it always gets a result.
        let job = EncodeJob::spawn(|_| guarded(&Run::begin(), || -> Result<u8, String> { panic!("boom") }));
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut result = None;
        while result.is_none() && Instant::now() < deadline {
            result = job.take_result();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(result, Some(Err(_))));
    }

    #[test]
    fn publishing_shows_its_progress_and_then_cancelling() {
        assert_eq!(progress_message(true, Some(false), false, 0.4), "Publishing tour… 40%");
        assert_eq!(progress_message(true, Some(true), false, 0.4), "Cancelling…");
        assert_eq!(progress_message(true, None, false, 0.25), "Exporting tour… 25%");
        assert_eq!(progress_message(true, None, true, 0.25), "Cancelling…", "a tour export being cancelled");
        assert_eq!(progress_message(false, None, false, 0.5), "Exporting… 50%");
    }

    #[test]
    fn adding_to_a_site_shows_its_progress_and_says_what_was_written() {
        assert_eq!(site_progress_message(false, 0.4), "Adding to site… 40%");
        assert_eq!(site_progress_message(true, 0.4), "Cancelling…");
        let tour = TourSummary { path: PathBuf::from("site/tours/a"), steps: 3, bytes: 10, squeezed: 0 };
        let mut s = SiteSummary { dir: PathBuf::from("site"), slug: "a".into(), replaced: false, tours: 1, tour, note: None };
        assert_eq!(site_message(&s), "Added to site: 3 steps");
        assert_eq!(site_more(&s), None);
        s.replaced = true;
        s.tour.steps = 1;
        s.tour.squeezed = 1;
        s.note = Some("The previous version of tours/a/ could not be removed.".into());
        assert_eq!(site_message(&s), "Replaced in site: 1 step");
        assert_eq!(site_more(&s).unwrap(), "1 picture was saved at lower quality to fit. The previous version of tours/a/ could not be removed.");
        // The test hook's folder is never saved.
        assert_eq!(remembered_folder(Path::new("sites/guides"), None).as_deref(), Some("sites/guides"));
        assert_eq!(remembered_folder(Path::new("sites/guides"), Some("sites/guides")), None);
        assert_eq!(remembered_folder(Path::new("sites/guides"), Some("other")).as_deref(), Some("sites/guides"));
        let now = utc_now();
        assert!(now.len() == 20 && now.ends_with('Z') && now.as_bytes()[10] == b'T', "{now}");
    }

    #[test]
    fn tour_stem_is_the_last_pattern_component_and_touches_no_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("captures");
        std::fs::create_dir(&out).unwrap();
        let now = chrono::Local::now();
        let ctx = |pattern: &str| Context { output_dir: out.clone(), pattern: pattern.into(), format: dest::ImageFormat::Png, jpeg_quality: 85, title: "Demo".into() };
        assert_eq!(tour_stem(&ctx("shots/${YYYY}/${title}"), now), "Demo");
        assert_eq!(tour_stem(&ctx("shots\\${title}"), now), "Demo");
        assert_eq!(tour_stem(&ctx("${title}"), now), "Demo");
        assert_eq!(tour_stem(&ctx("${title}.gif"), now), "Demo");
        // A dot that is not an export extension belongs to the name.
        assert_eq!(tour_stem(&ctx("${title} v1.2"), now), "Demo v1.2");
        assert_eq!(tour_stem(&ctx(""), now), "Mythicshot");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
        // A missing output folder is no obstacle and is not created.
        let missing = Context { output_dir: tmp.path().join("gone"), ..ctx("a/${title}") };
        assert_eq!(tour_stem(&missing, now), "Demo");
        assert!(!missing.output_dir.exists());
    }

    #[test]
    fn a_single_file_tour_is_named_like_the_folder_and_reported_with_its_size() {
        assert_eq!(tour_file_name("shot-01"), "shot-01-tour.html");
        assert_eq!(tour_file_message(3, 2_516_582), "Tour exported as one file: 3 steps · 2.4 MB");
        assert_eq!(tour_file_message(1, 50_000), "Tour exported as one file: 1 step · 49 KB");
        assert_eq!(tour_file_message(2, 655_360), "Tour exported as one file: 2 steps · 640 KB");
        assert_eq!((size_label(0), size_label(1_048_575), size_label(1_048_576)), ("0 KB".to_string(), "1024 KB".to_string(), "1.0 MB".to_string()));
    }

    #[test]
    fn a_tour_status_adds_a_large_file_warning_and_squeezed_pictures_after_its_message() {
        let squeezed = Some("2 pictures were saved at lower quality to fit.".to_string());
        assert_eq!(tour_more(Some(tour::LARGE_FILE_BYTES), None), None, "25 MB exactly is not large");
        assert_eq!(tour_more(Some(tour::LARGE_FILE_BYTES + 1), None).unwrap(), "Large file: it may be slow to open and too big to send.");
        assert_eq!(tour_more(Some(30 * 1_048_576), squeezed.clone()).unwrap(), "Large file: it may be slow to open and too big to send. 2 pictures were saved at lower quality to fit.");
        // A folder is never called large.
        assert_eq!(tour_more(None, squeezed.clone()), squeezed);
        assert_eq!(tour_more(None, None), None);
    }

    #[test]
    fn a_single_file_tour_never_closes_the_editor() {
        // It is a tour like the folder export: "Export and close" hands over to the file export.
        assert_eq!(after_export(true, true, true), AfterExport::StartFileExport);
        assert_eq!(after_export(true, false, false), AfterExport::Nothing);
    }

    #[test]
    fn tour_folder_name_never_collides() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(tour_dir(tmp.path(), "shot-01"), tmp.path().join("shot-01-tour"));
        std::fs::create_dir(tmp.path().join("shot-01-tour")).unwrap();
        assert_eq!(tour_dir(tmp.path(), "shot-01"), tmp.path().join("shot-01-tour-2"));
        std::fs::create_dir(tmp.path().join("shot-01-tour-2")).unwrap();
        assert_eq!(tour_dir(tmp.path(), "shot-01"), tmp.path().join("shot-01-tour-3"));
    }
}
